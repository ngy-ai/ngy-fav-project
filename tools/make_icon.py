"""生成应用图标。

用法：

    python tools/make_icon.py

产出 `assets/icon.ico`（多尺寸，供 Windows 可执行文件嵌入）和 `assets/icon.png`（预览用）。
形状画在 4 倍尺寸上再降采样，所以边缘自带抗锯齿，不必手工描边。
"""

from __future__ import annotations

import math
from pathlib import Path

from PIL import Image, ImageDraw

ASSETS = Path(__file__).resolve().parent.parent / "assets"

BASE = 256  # 最终基准边长
SUPERSAMPLE = 4
CANVAS = BASE * SUPERSAMPLE  # 绘制用画布

# 底色比 Windows 深色任务栏亮一档。纯黑会和任务栏糊在一起，轮廓就没了。
BG_TOP = (56, 68, 92)
BG_BOTTOM = (30, 40, 58)
# 星用暖金色，在深底上辨识度最高。
STAR_TOP = (255, 214, 102)
STAR_BOTTOM = (243, 158, 44)

CORNER_RATIO = 0.22
STAR_HEIGHT_RATIO = 0.63  # 星占画布高度的比例。16px 下再小就认不出来了。
STAR_INNER_RATIO = 0.382  # 内半径 / 外半径，五角星常用的黄金比例


def vertical_gradient(size: int, top: tuple[int, int, int], bottom: tuple[int, int, int]) -> Image.Image:
    """竖直渐变。逐行生成再放大，比逐像素赋值快三个数量级。"""
    column = Image.new("RGB", (1, size))
    pixels = column.load()
    for y in range(size):
        t = y / max(size - 1, 1)
        pixels[0, y] = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom))
    return column.resize((size, size), Image.Resampling.NEAREST)


def rounded_mask(size: int, radius: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size - 1, size - 1), radius=radius, fill=255)
    return mask


def star_polygon(canvas: int, height_ratio: float, inner_ratio: float) -> list[tuple[float, float]]:
    """五个角的顶点之一朝上、整体居中的五角星。

    五角星的重心不在外接圆圆心上（下半部分更挤），所以先按包围盒求出真实中心再平移，
    否则视觉上会往下掉。
    """
    outer = canvas * height_ratio / (1 + math.cos(math.radians(36)))
    inner = outer * inner_ratio

    points: list[tuple[float, float]] = []
    for index in range(10):
        radius = outer if index % 2 == 0 else inner
        angle = -math.pi / 2 + index * math.pi / 5
        points.append((radius * math.cos(angle), radius * math.sin(angle)))

    xs = [x for x, _ in points]
    ys = [y for _, y in points]
    offset_x = canvas / 2 - (min(xs) + max(xs)) / 2
    offset_y = canvas / 2 - (min(ys) + max(ys)) / 2
    return [(x + offset_x, y + offset_y) for x, y in points]


def render(canvas: int) -> Image.Image:
    icon = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    icon.paste(
        vertical_gradient(canvas, BG_TOP, BG_BOTTOM),
        (0, 0),
        rounded_mask(canvas, round(canvas * CORNER_RATIO)),
    )

    star_mask = Image.new("L", (canvas, canvas), 0)
    ImageDraw.Draw(star_mask).polygon(
        star_polygon(canvas, STAR_HEIGHT_RATIO, STAR_INNER_RATIO), fill=255
    )
    icon.paste(vertical_gradient(canvas, STAR_TOP, STAR_BOTTOM), (0, 0), star_mask)
    return icon


def main() -> None:
    ASSETS.mkdir(parents=True, exist_ok=True)
    base = render(CANVAS).resize((BASE, BASE), Image.Resampling.LANCZOS)
    base.save(ASSETS / "icon.png")

    sizes = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)]
    base.save(ASSETS / "icon.ico", format="ICO", sizes=sizes)
    print(f"已写入 {ASSETS / 'icon.ico'}（{len(sizes)} 个尺寸）与 {ASSETS / 'icon.png'}")


if __name__ == "__main__":
    main()
