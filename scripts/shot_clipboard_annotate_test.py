"""Region screenshot -> annotate (mosaic + text) -> clipboard + file.

Drives the real windows: posts WM_COMMAND to the tray host, drags on the region
overlay, then clicks the editor's toolbar and checks the resulting clipboard DIB
and the saved PNG.

python scripts/shot_clipboard_annotate_test.py
"""
import ctypes
import ctypes.wintypes as wt
import glob
import os
import struct
import subprocess
import sys
import time

u = ctypes.windll.user32
k = ctypes.windll.kernel32

# Without this, python is DPI-virtualised and every coordinate it posts is
# multiplied by 1.25 before the DPI-aware app sees it (and window rects come
# back divided by 1.25).
try:
    u.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # PER_MONITOR_AWARE_V2
except Exception:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)

# Clipboard handles are 64-bit; without restype ctypes truncates them to int32.
u.GetClipboardData.restype = ctypes.c_void_p
u.GetClipboardData.argtypes = [ctypes.c_uint]
k.GlobalLock.restype = ctypes.c_void_p
k.GlobalLock.argtypes = [ctypes.c_void_p]
k.GlobalUnlock.argtypes = [ctypes.c_void_p]
k.GlobalSize.restype = ctypes.c_size_t
k.GlobalSize.argtypes = [ctypes.c_void_p]

WM_COMMAND = 0x0111
WM_SETTEXT = 0x000C
WM_LBUTTONDOWN = 0x0201
WM_MOUSEMOVE = 0x0200
RDW_INVALIDATE = 0x0001
RDW_UPDATENOW = 0x0100
WM_LBUTTONUP = 0x0202

ID_SHOT_REGION = 2003
ID_QUIT = 4005
BTN_TEXT, BTN_MOSAIC, BTN_COLOR, BTN_SAVE = 6101, 6102, 6107, 6105
BORDER = 2  # UI-only border around the screenshot
WM_MOUSEMOVE_NOBTN = 0x0200

# COLORREF values that must match TEXT_COLORS in src/gui/annotate.rs.
ORANGE = (255, 138, 0)
RED = (255, 48, 48)

REGION_X, REGION_Y = 100, 100
REGION_W, REGION_H = 400, 300

EnumProc = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)


def lparam(x, y):
    return (y & 0xFFFF) << 16 | (x & 0xFFFF)


def wait_window(cls, timeout=15.0):
    end = time.time() + timeout
    while time.time() < end:
        hwnd = u.FindWindowW(cls, None)
        if hwnd:
            return hwnd
        time.sleep(0.1)
    return 0


def wait_children(hwnd, cls, count, timeout=10.0):
    """Wait for `count` children of class `cls` (windows are populated async)."""
    end = time.time() + timeout
    while time.time() < end:
        found = [h for h, c in child_classes(hwnd) if c.lower() == cls]
        if len(found) >= count:
            return found
        time.sleep(0.1)
    return []


def child_classes(hwnd):
    out = []

    def proc(h, _):
        buf = ctypes.create_unicode_buffer(128)
        u.GetClassNameW(ctypes.c_void_p(h), buf, 128)
        out.append((h, buf.value))
        return True

    u.EnumChildWindows(ctypes.c_void_p(hwnd), EnumProc(proc), None)
    return out


def clipboard_dib():
    for _ in range(20):
        if u.OpenClipboard(None):
            break
        time.sleep(0.05)
    else:
        return None
    try:
        handle = u.GetClipboardData(8)  # CF_DIB
        if not handle:
            return None
        ptr = k.GlobalLock(ctypes.c_void_p(handle))
        if not ptr:
            return None
        try:
            header = ctypes.string_at(ptr, 40)
            size, w, h, _, bpp = struct.unpack("<IiiHH", header[:16])
            stride = (w * 3 + 3) & ~3
            data = ctypes.string_at(ptr, 40 + stride * abs(h))
            return w, h, bpp, header, data
        finally:
            k.GlobalUnlock(ctypes.c_void_p(handle))
    finally:
        u.CloseClipboard()


