"""Annotation editor: the Box tool and Enter/Esc in the text prompt.

Run against the release build: python scripts/annotate_box_test.py
"""
import ctypes
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from shot_clipboard_annotate_test import (  # noqa: E402
    BORDER, BTN_SAVE, BTN_TEXT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    button_labels, child_classes, clipboard_dib, count_color, dib_pixel, lparam,
    near, session_locked, wait_children, wait_window,
)

u = ctypes.windll.user32
WM_COMMAND = 0x0111
WM_KEYDOWN = 0x0100
WM_SETTEXT = 0x000C

ID_SHOT_REGION = 2003
BTN_BOX = 6108
ORANGE = (255, 138, 0)

EXE = os.path.join("target", "release", "rec-r.exe")
REGION = (100, 100, 400, 300)


def click_button(editor, hwnd_id):
    u.PostMessageW(editor, WM_COMMAND, hwnd_id, 0)
    time.sleep(0.2)


def drag(editor, x0, y0, x1, y1):
    u.PostMessageW(editor, WM_LBUTTONDOWN, 0, lparam(BORDER + x0, BORDER + y0))
    time.sleep(0.1)
    u.PostMessageW(editor, WM_MOUSEMOVE, 0, lparam(BORDER + x1, BORDER + y1))
    time.sleep(0.1)
    u.PostMessageW(editor, WM_LBUTTONUP, 0, lparam(BORDER + x1, BORDER + y1))
    time.sleep(0.2)


def open_prompt(editor, x, y):
    u.PostMessageW(editor, WM_LBUTTONDOWN, 0, lparam(BORDER + x, BORDER + y))
    return wait_window("RecRAnnotateText", 3.0)


def edit_child(prompt):
    kids = wait_children(prompt, "edit", 1, 3.0)
    return kids[0] if kids else 0


def key_to(hwnd, vk):
    u.PostMessageW(hwnd, WM_KEYDOWN, vk, 0)
    time.sleep(0.4)


def main():
    if session_locked():
        print("session is locked: unlock the desktop and rerun")
        return 0

    if u.FindWindowW("RecRHost", None):
        print("rec-r is already running; exit it before running this test")
        return 2
    proc = subprocess.Popen([EXE, "app", "--http", "--addr", "127.0.0.1:7394"])
    host = 0
    try:
        host = wait_window("RecRHost")
        if not host:
            print("FAIL: RecRHost not found")
            return 1
        u.PostMessageW(host, WM_COMMAND, ID_SHOT_REGION, 0)
        overlay = wait_window("RecRRegionOverlay", 5.0)
        if not overlay:
            print("FAIL: no overlay")
            return 1
        x, y = REGION[0], REGION[1]
        u.PostMessageW(overlay, WM_LBUTTONDOWN, 0, lparam(x, y))
        u.PostMessageW(overlay, WM_MOUSEMOVE, 0, lparam(x + REGION[2], y + REGION[3]))
        u.PostMessageW(overlay, WM_LBUTTONUP, 0, lparam(x + REGION[2], y + REGION[3]))
        editor = wait_window("RecRAnnotate", 5.0)
        if not editor:
            print("FAIL: no editor")
            return 1
        toolbar = wait_window("RecRAnnotateToolbar", 5.0)
        assert toolbar, "separate toolbar did not appear"
        wait_children(toolbar, "button", 8)
        labels = button_labels(toolbar)
        print("labels:", labels)
        assert len([c for _, c in child_classes(toolbar) if c.lower() == "button"]) == 8
        has_box = "Box" in labels
        print(f"box button: {has_box}")
        if not has_box:
            print("RESULT: FAIL")
            return 1

        # 1) Box tool: drag a rectangle, it must be painted in the current colour.
        click_button(toolbar, BTN_BOX)
        drag(editor, 40, 60, 240, 180)

        # 2) Text prompt: Enter accepts.
        click_button(toolbar, BTN_TEXT)
        prompt = open_prompt(editor, 300, 200)
        if not prompt:
            print("FAIL: text prompt did not open")
            return 1
        edit = edit_child(prompt)
        if edit:
            buf = ctypes.create_unicode_buffer("HI")
            u.SendMessageW(ctypes.c_void_p(edit), WM_SETTEXT, 0, ctypes.byref(buf))
        key_to(prompt, 0x0D)  # Enter
        enter_closed = not u.IsWindow(prompt)
        print(f"prompt closed by Enter: {enter_closed}")

        # 3) Text prompt: Esc cancels.
        prompt = open_prompt(editor, 300, 260)
        if not prompt:
            print("FAIL: text prompt did not open the second time")
            return 1
        key_to(prompt, 0x1B)  # Esc
        esc_closed = not u.IsWindow(prompt)
        print(f"prompt closed by Esc: {esc_closed}")

        click_button(toolbar, BTN_SAVE)
        time.sleep(0.8)
        if u.IsWindow(editor):
            print("FAIL: editor did not close on Save")
            return 1

        dib = None
        for _ in range(12):
            dib = clipboard_dib()
            if dib:
                break
            time.sleep(0.25)
        if not dib:
            print("FAIL: clipboard has no bitmap")
            return 1
        left_edge = dib_pixel(dib, 40, 120)
        top_edge = dib_pixel(dib, 140, 60)
        print(f"box edge pixels: left={left_edge} top={top_edge}")
        box_ok = near(left_edge, ORANGE) and near(top_edge, ORANGE)
        typed = count_color(dib, 195, 230, 295, 380, ORANGE)
        cancelled = count_color(dib, 255, 292, 295, 380, ORANGE)
        print(f"typed text pixels: {typed}, cancelled-area pixels: {cancelled}")

        ok = box_ok and enter_closed and esc_closed and typed > 20 and cancelled == 0
        print("RESULT:", "PASS" if ok else "FAIL")
        return 0 if ok else 1
    finally:
        if host:
            u.PostMessageW(host, WM_COMMAND, 4005, 0)
        time.sleep(0.5)
        if proc.poll() is None:
            proc.terminate()


if __name__ == "__main__":
    sys.exit(main())