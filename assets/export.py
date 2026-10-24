"""Draws Replica's icons: the SVG masters, the .ico and .png files, and the preview sheet.

    python assets/export.py

Build-only (Python 3.10+ and Pillow; see assets/README.md). Every icon is described once, below,
as a list of shapes on a 16-unit grid; the same list is written as SVG and drawn as pixels. Each
pixel size is drawn on its own (supersampled, then reduced), not shrunk from a big image, and
32 px and below use the small drawing, flat and bolder, so they read on a taskbar (16 and
20 px with one page only).

The icons are in the family style Dialect shares: one bold circle with a vertical gradient, a soft
gloss ring, a dark face and a simple glyph in the brand's light tones. Replica's glyph is its own:
two offset pages, the front one with a "go back in time" arrow.
"""

from __future__ import annotations

import argparse
import math
import sys
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ASSETS = Path(__file__).resolve().parent
GRID = 16.0

APP_ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
APP_PNG_SIZES = [64, 128, 256, 512, 1024]
TRAY_ICO_SIZES = [16, 20, 24, 32]
# Up to these sizes, the small drawing (two pages), and the tiny one (one page).
SMALL_UP_TO = 32
TINY_UP_TO = 20

# The palette (assets/README.md lists it as tokens). Fixed: the same in both themes.
TEAL_200 = "#A5E8F3"  # The front page.
TEAL_300 = "#67D2E6"  # Gradient top; the back page's outline.
TEAL_400 = "#22A9C6"  # Gradient middle.
TEAL_500 = "#1497B5"  # The small icon's and the tray's flat disc.
TEAL_600 = "#0E7490"  # Gradient bottom: Replica's teal, the UI's accent.
FACE = "#0F2A31"  # The face, top.
FACE_DEEP = "#081B20"  # The face, bottom, and its ring; the arrow; the small icon's glyph.
WHITE = "#FFFFFF"
# Status dots on the tray icon.
RED = "#E0352B"
AMBER = "#F5A300"


# --- Shapes ------------------------------------------------------------------------------------


@dataclass
class Gradient:
    """A vertical gradient over the shape's box: (offset 0 to 1, colour, opacity) stops."""

    stops: list[tuple[float, str, float]]


Paint = str | Gradient | None


@dataclass
class RoundRect:
    x0: float
    y0: float
    x1: float
    y1: float
    radius: float
    fill: Paint = None
    stroke: Paint = None
    width: float = 0.0  # A stroke lies inside the rectangle.


@dataclass
class Circle:
    cx: float
    cy: float
    r: float
    fill: Paint = None
    stroke: Paint = None
    width: float = 0.0  # A stroke is centred on the circle, as in SVG.
    opacity: float = 1.0


@dataclass
class Arc:
    """A stroke along a circle, from `start` to `end` degrees, clockwise on screen (0 = 3 o'clock)."""

    cx: float
    cy: float
    r: float
    start: float
    end: float
    width: float
    stroke: Paint = None


@dataclass
class Polygon:
    points: list[tuple[float, float]]
    fill: Paint = None


Shape = RoundRect | Circle | Arc | Polygon


@dataclass
class Erase:
    """Cuts these shapes out of everything drawn before it (their colours are ignored)."""

    shapes: list[Shape]


@dataclass
class Group:
    """Steps drawn on a layer of their own, so an [`Erase`] in it cuts only what's in it."""

    steps: list["Step"]


Step = Shape | Erase | Group


@dataclass
class Icon:
    steps: list[Step] = field(default_factory=list)

    def add(self, *steps: Step) -> "Icon":
        self.steps.extend(steps)
        return self


# --- The drawings ------------------------------------------------------------------------------


def history_arrow(cx: float, cy: float, r: float, width: float, head: float, colour: str, turn: float = 0.0) -> list[Shape]:
    """A counter-clockwise circular arrow ("go back in time") round (cx, cy)."""
    # The arc runs from the head (upper right) clockwise round to its tail; the head points the
    # other way, counter-clockwise.
    start, end = -50.0 + turn, 215.0 + turn
    a = math.radians(start)
    px, py = cx + r * math.cos(a), cy + r * math.sin(a)
    tangent = (math.sin(a), -math.cos(a))  # counter-clockwise on screen
    normal = (math.cos(a), math.sin(a))
    tip = (px + tangent[0] * head * 0.9, py + tangent[1] * head * 0.9)
    back = (px - tangent[0] * head * 0.15, py - tangent[1] * head * 0.15)
    base1 = (back[0] + normal[0] * head * 0.62, back[1] + normal[1] * head * 0.62)
    base2 = (back[0] - normal[0] * head * 0.62, back[1] - normal[1] * head * 0.62)
    return [
        Arc(cx, cy, r, start + 8.0, end, width, stroke=colour),
        Polygon([tip, base1, base2], fill=colour),
    ]


