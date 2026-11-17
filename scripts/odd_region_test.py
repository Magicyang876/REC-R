"""Drag an odd-sized region in the overlay and check the produced video.

    python scripts/odd_region_test.py

A hand-dragged rect is rarely even (401x302 here) and yuv420p cannot encode that,
so the recorded file must come out with even dimensions and still decode.
"""

import ctypes
import ctypes.wintypes as wt
import glob
import os
import subprocess
import time

user32 = ctypes.windll.user32
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

WM_HOTKEY = 0x0312
WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_LBUTTONUP = 0x0201, 0x0200, 0x0202
MK_LBUTTON = 1
OUT = os.path.expandvars(r"%USERPROFILE%\Videos\Rec-R")
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, "target", "release", "rec-r.exe")
X1, Y1, X2, Y2 = 701, 401, 1102, 703  # 401 x 302, deliberately odd


def find_class(want):
    hits = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def cb(hwnd, _):
        buf = ctypes.create_unicode_buffer(120)
        user32.GetClassNameW(hwnd, buf, 120)
        if buf.value == want:
            hits.append(hwnd)
        return True

    user32.EnumWindows(cb, None)
    return hits


def wait_class(want, timeout=6.0):
    end = time.time() + timeout
    while time.time() < end:
        hits = find_class(want)
        if hits:
            return hits[0]
        time.sleep(0.05)
    return None


def lparam(x, y):
    return ((y & 0xFFFF) << 16) | (x & 0xFFFF)


def main():
    host = wait_class("RecRHost", 2.0)
    if not host:
        raise SystemExit("rec-r app is not running")
    before = set(glob.glob(os.path.join(OUT, "rec-*.mp4")))

    user32.PostMessageW(host, WM_HOTKEY, 5, 0)
    overlay = wait_class("RecRRegionOverlay")
    if not overlay:
        raise SystemExit("no overlay")
    user32.PostMessageW(overlay, WM_LBUTTONDOWN, MK_LBUTTON, lparam(X1, Y1))
    for i in range(1, 9):
        user32.PostMessageW(
            overlay, WM_MOUSEMOVE, MK_LBUTTON,
            lparam(X1 + (X2 - X1) * i // 8, Y1 + (Y2 - Y1) * i // 8),
        )
        time.sleep(0.01)
    user32.PostMessageW(overlay, WM_LBUTTONUP, 0, lparam(X2, Y2))
    print(f"dragged {X2 - X1}x{Y2 - Y1}")

    if not wait_class("RecRRecFrame", 6.0):
        raise SystemExit("no frame")
    time.sleep(2.0)
    user32.PostMessageW(host, WM_HOTKEY, 6, 0)

    end = time.time() + 30
    rec = None
    while time.time() < end:
        new = sorted(set(glob.glob(os.path.join(OUT, "rec-*.mp4"))) - before)
        if new:
            rec = new[-1]
            size = -1
            for _ in range(60):
                now = os.path.getsize(rec)
                if now == size:
                    break
                size = now
                time.sleep(0.4)
            break
        time.sleep(0.4)
    if not rec:
        raise SystemExit("no recording produced")

    print(f"video: {os.path.basename(rec)} {os.path.getsize(rec)} bytes")
    assert os.path.getsize(rec) > 2000, "recording is suspiciously small"
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "stream=width,height,nb_frames", "-of", "csv=p=0", rec],
        capture_output=True, text=True,
    )
    print("ffprobe:", probe.stdout.strip())
    w, h = (int(v) for v in probe.stdout.strip().split(",")[:2])
    assert w % 2 == 0 and h % 2 == 0, f"odd dimensions survived: {w}x{h}"
    decode = subprocess.run(
        ["ffmpeg", "-v", "error", "-nostdin", "-xerror", "-i", rec, "-f", "null", "-"],
        capture_output=True, text=True,
    )
    assert decode.returncode == 0 and not decode.stderr.strip(), decode.stderr[:300]
    print("odd-size region recording is playable")


if __name__ == "__main__":
    main()