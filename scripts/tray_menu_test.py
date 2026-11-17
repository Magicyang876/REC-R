"""Check that the tray icon opens its popup menu for every event the shell sends.

Start `rec-r app` first, then: python scripts/tray_menu_test.py

Everything is done by posting window messages: no cursor movement, no foreground
stealing (except whatever TrackPopupMenu itself does), so it is safe to run while
the user works.
"""

import ctypes
import ctypes.wintypes as wt
import time

u = ctypes.windll.user32
u.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

HOST_CLASS = "RecRHost"
MENU_CLASS = "#32768"
WM_APP = 0x8000
WM_TRAY = WM_APP + 1
WM_KEYDOWN = 0x0100
WM_KEYUP = 0x0101
WM_CANCELMODE = 0x001F
VK_ESCAPE = 0x1B
WM_USER = 0x0400
WM_COMMAND = 0x0111
ID_QUIT = 4005

EVENTS = [
    ("WM_RBUTTONUP", 0x0205),
    ("WM_CONTEXTMENU", 0x007B),
    ("NIN_SELECT", WM_USER),
]

EnumProc = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)


def find_window(cls):
    found = []

    def proc(hwnd, _):
        buf = ctypes.create_unicode_buffer(256)
        u.GetClassNameW(ctypes.c_void_p(hwnd), buf, 256)
        if buf.value == cls:
            found.append(hwnd)
            return False
        return True

    u.EnumWindows(EnumProc(proc), None)
    return found[0] if found else None


def menu_size(hwnd):
    r = wt.RECT()
    u.GetWindowRect(ctypes.c_void_p(hwnd), ctypes.byref(r))
    return r.right - r.left, r.bottom - r.top


def wait_for(cls, timeout=2.0):
    end = time.time() + timeout
    while time.time() < end:
        hwnd = find_window(cls)
        if hwnd:
            return hwnd
        time.sleep(0.05)
    return None


def wait_gone(cls, timeout=2.0):
    end = time.time() + timeout
    while time.time() < end:
        if not find_window(cls):
            return True
        time.sleep(0.05)
    return False


def main():
    host = find_window(HOST_CLASS)
    if not host:
        print("no RecRHost window - start `rec-r app` first")
        return 1

    failures = 0
    for name, event in EVENTS:
        u.PostMessageW(ctypes.c_void_p(host), WM_TRAY, 1, event)
        menu = wait_for(MENU_CLASS)
        if menu:
            w, h = menu_size(menu)
            # A menubar menu (CreateMenu) passed to TrackPopupMenu renders as a
            # 6 px wide strip - that is the regression this guards against.
            if w < 120 or h < 120:
                failures += 1
                print(f"{name:16} menu is malformed: {w}x{h} px")
            # TrackPopupMenu is modal: dismiss it again with Escape.
            u.PostMessageW(ctypes.c_void_p(menu), WM_KEYDOWN, VK_ESCAPE, 0)
            u.PostMessageW(ctypes.c_void_p(menu), WM_KEYUP, VK_ESCAPE, 0)
            closed = wait_gone(MENU_CLASS)
            if not closed:
                u.PostMessageW(ctypes.c_void_p(host), WM_CANCELMODE, 0, 0)
                wait_gone(MENU_CLASS)
            print(f"{name:16} menu opened (0x{menu:x}), closed={closed}")
        else:
            failures += 1
            print(f"{name:16} NO MENU")
        time.sleep(0.3)

    if failures == 0:
        # The last item of the menu must actually quit the app.
        u.PostMessageW(ctypes.c_void_p(host), 0x0111, ID_QUIT, 0)
        if wait_gone(HOST_CLASS, 5.0):
            print(f"{'Exit item':16} app closed")
        else:
            failures += 1
            print(f"{'Exit item':16} app still running")

    print("tray menu cases " + ("passed" if failures == 0 else f"FAILED ({failures})"))
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())