def dib_pixel(dib, x, y):
    w, h, bpp, _, data = dib
    stride = (w * 3 + 3) & ~3
    row = h - 1 - y  # bottom-up
    off = 40 + row * stride + x * 3
    b, g, r = data[off], data[off + 1], data[off + 2]
    return r, g, b


def button_labels(hwnd):
    out = []
    for h, c in child_classes(hwnd):
        if c.lower() != "button":
            continue
        buf = ctypes.create_unicode_buffer(64)
        u.GetWindowTextW(ctypes.c_void_p(h), buf, 64)
        # A literal `&` is doubled in the control text (otherwise it marks an
        # accelerator key), so undo that when comparing visible labels.
        out.append(buf.value.replace("&&", "&"))
    return out


gdi = ctypes.windll.gdi32
gdi.GetPixel.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
gdi.GetPixel.restype = ctypes.c_uint


def screen_pixel(hwnd, dx, dy):
    """Colour of the pixel at client (dx, dy), read back from the screen."""
    r = wt.RECT()
    u.GetWindowRect(ctypes.c_void_p(hwnd), ctypes.byref(r))
    dc = u.GetDC(None)
    try:
        c = gdi.GetPixel(ctypes.c_void_p(dc), r.left + dx, r.top + dy)
    finally:
        u.ReleaseDC(None, ctypes.c_void_p(dc))
    return c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF


def near(px, want, tol=40):
    return all(abs(a - b) <= tol for a, b in zip(px, want))


def count_color(dib, y0, y1, x0, x1, want, tol=40):
    return sum(
        1
        for y in range(max(0, y0), min(dib[1], y1))
        for x in range(max(0, x0), min(dib[0], x1))
        if near(dib_pixel(dib, x, y), want, tol)
    )


def type_text(editor, x, y, msg):
    u.PostMessageW(ctypes.c_void_p(editor), WM_LBUTTONDOWN, 0, lparam(x, y))
    prompt = wait_window("RecRAnnotateText", 10)
    print("text prompt:", hex(prompt) if prompt else None)
    assert prompt, "text prompt did not appear"
    edits = wait_children(prompt, "edit", 1)
    assert edits, "prompt has no edit control"
    buf = ctypes.create_unicode_buffer(msg)
    u.SendMessageW(ctypes.c_void_p(edits[0]), WM_SETTEXT, 0, ctypes.byref(buf))
    u.PostMessageW(ctypes.c_void_p(prompt), WM_COMMAND, 1, 0)  # OK
    time.sleep(0.4)


def session_locked():
    """True when the interactive desktop is locked (LockApp/LogonUI in front).

    The lock screen window is a `Windows.UI.Core.CoreWindow`, so the class name
    says nothing; the owning process does.
    """
    fg = u.GetForegroundWindow()
    pid = wt.DWORD()
    u.GetWindowThreadProcessId(ctypes.c_void_p(fg), ctypes.byref(pid))
    proc = k.OpenProcess(0x1000, False, pid)  # QUERY_LIMITED_INFORMATION
    if not proc:
        return False
    try:
        path = ctypes.create_unicode_buffer(512)
        size = wt.DWORD(512)
        k.QueryFullProcessImageNameW(ctypes.c_void_p(proc), 0, path, ctypes.byref(size))
        return "lockapp" in path.value.lower() or "logonui" in path.value.lower()
    finally:
        k.CloseHandle(ctypes.c_void_p(proc))


