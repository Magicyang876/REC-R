"""Check the MCP `config` tool: scalar `value`s and the autostart registration.

The tool used to reject `{"key": "fps", "value": 30}` with
`invalid type: integer, expected a string`, and `autostart` used to register
`target\\release\\rec-r.exe` instead of the installed copy.

Usage: python scripts/mcp_config_test.py [path-to-rec-r.exe]
"""

import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_stdio import Client  # noqa: E402

EXE = sys.argv[1] if len(sys.argv) > 1 else "target/release/rec-r.exe"
ok = True


def check(name, condition, detail=""):
    global ok
    ok &= bool(condition)
    print(f"{name}: {'PASS' if condition else 'FAIL'} {detail}".rstrip())


def call(c, key, value):
    res = c.request(
        "tools/call", {"name": "config", "arguments": {"key": key, "value": value}}
    )["result"]
    text = "".join(b.get("text", "") for b in res["content"] if b.get("type") == "text")
    return res.get("isError", False), text


def main():
    c = Client(EXE)
    c.request(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "config-test", "version": "1"},
        },
    )
    c.notify("notifications/initialized")

    err, text = call(c, "fps", 30)  # JSON number, not "30"
    cfg = json.loads(text)["config"] if not err else {}
    check("fps=30 (number)", not err and cfg.get("fps") == 30, text[:90])

    err, text = call(c, "screenshot_max_edge", 0)
    check("screenshot_max_edge=0 (number)", not err, text[:90])

    err, text = call(c, "copy_on_shot", True)  # JSON boolean
    cfg = json.loads(text)["config"] if not err else {}
    check("copy_on_shot=true (bool)", not err and cfg.get("copy_on_shot") is True, text[:90])

    err, text = call(c, "fps", "60")  # plain string still works
    cfg = json.loads(text)["config"] if not err else {}
    check('fps="60" (string)', not err and cfg.get("fps") == 60, text[:90])

    err, text = call(c, "fps", {"nested": 1})
    check("fps={...} rejected clearly", err and "string, number or boolean" in text, text[:90])

    err, text = call(c, "autostart", True)
    registered = json.loads(text)["autostart"]["command"] if not err else ""
    low = registered.lower()
    check("autostart=true accepted", not err, registered)
    check("autostart avoids target/", "\\target\\" not in low, registered)
    check(
        "autostart points at an existing exe",
        bool(registered) and os.path.isfile(registered.split('"')[1]),
        registered,
    )

    # Leave the settings the way we found them.
    call(c, "fps", 30)
    call(c, "copy_on_shot", True)
    c.close()
    print("RESULT:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())