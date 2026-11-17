"""Drive the rec-r MCP stdio server directly (no client library needed).

Usage: python scripts/mcp_stdio.py [path-to-rec-r.exe]
"""

import json
import subprocess
import sys

EXE = sys.argv[1] if len(sys.argv) > 1 else "target/debug/rec-r.exe"


class Client:
    def __init__(self, exe):
        self.proc = subprocess.Popen(
            [exe, "serve"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self.seq = 0

    def send(self, payload):
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params=None):
        self.seq += 1
        payload = {"jsonrpc": "2.0", "id": self.seq, "method": method}
        if params is not None:
            payload["params"] = params
        self.send(payload)
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise SystemExit("server closed stdout")
            line = line.strip()
            if not line:
                continue
            msg = json.loads(line)
            if msg.get("id") == self.seq:
                return msg

    def notify(self, method):
        self.send({"jsonrpc": "2.0", "method": method})

    def close(self):
        try:
            self.proc.stdin.close()
        finally:
            self.proc.wait(timeout=10)


def main():
    c = Client(EXE)
    init = c.request(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "stdio-smoke", "version": "1"},
        },
    )
    info = init["result"]["serverInfo"]
    print("server :", info["name"], info["version"])
    c.notify("notifications/initialized")

    tools = c.request("tools/list")["result"]["tools"]
    print("tools  :", ", ".join(t["name"] for t in tools))

    shot = c.request(
        "tools/call", {"name": "screenshot", "arguments": {"target": "display", "monitor": 0}}
    )["result"]
    images = [b for b in shot["content"] if b["type"] == "image"]
    meta = json.loads([b for b in shot["content"] if b["type"] == "text"][0]["text"])
    print(f"shot   : {meta['width']}x{meta['height']} {meta['format']} engine={meta['engine']}")
    print(f"         inline image bytes b64={len(images[0]['data']) if images else 0}")

    status = c.request("tools/call", {"name": "record_status", "arguments": {}})["result"]
    print("status :", status["content"][0]["text"][:120])
    c.close()


if __name__ == "__main__":
    main()