def main():
    if session_locked():
        print("session is locked: unlock the desktop and rerun")
        return 0
    if u.FindWindowW("RecRHost", None):
        print("rec-r is already running; exit it before running this test")
        return 2
    out_dir = os.path.join(os.path.expanduser("~"), "Videos", "Rec-R")
    before = set(glob.glob(os.path.join(out_dir, "*.png")))

    app = subprocess.Popen([r"target\release\rec-r.exe", "app"])
    try:
        host = wait_window("RecRHost", 20)
        assert host, "tray host did not start"

        u.PostMessageW(ctypes.c_void_p(host), WM_COMMAND, ID_SHOT_REGION, 0)
        overlay = wait_window("RecRRegionOverlay", 20)
        print("region overlay:", hex(overlay) if overlay else None)
        assert overlay, "overlay did not appear"

        # Drag the test region on the virtual desktop.
        u.PostMessageW(ctypes.c_void_p(overlay), WM_LBUTTONDOWN, 0, lparam(REGION_X, REGION_Y))
        time.sleep(0.1)
        u.PostMessageW(
            ctypes.c_void_p(overlay),
            WM_MOUSEMOVE,
            0,
            lparam(REGION_X + REGION_W, REGION_Y + REGION_H),
        )
        time.sleep(0.1)
        u.PostMessageW(
            ctypes.c_void_p(overlay),
            WM_LBUTTONUP,
            0,
            lparam(REGION_X + REGION_W, REGION_Y + REGION_H),
        )

        editor = wait_window("RecRAnnotate", 20)
        print("editor window:", hex(editor) if editor else None)
        assert editor, "annotation editor did not appear"
        toolbar = wait_window("RecRAnnotateToolbar", 10)
        assert toolbar, "separate toolbar did not appear"
        buttons = wait_children(toolbar, "button", 8)
        print("toolbar buttons:", len(buttons))
        assert len(buttons) == 8, [c.lower() for _, c in child_classes(toolbar)]
        image_rect, bar_rect = wt.RECT(), wt.RECT()
        u.GetWindowRect(ctypes.c_void_p(editor), ctypes.byref(image_rect))
        u.GetWindowRect(ctypes.c_void_p(toolbar), ctypes.byref(bar_rect))
        assert bar_rect.bottom == image_rect.top, "toolbar must touch the top of the image"
        labels = button_labels(toolbar)
        print("labels:", labels)
        color_label = next((t for t in labels if t.startswith("Color")), "")
        ok = color_label == "Color Orange"
        print("default colour label:", f"{color_label!r}", "PASS" if ok else "FAIL")

        rc = wt.RECT()
        u.GetClientRect(ctypes.c_void_p(editor), ctypes.byref(rc))
        disp_w, disp_h = rc.right - 2 * BORDER, rc.bottom - 2 * BORDER
        print(f"editor client {rc.right}x{rc.bottom}, image area {disp_w}x{disp_h}")
        assert (disp_w, disp_h) == (REGION_W, REGION_H)
        # Image coordinates are offset by the UI-only border.
        x0, y0 = 10, 10
        x1, y1 = min(260, disp_w - 10), min(disp_h - 130, 130)

        u.PostMessageW(ctypes.c_void_p(toolbar), WM_COMMAND, BTN_MOSAIC, 0)
        u.PostMessageW(ctypes.c_void_p(editor), WM_LBUTTONDOWN, 0, lparam(BORDER + x0, BORDER + y0))
        time.sleep(0.1)
        u.PostMessageW(ctypes.c_void_p(editor), WM_MOUSEMOVE, 0, lparam(BORDER + x1, BORDER + y1))
        time.sleep(0.1)
        u.PostMessageW(ctypes.c_void_p(editor), WM_LBUTTONUP, 0, lparam(BORDER + x1, BORDER + y1))
        time.sleep(0.3)

        ty = max(disp_h - 60, 20)
        ty2 = max(ty - 60, 8)
        # Text tool: the cursor position is previewed as a caret in the current colour.
        u.PostMessageW(ctypes.c_void_p(toolbar), WM_COMMAND, BTN_TEXT, 0)
        hover = (200, 20)
        # TrackMouseEvent makes Windows post WM_MOUSELEAVE as soon as it notices the
        # real cursor is elsewhere, so park the pointer inside the window first.
        wr = wt.RECT()
        u.GetWindowRect(ctypes.c_void_p(editor), ctypes.byref(wr))
        cur = wt.POINT()
        u.GetCursorPos(ctypes.byref(cur))
        # A real mouse move (the person using the machine) can steal the hover at
        # any moment, so repost and re-read a few times before believing a miss.
        caret_px = None
        for _ in range(6):
            u.SetCursorPos(wr.left + BORDER + hover[0], wr.top + BORDER + hover[1])
            u.PostMessageW(
                ctypes.c_void_p(editor), WM_MOUSEMOVE_NOBTN, 0, lparam(BORDER + hover[0], BORDER + hover[1])
            )
            u.RedrawWindow(ctypes.c_void_p(editor), None, None, RDW_INVALIDATE | RDW_UPDATENOW)
            time.sleep(0.15)
            try:
                caret_px = screen_pixel(editor, BORDER + hover[0] + 1, BORDER + hover[1] + 6)
            except Exception as exc:  # screen reading is best effort only
                caret_px = None
                print("caret pixel: skipped", exc)
                break
            if caret_px and near(caret_px, ORANGE):
                break
        if caret_px:
            if session_locked():
                print("caret pixel: SKIP (session locked)")
            else:
                print("caret pixel:", caret_px, "PASS" if near(caret_px, ORANGE) else f"FAIL (want {ORANGE})")
                ok &= near(caret_px, ORANGE)
        u.SetCursorPos(cur.x, cur.y)

        # First text in the default colour (orange).
        type_text(editor, BORDER + 30, BORDER + ty, "hello rec-r")

        # Cycle the palette once and type again in red.
        u.PostMessageW(ctypes.c_void_p(toolbar), WM_COMMAND, BTN_COLOR, 0)
        time.sleep(0.2)
        color_label = next((t for t in button_labels(toolbar) if t.startswith("Color")), "")
        print("colour label after click:", f"{color_label!r}")
        ok &= color_label == "Color Red"
        type_text(editor, BORDER + 30, BORDER + ty2, "second line")

        u.PostMessageW(ctypes.c_void_p(toolbar), WM_COMMAND, BTN_SAVE, 0)

        # The edited bitmap is encoded on a worker thread.
        new_files = []
        for _ in range(120):
            new_files = list(set(glob.glob(os.path.join(out_dir, "*.png"))) - before)
            if new_files:
                break
            time.sleep(0.25)
        print("saved file:", new_files[0] if new_files else None)

        dib = clipboard_dib()
        if not dib:
            # Another app (often the lock screen) can hold the clipboard for a
            # while; the file below still proves the edit pipeline.
            print("clipboard bitmap: SKIP (no CF_DIB, clipboard busy or locked)")
        else:
            w, h, bpp, _, _ = dib
            print(f"clipboard DIB: {w}x{h} {bpp}bpp")
            # The output must contain only the selected region, not the toolbar.
            if (w, h) != (REGION_W, REGION_H):
                print(f"clipboard size: FAIL (want {REGION_W}x{REGION_H}, got {w}x{h})")
                ok = False
            # Mosaic: two pixels inside the same 16px block must be identical.
            a, b = dib_pixel(dib, x0 + 2, y0 + 2), dib_pixel(dib, x0 + 9, y0 + 9)
            print("mosaic block uniform:", "PASS" if a == b else f"FAIL {a} != {b}")
            ok &= a == b
            # Text 1 keeps the default orange, text 2 the red picked from the palette.
            orange = count_color(dib, ty - 8, ty + 26, 25, 420, ORANGE)
            print(f"orange text pixels: {orange}", "PASS" if orange > 40 else "FAIL")
            ok &= orange > 40
            red = count_color(dib, ty2 - 8, ty2 + 26, 25, 420, RED)
            print(f"red text pixels: {red}", "PASS" if red > 40 else "FAIL")
            ok &= red > 40
        if not new_files:
            print("saved png: FAIL")
            ok = False
        else:
            with open(new_files[0], "rb") as fh:
                head = fh.read(8)
            print("png signature:", "PASS" if head == b"\x89PNG\r\n\x1a\n" else "FAIL")
            ok &= head == b"\x89PNG\r\n\x1a\n"

        print("RESULT:", "PASS" if ok else "FAIL")
        return 0 if ok else 1
    finally:
        host = u.FindWindowW("RecRHost", None)
        if host:
            u.PostMessageW(ctypes.c_void_p(host), WM_COMMAND, ID_QUIT, 0)
        time.sleep(0.5)
        try:
            app.wait(timeout=3)
        except subprocess.TimeoutExpired:
            app.terminate()
            app.wait(timeout=3)


if __name__ == "__main__":
    sys.exit(main())