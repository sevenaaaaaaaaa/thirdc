#!/usr/bin/env python3
"""生成桌面端图标（desktop/icons/*）。

契约：颜色用 oklch 声明（与 tokens.css 同源），脚本里换算到 sRGB——
仓库里不出现 hex 字面量。图形语义：三个知识节点 + 连线（画布 = 知识图谱）。

用法：python3 scripts/gen_icons.py
产物：icon.png(1024) / 32x32.png / 64x64.png / 128x128.png / 128x128@2x.png
      / icon-512.png / icon.icns / icon.ico
"""
from __future__ import annotations

import math
import struct
from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "desktop" / "icons"
SS = 4  # 超采样倍数


# ── oklch → sRGB ────────────────────────────────────────────────────────────
def oklch(l: float, c: float, h: float) -> tuple[int, int, int]:
    """l: 0..1，c: chroma，h: 角度。返回 8bit sRGB。"""
    hr = math.radians(h)
    a, b = c * math.cos(hr), c * math.sin(hr)
    l_ = l + 0.3963377774 * a + 0.2158037573 * b
    m_ = l - 0.1055613458 * a - 0.0638541728 * b
    s_ = l - 0.0894841775 * a - 1.2914855480 * b
    lc, mc, sc = l_**3, m_**3, s_**3
    lin = (
        +4.0767416621 * lc - 3.3077115913 * mc + 0.2309699292 * sc,
        -1.2684380046 * lc + 2.6097574011 * mc - 0.3413193965 * sc,
        -0.0041960863 * lc - 0.7034186147 * mc + 1.7076147010 * sc,
    )

    def enc(u: float) -> int:
        u = max(0.0, min(1.0, u))
        u = 12.92 * u if u <= 0.0031308 else 1.055 * u ** (1 / 2.4) - 0.055
        return round(u * 255)

    return tuple(enc(v) for v in lin)  # type: ignore[return-value]


A0 = oklch(0.60, 0.17, 258)  # 起点：品牌蓝（accent 同族，提亮）
A1 = oklch(0.63, 0.19, 300)  # 终点：紫
INK = (255, 255, 255)


def gradient(size: int) -> Image.Image:
    """对角线性渐变，逐行插值（1024 下肉眼无带状）。"""
    img = Image.new("RGB", (size, size))
    px = img.load()
    for y in range(size):
        for x in range(size):
            t = (x + y) / (2 * (size - 1))
            px[x, y] = tuple(round(A0[i] + (A1[i] - A0[i]) * t) for i in range(3))
    return img


def squircle_mask(size: int, n: float = 4.6) -> Image.Image:
    """超椭圆遮罩（|x|^n + |y|^n = 1），比圆角矩形更接近 macOS 图标轮廓。"""
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    r = size / 2
    pts = []
    for i in range(721):
        th = i * math.pi / 360
        ct, st = math.cos(th), math.sin(th)
        x = math.copysign(abs(ct) ** (2 / n), ct)
        y = math.copysign(abs(st) ** (2 / n), st)
        pts.append((r + x * r, r + y * r))
    d.polygon(pts, fill=255)
    return m


def draw_mark(img: Image.Image) -> None:
    """三节点知识图谱：三条连线 + 三个实心节点（32px 下仍能分辨）。"""
    s = img.size[0]
    d = ImageDraw.Draw(img, "RGBA")
    # 相对坐标（0..1），在 32px 下仍能分辨
    nodes = [(0.30, 0.34), (0.72, 0.28), (0.52, 0.72)]
    edges = [(0, 1), (0, 2), (1, 2)]
    lw = max(1, round(s * 0.042))
    for a, b in edges:
        d.line(
            [(nodes[a][0] * s, nodes[a][1] * s), (nodes[b][0] * s, nodes[b][1] * s)],
            fill=INK + (150,),
            width=lw,
        )
    radii = [0.112, 0.082, 0.094]
    for (cx, cy), rr in zip(nodes, radii):
        r = rr * s
        box = [cx * s - r, cy * s - r, cx * s + r, cy * s + r]
        d.ellipse(box, fill=INK + (255,))


def master(size: int) -> Image.Image:
    big = size * SS
    base = gradient(big).convert("RGBA")
    draw_mark(base)
    base.putalpha(squircle_mask(big))
    return base.resize((size, size), Image.LANCZOS)


def write_icns(png_by_type: dict[bytes, bytes], path: Path) -> None:
    """最小 icns 写入器：magic + 逐块 (OSType, len, PNG)。"""
    body = b"".join(t + struct.pack(">I", len(p) + 8) + p for t, p in png_by_type.items())
    path.write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    m1024 = master(1024)

    sizes = {
        "icon.png": 1024,
        "icon-512.png": 512,
        "128x128@2x.png": 256,
        "128x128.png": 128,
        "64x64.png": 64,
        "32x32.png": 32,
    }
    rendered: dict[int, Image.Image] = {}
    for name, sz in sizes.items():
        im = m1024 if sz == 1024 else m1024.resize((sz, sz), Image.LANCZOS)
        rendered[sz] = im
        im.save(OUT / name)

    # icns：ic07..ic14（128/256/512/1024 各 1x/2x）
    def enc(sz: int) -> bytes:
        import io

        buf = io.BytesIO()
        (rendered.get(sz) or m1024.resize((sz, sz), Image.LANCZOS)).save(buf, "PNG")
        return buf.getvalue()

    write_icns(
        {
            b"ic07": enc(128),
            b"ic08": enc(256),
            b"ic09": enc(512),
            b"ic10": enc(1024),
            b"ic11": enc(32),
            b"ic12": enc(64),
            b"ic13": enc(256),
            b"ic14": enc(512),
        },
        OUT / "icon.icns",
    )

    m1024.save(OUT / "icon.ico", sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    print("icons ->", OUT)


if __name__ == "__main__":
    main()