def centre(box: tuple[float, float, float, float]) -> tuple[float, float]:
    return (box[0] + box[2]) / 2, (box[1] + box[3]) / 2


def grow(box: tuple[float, float, float, float], by: float) -> tuple[float, float, float, float]:
    return box[0] - by, box[1] - by, box[2] + by, box[3] + by


def pages(back: tuple, front: tuple, radius: float, outline: float, gap: float, back_ink: str, front_ink: str) -> list[Step]:
    """The back page as an outline, then the front page solid, with a gap cut out of the back
    page round it, so what's behind shows between them."""
    return [
        Group([RoundRect(*back, radius, stroke=back_ink, width=outline), Erase([RoundRect(*grow(front, gap), radius + gap)])]),
        RoundRect(*front, radius, fill=front_ink),
    ]


# The large drawing (48 px and up), on the 16-unit grid: a 512-unit master divided by 32.
BACK_LARGE = (4.5, 4.7, 9.1, 10.3)
FRONT_LARGE = (6.9, 6.0, 11.5, 11.6)


def app_icon_large() -> Icon:
    return Icon().add(
        Circle(8, 8, 7.375, fill=Gradient([(0, TEAL_300, 1), (0.55, TEAL_400, 1), (1, TEAL_600, 1)])),
        Circle(8, 8, 6.6875, stroke=Gradient([(0, WHITE, 0.35), (1, WHITE, 0)]), width=0.1875, opacity=0.6),
        Circle(8, 8, 5.625, fill=Gradient([(0, FACE, 1), (1, FACE_DEEP, 1)]), stroke=FACE_DEEP, width=0.5),
        *pages(BACK_LARGE, FRONT_LARGE, 0.55, 0.62, 0.5, TEAL_300, TEAL_200),
        *history_arrow(*centre(FRONT_LARGE), 1.4, 0.62, 1.05, FACE_DEEP),
    )


# The small drawings, and the tray's: a bright flat disc, the glyph in the deep face colour with
# the arrow cut through it. 24 and 32 px keep both pages, in bold lines; 16 and 20 px have room
# for one page only.
BACK_SMALL = (3.2, 3.1, 10.0, 10.6)
FRONT_SMALL = (5.7, 4.9, 12.8, 12.9)
PAGE_TINY = (4.0, 3.0, 12.0, 13.0)


def small_glyph(size: int, heavy: bool = False, turn: float = 0.0) -> list[Step]:
    """The glyph at `size` px: one page up to [`TINY_UP_TO`], both pages above. `heavy` (the
    tray's) draws its lines a little bolder."""
    extra = 0.15 if heavy else 0.0
    if size <= TINY_UP_TO:
        cx, cy = centre(PAGE_TINY)
        return [
            RoundRect(*PAGE_TINY, 1.2, fill=FACE_DEEP),
            *history_arrow(cx, cy + 0.6, 2.35, 1.45 + extra, 3.4, TEAL_500, turn),
        ]
    cx, cy = centre(FRONT_SMALL)
    return [
        *pages(BACK_SMALL, FRONT_SMALL, 0.9, 1.5 + extra, 0.95, FACE_DEEP, FACE_DEEP),
        *history_arrow(cx, cy + 0.55, 1.95, 1.35 + extra, 2.9, TEAL_500, turn),
    ]


def app_icon_small(size: int) -> Icon:
    return Icon().add(Circle(8, 8, 7.5, fill=TEAL_500), *small_glyph(size))


def app_icon(size: int) -> Icon:
    return app_icon_small(size) if size <= SMALL_UP_TO else app_icon_large()


TRAY_STATES = ["idle", "running-1", "running-2", "failed", "overdue"]
# The tray icon is a teal disc that reads on either taskbar (3.09:1 on light, 4.75:1 on dark),
# so both of a state's files are drawn the same; the names stay for the code that picks one.
TASKBARS = ["on-light", "on-dark"]


