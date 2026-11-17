"""Drive the tray UI of a running `rec-r app`: popup menu + menu commands.

Start `rec-r app` first, then: python scripts/tray_test.py
"""

import ctypes
import glob
import os
import subprocess
import sys
import time

HOST_CLASS = "RecRHost"
WM_APP = 0x8000
WM_TRAY = WM_APP + 1
WM_RBUTTONUP = 0x0205
WM_COMMAND = 0x0111
WM_KEYDOWN = 0x0100
VK_ESCAPE = 0x1B

TRAY_UID = 1
ID_SHOT_PRIMARY = 2001
ID_REC_PRIMARY = 3001
ID_REC_STOP = 3004
ID_OPEN_OUTPUT = 4001
ID_DOCTOR = 4003

user32 = ctypes.WinDLL("user32", use_last_error=True)
try:
    user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
except Exception:
    pass

WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)


def find_window(cls):
    found = []
    deadline = time.time() + 5
    while time.time() < deadline:
        found.clear()

        def cb(hwnd, _):
            buf = ctypes.create_unicode_buffer(256)
            user32.GetClassNameW(hwnd, buf, 256)
            if buf.value == cls:
                found.append(hwnd)
            return True

        user32.EnumWindows(WNDENUMPROC(cb), None)
        if found:
            return found[0]
        time.sleep(0.1)
    raise SystemExit(f"window class {cls} not found - is `rec-r app` running?")


def output_dir():
    out = subprocess.run(
        ["target/debug/rec-r.exe", "config", "--show"], capture_output=True, text=True
    ).stdout
    for line in out.splitlines():
        if "output_dir" in line:
            return line.split(":", 1)[1].strip().strip('",')
    return os.path.expanduser("~/Videos/Rec-R")


def command(hwnd, cmd_id):
    user32.PostMessageW(hwnd, WM_COMMAND, cmd_id, 0)
    time.sleep(1.5)


def main():
    hwnd = find_window(HOST_CLASS)
    print(f"host window: {hwnd}")

    # The popup menu is modal (TrackPopupMenu blocks the UI thread), so it is
    # only opened when explicitly asked for: `--menu` captures it.
    if "--menu" in sys.argv:
        user32.PostMessageW(hwnd, WM_TRAY, TRAY_UID, WM_RBUTTONUP)
        time.sleep(1.0)
        shot = subprocess.run(
            ["target/debug/rec-r.exe", "shot", "-o", "tray-menu.png"],
            capture_output=True,
            text=True,
        )
        print("menu screenshot:", (shot.stdout or shot.stderr).strip().splitlines()[0])
        user32.keybd_event(VK_ESCAPE, 0, 0, 0)
        user32.keybd_event(VK_ESCAPE, 0, 2, 0)
        time.sleep(0.5)

    out = output_dir()
    before = set(glob.glob(os.path.join(out, "rec-*.mp4")))
    command(hwnd, ID_REC_PRIMARY)
    time.sleep(2.5)
    middle = set(glob.glob(os.path.join(out, "rec-*.mp4"))) - before
    print("recording file:", middle or "MISSING")
    command(hwnd, ID_REC_STOP)
    time.sleep(1.5)
    for path in middle:
        size = os.path.getsize(path)
        print(f"stopped: {os.path.basename(path)} {size} bytes")
        if size < 10_000:
            raise SystemExit("recording from the tray menu looks empty")

    shots_before = set(glob.glob(os.path.join(out, "shot-*.png")))
    command(hwnd, ID_SHOT_PRIMARY)
    time.sleep(1.0)
    new_shots = set(glob.glob(os.path.join(out, "shot-*.png"))) - shots_before
    print("screenshot file:", [os.path.basename(p) for p in new_shots] or "MISSING")
    if not new_shots:
        raise SystemExit("tray screenshot produced no file")

    print("\ntray menu cases passed")


if __name__ == "__main__":
    sys.exit(main())