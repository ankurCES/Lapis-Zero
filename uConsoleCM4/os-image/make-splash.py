"""Renders the Lapis-Deck boot splash art into stage-lapis/files/plymouth/lapis/ (committed).

    uv run --with pillow python os-image/make-splash.py [path/to/Hack-Regular.ttf]

The art is upright landscape on square canvases (Plymouth's Image.Rotate keeps the size, so a
square turns without clipping); lapis.script turns it onto the portrait panel. Colours are the
HUD's (crates/lapis-ui/src/theme.rs); the font is egui's Hack, the HUD's monospace.
"""

import glob
import math
import os
import random
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "stage-lapis/files/plymouth/lapis")
S, K = 720, 4  # canvas size, supersampling factor
CX, CY, R = 360, 250, 128  # gem centre and outer radius (canvas pixels)
TEXT, CYAN, FAINT = (240, 244, 247), (0, 255, 255), (98, 110, 122)
DEEP, BRIGHT = (14, 28, 96), (58, 108, 226)  # lapis lazuli, shadowed to lit facet
GOLD = [(212, 175, 55), (255, 184, 64), (240, 214, 120)]  # pyrite flecks


def hexagon(r, cx=CX, cy=CY, k=1):
    return [((cx + r * math.sin(math.pi / 3 * i)) * k, (cy - r * math.cos(math.pi / 3 * i)) * k) for i in range(6)]


def mix(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def gem(d):
    outer, table = hexagon(R, k=K), hexagon(R * 0.48, CX - 8, CY - 10, K)  # table off-centre: a cut stone
    for i in range(6):
        j = (i + 1) % 6
        for tri in ((outer[i], outer[j], table[i]), (outer[j], table[j], table[i])):
            mx = sum(p[0] for p in tri) / 3 - CX * K
            my = sum(p[1] for p in tri) / 3 - CY * K
            lit = 0.5 + 0.5 * (-0.55 * mx - 0.8 * my) / (math.hypot(mx, my) or 1)  # light from top left
            d.polygon(tri, fill=mix(DEEP, BRIGHT, lit * lit))
    d.polygon(table, fill=mix(DEEP, BRIGHT, 0.62))
    for i in range(6):
        d.line([outer[i], table[i]], fill=(110, 160, 255, 120), width=K)
    d.line(table + table[:1], fill=(150, 190, 255, 150), width=K)
    rng = random.Random(7)  # fixed seed: the same flecks every render
    for _ in range(26):
        a, r = rng.uniform(0, 2 * math.pi), R * 0.8 * math.sqrt(rng.random())
        x, y, s = CX + r * math.cos(a), CY + r * math.sin(a), rng.uniform(1.2, 3.4)
        d.ellipse([(x - s) * K, (y - s) * K, (x + s) * K, (y + s) * K], fill=rng.choice(GOLD))
    d.line(outer + outer[:1], fill=CYAN + (230,), width=3 * K, joint="curve")


def spaced(d, y, text, font, colours, gap, stroke=0):
    """Centred, letter-spaced text; colours[i] colours character i."""
    widths = [d.textlength(c, font=font) for c in text]
    x = (S * K - sum(widths) - gap * (len(text) - 1)) / 2
    for c, w, fill in zip(text, widths, colours):
        d.text((x, y), c, font=font, fill=fill, stroke_width=stroke, stroke_fill=fill)
        x += w + gap


def main():
    fonts = sys.argv[1:] or glob.glob(os.path.expanduser("~/.cargo/registry/src/*/epaint_default_fonts-*/fonts/Hack-Regular.ttf"))
    if not fonts:
        sys.exit("Hack-Regular.ttf not found: pass its path (egui's epaint_default_fonts crate ships it)")
    os.makedirs(OUT, exist_ok=True)

    art = Image.new("RGBA", (S * K, S * K), (0, 0, 0, 0))
    d = ImageDraw.Draw(art)
    gem(d)
    word = "LAPIS DECK"
    spaced(d, 418 * K, word, ImageFont.truetype(fonts[0], 58 * K), [TEXT] * 5 + [CYAN] * 5, 10 * K, K)
    d.line([(250 * K, 506 * K), (470 * K, 506 * K)], fill=CYAN + (110,), width=K)
    spaced(d, 522 * K, "CYBERDECK OS", ImageFont.truetype(fonts[0], 17 * K), [FAINT] * 12, 7 * K)
    art.resize((S, S), Image.LANCZOS).save(os.path.join(OUT, "logo.png"), optimize=True)

    glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    g = ImageDraw.Draw(glow)
    g.polygon(hexagon(R + 26), fill=(20, 90, 230, 150))
    g.line(hexagon(R + 6) + hexagon(R + 6)[:1], fill=CYAN + (200,), width=10)
    glow.filter(ImageFilter.GaussianBlur(26)).save(os.path.join(OUT, "glow.png"), optimize=True)

    dot = Image.new("RGBA", (16 * K, 16 * K), (0, 0, 0, 0))
    ImageDraw.Draw(dot).ellipse([5 * K, 5 * K, 11 * K, 11 * K], fill=CYAN + (255,))
    dot = Image.alpha_composite(dot.filter(ImageFilter.GaussianBlur(3 * K)), dot)
    dot.resize((16, 16), Image.LANCZOS).save(os.path.join(OUT, "dot.png"), optimize=True)
    print("wrote", OUT)


if __name__ == "__main__":
    main()
