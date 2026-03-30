# stui plugin — torrentio (RPC)

Resolves movie and series IMDB IDs to magnet links via the public
[Torrentio](https://torrentio.strem.fun) API.

**Type:** RPC plugin (Python 3, no dependencies beyond stdlib)  
**Capability:** `streams`  
**Language:** Python 3.8+

## Install

```bash
mkdir -p ~/.stui/plugins/torrentio-rpc
cp plugin.py plugin.json ~/.stui/plugins/torrentio-rpc/
chmod +x ~/.stui/plugins/torrentio-rpc/plugin.py
# stui hot-loads it within 500ms — no restart needed
```

## Configuration

All configuration is via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `TORRENTIO_BASE_URL` | `https://torrentio.strem.fun` | API base URL |
| `TORRENTIO_PROVIDERS` | `yts\|eztv\|rarbg\|...` | Pipe-separated provider list |
| `TORRENTIO_DEBRID` | _(none)_ | Debrid config, e.g. `realdebrid=TOKEN` |
| `TORRENTIO_TIMEOUT` | `10` | HTTP request timeout in seconds |
| `STUI_LOG` | _(none)_ | Set to `debug` for verbose plugin logging |

### With Real-Debrid

```bash
export TORRENTIO_DEBRID="realdebrid=YOUR_API_TOKEN"
```

### Custom providers

```bash
export TORRENTIO_PROVIDERS="yts|1337x|thepiratebay"
```

## How it works

When stui needs streams for a media item:

1. Runtime sends: `{"id":"req-1","method":"streams.resolve","params":{"id":"tt0816692"}}`
2. Plugin calls `https://torrentio.strem.fun/{config}/stream/movie/tt0816692.json`
3. Plugin parses the response, extracts quality/seeders/size from stream names
4. Plugin returns: `[{"url":"magnet:?xt=...","name":"1080p BluRay","quality":"1080p","seeders":1500}]`

Series work with `tt0944947:1:1` format (IMDB ID : season : episode).

## Debugging

Run the plugin manually to test it:

```bash
echo '{"id":"1","method":"handshake","params":{}}' | python3 plugin.py
echo '{"id":"2","method":"streams.resolve","params":{"id":"tt0816692"}}' | python3 plugin.py
```
