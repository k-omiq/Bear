"""Contact sheet: each concept row shows flare and sunburst on light and dark, plus a 32 px preview."""
import json, sys
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

here = Path(__file__).parent
slugs = [s for s, _ in json.load(open(here / "concepts.json"))["concepts"]]
if len(sys.argv) > 1:
    slugs = [s for s in slugs if s.split("-")[0] in sys.argv[1].split(",")]
T = 220
font = ImageFont.truetype("/System/Library/Fonts/Helvetica.ttc", 20)
cols = [("flare", "#F6F5F2"), ("flare", "#1E1E1E"), ("sunburst", "#F6F5F2"), ("sunburst", "#1E1E1E")]
W = 260 + len(cols) * (T + 10) + 120
sheet = Image.new("RGB", (W, len(slugs) * (T + 10) + 10), "#DDDCD8")
d = ImageDraw.Draw(sheet)
for r, slug in enumerate(slugs):
    y = 10 + r * (T + 10)
    d.text((10, y + T // 2 - 10), slug, fill="#141514", font=font)
    for c, (model, bg) in enumerate(cols):
        x = 260 + c * (T + 10)
        cell = Image.new("RGBA", (T, T), bg)
        p = here / "out" / f"{slug}-{model}.png"
        if p.exists():
            im = Image.open(p).convert("RGBA").resize((T, T), Image.LANCZOS)
            cell.alpha_composite(im)
            if c == 3:  # tiny preview of sunburst on light bg
                small = Image.open(p).convert("RGBA").resize((32, 32), Image.LANCZOS)
                sb = Image.new("RGBA", (48, 48), "#F6F5F2"); sb.alpha_composite(small, (8, 8))
                sheet.paste(sb.convert("RGB"), (x + T + 20, y + T // 2 - 24))
        sheet.paste(cell.convert("RGB"), (x, y))
out = here / (f"sheet-{sys.argv[1]}.png" if len(sys.argv) > 1 else "sheet.png")
sheet.save(out); print(out)
