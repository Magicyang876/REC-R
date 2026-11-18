"""Tray menu: 'Open output folder' and the config file must really open something.

Run against the release build: python scripts/tray_open_test.py
"""
import ctypes
import os
import subprocess
import sys
import time

user32 = ctypes.windll.user32
WM_COMMAND = 0x0111
ID_OPEN_OUTPUT = 4001
ID_CONFIG = 4004
ID_QUIT = 4005

EXE = os.path.join("target", "release", "rec-r.exe")


def windows():
    """(hwnd, class, title) for every visible top-level window."""
    out = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
    def cb(hwnd, _):
        if user32.IsWindowVisible(hwnd):
            cls = ctypes.create_unicode_buffer(256)
            title = ctypes.create_unicode_buffer(512)
            user32.GetClassNameW(hwnd, cls, 256)
            user32.GetWindowTextW(hwnd, title, 512)
            out.append((hwnd, cls.value, title.value))
        return True

    user32.EnumWindows(cb, None)
    return out


def wait_new(before, match, timeout=8.0):
    end = time.time() + timeout
    while time.time() < end:
        for hwnd, cls, title in windows():
            if hwnd in before:
                continue
            if match(cls, title):
                return (hwnd, cls, title)
        time.sleep(0.25)
    return None


def main():
    proc = subprocess.Popen([EXE, "app", "--http", "--addr", "127.0.0.1:7395"])
    try:
        host = 0
        for _ in range(50):
            host = user32.FindWindowW("RecRHost", None)
            if host:
                break
            time.sleep(0.2)
        if not host:
            print("FAIL: RecRHost not found")
            return 1

        before = {hwnd for hwnd, _, _ in windows()}
        user32.PostMessageW(host, WM_COMMAND, ID_OPEN_OUTPUT, 0)
        folder = wait_new(before, lambda cls, title: cls == "CabinetWClass" and "Rec-R" in title)
        print("output folder window:", folder[1:] if folder else None)

        before = {hwnd for hwnd, _, _ in windows()}
        user32.PostMessageW(host, WM_COMMAND, ID_CONFIG, 0)
        config = wait_new(
            before,
            lambda cls, title: "config.json" in title,
        )
        print("config window:", config[1:] if config else None)

        ok = bool(folder) and bool(config)
        print("RESULT:", "PASS" if ok else "FAIL")
        return 0 if ok else 1
    finally:
        user32.PostMessageW(host, WM_COMMAND, ID_QUIT, 0)
        time.sleep(0.6)
        if proc.poll() is None:
            proc.terminate()


if __name__ == "__main__":
    sys.exit(main())