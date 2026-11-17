"""Ask the running tray app (MCP over HTTP) to screenshot a region and save it.

    python scripts/app_shot.py <x> <y> <w> <h> [out.png]

Used to compare what the *app process* sees with what a fresh `rec-r shot`
process sees; a difference there means the app holds a stale capture surface.
"""

import base64
import json
import sys
import urllib.request

URL = "http://127.0.0.1:7391/mcp"
_session = {"id": None}


def post(payload):
    req = urllib.request.Request(URL, data=json.dumps(payload).encode(), method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json, text/event-stream")
    if _session["id"]:
        req.add_header("mcp-session-id", _session["id"])
    resp = urllib.request.urlopen(req, timeout=120)
    sid = resp.headers.get("mcp-session-id")
    if sid:
        _session["id"] = sid
    raw = resp.read().decode("utf-8", "replace")
    if not raw.strip():
        return None
    if raw.lstrip().startswith("{"):
        return json.loads(raw)
    for line in raw.splitlines():
        if not line.startswith("data:"):
            continue
        body = line[5:].strip()
        # The stream starts with an empty `data:` event that only carries the
        # reconnect hint; skipping it is what makes this parser work.
        if body:
            return json.loads(body)
    return None


def rpc(method, params=None, notify=False):
    payload = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        payload["params"] = params
    if not notify:
        payload["id"] = 1
    return post(payload)


rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "app_shot", "version": "0"}})
try:
    rpc("notifications/initialized", {}, notify=True)
except Exception:
    pass

x, y, w, h = (int(v) for v in sys.argv[1:5])
out = sys.argv[5] if len(sys.argv) > 5 else "app-shot.png"
res = rpc("tools/call", {"name": "screenshot", "arguments": {
    "region": f"{x},{y},{w},{h}", "inline": True}})
blocks = (res.get("result") or {}).get("content") or []
for block in blocks:
    if block.get("type") == "image":
        with open(out, "wb") as fh:
            fh.write(base64.b64decode(block["data"]))
        print("saved", out, block.get("mimeType"))
    elif block.get("type") == "text":
        meta = json.loads(block["text"])
        print("app says:", {k: meta.get(k) for k in ("width", "height", "engine", "target", "path")})