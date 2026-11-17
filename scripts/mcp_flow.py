"""End-to-end MCP flow: screenshot + record the primary display.

Usage: python scripts/mcp_flow.py [http://127.0.0.1:7391/mcp]
"""

import json
import sys
import time

sys.path.insert(0, __file__.rsplit("\\", 1)[0].rsplit("/", 1)[0])
from mcp_smoke import rpc, _session  # noqa: E402


def call(name, **args):
    res = rpc("tools/call", {"name": name, "arguments": args})
    if "error" in res:
        raise SystemExit(f"{name} failed: {res['error']}")
    result = res["result"]
    images = [b for b in result.get("content", []) if b.get("type") == "image"]
    texts = [b["text"] for b in result.get("content", []) if b.get("type") == "text"]
    payload = None
    for text in texts:
        try:
            payload = json.loads(text)
            break
        except json.JSONDecodeError:
            pass
    return result.get("isError"), payload, images, texts


def main():
    init = rpc(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "mcp-flow", "version": "1"},
        },
    )
    print("server:", init["result"]["serverInfo"]["name"], init["result"]["serverInfo"]["version"])
    rpc("notifications/initialized", notify=True)

    err, shot, images, texts = call("screenshot", target="display", monitor=0, inline=True)
    print(f"\nscreenshot isError={err} {shot}")
    for img in images:
        print(f"  image: {img['mimeType']} {len(img['data'])} b64 chars")

    err, rec, _, _ = call("record_start", target="display", monitor=0, fps=30)
    print(f"\nrecord_start isError={err} {json.dumps(rec, ensure_ascii=False)[:400]}")
    rid = (rec or {}).get("id")

    for _ in range(3):
        time.sleep(1)
        err, st, _, _ = call("record_status")
        print("  status:", json.dumps(st, ensure_ascii=False)[:300])

    err, stopped, _, _ = call("record_stop", id=rid)
    print(f"\nrecord_stop isError={err} {json.dumps(stopped, ensure_ascii=False)[:500]}")


if __name__ == "__main__":
    main()