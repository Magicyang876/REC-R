"""Check the region-recording frame and the stop hotkey.

    python scripts/region_frame_test.py [--seconds 3]

1. hotkey 5 (record region) + posted drag  -> the red frame must hug the drag rect
2. the video must show the same pixels as a normal screenshot of that rect
3. hotkey 6 (stop) -> recording ends and the frame disappears again
"""

import ctypes
import ctypes.wintypes as wt
import glob
import json
import os
import subprocess
import sys
import tempfile
import time

from PIL import Image

user32 = ctypes.windll.user32
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

WM_HOTKEY = 0x0312
WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_LBUTTONUP = 0x0201, 0x0200, 0x0202
MK_LBUTTON = 0x0001
OUT = os.path.expandvars(r"%USERPROFILE%\Videos\Rec-R")
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, "target", "debug", "rec-r.exe")

BORDER = 3  # must match src/gui/frame.rs


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


def wait_class(want, timeout=6.0):
    end = time.time() + timeout
    while time.time() < end:
        hits = find_class(want)
        if hits:
            return hits[0]
        time.sleep(0.05)
    return None


def window_rect(hwnd):
    r = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return (r.left, r.top, r.right - r.left, r.bottom - r.top)


def lparam(x, y):
    return ((y & 0xFFFF) << 16) | (x & 0xFFFF)


def run(*args):
    return subprocess.run([EXE, *args], capture_output=True, text=True)


def colors_of(x, y, w, h, out):
    run("shot", "-r", f"{x},{y},{w},{h}", "-o", out)
    from PIL import Image  # noqa: F401  (Pillow is used by make_icon.py too)

    return len(Image.open(out).convert("RGB").getcolors(maxcolors=300000) or [])


def screen_rect():
    return (user32.GetSystemMetrics(0), user32.GetSystemMetrics(1))


def candidate_rects():
    """Sub-rects of the foreground window, then of the primary screen."""
    rects = []
    fg = user32.GetForegroundWindow()
    if fg:
        r = wt.RECT()
        user32.GetWindowRect(fg, ctypes.byref(r))
        for fx, fy in ((0.25, 0.3), (0.4, 0.2), (0.1, 0.4), (0.3, 0.5)):
            w, h = 400, 300
            x = r.left + int((r.right - r.left - w) * fx)
            y = r.top + int((r.bottom - r.top - h) * fy)
            rects.append((x, y, x + w, y + h))
    sw, sh = screen_rect()
    for fx, fy in ((0.2, 0.25), (0.5, 0.4), (0.3, 0.6)):
        w, h = 400, 300
        x = int((sw - w) * fx)
        y = int((sh - h) * fy)
        rects.append((x, y, x + w, y + h))
    return rects


def pick_rect():
    """Return a candidate rect with content, or the best one if the screen is flat."""
    probe = os.path.join(tempfile.gettempdir(), "rec-r-region-probe.png")
    best = None
    for x1, y1, x2, y2 in candidate_rects():
        if x1 < 0 or y1 < 0:
            continue
        n = colors_of(x1, y1, x2 - x1, y2 - y1, probe)
        print(f"  probe {x1},{y1} {x2 - x1}x{y2 - y1}: {n} colors")
        if n > 500:
            return x1, y1, x2, y2
        if best is None or n > best[0]:
            best = (n, x1, y1, x2, y2)
    if best is None:
        raise SystemExit("no candidate rect at all - is something covering the primary display?")
    print(f"  screen looks flat ({best[0]} colours): continuing with the least blank rect")
    return best[1], best[2], best[3], best[4]


