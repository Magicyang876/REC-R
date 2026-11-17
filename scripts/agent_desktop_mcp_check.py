"""Smoke-test the agent-desktop MCP server over stdio (no GUI, no focus changes).

    python scripts/agent_desktop_mcp_check.py [path-to-agent-desktop.exe]

Starts `agent-desktop mcp`, performs the MCP handshake, lists tools and calls one
read-only tool (`desktop_status`), then shuts the server down.
"""

import json
import os
import subprocess
import sys

DEFAULT = os.path.join(
    os.environ.get("APPDATA", os.path.expanduser("~")),
    "npm",
    "node_modules",
    "agent-desktop",
    "bin",
    "agent-desktop-win32-x64.exe",
)


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else DEFAULT
    if not os.path.exists(exe):
        raise SystemExit(f"not found: {exe}")

    proc = subprocess.Popen(
        [exe, "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        bufsize=1,
    )

    next_id = 0

    def call(method, params=None):
        nonlocal next_id
        next_id += 1
        msg = {"jsonrpc": "2.0", "id": next_id, "method": method}
        if params is not None:
            msg["params"] = params
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
        while True:
            line = proc.stdout.readline()
            if not line:
                raise SystemExit(f"server closed the pipe during {method}")
            line = line.strip()
            if not line or not line.startswith("{"):
                continue  # logs on stdout
            msg = json.loads(line)
            if msg.get("id") == next_id:
                if "error" in msg:
                    raise SystemExit(f"{method} failed: {msg['error']}")
                return msg["result"]

    try:
        init = call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "rec-r-check", "version": "1"},
            },
        )
        info = init["serverInfo"]
        print(f"server: {info.get('name')} {info.get('version')}")
        proc.stdin.write(
            json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"
        )
        proc.stdin.flush()

        tools = call("tools/list")["tools"]
        print(f"tools: {len(tools)}")
        for want in ("desktop_status", "desktop_snapshot", "desktop_click", "desktop_screenshot"):
            names = [t["name"] for t in tools]
            print(f"  {want}: {'yes' if want in names else 'MISSING'}")

        status = call("tools/call", {"name": "desktop_status", "arguments": {}})
        text = " ".join(
            b.get("text", "") for b in status.get("content", []) if b.get("type") == "text"
        )
        print("desktop_status:", text[:200].replace("\n", " ") or json.dumps(status)[:200])
        print("agent-desktop mcp ok")
    finally:
        proc.terminate()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()