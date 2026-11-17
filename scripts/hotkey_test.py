"""Exercise the tray hotkeys by posting WM_HOTKEY to the rec-r host window.

    python scripts/hotkey_test.py [--seconds 2]

Needs `rec-r app` running. Posts the hotkey messages the OS would deliver
(id 4 = shot region, id 5 = record region), drives the overlay with posted mouse
messages, and checks that a file shows up. Nothing is moved or clicked for real.
"""

import ctypes
import glob
import os
import subprocess
import sys
import time

user32 = ctypes.windll.user32
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

WM_HOTKEY = 0x0312
WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_LBUTTONUP = 0x0201, 0x0200, 0x0202
MK_LBUTTON = 0x0001
OUT = os.path.expandvars(r"%USERPROFILE%\Videos\Rec-R")
EXE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                   "target", "debug", "rec-r.exe")


def find_class(want):
    found = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def cb(hwnd, _):
        buf = ctypes.create_unicode_buffer(120)
        user32.GetClassNameW(hwnd, buf, 120)
        if buf.value == want:
            found.append(hwnd)
        return True

    user32.EnumWindows(cb, None)
    return found


def wait_class(want, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        hits = find_class(want)
        if hits:
            return hits[0]
        time.sleep(0.05)
    return None


def lparam(x, y):
    return ((y & 0xFFFF) << 16) | (x & 0xFFFF)


def new_files(suffix, before):
    return sorted(set(glob.glob(os.path.join(OUT, suffix))) - before)


def main():
    seconds = 2
    if "--seconds" in sys.argv:
        seconds = int(sys.argv[sys.argv.index("--seconds") + 1])

    host = wait_class("RecRHost", 2.0)
    if not host:
        raise SystemExit("rec-r app is not running (no RecRHost window)")

    pngs = set(glob.glob(os.path.join(OUT, "shot-*.png")))
    mp4s = set(glob.glob(os.path.join(OUT, "rec-*.mp4")))

    # 1. shot region (hotkey id 4)
    user32.PostMessageW(host, WM_HOTKEY, 4, 0)
    overlay = wait_class("RecRRegionOverlay")
    if not overlay:
        raise SystemExit("shot-region hotkey did not open the overlay")
    user32.PostMessageW(overlay, WM_LBUTTONDOWN, MK_LBUTTON, lparam(300, 260))
    for x in range(320, 701, 20):
        user32.PostMessageW(overlay, WM_MOUSEMOVE, MK_LBUTTON, lparam(x, 460))
        time.sleep(0.01)
    user32.PostMessageW(overlay, WM_LBUTTONUP, 0, lparam(700, 460))
    time.sleep(1.2)
    shots = new_files("shot-*.png", pngs)
    print(f"hotkey 4 (shot region): {shots[-1] if shots else 'NO FILE'}")
    assert shots, "shot-region hotkey produced no screenshot"

    # 2. record region (hotkey id 5): select, record, stop with the same hotkey
    user32.PostMessageW(host, WM_HOTKEY, 5, 0)
    overlay = wait_class("RecRRegionOverlay")
    if not overlay:
        raise SystemExit("record-region hotkey did not open the overlay")
    user32.PostMessageW(overlay, WM_LBUTTONDOWN, MK_LBUTTON, lparam(200, 200))
    for x in range(220, 601, 20):
        user32.PostMessageW(overlay, WM_MOUSEMOVE, MK_LBUTTON, lparam(x, 400))
        time.sleep(0.01)
    user32.PostMessageW(overlay, WM_LBUTTONUP, 0, lparam(600, 400))
    time.sleep(seconds + 0.3)
    # Second press toggles the recording off.
    user32.PostMessageW(host, WM_HOTKEY, 5, 0)
    time.sleep(2.5)

    recs = new_files("rec-*.mp4", mp4s)
    if not recs:
        raise SystemExit("record-region hotkey produced no video")
    path = recs[-1]
    size = os.path.getsize(path)
    print(f"hotkey 5 (record region): {os.path.basename(path)} {size} bytes")
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height,nb_frames", "-of", "csv=p=0", path],
        capture_output=True, text=True,
    )
    print("ffprobe:", probe.stdout.strip() or probe.stderr.strip())
    assert size > 10_000, "recording looks empty"
    print("hotkey test ok")


if __name__ == "__main__":
    main()