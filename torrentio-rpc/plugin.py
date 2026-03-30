#!/usr/bin/env python3
"""
stui RPC plugin — Torrentio stream provider
============================================

Resolves movie/series IMDB IDs to magnet links and torrent URLs via the
public Torrentio API (https://torrentio.strem.fun).

Capabilities: streams

Usage
-----
Make executable and place in ~/.stui/plugins/torrentio-rpc/:

    chmod +x plugin.py
    mkdir -p ~/.stui/plugins/torrentio-rpc
    cp plugin.py ~/.stui/plugins/torrentio-rpc/

stui detects it within 500ms and hot-loads it.  No restart needed.

Configuration
-------------
Set TORRENTIO_PROVIDERS to a pipe-separated list of debrid/scraper configs:
    export TORRENTIO_PROVIDERS="yts|eztv|rarbg|1337x|thepiratebay"

Set TORRENTIO_DEBRID to use a debrid service (Real-Debrid, AllDebrid, etc.):
    export TORRENTIO_DEBRID="realdebrid=MYTOKEN"

Protocol
--------
Implements the stui JSON-RPC plugin protocol (NDJSON over stdin/stdout):
  handshake        → name, version, capabilities
  streams.resolve  → Vec<Stream> for a given IMDB ID
  shutdown         → graceful exit

The plugin exits cleanly when the runtime sends `shutdown` or closes stdin.
"""

import json
import logging
import os
import re
import sys
import urllib.request
import urllib.error
from typing import Any

# ── Logging ───────────────────────────────────────────────────────────────────
# Write to stderr only — stdout is the JSON-RPC channel.
logging.basicConfig(
    stream=sys.stderr,
    level=logging.DEBUG if os.getenv("STUI_LOG", "").lower() in ("debug", "trace") else logging.WARNING,
    format="[torrentio-rpc] %(levelname)s %(message)s",
)
log = logging.getLogger("torrentio")

# ── Config ────────────────────────────────────────────────────────────────────
BASE_URL         = os.getenv("TORRENTIO_BASE_URL", "https://torrentio.strem.fun")
PROVIDERS        = os.getenv("TORRENTIO_PROVIDERS", "yts|eztv|rarbg|1337x|thepiratebay|kickass|horriblesubs|nyaasi")
DEBRID           = os.getenv("TORRENTIO_DEBRID", "")          # e.g. "realdebrid=TOKEN"
REQUEST_TIMEOUT  = int(os.getenv("TORRENTIO_TIMEOUT", "10"))  # seconds

# Build the config path segment (Torrentio uses /{config}/stream/...)
_config_parts = [f"providers={PROVIDERS}"]
if DEBRID:
    _config_parts.append(DEBRID)
CONFIG_SEGMENT = "|".join(_config_parts)

# ── Helpers ───────────────────────────────────────────────────────────────────

def _fetch_json(url: str) -> Any:
    """Fetch a URL and return parsed JSON, or raise on error."""
    log.debug("GET %s", url)
    req = urllib.request.Request(
        url,
        headers={"User-Agent": "stui/1.0 (github.com/stui/stui)"},
    )
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"HTTP {e.code}: {e.reason}") from e
    except Exception as e:
        raise RuntimeError(str(e)) from e


def _parse_quality(name: str) -> str | None:
    """Extract a quality label from a stream name."""
    for pat, label in [
        (r"2160p|4K|UHD",  "4K"),
        (r"1080p",         "1080p"),
        (r"720p",          "720p"),
        (r"480p",          "480p"),
        (r"SD",            "SD"),
    ]:
        if re.search(pat, name, re.IGNORECASE):
            return label
    return None


def _parse_seeders(name: str) -> int | None:
    """Extract seeder count from a Torrentio stream name (e.g. '👤 1,234')."""
    m = re.search(r"👤\s*([\d,]+)", name)
    if m:
        try:
            return int(m.group(1).replace(",", ""))
        except ValueError:
            pass
    # Also try plain "Seeds: 123"
    m = re.search(r"[Ss]eeds?:?\s*(\d+)", name)
    if m:
        return int(m.group(1))
    return None


def _parse_size(name: str) -> int | None:
    """Extract file size in bytes from a stream name (e.g. '💾 2.3 GB')."""
    m = re.search(r"💾\s*([\d.]+)\s*(GB|MB|KB)", name, re.IGNORECASE)
    if not m:
        m = re.search(r"([\d.]+)\s*(GB|MB|KB)", name, re.IGNORECASE)
    if m:
        num, unit = float(m.group(1)), m.group(2).upper()
        multiplier = {"GB": 1 << 30, "MB": 1 << 20, "KB": 1 << 10}.get(unit, 1)
        return int(num * multiplier)
    return None