def tray_icon(state: str, size: int) -> Icon:
    """The small icon at `size` filling its square, with heavier strokes; the running frames
    turn only the arrow. A status dot sits at the bottom right in a cut-out ring, so it stands
    apart from the disc on either taskbar."""
    turn = 180.0 if state == "running-2" else 0.0
    icon = Icon().add(Circle(8, 8, 7.75, fill=TEAL_500), *small_glyph(size, heavy=True, turn=turn))
    dot = {"failed": RED, "overdue": AMBER}.get(state)
    if dot:
        cx, cy, r = 12.2, 12.2, 3.6
        icon.add(Erase([Circle(cx, cy, r + 1.1)]), Circle(cx, cy, r, fill=dot))
    return icon


# --- SVG ---------------------------------------------------------------------------------------


def fmt(v: float) -> str:
    return f"{v:.3f}".rstrip("0").rstrip(".")


class Svg:
    def __init__(self) -> None:
        self.defs: list[str] = []

    def paint(self, p: Paint) -> str:
        if p is None:
            return "none"
        if isinstance(p, str):
            return p
        n = len(self.defs) + 1
        stops = "".join(
            f'<stop offset="{fmt(o)}" stop-color="{c}"' + (f' stop-opacity="{fmt(a)}"' if a != 1 else "") + "/>"
            for o, c, a in p.stops
        )
        self.defs.append(f'<linearGradient id="g{n}" x1="0" y1="0" x2="0" y2="1">{stops}</linearGradient>')
        return f"url(#g{n})"

    def attrs(self, fill: Paint, stroke: Paint = None, width: float = 0.0, erase: bool = False) -> str:
        if erase:
            fill = "#000" if fill or not stroke else None
            stroke = "#000" if stroke else None
        out = f'fill="{self.paint(fill)}"'
        if stroke:
            out += f' stroke="{self.paint(stroke)}" stroke-width="{fmt(width)}"'
        return out

    def shape(self, shape: Shape, erase: bool = False) -> str:
        if isinstance(shape, RoundRect):
            x0, y0, x1, y1, r = shape.x0, shape.y0, shape.x1, shape.y1, shape.radius
            parts = []
            if shape.fill or erase and not shape.stroke:
                parts.append(
                    f'<rect x="{fmt(x0)}" y="{fmt(y0)}" width="{fmt(x1 - x0)}" height="{fmt(y1 - y0)}"'
                    f' rx="{fmt(r)}" {self.attrs(shape.fill, erase=erase)}/>'
                )
            if shape.stroke:
                h = shape.width / 2
                parts.append(
                    f'<rect x="{fmt(x0 + h)}" y="{fmt(y0 + h)}" width="{fmt(x1 - x0 - shape.width)}"'
                    f' height="{fmt(y1 - y0 - shape.width)}" rx="{fmt(max(r - h, 0))}"'
                    f" {self.attrs(None, shape.stroke, shape.width, erase)}/>"
                )
            return "".join(parts)
        if isinstance(shape, Circle):
            opacity = f' opacity="{fmt(shape.opacity)}"' if shape.opacity != 1 else ""
            return (
                f'<circle cx="{fmt(shape.cx)}" cy="{fmt(shape.cy)}" r="{fmt(shape.r)}"'
                f" {self.attrs(shape.fill, shape.stroke, shape.width, erase)}{opacity}/>"
            )
        if isinstance(shape, Arc):
            a0, a1 = math.radians(shape.start), math.radians(shape.end)
            x0, y0 = shape.cx + shape.r * math.cos(a0), shape.cy + shape.r * math.sin(a0)
            x1, y1 = shape.cx + shape.r * math.cos(a1), shape.cy + shape.r * math.sin(a1)
            large = 1 if (shape.end - shape.start) % 360 > 180 else 0
            d = f"M{fmt(x0)} {fmt(y0)}A{fmt(shape.r)} {fmt(shape.r)} 0 {large} 1 {fmt(x1)} {fmt(y1)}"
            return f'<path d="{d}" {self.attrs(None, shape.stroke, shape.width, erase)}/>'
        if isinstance(shape, Polygon):
            points = " ".join(f"{fmt(x)},{fmt(y)}" for x, y in shape.points)
            return f'<polygon points="{points}" {self.attrs(shape.fill, erase=erase)}/>'
        raise TypeError(shape)


