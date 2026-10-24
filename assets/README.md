# Replica's icons

Original art for Replica, under Replica's MIT licence. No third-party icon sets, fonts or
images are used in any of the icons.

**Every file here except this README is generated** by `export.py`. To change an icon, edit the
shapes in `export.py` and run it again; don't edit the SVG, ICO or PNG files by hand.

```
python assets/export.py
```

It needs Python 3.10 or later and [Pillow](https://python-pillow.org/) (`pip install pillow`;
made with Pillow 12.3.0). Both are build-only tools and don't ship in Replica. With the same
Pillow version, it writes the same bytes every time.

## The files

| File | What |
|---|---|
| `icon/replica.svg` | The app icon's master, used for 40 px and up |
| `icon/replica-small.svg` | The small variant, used at 24 and 32 px: a flat disc, both pages in bold lines |
| `icon/replica-tiny.svg` | The smallest, used at 16 and 20 px: a flat disc and one page |
| `icon/replica.ico` | The exe's, the installer's and the window's icon: 16, 20, 24, 32, 40, 48, 64 and 256 px |
| `icon/replica-64.png` … `replica-1024.png` | 64, 128, 256, 512 and 1024 px, for the README, the docs and surmise.it/tools |
| `tray/<state>-on-light.svg`, `.ico` | The tray icon for a **light** taskbar: 16, 20, 24 and 32 px (the SVG is the 24 and 32 px drawing) |
| `tray/<state>-on-dark.svg`, `.ico` | The tray icon for a **dark** taskbar (the same drawing: see below) |
| `preview.png` | Every icon at its real sizes on light and dark, for review |

The tray states: `idle`; `running-1` and `running-2`, two frames to alternate while a
job runs (the arrow turns half a turn), with progress in the tooltip; `failed`, with a red dot;
and `overdue`, with an amber dot.

## The icon

**Two pages, back in time.** Two offset pages, the front one with a counter-clockwise "go back
in time" arrow: a copy of your files that you can go back to. Pages rather than folders, and no
clock face, so it doesn't look like Windows File History, Windows Backup or OneDrive.

It's built the way Dialect's icon is, so the tools sit together as a family:
one bold circle with a vertical gradient, a soft gloss ring, a dark face, and the glyph in the
brand's light tones. Dialect's master is on a 512-unit grid; Replica's shapes are on a 16-unit
grid, the same measurements divided by 32 (the circle 7.375, the gloss ring 6.6875, the face
5.625 with a 0.5 ring).

- **The large icon** (40 px and up): the back page as an outline in teal 300, the front page
  solid in teal 200, a gap cut out of the back page round it so the face shows between them,
  and the arrow in the deep face colour.
- **The small icon** (16 to 32 px), as Dialect's `icon-small.svg` is: no gradient, gloss or
  face. A bright flat teal 500 disc, with the glyph in the deep face colour and the arrow cut
  through it in the disc's teal, with a bigger arrowhead. At 24 and 32 px both pages, in lines
  about 2 px wide at 24; at 16 and 20 px there's room for **one page only**, with the arrow.
- **The tray icon**, as Dialect's `tray.svg` is: the small icon filling its square (one page at
  16 and 20 px, both at 24 and 32), with heavier lines; the running frames turn only the
  arrow. A status dot sits at the bottom right in a cut-out ring, so it stands apart from the
  disc. The disc reads on either taskbar, so `on-light` and `on-dark` are drawn the same; both
  names stay for the tray code, which picks one by the taskbar's theme.
- Each pixel size is drawn on its own, supersampled and then reduced, rather than shrunk from
  the biggest image.

**Contrast** (WCAG 2, printed by `export.py`):

| Pair | Ratio |
|---|---|
| The small icon's and tray's disc on a light taskbar (`#F3F3F3`) | 3.09:1 |
| The disc on a dark taskbar (`#202020`) | 4.75:1 |
| The glyph (`--brand-face-deep`) on the disc | 5.15:1 |
| The large icon's front page (`--brand-teal-200`) on the face | 11.06:1 |

The disc is as bright a teal as keeps 3:1 on a light taskbar: a brighter one, such as `#22B8CF`,
is 2.1:1 there, and the icon would fade into a light taskbar.

## Palette

The icon's colours, fixed and the same in both themes. The UI's own accent is teal 600, the
light theme's `accent` in `crates/replica/src/ui/theme.rs`.

| Token | Hex | Used for |
|---|---|---|
| `--brand-teal-200` | `#A5E8F3` | The large icon's front page |
| `--brand-teal-300` | `#67D2E6` | Light: the gradient's top, the back page's outline |
| `--brand-teal-400` | `#22A9C6` | Base: the gradient's middle |
| `--brand-teal-500` | `#1497B5` | The small icon's and the tray's flat disc |
| `--brand-teal-600` | `#0E7490` | Dark: the gradient's bottom. Replica's teal |
| `--brand-face` | `#0F2A31` | The face, top |
| `--brand-face-deep` | `#081B20` | The face, bottom, and its ring; the arrow; the small icon's glyph |
| `--status-failed` | `#E0352B` | The tray's failed dot |
| `--status-overdue` | `#F5A300` | The tray's overdue dot |

The labels in `preview.png` use the Windows system font (Segoe UI) when it's there. The preview
is only for review and doesn't ship.
