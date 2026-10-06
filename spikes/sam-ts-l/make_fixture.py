"""Create a deterministic, self-generated page for numerical parity testing."""

from pathlib import Path

from PIL import Image, ImageDraw


def main() -> None:
    target = Path(__file__).parent / "fixtures" / "synthetic_page.png"
    target.parent.mkdir(parents=True, exist_ok=True)
    image = Image.new("RGB", (701, 957), (245, 241, 230))
    draw = ImageDraw.Draw(image)
    draw.rectangle((28, 34, 672, 920), outline=(45, 45, 51), width=5)
    draw.ellipse((85, 85, 613, 441), fill=(255, 255, 255), outline=(30, 30, 35), width=5)
    draw.polygon([(500, 402), (552, 491), (433, 428)], fill=(255, 255, 255))
    draw.line([(500, 402), (552, 491), (433, 428)], fill=(30, 30, 35), width=5)
    # Deliberate disconnected, thin, and diagonal marks exercise threshold edges.
    for row, y in enumerate((134, 193, 252, 311, 370)):
        left = 145 + row * 7
        draw.rectangle((left, y, left + 330, y + 9), fill=(20, 22, 28))
        for x in range(left + 11, left + 329, 47):
            draw.rectangle((x, y - 13, x + 8, y + 25), fill=(20, 22, 28))
    draw.line([(72, 665), (239, 527), (473, 752)], fill=(171, 28, 44), width=24)
    draw.line([(112, 787), (312, 551), (603, 783)], fill=(25, 69, 153), width=9)
    for i in range(16):
        x = 75 + i * 34
        y = 836 + (i % 3) * 9
        draw.ellipse((x, y, x + 5, y + 5), fill=(16, 16, 16))
    image.save(target, optimize=True)


if __name__ == "__main__":
    main()