def svg_body(svg: Svg, steps: list[Step], masks: list[str]) -> str:
    body = ""
    for step in steps:
        if isinstance(step, Group):
            body += f"<g>{svg_body(svg, step.steps, masks)}</g>"
        elif isinstance(step, Erase):
            n = len(masks) + 1
            cut = "".join(svg.shape(s, erase=True) for s in step.shapes)
            masks.append(
                f'<mask id="cut{n}" maskUnits="userSpaceOnUse" x="0" y="0" width="16" height="16">'
                f'<rect width="16" height="16" fill="#fff"/>{cut}</mask>'
            )
            body = f'<g mask="url(#cut{n})">{body}</g>'
        else:
            body += svg.shape(step)
    return body


def to_svg(icon: Icon) -> str:
    svg = Svg()
    masks: list[str] = []
    body = svg_body(svg, icon.steps, masks)
    defs = svg.defs + masks
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 16 16">\n'
        "<!-- Replica icon (MIT). Generated by assets/export.py: edit that, not this. -->\n"
        f"{'<defs>' + ''.join(defs) + '</defs>' if defs else ''}{body}\n</svg>\n"
    )


# --- Pixels ------------------------------------------------------------------------------------


def rgb(colour: str) -> tuple[int, int, int]:
    c = colour.lstrip("#")
    return int(c[0:2], 16), int(c[2:4], 16), int(c[4:6], 16)


def supersample(size: int) -> int:
    """How many times bigger a size is drawn before it's reduced: less for the big ones, which
    have pixels enough."""
    return 8 if size <= 64 else 4 if size <= 256 else 2


def fill_image(paint: str | Gradient, top: float, bottom: float, big: int) -> Image.Image:
    """`paint` over the whole canvas; a gradient runs from `top` to `bottom` (pixels)."""
    if isinstance(paint, str):
        return Image.new("RGBA", (big, big), (*rgb(paint), 255))
    column = Image.new("RGBA", (1, big))
    stops = paint.stops
    for y in range(big):
        t = min(max((y + 0.5 - top) / max(bottom - top, 1e-6), 0.0), 1.0)
        for (o0, c0, a0), (o1, c1, a1) in zip(stops, stops[1:]):
            if t <= o1 or (o1, c1, a1) == stops[-1]:
                f = 0.0 if o1 == o0 else min(max((t - o0) / (o1 - o0), 0.0), 1.0)
                r0, r1 = rgb(c0), rgb(c1)
                colour = tuple(round(r0[i] + (r1[i] - r0[i]) * f) for i in range(3))
                column.putpixel((0, y), (*colour, round(255 * (a0 + (a1 - a0) * f))))
                break
    return column.resize((big, big), Image.Resampling.NEAREST)


def parts(shape: Shape, k: float, big: int) -> list[tuple[Image.Image, Paint, float, float]]:
    """Each painted part of `shape` as (its coverage mask, its paint, and the top and bottom of
    the box a gradient spans), scaled by `k` pixels per grid unit."""

    def mask() -> tuple[Image.Image, ImageDraw.ImageDraw]:
        m = Image.new("L", (big, big), 0)
        return m, ImageDraw.Draw(m)

    out = []
    if isinstance(shape, RoundRect):
        box = [shape.x0 * k, shape.y0 * k, shape.x1 * k - 1, shape.y1 * k - 1]
        if shape.fill:
            m, d = mask()
            d.rounded_rectangle(box, radius=shape.radius * k, fill=255)
            out.append((m, shape.fill, box[1], box[3]))
        if shape.stroke:
            m, d = mask()
            d.rounded_rectangle(box, radius=shape.radius * k, outline=255, width=round(shape.width * k))
            out.append((m, shape.stroke, box[1], box[3]))
    elif isinstance(shape, Circle):
        box = [(shape.cx - shape.r) * k, (shape.cy - shape.r) * k, (shape.cx + shape.r) * k, (shape.cy + shape.r) * k]
        if shape.fill:
            m, d = mask()
            d.ellipse([box[0], box[1], box[2] - 1, box[3] - 1], fill=255)
            out.append((m, shape.fill, box[1], box[3]))
        if shape.stroke:
            m, d = mask()
            h = shape.width * k / 2
            d.ellipse([box[0] - h, box[1] - h, box[2] + h - 1, box[3] + h - 1], fill=255)
            d.ellipse([box[0] + h, box[1] + h, box[2] - h - 1, box[3] - h - 1], fill=0)
            out.append((m, shape.stroke, box[1], box[3]))
    elif isinstance(shape, Arc):
        outer = shape.r + shape.width / 2
        box = [(shape.cx - outer) * k, (shape.cy - outer) * k, (shape.cx + outer) * k - 1, (shape.cy + outer) * k - 1]
        m, d = mask()
        d.arc(box, shape.start, shape.end, fill=255, width=round(shape.width * k))
        out.append((m, shape.stroke, box[1], box[3]))
    elif isinstance(shape, Polygon):
        m, d = mask()
        d.polygon([(x * k, y * k) for x, y in shape.points], fill=255)
        ys = [y * k for _, y in shape.points]
        out.append((m, shape.fill, min(ys), max(ys)))
    else:
        raise TypeError(shape)
    return out


