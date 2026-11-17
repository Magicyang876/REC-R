import sys

from PIL import Image

a = Image.open(sys.argv[1]).convert("RGB")
b = Image.open(sys.argv[2]).convert("RGB")
sheet = Image.new("RGB", (max(a.width, b.width), a.height + b.height + 6), (255, 0, 255))
sheet.paste(a, (0, 0))
sheet.paste(b, (0, a.height + 6))
sheet.save(sys.argv[3])
for name, im in (("top", a), ("bottom", b)):
    print(name, im.size, "colors", len(im.getcolors(1 << 20) or []), "mean", [round(v) for v in im.resize((1, 1)).getpixel((0, 0))])
print("wrote", sys.argv[3])