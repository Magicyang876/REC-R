"""Locate a recorded frame inside a full-screen screenshot (numpy, exact).

    python scripts/find_frame.py <video.mp4 | frame.png> <screen.png>

Prints the best matching offsets with normalised cross-correlation, so a capture
whose coordinates are wrong shows up as an offset that is not (0,0).
"""

import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image


def load_frame(src, tmp):
    if src.lower().endswith(".png"):
        return Image.open(src).convert("L")
    png = tmp + "/find-frame.png"
    subprocess.run(
        ["ffmpeg", "-v", "error", "-sseof", "-0.1", "-i", src, "-frames:v", "1", "-y", png],
        check=True,
    )
    return Image.open(png).convert("L")


def main():
    src, screen_path = sys.argv[1], sys.argv[2]
    tmp = tempfile.gettempdir()
    frame = load_frame(src, tmp)
    screen = Image.open(screen_path).convert("L")
    print(f"frame {frame.size}, screen {screen.size}")

    f = np.asarray(frame, dtype=np.float32)
    s = np.asarray(screen, dtype=np.float32)
    fh, fw = f.shape
    if (fh, fw) > s.shape:
        raise SystemExit("frame is larger than the screen")
    f = f - f.mean()
    f_norm = np.sqrt((f * f).sum())

    # Stride-4 search, then refine the winner with a stride-1 neighbourhood.
    def ncc_at(x, y):
        win = s[y:y + fh, x:x + fw]
        win = win - win.mean()
        denom = np.sqrt((win * win).sum()) * f_norm
        return 1.0 if denom == 0 else float((f * win).sum() / denom)

    scores = []
    for y in range(0, s.shape[0] - fh + 1, 4):
        for x in range(0, s.shape[1] - fw + 1, 4):
            scores.append((ncc_at(x, y), x, y))
    scores.sort(reverse=True)
    best = scores[0]
    for y in range(max(0, best[2] - 4), min(s.shape[0] - fh, best[2] + 4) + 1):
        for x in range(max(0, best[1] - 4), min(s.shape[1] - fw, best[1] + 4) + 1):
            score = ncc_at(x, y)
            if score > best[0]:
                best = (score, x, y)
    print(f"best match: {best[1]},{best[2]}  ncc {best[0]:.4f}")
    print("runners up:", [(round(sc, 3), x, y) for sc, x, y in scores[:5] if (x, y) != (best[1], best[2])][:4])


if __name__ == "__main__":
    main()