def draw_steps(steps: list[Step], big: int, k: float) -> Image.Image:
    canvas = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    for step in steps:
        if isinstance(step, Group):
            canvas = Image.alpha_composite(canvas, draw_steps(step.steps, big, k))
            continue
        if isinstance(step, Erase):
            cut = Image.new("L", (big, big), 0)
            for shape in step.shapes:
                for m, *_ in parts(shape, k, big):
                    cut = Image.composite(Image.new("L", (big, big), 255), cut, m)
            alpha = Image.composite(Image.new("L", (big, big), 0), canvas.getchannel("A"), cut)
            canvas.putalpha(alpha)
            continue
        opacity = step.opacity if isinstance(step, Circle) else 1.0
        for m, paint, top, bottom in parts(step, k, big):
            layer = fill_image(paint, top, bottom, big)
            a = Image.composite(layer.getchannel("A"), Image.new("L", (big, big), 0), m)
            if opacity != 1.0:
                a = a.point(lambda v: round(v * opacity))
            layer.putalpha(a)
            canvas = Image.alpha_composite(canvas, layer)
    return canvas


def render(icon: Icon, size: int) -> Image.Image:
    big = size * supersample(size)
    return draw_steps(icon.steps, big, big / GRID).resize((size, size), Image.Resampling.LANCZOS)


def save_ico(path: Path, draw, sizes: list[int]) -> dict[int, Image.Image]:
    images = {s: draw(s) for s in sizes}
    largest = images[sizes[-1]]
    largest.save(path, format="ICO", sizes=[(s, s) for s in sizes], append_images=[images[s] for s in sizes[:-1]])
    return images


# --- Contrast ----------------------------------------------------------------------------------


