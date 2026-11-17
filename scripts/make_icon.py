"""Render the rec-r app icon (no external assets, only Pillow).

    python scripts/make_icon.py

Writes assets/rec-r.ico (16..256 px, PNG-compressed entries), assets/icon-256.png
and assets/icon-16.png (a 16 px preview, upscaled x8 for inspection).
"""

from PIL import Image, ImageDraw

S = 1024  # supersampled canvas
INSET = 6
RADIUS = 224
BRACKET = 112  # stroke width of the crop marks
ARM = 286
MARGIN = 120
RED = (255, 59, 48, 255)
RED_DARK = (198, 32, 24, 255)


def rounded_mask(size, inset, radius):
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (inset, inset, size - inset - 1, size - inset - 1), radius=radius, fill=255
    )
    return mask


def gradient(size, top, bottom):
    img = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / max(size - 1, 1)
        img.putpixel(
            (0, y),
            tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)),
        )
    return img.resize((size, size), Image.BILINEAR)


def build():
    canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    body = gradient(S, (44, 54, 72), (22, 27, 38)).convert("RGBA")
    mask = rounded_mask(S, INSET, RADIUS)
    canvas.paste(body, (0, 0), mask)

    draw = ImageDraw.Draw(canvas)

    # Crop marks: four L-shaped brackets, drawn as thick rounded strokes.
    w = BRACKET
    x0, y0 = MARGIN, MARGIN
    x1, y1 = S - MARGIN, S - MARGIN
    for (cx, cy, dx, dy) in (
        (x0, y0, 1, 1),
        (x1, y0, -1, 1),
        (x0, y1, 1, -1),
        (x1, y1, -1, -1),
    ):
        draw.rounded_rectangle(
            (min(cx, cx + dx * ARM), min(cy, cy + dy * w), max(cx, cx + dx * ARM), max(cy, cy + dy * w)),
            radius=w // 2,
            fill=(255, 255, 255, 255),
        )
        draw.rounded_rectangle(
            (min(cx, cx + dx * w), min(cy, cy + dy * ARM), max(cx, cx + dx * w), max(cy, cy + dy * ARM)),
            radius=w // 2,
            fill=(255, 255, 255, 255),
        )

    # Flat record dot: any inner detail turns to mush at 16 px.
    r = 150
    c = S // 2
    draw.ellipse((c - r, c - r, c + r, c + r), fill=RED, outline=RED_DARK, width=26)

    return canvas


def main():
    big = build()
    big.save("assets/icon-256.png")
    big.resize((16, 16), Image.LANCZOS).resize((128, 128), Image.NEAREST).save(
        "assets/icon-16.png"
    )
    big.save(
        "assets/rec-r.ico",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("wrote assets/rec-r.ico, assets/icon-256.png, assets/icon-16.png")


if __name__ == "__main__":
    main()