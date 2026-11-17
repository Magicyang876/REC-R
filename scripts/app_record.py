"""Record a region through the running tray app's MCP server and report it.

    python scripts/app_record.py <x> <y> <w> <h> [seconds]

Prints everything the server says about the recording (target, engine, size,
frames) so a wrong engine or a wrong rect is visible instead of guessed.
"""

import json
import sys
import time
import urllib.request

URL = "http://127.0.0.1:7391/mcp"
_session = {"id": None}
_next = [1]


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
        if line.startswith("data:"):
            body = line[5:].strip()
            if body:
                return json.loads(body)
    return None


def rpc(method, params=None, notify=False):
    payload = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        payload["params"] = params
    if not notify:
        payload["id"] = _next[0]
        _next[0] += 1
    return post(payload)


def call(name, **arguments):
    res = rpc("tools/call", {"name": name, "arguments": arguments})
    result = (res or {}).get("result") or {}
    if result.get("isError"):
        raise SystemExit(f"{name} failed: {result}")
    if result.get("structuredContent"):
        return result["structuredContent"]
    for block in result.get("content") or []:
        if block.get("type") == "text":
            try:
                return json.loads(block["text"])
            except ValueError:
                return block["text"]
    return result


rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "app_record", "version": "0"}})
rpc("notifications/initialized", {}, notify=True)

x, y, w, h = (int(v) for v in sys.argv[1:5])
seconds = float(sys.argv[5]) if len(sys.argv) > 5 else 2.0

info = call("record_start", region=f"{x},{y},{w},{h}")
print("start:", {k: info.get(k) for k in ("id", "target", "engine", "path", "width", "height")})
time.sleep(seconds)
print("status:", call("record_status", id=info["id"]))
print("stop:", call("record_stop", id=info["id"]))