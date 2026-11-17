"""Smoke test the rec-r MCP server over streamable HTTP.

Usage: python scripts/mcp_smoke.py [http://127.0.0.1:7391/mcp] [tool ...]
"""

import json
import sys
import urllib.request

URL = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:7391/mcp"
TOOLS = sys.argv[2:]

_session = {"id": None}
_id = [0]


def post(payload):
    body = json.dumps(payload).encode()
    req = urllib.request.Request(URL, data=body, method="POST")
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
    # SSE: take the last data: line that parses as JSON
    out = None
    for line in raw.splitlines():
        if line.startswith("data:") and line[5:].strip():
            try:
                out = json.loads(line[5:].strip())
            except json.JSONDecodeError:
                pass
    return out


def rpc(method, params=None, notify=False):
    payload = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        payload["params"] = params
    if not notify:
        _id[0] += 1
        payload["id"] = _id[0]
    return post(payload)


def main():
    init = rpc(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "mcp-smoke", "version": "1"},
        },
    )
    info = init["result"]["serverInfo"]
    print(f"server  : {info['name']} {info['version']}  ({init['result']['protocolVersion']})")
    print(f"session : {_session['id']}")
    rpc("notifications/initialized", notify=True)

    tools = rpc("tools/list")["result"]["tools"]
    print(f"tools   : {len(tools)}")
    for t in tools:
        print(f"  - {t['name']}: {t['description'].splitlines()[0][:80]}")

    for name in TOOLS:
        print(f"\n==> {name}")
        result = rpc("tools/call", {"name": name, "arguments": {}})["result"]
        for block in result.get("content", []):
            kind = block.get("type")
            if kind == "image":
                print(f"  [image {block.get('mimeType')} {len(block.get('data',''))} b64 chars]")
            elif kind == "text":
                print("  " + block["text"][:1500].replace("\n", "\n  "))
        print(f"  isError={result.get('isError')}")


if __name__ == "__main__":
    main()