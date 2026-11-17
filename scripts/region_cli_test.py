"""Verify `rec-r region` (standalone overlay) returns the dragged rectangle.

Run with no tray app running: python scripts/region_cli_test.py
"""

import ctypes
import json
import subprocess
import time

OVERLAY_CLASS = "RecRRegionOverlay"
WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
MK_LBUTTON = 0x0001

user32 = ctypes.WinDLL("user32", use_last_error=True)
try:
    user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
except Exception:
    pass
WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)


def lparam(x, y):
    return (y << 16) | (x & 0xFFFF)


def find_overlay(timeout=10.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        found = []

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
    raise SystemExit("overlay never appeared")


def main():
    proc = subprocess.Popen(
        ["target/debug/rec-r.exe", "region", "--json"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    hwnd = find_overlay()
    # Give the window time to paint and grab the cursor before injecting input.
    time.sleep(0.6)
    user32.PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lparam(300, 200))
    for i in range(1, 9):
        user32.PostMessageW(hwnd, WM_MOUSEMOVE, MK_LBUTTON, lparam(300 + 50 * i, 200 + 30 * i))
        time.sleep(0.04)
    user32.PostMessageW(hwnd, WM_LBUTTONUP, 0, lparam(700, 440))
    out, err = proc.communicate(timeout=20)
    print("stdout:", out.strip())
    if err.strip():
        print("stderr:", err.strip())
    rect = json.loads(out)
    assert rect["x"] == 300 and rect["y"] == 200 and rect["width"] == 400 and rect["height"] == 240, rect
    print("region CLI ok")


if __name__ == "__main__":
    main()