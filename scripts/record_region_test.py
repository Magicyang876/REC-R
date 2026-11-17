"""Record a hand-picked region, both from the CLI and from MCP (`region: "select"`).

Everything is driven by posted window messages, so the mouse is not touched.

    python scripts/record_region_test.py           # CLI + MCP
    python scripts/record_region_test.py cli       # only the CLI path
"""

import ctypes
import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mcp_smoke  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(ROOT, "target", "debug", "rec-r.exe")
ADDR = "http://127.0.0.1:7409/mcp"

u = ctypes.windll.user32
u.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

OVERLAY = "RecRRegionOverlay"
WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
MK_LBUTTON = 0x0001

EnumProc = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)


def find_class(cls):
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


def wait_overlay(timeout=10.0):
    end = time.time() + timeout
    while time.time() < end:
        hwnd = find_class(OVERLAY)
        if hwnd:
            return hwnd
        time.sleep(0.05)
    raise SystemExit("overlay never appeared")


def drag(x0, y0, x1, y1):
    hwnd = wait_overlay()
    time.sleep(0.2)  # let it paint the dimmed snapshot
    post = u.PostMessageW

    def lp(x, y):
        return (y << 16) | (x & 0xFFFF)

    post(ctypes.c_void_p(hwnd), WM_LBUTTONDOWN, MK_LBUTTON, lp(x0, y0))
    steps = 8
    for i in range(1, steps + 1):
        time.sleep(0.03)
        post(
            ctypes.c_void_p(hwnd),
            WM_MOUSEMOVE,
            MK_LBUTTON,
            lp(x0 + (x1 - x0) * i // steps, y0 + (y1 - y0) * i // steps),
        )
    post(ctypes.c_void_p(hwnd), WM_LBUTTONUP, 0, lp(x1, y1))


def last_json(text):
    """`record --json` prints the start info, then the stop result; grab the last document."""
    decoder = json.JSONDecoder()
    best = None
    offset = 0
    for line in text.splitlines(keepends=True):
        if line.strip() in ("[", "{"):
            try:
                best = decoder.raw_decode(text[offset:].lstrip())[0]
            except json.JSONDecodeError:
                pass
        offset += len(line)
    if best is None:
        raise SystemExit(f"no JSON in output:\n{text}")
    return best


def cli_test():
    proc = subprocess.Popen(
        [EXE, "record", "-r", "--seconds", "2", "--json", "-q", "fast"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    drag(300, 200, 700, 440)
    out, err = proc.communicate(timeout=120)
    if proc.returncode != 0:
        raise SystemExit(f"cli failed: {err.strip()}")
    info = last_json(out)
    if isinstance(info, list):
        info = info[0]
    print(
        f"cli  record -r: {info['width']}x{info['height']} "
        f"{info['frames']} frames {info['duration_sec']:.2f}s -> {os.path.basename(info['path'])}"
    )
    if (info["width"], info["height"]) != (400, 240):
        raise SystemExit(f"cli: unexpected size {info['width']}x{info['height']}, want 400x240")
    return info


def call(name, **args):
    res = mcp_smoke.rpc("tools/call", {"name": name, "arguments": args})
    if "error" in res:
        raise SystemExit(f"{name} failed: {res['error']}")
    result = res["result"]
    texts = [b["text"] for b in result.get("content", []) if b.get("type") == "text"]
    payload = None
    for text in texts:
        try:
            payload = json.loads(text)
        except json.JSONDecodeError:
            pass
    return result.get("isError"), payload, texts


def mcp_test():
    mcp_smoke.URL = ADDR
    server = subprocess.Popen(
        [EXE, "serve", "--http", "--addr", "127.0.0.1:7409"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(60):
            time.sleep(0.25)
            try:
                if mcp_smoke.rpc("initialize", {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "region-test", "version": "1"},
                }).get("result"):
                    break
            except Exception:
                continue
        else:
            raise SystemExit("MCP server did not come up")

        # record_start blocks until the human finishes dragging, so drag concurrently.
        thread = threading.Thread(target=lambda: drag(120, 140, 520, 380), daemon=True)
        thread.start()
        is_error, info, texts = call("record_start", region="select", fps=15, quality="fast")
        thread.join(timeout=20)
        if is_error:
            raise SystemExit(f"mcp record_start: {texts}")
        print(
            f"mcp  record_start(region=select): {info['width']}x{info['height']} "
            f"id {info['id']} -> {os.path.basename(info['path'])}"
        )
        time.sleep(1.2)
        _, stopped, _ = call("record_stop", id=info["id"])
        first = stopped["stopped"][0]
        print(f"mcp  record_stop: {first['frames']} frames {first['duration_sec']:.2f}s")
        if (info["width"], info["height"]) != (400, 240) or first["frames"] < 10:
            raise SystemExit("mcp: unexpected region size or frame count")
    finally:
        server.terminate()
        server.wait(timeout=10)


def main():
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    if which in ("all", "cli"):
        cli_test()
    if which in ("all", "mcp"):
        mcp_test()
    print("record region ok")


if __name__ == "__main__":
    main()