# ── Stream resolution ─────────────────────────────────────────────────────────

def resolve_streams(media_id: str) -> list[dict]:
    """
    Resolve a media ID to a list of stream objects.

    media_id formats:
      - "tt0816692"          — bare IMDB ID (movie)
      - "tt0944947:1:1"      — series with season/episode
      - "tmdb:movie:12345"   — TMDB ID (converted to Torrentio format)
    """
    # Determine Torrentio stream type and ID
    if ":" in media_id:
        parts = media_id.split(":")
        if parts[0] == "tmdb":
            log.warning("TMDB IDs not yet supported; skipping")
            return []
        # series:season:episode or provider:namespace:id
        if len(parts) == 3 and parts[0].startswith("tt"):
            imdb_id, season, episode = parts
            stream_type = "series"
            stream_id   = f"{imdb_id}:{season}:{episode}"
        else:
            # Unknown format — try as a bare movie
            imdb_id    = parts[-1]
            stream_type = "movie"
            stream_id   = imdb_id
    else:
        imdb_id    = media_id
        stream_type = "movie"
        stream_id  = imdb_id

    url = f"{BASE_URL}/{CONFIG_SEGMENT}/stream/{stream_type}/{stream_id}.json"

    try:
        data = _fetch_json(url)
    except RuntimeError as e:
        log.error("torrentio fetch failed: %s", e)
        return []

    streams = data.get("streams", [])
    log.debug("torrentio returned %d streams for %s", len(streams), stream_id)

    results = []
    for s in streams:
        name      = s.get("title", s.get("name", "Unknown"))
        url_field = s.get("url") or s.get("infoHash") or ""

        if not url_field:
            continue

        # Torrentio uses infoHash for magnet links
        if s.get("infoHash") and not url_field.startswith("magnet:"):
            trackers  = [
                "udp://tracker.opentrackr.org:1337/announce",
                "udp://open.demonii.com:1337/announce",
                "udp://tracker.openbittorrent.com:80/announce",
            ]
            tracker_str = "&".join(f"tr={t}" for t in trackers)
            url_field   = f"magnet:?xt=urn:btih:{url_field}&dn={urllib.request.quote(name)}&{tracker_str}"

        stream: dict[str, Any] = {
            "url":      url_field,
            "name":     name,
        }

        quality = _parse_quality(name)
        if quality:
            stream["quality"] = quality

        seeders = _parse_seeders(name)
        if seeders is not None:
            stream["seeders"] = seeders

        size = _parse_size(name)
        if size is not None:
            stream["size_bytes"] = size

        # Behavioural hint from Torrentio
        if s.get("behaviorHints", {}).get("bingeGroup"):
            stream["name"] = f"[binge] {name}"

        results.append(stream)

    return results


# ── RPC dispatch ──────────────────────────────────────────────────────────────

def respond(req_id: str, result: Any) -> None:
    msg = json.dumps({"id": req_id, "result": result}, ensure_ascii=False)
    sys.stdout.write(msg + "\n")
    sys.stdout.flush()


def respond_error(req_id: str, code: int, message: str) -> None:
    msg = json.dumps({"id": req_id, "error": {"code": code, "message": message}})
    sys.stdout.write(msg + "\n")
    sys.stdout.flush()


def main() -> None:
    log.info("torrentio-rpc starting")

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue

        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            log.error("invalid JSON from runtime: %s", e)
            continue

        req_id  = req.get("id", "unknown")
        method  = req.get("method", "")
        params  = req.get("params", {})

        log.debug("← %s (id=%s)", method, req_id)

        if method == "handshake":
            respond(req_id, {
                "name":         "torrentio",
                "version":      "1.0.0",
                "capabilities": ["streams"],
                "description":  "Stream provider via Torrentio (torrent magnet links)",
            })

        elif method == "streams.resolve":
            media_id = params.get("id", "")
            if not media_id:
                respond_error(req_id, -32602, "missing 'id' param")
                continue
            try:
                streams = resolve_streams(media_id)
                respond(req_id, streams)
            except Exception as e:
                log.exception("streams.resolve failed")
                respond_error(req_id, -32000, str(e))

        elif method == "shutdown":
            respond(req_id, {})
            log.info("shutdown requested — exiting")
            break

        else:
            # Unknown methods are silently ignored to stay forward-compatible
            log.debug("unknown method '%s' — ignoring", method)
            respond(req_id, None)

    log.info("torrentio-rpc exiting")


if __name__ == "__main__":
    main()
