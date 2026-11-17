"""Exercise the region-selection overlay through MCP while the tray app runs.

Start `rec-r app --http` first, then: python scripts/overlay_test.py

The overlay is driven with posted window messages (deterministic, no cursor
control needed) and also once with real input to prove the OS routing works.
"""

import ctypes
import json
import sys
import threading
import time
import urllib.request

URL = "http://127.0.0.1:7391/mcp"
OVERLAY_CLASS = "RecRRegionOverlay"

WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
WM_KEYDOWN = 0x0100
WM_KEYUP = 0x0101
MK_LBUTTON = 0x0001
VK_ESCAPE = 0x1B

user32 = ctypes.WinDLL("user32", use_last_error=True)
# Without this, the OS virtualises coordinates for this DPI-unaware process and
# the overlay would receive 1.25x the posted values on a scaled display.
try:
    user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # PER_MONITOR_AWARE_V2
except Exception:  # pragma: no cover - older Windows
    pass
WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)

_session = {"id": None}
_id = [0]


def lparam(x, y):
    return (y << 16) | (x & 0xFFFF)


def find_overlay(timeout=5.0):
    found = []
    deadline = time.time() + timeout
    while time.time() < deadline:
        found.clear()

        def cb(hwnd, _):
            buf = ctypes.create_unicode_buffer(256)
            user32.GetClassNameW(hwnd, buf, 256)
            if buf.value == OVERLAY_CLASS:
                found.append(hwnd)
            return True

        user32.EnumWindows(WNDENUMPROC(cb), None)
        if found:
            return found[0]
        time.sleep(0.1)
    raise SystemExit(f"overlay window {OVERLAY_CLASS} never appeared")


def drag_via_messages(hwnd, x1, y1, x2, y2):
    user32.PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lparam(x1, y1))
    steps = 8
    for i in range(1, steps + 1):
        x = x1 + (x2 - x1) * i // steps
        y = y1 + (y2 - y1) * i // steps
        user32.PostMessageW(hwnd, WM_MOUSEMOVE, MK_LBUTTON, lparam(x, y))
        time.sleep(0.02)
    user32.PostMessageW(hwnd, WM_LBUTTONUP, 0, lparam(x2, y2))


def post(payload):
    req = urllib.request.Request(URL, data=json.dumps(payload).encode(), method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json, text/event-stream")
    if _session["id"]:
        req.add_header("mcp-session-id", _session["id"])
    resp = urllib.request.urlopen(req, timeout=180)
    sid = resp.headers.get("mcp-session-id")
    if sid:
        _session["id"] = sid
    raw = resp.read().decode("utf-8", "replace")
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


def call(name, **args):
    return rpc("tools/call", {"name": name, "arguments": args})["result"]


def text_of(result):
    for block in result.get("content", []):
        if block.get("type") == "text":
            try:
                return json.loads(block["text"])
            except json.JSONDecodeError:
                return block["text"]
    return None


def run_case(label, drive):
    box = {}

    def worker():
        box["value"] = call("select_region", timeout_sec=30)

    thread = threading.Thread(target=worker, daemon=True)
    thread.start()
    hwnd = find_overlay()
    time.sleep(0.3)
    drive(hwnd)
    thread.join(timeout=25)
    result = text_of(box.get("value", {}))
    print(f"[{label}] {result}")
    return result


def main():
    init = rpc(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "overlay-test", "version": "1"},
        },
    )
    print("server:", init["result"]["serverInfo"]["name"])
    rpc("notifications/initialized", notify=True)

    rect = run_case("drag", lambda h: drag_via_messages(h, 400, 300, 1000, 700))
    assert isinstance(rect, dict) and rect.get("width") == 600 and rect.get("height") == 400, rect

    cancel = run_case(
        "escape",
        lambda h: (
            user32.PostMessageW(h, WM_KEYDOWN, VK_ESCAPE, 0),
            user32.PostMessageW(h, WM_KEYUP, VK_ESCAPE, 0),
        ),
    )
    assert isinstance(cancel, dict) and cancel.get("cancelled") is True, cancel

    def tiny_then_escape(hwnd):
        # A 4x4 drag is below the minimum size, so it must not confirm; Esc cancels.
        drag_via_messages(hwnd, 10, 10, 14, 14)
        time.sleep(0.1)
        user32.PostMessageW(hwnd, WM_KEYDOWN, VK_ESCAPE, 0)

    tiny = run_case("too-small", tiny_then_escape)
    assert isinstance(tiny, dict) and tiny.get("cancelled") is True, tiny
    print("\nall overlay cases passed")


if __name__ == "__main__":
    sys.exit(main())