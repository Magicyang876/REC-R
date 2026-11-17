"""Starting the tray app the way Explorer does must not leave a console window.

`CREATE_NEW_CONSOLE` reproduces a double-click: Windows gives the process its own
console. The app hides and releases it and logs to %APPDATA%\\Rec-R\\rec-r.log.

python scripts/tray_no_console_test.py
"""
import ctypes
import ctypes.wintypes as wt
import os
import subprocess
import sys
import time

u = ctypes.windll.user32
CREATE_NEW_CONSOLE = 0x00000010
WM_COMMAND = 0x0111
ID_QUIT = 4005

EnumProc = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)


def windows_of_pid(pid, cls=None):
    found = []

    def proc(hwnd, _):
        owner = wt.DWORD()
        u.GetWindowThreadProcessId(ctypes.c_void_p(hwnd), ctypes.byref(owner))
        if owner.value == pid:
            buf = ctypes.create_unicode_buffer(256)
            u.GetClassNameW(ctypes.c_void_p(hwnd), buf, 256)
            if cls is None or buf.value == cls:
                found.append((hwnd, buf.value))
        return True

    u.EnumWindows(EnumProc(proc), None)
    return found


def find_host(timeout=20.0):
    end = time.time() + timeout
    while time.time() < end:
        for hwnd, cls in windows_of_pid_all("RecRHost"):
            return hwnd
        time.sleep(0.2)
    return 0


def windows_of_pid_all(cls):
    found = []

    def proc(hwnd, _):
        buf = ctypes.create_unicode_buffer(256)
        u.GetClassNameW(ctypes.c_void_p(hwnd), buf, 256)
        if buf.value == cls:
            found.append((hwnd, buf.value))
        return True

    u.EnumWindows(EnumProc(proc), None)
    return found


def log_path():
    return os.path.join(os.environ["APPDATA"], "Rec-R", "rec-r.log")


def main():
    exe = r"target\release\rec-r.exe"
    log = log_path()
    before = os.path.getsize(log) if os.path.exists(log) else 0

    proc = subprocess.Popen(
        [exe, "app", "--http", "--addr", "127.0.0.1:7401"],
        creationflags=CREATE_NEW_CONSOLE,
    )
    try:
        time.sleep(3.0)
        consoles = windows_of_pid(proc.pid, "ConsoleWindowClass")
        host = find_host(5.0)
        print(f"pid={proc.pid} tray_window=0x{host:X}")
        print(f"console windows owned by the app: {len(consoles)}")
        print("no console window:", "PASS" if not consoles else f"FAIL {consoles}")
        print("tray running:", "PASS" if host else "FAIL")

        grew = os.path.getsize(log) > before if os.path.exists(log) else False
        tail = ""
        if grew:
            with open(log, "r", encoding="utf-8", errors="replace") as fh:
                fh.seek(before)
                tail = fh.read()
        logged = "no console; logging to" in tail
        print("log file written:", "PASS" if grew and logged else f"FAIL {tail!r}")

        if host:
            u.PostMessageW(ctypes.c_void_p(host), WM_COMMAND, ID_QUIT, 0)
        time.sleep(0.5)
        return 0 if (not consoles and host and logged) else 1
    finally:
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            subprocess.run(["taskkill", "/PID", str(proc.pid), "/F"], capture_output=True)


if __name__ == "__main__":
    sys.exit(main())