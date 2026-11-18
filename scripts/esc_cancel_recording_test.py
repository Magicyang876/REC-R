"""Esc cancels: region selection overlay and a running recording.

Run against the release build: python scripts/esc_cancel_recording_test.py
"""
import ctypes
import glob
import json
import os
import subprocess
import sys
import time

user32 = ctypes.windll.user32
WM_COMMAND = 0x0111
WM_KEYDOWN = 0x0100
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
WM_MOUSEMOVE = 0x0200

ID_SHOT_REGION = 2003
ID_REC_PRIMARY = 3001
VK_ESCAPE = 0x1B
KEYEVENTF_KEYUP = 0x0002

EXE = r"target\release\rec-r.exe"


def find(cls):
    return user32.FindWindowW(cls, None)


def wait_window(cls, timeout=15.0):
    end = time.time() + timeout
    while time.time() < end:
        hwnd = find(cls)
        if hwnd:
            return hwnd
        time.sleep(0.2)
    return 0


def output_dir():
    try:
        text = subprocess.run([EXE, "config"], capture_output=True, text=True).stdout
        start = text.index("{")
        cfg = json.loads(text[start:])
        if cfg.get("output_dir"):
            return cfg["output_dir"]
    except Exception:
        pass
    return os.path.join(os.path.expanduser("~"), "Videos", "Rec-R")


def recordings(directory):
    return set(glob.glob(os.path.join(directory, "*.mp4")))


def press_escape():
    user32.keybd_event(VK_ESCAPE, 0, 0, 0)
    time.sleep(0.05)
    user32.keybd_event(VK_ESCAPE, 0, KEYEVENTF_KEYUP, 0)


def lparam(x, y):
    return (y << 16) | x


def test_overlay_escape(host):
    """Region selection: drag nothing, press Esc, the overlay must vanish."""
    user32.PostMessageW(host, WM_COMMAND, ID_SHOT_REGION, 0)
    overlay = wait_window("RecRRegionOverlay", 5.0)
    if not overlay:
        print("overlay: FAIL (did not appear)")
        return False
    ctypes.windll.kernel32.Sleep(300)
    user32.PostMessageW(overlay, WM_KEYDOWN, VK_ESCAPE, 0)
    time.sleep(0.5)
    gone = not find("RecRRegionOverlay")
    editor = find("RecRAnnotate")
    print(f"overlay: closed={gone} editor_opened={bool(editor)}")
    return gone and not editor


def test_recording_escape(host, directory):
    """Start a recording from the tray, press Esc, the file must be removed."""
    before = recordings(directory)
    user32.PostMessageW(host, WM_COMMAND, ID_REC_PRIMARY, 0)

    new = None
    end = time.time() + 15.0
    while time.time() < end:
        fresh = recordings(directory) - before
        if fresh:
            new = sorted(fresh)[0]
            break
        time.sleep(0.2)
    if not new:
        print("recording: FAIL (no file appeared)")
        return False
    time.sleep(1.5)  # let ffmpeg write some frames
    print(f"recording: started {os.path.basename(new)} ({os.path.getsize(new)} bytes)")

    press_escape()

    end = time.time() + 20.0
    while time.time() < end:
        if not os.path.exists(new):
            print("recording: PASS (Esc cancelled and the file was removed)")
            return True
        time.sleep(0.3)
    left = os.path.getsize(new) if os.path.exists(new) else -1
    print(f"recording: FAIL (file still there, {left} bytes)")
    return False


def main():
    directory = output_dir()
    print(f"output dir: {directory}")
    existing = find("RecRHost")
    proc = None
    if existing:
        print("using the already running tray instance")
    else:
        proc = subprocess.Popen([EXE, "app", "--http", "--addr", "127.0.0.1:7396"])
    host = existing or wait_window("RecRHost")
    try:
        if not host:
            print("FAIL: RecRHost window not found")
            return 1
        print(f"host hwnd=0x{host:X}")

        ok_overlay = test_overlay_escape(host)
        ok_recording = test_recording_escape(host, directory)

        print("RESULT:", "PASS" if (ok_overlay and ok_recording) else "FAIL")
        return 0 if (ok_overlay and ok_recording) else 1
    finally:
        if proc is not None:
            user32.PostMessageW(host, WM_COMMAND, 4005, 0)  # Exit
            time.sleep(0.5)
            if proc.poll() is None:
                proc.terminate()


if __name__ == "__main__":
    sys.exit(main())