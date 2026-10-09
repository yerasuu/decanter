"""Draws the gamepad button icons decanter puts into Bloodborne's menus and HUD, in three styles:
`abxy` (A B X Y faces, LB/RB/LT/RT), `ps` (cross circle square triangle, L1/R1/L2/R2) and `bayx`
(B A Y X faces, L/R/ZL/ZR, + and -). Each SDL button name gets the label its pad prints.

Run from the repository root after changing the art:
    nix-shell -p "python3.withPackages (p: [ p.pillow ])" --run \
        "python3 crates/ps/assets/glyphs/generate.py app/assets/fonts/Inter.ttf crates/ps/assets/glyphs"

Every icon keeps to one gray ramp (rim, fill) plus one accent: decanter encodes them as BC7 mode 6,
one color line per 4x4 block, and a block mixing more than that shows a tinted fringe.
"""
import os, sys
from PIL import Image, ImageDraw, ImageFont

FONT, OUT = sys.argv[1:3]
SS = 8
RIM, FILL, WHITE = (150, 150, 156, 255), (44, 44, 48, 255), (235, 235, 238, 255)
SHARED = {'misc1': 'M', 'paddle1': 'P1', 'paddle2': 'P2', 'paddle3': 'P3', 'touchpad': 'TP',
          'stick-l': 'L', 'stick-r': 'R'}
STYLES = {
    'abxy': {
        'faces': {'a': ('A', (108, 196, 74, 255)), 'b': ('B', (226, 74, 66, 255)),
                  'x': ('X', (64, 140, 230, 255)), 'y': ('Y', (240, 196, 50, 255))},
        'labels': {'leftshoulder': 'LB', 'rightshoulder': 'RB', 'lefttrigger': 'LT', 'righttrigger': 'RT',
                   'leftstick': 'LS', 'rightstick': 'RS'},
        'start': 'menu', 'back': 'view',
    },
    'ps': {
        'shapes': {'a': ('cross', (110, 150, 235, 255)), 'b': ('circle', (232, 92, 96, 255)),
                   'x': ('square', (222, 130, 196, 255)), 'y': ('triangle', (90, 200, 160, 255))},
        'labels': {'leftshoulder': 'L1', 'rightshoulder': 'R1', 'lefttrigger': 'L2', 'righttrigger': 'R2',
                   'leftstick': 'L3', 'rightstick': 'R3', 'start': 'OPT', 'back': 'SH'},
    },
    'bayx': {
        'faces': {'a': ('B', (235, 235, 238, 255)), 'b': ('A', (235, 235, 238, 255)),
                  'x': ('Y', (235, 235, 238, 255)), 'y': ('X', (235, 235, 238, 255))},
        'labels': {'leftshoulder': 'L', 'rightshoulder': 'R', 'lefttrigger': 'ZL', 'righttrigger': 'ZR',
                   'leftstick': 'LS', 'rightstick': 'RS', 'start': '+', 'back': '\u2212'},
    },
}


def base(size):
    img = Image.new('RGBA', (size * SS, size * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    r = size * SS
    d.ellipse((r * 0.02, r * 0.02, r * 0.98, r * 0.98), fill=RIM)
    d.ellipse((r * 0.10, r * 0.10, r * 0.90, r * 0.90), fill=FILL)
    return img, d, r


def text(d, r, label, color):
    size = 0.72 if len(label) == 1 else 0.46
    font = ImageFont.truetype(FONT, int(r * size))
    box = d.textbbox((0, 0), label, font=font)
    d.text(((r - (box[2] - box[0])) / 2 - box[0], (r - (box[3] - box[1])) / 2 - box[1]), label, font=font, fill=color)


def arrow(d, r, direction):
    c, s = r / 2, r * 0.2
    points = {'dpup': [(c, c - s), (c - s, c + s * 0.6), (c + s, c + s * 0.6)],
              'dpdown': [(c, c + s), (c - s, c - s * 0.6), (c + s, c - s * 0.6)],
              'dpleft': [(c - s, c), (c + s * 0.6, c - s), (c + s * 0.6, c + s)],
              'dpright': [(c + s, c), (c - s * 0.6, c - s), (c - s * 0.6, c + s)]}[direction]
    d.polygon(points, fill=WHITE)


def shape(d, r, kind, color):
    c, s, w = r / 2, r * 0.2, int(r * 0.075)
    if kind == 'cross':
        d.line((c - s, c - s, c + s, c + s), fill=color, width=w)
        d.line((c - s, c + s, c + s, c - s), fill=color, width=w)
    elif kind == 'circle':
        d.ellipse((c - s, c - s, c + s, c + s), outline=color, width=w)
    elif kind == 'square':
        d.rectangle((c - s * 0.9, c - s * 0.9, c + s * 0.9, c + s * 0.9), outline=color, width=w)
    else:
        d.polygon([(c, c - s * 1.05), (c - s * 1.1, c + s * 0.8), (c + s * 1.1, c + s * 0.8)], outline=color,
                  width=w)


def menu(d, r):
    for y in (0.38, 0.5, 0.62):
        d.rectangle((r * 0.32, r * (y - 0.03), r * 0.68, r * (y + 0.03)), fill=WHITE)


def view(d, r):
    w = SS * 2
    d.rectangle((r * 0.38, r * 0.32, r * 0.66, r * 0.56), outline=WHITE, width=w)
    d.rectangle((r * 0.32, r * 0.44, r * 0.6, r * 0.68), fill=FILL, outline=WHITE, width=w)


def draw(style, name, size):
    img, d, r = base(size)
    spec = STYLES[style]
    if name in spec.get('faces', {}):
        text(d, r, *spec['faces'][name])
    elif name in spec.get('shapes', {}):
        shape(d, r, *spec['shapes'][name])
    elif name in spec['labels']:
        text(d, r, spec['labels'][name], WHITE)
    elif name in SHARED:
        text(d, r, SHARED[name], WHITE)
    elif name.startswith('dp'):
        arrow(d, r, name)
    elif spec.get(name) == 'menu':
        menu(d, r)
    elif spec.get(name) == 'view':
        view(d, r)
    else:
        raise SystemExit(f'no {style} art for {name}')
    return img.resize((size, size), Image.LANCZOS)


NAMES = ['a', 'b', 'x', 'y', 'leftshoulder', 'rightshoulder', 'lefttrigger', 'righttrigger', 'leftstick',
         'rightstick', 'start', 'back', 'dpup', 'dpdown', 'dpleft', 'dpright'] + list(SHARED)
for style in STYLES:
    os.makedirs(os.path.join(OUT, style), exist_ok=True)
    for name in NAMES:
        for size in (32, 26):
            draw(style, name, size).save(os.path.join(OUT, style, f'{name}_{size}.png'), optimize=True)
print(len(STYLES) * len(NAMES) * 2, 'icons')
