"""Tray menu checks: 'Copy MCP config' (clipboard) and 'Restart' (relaunch).

Run with the release build: python scripts/tray_clipboard_restart_test.py
"""
import ctypes
import subprocess
import sys
import time

user32 = ctypes.windll.user32
WM_COMMAND = 0x0111

ID_COPY_MCP = 4008
ID_RESTART = 4009


def find_host():
    return user32.FindWindowW("RecRHost", None)


def wait_host(timeout=15.0):
    end = time.time() + timeout
    while time.time() < end:
        hwnd = find_host()
        if hwnd:
            return hwnd
        time.sleep(0.2)
    return 0


def pid_of(hwnd):
    pid = ctypes.c_ulong()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def clipboard():
    return subprocess.run(
        ["powershell", "-NoProfile", "-Command", "Get-Clipboard -Raw"],
        capture_output=True, text=True,
    ).stdout.strip()


def main():
    exe = r"target\release\rec-r.exe"
    proc = subprocess.Popen([exe, "app", "--http", "--addr", "127.0.0.1:7399"])
    try:
        hwnd = wait_host()
        if not hwnd:
            print("FAIL: RecRHost window not found")
            return 1
        first_pid = pid_of(hwnd)
        print(f"host hwnd=0x{hwnd:X} pid={first_pid}")

        # 1) Copy MCP config -> clipboard
        user32.PostMessageW(hwnd, WM_COMMAND, ID_COPY_MCP, 0)
        time.sleep(0.8)
        text = clipboard()
        ok_config = '"mcpServers"' in text and '"rec-r"' in text and '"serve"' in text
        print("clipboard JSON:", text.replace("\n", " "))
        print("copy mcp config:", "PASS" if ok_config else "FAIL")

        # 2) Restart -> same window class comes back with a new pid
        user32.PostMessageW(hwnd, WM_COMMAND, ID_RESTART, 0)
        time.sleep(1.0)
        gone = find_host() == 0
        new_hwnd = wait_host(20.0)
        second_pid = pid_of(new_hwnd) if new_hwnd else 0
        restarted = bool(new_hwnd) and second_pid != first_pid
        print(f"window gone after restart: {gone}; new pid={second_pid}")
        print("restart:", "PASS" if restarted else "FAIL")

        if new_hwnd:
            user32.PostMessageW(new_hwnd, WM_COMMAND, 4005, 0)  # ID_QUIT
        return 0 if (ok_config and restarted) else 1
    finally:
        time.sleep(0.5)
        proc.poll()
        subprocess.run(["taskkill", "/IM", "rec-r.exe", "/F"], capture_output=True)


if __name__ == "__main__":
    sys.exit(main())