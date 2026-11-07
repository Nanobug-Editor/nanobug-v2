"""Render production Rust motion traces as a comparison GIF; this is not a screen recording."""
import argparse
import csv
from collections import defaultdict
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


def font(size):
    """Return a readable font at pixel `size`, using the Windows CJK font when available."""
    path = Path("C:/Windows/Fonts/msyh.ttc")
    return ImageFont.truetype(str(path), size) if path.exists() else ImageFont.load_default()


def hull(points):
    """Return a non-crossing convex outline for the sampled `points`."""
    points = sorted(set(points))
    def cross(a, b, c):
        """Signed turn of points `a`, `b`, `c`."""
        return (b[0]-a[0])*(c[1]-a[1])-(b[1]-a[1])*(c[0]-a[0])
    result = []
    for point in points:
        while len(result) > 1 and cross(result[-2], result[-1], point) <= 0:
            result.pop()
        result.append(point)
    lower = len(result)
    for point in reversed(points[:-1]):
        while len(result) > lower and cross(result[-2], result[-1], point) <= 0:
            result.pop()
        result.append(point)
    return result[:-1]


def render(trace, output):
    """Load the model CSV at `trace` and write the comparison GIF to `output`."""
    samples = defaultdict(list)
    with trace.open(encoding="utf-8", newline="") as stream:
        for row in csv.DictReader(stream):
            points = [(float(row[f"x{i}"]), float(row[f"y{i}"])) for i in range(4)]
            samples[int(row["frame"])].append((int(row["effect"]), int(row["kind"]), float(row["value"]), points))
    background = Image.new("RGBA", (940, 490), "#0b1624")
    draw = ImageDraw.Draw(background)
    draw.text((18, 9), "光标动效模型对比 · F6 切换", font=font(19), fill="#dce7ed")
    labels = ["流体 / 四角跟随", "弹性 / 小幅回弹", "余晖 / 渐隐轨迹", "涟漪 / 落点圆环"]
    colors = [(157, 240, 212), (242, 208, 155), (145, 213, 255), (198, 178, 250)]
    origins = [(15, 46), (480, 46), (15, 253), (480, 253)]
    for effect, (x, y) in enumerate(origins):
        draw.rounded_rectangle((x, y, x+445, y+193), radius=6, outline="#63748a", width=1)
        draw.text((x+12, y+9), labels[effect], font=font(16), fill=colors[effect])
        for row in range(5):
            draw.line((x+20, y+49+row*28, x+425, y+49+row*28), fill="#172839")
        draw.text((x+20, y+170), "长跳 → 换向 → 块/竖线 → 连续短步", font=font(12), fill="#91a5bf")
    draw.text((18, 465), "由实际 Rust 动效模型采样，非窗口录屏；逻辑输入位置始终立即更新。", font=font(13), fill="#91a5bf")
    frames = []
    for frame in sorted(samples):
        image = background.copy()
        overlay = Image.new("RGBA", image.size)
        painter = ImageDraw.Draw(overlay)
        for effect, kind, value, points in samples[frame]:
            ox, oy = origins[effect]
            points = [(x+ox, y+oy) for x, y in points]
            color = colors[effect]
            if kind == 3:
                painter.line(points+[points[0]], fill=(130, 150, 170, 70), width=1)
            elif kind == 2:
                x, y = points[0]
                radius = 5+value*29
                painter.ellipse((x-radius, y-radius, x+radius, y+radius), outline=(*color, int((1-value)**2*170)), width=1)
            else:
                outline = hull(points)
                if len(outline) > 2:
                    painter.polygon(outline, fill=(*color, 184 if kind == 0 else int(65*value*value)))
        frames.append(Image.alpha_composite(image, overlay).convert("RGB"))
    output.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(output, save_all=True, append_images=frames[1:], duration=20, loop=0, optimize=True, disposal=2)
    print(f"{output.resolve()} ({len(frames)} frames)")


def main():
    """Parse trace/output paths and produce a deterministic comparison artifact."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    render(args.trace, args.output)


if __name__ == "__main__":
    main()