def contrast(a: str, b: str) -> float:
    """WCAG 2's contrast ratio between two opaque colours."""

    def lum(c: str) -> float:
        v = [x / 255 for x in rgb(c)]
        v = [x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4 for x in v]
        return 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]

    la, lb = lum(a), lum(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


LIGHT_TASKBAR = "#F3F3F3"
DARK_TASKBAR = "#202020"


# --- The preview sheet -------------------------------------------------------------------------


def font(size: int) -> ImageFont.ImageFont:
    for name in ["segoeui.ttf", "arial.ttf", "DejaVuSans.ttf"]:
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return ImageFont.load_default()


PREVIEW_SIZES = [16, 24, 32, 48, 256, 512]


def preview(app: dict[int, Image.Image], tray: dict[tuple[str, str], dict[int, Image.Image]]) -> Image.Image:
    rows = [("Replica", app)]
    row_h = 560
    tray_y = 90 + len(rows) * 2 * row_h // 2 + 40
    sheet = Image.new("RGB", (1500, tray_y + 340), "#FAFAFA")
    d = ImageDraw.Draw(sheet)
    title, label, small = font(26), font(16), font(13)

    d.text((30, 20), "Replica's icons", fill="#111", font=title)
    d.text((30, 58), "Actual pixels at 16, 24, 32, 48, 256 and 512 px", fill="#444", font=label)

    y = 90
    for name, images in rows:
        for bg, fg in [(LIGHT_TASKBAR, "#333"), (DARK_TASKBAR, "#DDD")]:
            d.rectangle([30, y, 1470, y + 270], fill=bg)
            d.text((40, y + 8), f"{name}, on {'light' if bg == LIGHT_TASKBAR else 'dark'}", fill=fg, font=small)
            x = 50
            for s in PREVIEW_SIZES:
                shown = images[s] if s < 512 else images[s].resize((256, 256), Image.Resampling.LANCZOS)
                h = shown.size[0]
                sheet.paste(shown, (x, y + 30 + (220 - min(h, 220)) // 2 if h <= 220 else y + 8), shown)
                d.text((x, y + 250), f"{s} px" + (" (at half size)" if s == 512 else ""), fill=fg, font=small)
                x += h + 40
            y += 280

    d.text(
        (30, tray_y - 30),
        "Tray: 16, 24 and 32 px (100, 150 and 200% scaling), then 16 px at 400%",
        fill="#444",
        font=label,
    )
    for row, taskbar in enumerate(TASKBARS):
        bg, fg = (LIGHT_TASKBAR, "#333") if taskbar == "on-light" else (DARK_TASKBAR, "#DDD")
        y = tray_y + row * 140
        d.rectangle([30, y, 1470, y + 128], fill=bg)
        d.text((40, y + 6), f"{taskbar.replace('on-', '')} taskbar", fill=fg, font=small)
        x = 40
        for state in TRAY_STATES:
            images = tray[(state, taskbar)]
            cx = x
            for s in [16, 24, 32]:
                sheet.paste(images[s], (cx, y + 44 + (32 - s) // 2), images[s])
                cx += s + 10
            zoomed = images[16].resize((64, 64), Image.Resampling.NEAREST)
            sheet.paste(zoomed, (cx + 4, y + 28), zoomed)
            d.text((x, y + 104), state, fill=fg, font=small)
            x += 280
    return sheet


# --- Main --------------------------------------------------------------------------------------


def main() -> int:
    argparse.ArgumentParser(description=__doc__.splitlines()[0]).parse_args()

    icon_dir = ASSETS / "icon"
    tray_dir = ASSETS / "tray"
    icon_dir.mkdir(exist_ok=True)
    tray_dir.mkdir(exist_ok=True)

    (icon_dir / "replica.svg").write_text(to_svg(app_icon_large()), encoding="utf-8", newline="\n")
    (icon_dir / "replica-small.svg").write_text(to_svg(app_icon_small(32)), encoding="utf-8", newline="\n")
    (icon_dir / "replica-tiny.svg").write_text(to_svg(app_icon_small(16)), encoding="utf-8", newline="\n")
    app = save_ico(icon_dir / "replica.ico", lambda s: render(app_icon(s), s), APP_ICO_SIZES)
    for s in APP_PNG_SIZES:
        image = app.get(s) or render(app_icon(s), s)
        app[s] = image
        image.save(icon_dir / f"replica-{s}.png", optimize=True)
    for s in PREVIEW_SIZES:
        app.setdefault(s, render(app_icon(s), s))

    tray = {}
    for state in TRAY_STATES:
        for taskbar in TASKBARS:
            stem = f"{state}-{taskbar}"
            # The SVG is the 24 and 32 px drawing; 16 and 20 px have one page (the .ico has both).
            (tray_dir / f"{stem}.svg").write_text(to_svg(tray_icon(state, 32)), encoding="utf-8", newline="\n")
            tray[(state, taskbar)] = save_ico(tray_dir / f"{stem}.ico", lambda s: render(tray_icon(state, s), s), TRAY_ICO_SIZES)

    preview(app, tray).save(ASSETS / "preview.png", optimize=True)

    print(f"Wrote the icons to {icon_dir}, {tray_dir} and {ASSETS / 'preview.png'}")
    print("Contrast (WCAG 2):")
    print(f"  the small icon's and tray's disc {TEAL_500} on a light taskbar: {contrast(TEAL_500, LIGHT_TASKBAR):.2f}:1")
    print(f"  on a dark taskbar: {contrast(TEAL_500, DARK_TASKBAR):.2f}:1")
    print(f"  the glyph {FACE_DEEP} on the disc: {contrast(FACE_DEEP, TEAL_500):.2f}:1")
    print(f"  the large icon's front page {TEAL_200} on the face {FACE}: {contrast(TEAL_200, FACE):.2f}:1")
    return 0


if __name__ == "__main__":
    sys.exit(main())