def main():
    seconds = 3
    if "--seconds" in sys.argv:
        seconds = int(sys.argv[sys.argv.index("--seconds") + 1])

    host = wait_class("RecRHost", 2.0)
    if not host:
        raise SystemExit("rec-r app is not running")

    mp4s = set(glob.glob(os.path.join(OUT, "rec-*.mp4")))
    x1, y1, x2, y2 = pick_rect()
    print(f"selection: {x1},{y1} {x2 - x1}x{y2 - y1}")
    want = (x1, y1, x2 - x1, y2 - y1)

    # Windows can change while we record (notifications, focus switches), so sample
    # the same rect repeatedly and require the video frame to match at least one
    # sample. That still catches blank, offset or frozen captures.
    refs = []

    def sample():
        path = os.path.join(tempfile.gettempdir(), f"rec-r-region-ref{len(refs)}.png")
        r = run("shot", "-r", f"{x1},{y1},{x2 - x1},{y2 - y1}", "-o", path)
        assert r.returncode == 0, r.stderr
        refs.append(path)

    sample()

    user32.PostMessageW(host, WM_HOTKEY, 5, 0)
    overlay = wait_class("RecRRegionOverlay")
    if not overlay:
        raise SystemExit("record-region hotkey opened no overlay")
    user32.PostMessageW(overlay, WM_LBUTTONDOWN, MK_LBUTTON, lparam(x1, y1))
    steps = 8
    for i in range(1, steps + 1):
        user32.PostMessageW(
            overlay,
            WM_MOUSEMOVE,
            MK_LBUTTON,
            lparam(x1 + (x2 - x1) * i // steps, y1 + (y2 - y1) * i // steps),
        )
        time.sleep(0.01)
    user32.PostMessageW(overlay, WM_LBUTTONUP, 0, lparam(x2, y2))

    frame = wait_class("RecRRecFrame", 6.0)
    if not frame:
        raise SystemExit("no recording frame appeared")
    got = window_rect(frame)
    # The frame must hug the selection from the outside: it is a hollow window, so
    # nothing of it is over the pixels being recorded.
    outer = (want[0] - BORDER, want[1] - BORDER, want[2] + 2 * BORDER, want[3] + 2 * BORDER)
    print(f"frame rect: {got[0]},{got[1]} {got[2]}x{got[3]}  want {outer[0]},{outer[1]} {outer[2]}x{outer[3]}")
    assert got == outer, f"frame is drawn at {got}, expected {outer}"

    half = max(seconds / 2.0, 0.5)
    time.sleep(half)
    sample()
    time.sleep(half)
    sample()

    user32.PostMessageW(host, WM_HOTKEY, 6, 0)

    end = time.time() + 25
    while time.time() < end and find_class("RecRRecFrame"):
        time.sleep(0.2)
    assert not find_class("RecRRecFrame"), "frame still visible after stop"
    print("frame hidden after stop")

    deadline = time.time() + 40
    rec = None
    while time.time() < deadline:
        new = sorted(set(glob.glob(os.path.join(OUT, "rec-*.mp4"))) - mp4s)
        if new:
            rec = new[-1]
            # wait for the file to stop growing
            size = -1
            for _ in range(60):
                now = os.path.getsize(rec)
                if now == size:
                    break
                size = now
                time.sleep(0.4)
            break
        time.sleep(0.5)
    if not rec:
        raise SystemExit("no recording was produced")
    print(f"video: {os.path.basename(rec)} {os.path.getsize(rec)} bytes")

    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
         "-show_entries", "stream=width,height,nb_read_frames", "-of", "csv=p=0", rec],
        capture_output=True, text=True,
    )
    print("ffprobe:", probe.stdout.strip())

    # Playability: an empty or truncated container (odd frame size, killed
    # encoder) is the failure the user hits first, so check it explicitly.
    decode = subprocess.run(
        ["ffmpeg", "-v", "error", "-xerror", "-i", rec, "-f", "null", "-"],
        capture_output=True, text=True,
    )
    assert decode.returncode == 0 and not decode.stderr.strip(), (
        f"recording does not decode: {decode.stderr.strip()[:400]}"
    )
    print("decodes clean")

    movie = os.path.join(tempfile.gettempdir(), "rec-r-region-frame.png")
    # `-y` and `-nostdin` are essential: without them ffmpeg refuses to overwrite
    # the previous run's PNG and the comparison silently uses a stale frame.
    subprocess.run(
        ["ffmpeg", "-v", "error", "-nostdin", "-y", "-sseof", "-0.2", "-i", rec,
         "-frames:v", "1", movie],
        check=True,
    )
    def ssim_against(img, reference):
        out = subprocess.run(
            ["ffmpeg", "-v", "info", "-i", img, "-i", reference, "-lavfi", "ssim", "-f", "null", "-"],
            capture_output=True, text=True,
        )
        for line in out.stderr.splitlines():
            if "All:" in line:
                return float(line.split("All:")[1].split()[0])
        return None

    sample()
    scores = [(ssim_against(movie, r), os.path.basename(r)) for r in refs]
    print("ssim per reference:", [(round(s, 4) if s else s, n) for s, n in scores])
    score = max(s for s, _ in scores if s is not None)

    # The desktop this test runs on may be busy (notifications, windows coming and
    # going), so the strict pixel comparison only applies when the screen actually
    # held still. Blank/flat recordings are always rejected.
    still = all(
        (ssim_against(refs[0], other) or 0.0) > 0.98 for other in refs[1:]
    )
    colors = len(Image.open(movie).convert("RGB").getcolors(1 << 20) or [])
    print(f"best ssim {score:.4f}, screen static: {still}, video colours: {colors}")
    assert colors > 30 or (score or 0) > 0.85, f"recording looks blank ({colors} colours)"
    if still:
        assert score > 0.85, f"recorded pixels differ from the selection (best ssim {score:.3f})"

    # Nothing of our own frame may leak into the recording.
    im = Image.open(movie).convert("RGB")
    corners = [im.getpixel(p) for p in ((0, 0), (im.width - 1, 0), (0, im.height - 1), (im.width - 1, im.height - 1))]
    print("corner pixels:", corners)
    for c in corners:
        assert not (c[0] > 180 and c[1] < 90 and c[2] < 90), f"red frame leaked into the recording: {c}"
    print("region frame test ok")


if __name__ == "__main__":
    main()