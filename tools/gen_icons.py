#!/usr/bin/env python3
"""Needs: pip install shapely svgpathtools numpy   (only to regenerate; the SVGs are committed)

Generates the app's icon family into data/icons/hicolor/scalable.

UI glyphs: 16px grid, 1.5px round strokes, drawn as symbolic icons (GTK
recolours them). File icons: soft two-tone pages and folders in one
restrained palette so they look the same under any system icon theme.
"""
import os

ROOT = os.path.join(os.path.dirname(__file__), "..", "data", "icons", "hicolor", "scalable")
INK = "#2e3436"

def write(sub, name, body):
    d = os.path.join(ROOT, sub)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, name + ".svg"), "w") as f:
        f.write(body)

def _outline(inner):
    """Turn stroked SVG primitives into one filled outline.

    GTK recolours symbolic icons by forcing `fill` on every path/circle/rect and
    ignores strokes, so strokes must be converted to outlines up front.
    """
    import xml.etree.ElementTree as ET
    from shapely.geometry import LineString, Point, box
    from shapely.ops import unary_union
    from svgpathtools import parse_path
    import numpy as np

    half = 0.75
    parts = []
    for el in ET.fromstring(f"<g>{inner}</g>"):
        tag = el.tag.split('}')[-1]
        a = el.attrib
        if tag == "circle":
            c = Point(float(a["cx"]), float(a["cy"])).buffer(float(a["r"]), quad_segs=16)
            parts.append(c if a.get("fill", "none") != "none" else c.exterior.buffer(half, quad_segs=8))
        elif tag == "rect":
            x, y, w, h = (float(a[k]) for k in ("x", "y", "width", "height"))
            r = float(a.get("rx", 0))
            shape = box(x + r, y + r, x + w - r, y + h - r).buffer(r, quad_segs=16) if r else box(x, y, x + w, y + h)
            parts.append(shape.exterior.buffer(half, quad_segs=8, join_style="round"))
        elif tag == "path":
            for sub in parse_path(a["d"]).continuous_subpaths():
                pts = []
                for seg in sub:
                    n = 2 if seg.__class__.__name__ == "Line" else 28
                    for t in np.linspace(0, 1, n):
                        z = seg.point(t)
                        pts.append((z.real, z.imag))
                if len(pts) >= 2:
                    parts.append(LineString(pts).buffer(half, quad_segs=8, cap_style="round", join_style="round"))
    geom = unary_union(parts)
    polys = list(geom.geoms) if geom.geom_type == "MultiPolygon" else [geom]
    d = []
    for poly in polys:
        for ring in [poly.exterior, *poly.interiors]:
            pts = list(ring.coords)
            d.append("M" + "L".join(f"{x:.2f} {y:.2f}" for x, y in pts[:-1]) + "Z")
    return "".join(d)


def sym(name, inner, sub="actions"):
    d = _outline(inner)
    write(sub, f"fm-{name}-symbolic",
        f'<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">'
        f'<path fill="{INK}" fill-rule="evenodd" d="{d}"/></svg>\n')

def dot(x, y, r=1.1):
    return f'<circle cx="{x}" cy="{y}" r="{r}" fill="{INK}" stroke="none"/>'

# ---- toolbar / actions ----
sym("sidebar", '<rect x="2" y="2.75" width="12" height="10.5" rx="2.5"/><path d="M6 3.2v9.6"/>')
sym("back", '<path d="M9.8 3.5 5.3 8l4.5 4.5"/>')
sym("forward", '<path d="M6.2 3.5 10.7 8l-4.5 4.5"/>')
sym("up", '<path d="M3.5 9.8 8 5.3l4.5 4.5"/>')
sym("view-grid", ''.join(f'<rect x="{x}" y="{y}" width="4.5" height="4.5" rx="1.3"/>' for x in (2.5, 9) for y in (2.5, 9)))
sym("view-list", '<path d="M6 4h7.5M6 8h7.5M6 12h7.5"/>' + dot(2.7, 4) + dot(2.7, 8) + dot(2.7, 12))
sym("view-columns", '<rect x="2" y="2.75" width="12" height="10.5" rx="2.2"/><path d="M6.1 3v10M9.9 3v10"/>')
sym("view-gallery", '<rect x="2" y="2.5" width="12" height="7" rx="1.8"/><path d="M3.2 12.6h2M7 12.6h2M10.8 12.6h2"/>')
sym("options", '<path d="M2.5 5h5.2M12.3 5h1.2M2.5 11h1.2M8.3 11h5.2"/><circle cx="10" cy="5" r="1.7"/><circle cx="6" cy="11" r="1.7"/>')
sym("eye", '<path d="M1.8 8s2.2-4.4 6.2-4.4S14.2 8 14.2 8s-2.2 4.4-6.2 4.4S1.8 8 1.8 8z"/><circle cx="8" cy="8" r="1.9"/>')
sym("history", '<circle cx="8" cy="8" r="5.6"/><path d="M8 5v3.2l2.1 1.3"/>')
sym("search", '<circle cx="7" cy="7" r="4.3"/><path d="m10.3 10.3 3.2 3.2"/>')
sym("more", dot(3.4, 8, 1.25) + dot(8, 8, 1.25) + dot(12.6, 8, 1.25))
sym("activity", '<path d="M13.2 7.6A5.2 5.2 0 0 0 4 4.9M2.8 8.4A5.2 5.2 0 0 0 12 11.1"/><path d="M11.4 2.6v2.7H8.7M4.6 13.4v-2.7h2.7"/>')
sym("plus", '<path d="M8 3.2v9.6M3.2 8h9.6"/>')
sym("close", '<path d="m4 4 8 8M12 4l-8 8"/>')
sym("eject", '<path d="M8 3 13 9.6H3z"/><path d="M3.2 12.7h9.6"/>')
sym("check", '<path d="m3.5 8.4 3 3 6-6.4"/>')
sym("sort", '<path d="M4.5 13V3M2.2 5.3 4.5 3l2.3 2.3M11.5 3v10M9.2 10.7l2.3 2.3 2.3-2.3"/>')
sym("sliders", '<path d="M2.5 5h5.2M12.3 5h1.2M2.5 11h1.2M8.3 11h5.2"/><circle cx="10" cy="5" r="1.7"/><circle cx="6" cy="11" r="1.7"/>')

sym("pause", '<path d="M5.6 3.5v9M10.4 3.5v9"/>')
sym("sort-asc", '<path d="M8 13V3.2M4.6 6.6 8 3.2l3.4 3.4"/>')
sym("sort-desc", '<path d="M8 3v9.8M4.6 9.4 8 12.8l3.4-3.4"/>')
sym("undo", '<path d="M5.2 3.6 2.6 6.2l2.6 2.6"/><path d="M3 6.2h5.6a3.6 3.6 0 0 1 0 7.2H6"/>')
sym("edit", '<path d="m3 13 .7-3 7-7a1.4 1.4 0 0 1 2 0l.3.3a1.4 1.4 0 0 1 0 2l-7 7z"/><path d="m9.7 4.3 2 2"/>')
sym("trash", '<path d="M3 4.5h10M6.2 4.5V3h3.6v1.5M4.4 4.5l.6 8.2a1 1 0 0 0 1 .9h4a1 1 0 0 0 1-.9l.6-8.2"/>')
sym("bolt", '<path d="M9 2 4.2 9H8l-1 5 4.8-7H8z"/>')
sym("lock", '<rect x="3.4" y="7" width="9.2" height="6.5" rx="1.8"/><path d="M5.4 7V5.2a2.6 2.6 0 0 1 5.2 0V7"/>')
sym("folder", '<path d="M2.5 4.8a1.3 1.3 0 0 1 1.3-1.3h2.6l1.4 1.6h4.4a1.3 1.3 0 0 1 1.3 1.3v5.2a1.3 1.3 0 0 1-1.3 1.3H3.8a1.3 1.3 0 0 1-1.3-1.3z"/>', "places")

# ---- places ----
P = "places"
sym("home", '<path d="M2.4 7.6 8 3l5.6 4.6"/><path d="M3.8 6.8V13h8.4V6.8"/><path d="M6.6 13V9.8h2.8V13"/>', P)
sym("desktop", '<rect x="2" y="2.8" width="12" height="8.2" rx="1.9"/><path d="M5.8 13.4h4.4M8 11v2.4"/>', P)
sym("documents", '<path d="M4.6 2.5h4.7l2.7 2.7v7.3a1 1 0 0 1-1 1H4.6a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1z"/><path d="M9.1 2.7v2.8h2.8M5.9 8.6h4.2M5.9 11h4.2"/>', P)
sym("downloads", '<path d="M8 2.5v7.6M4.8 7.1 8 10.3l3.2-3.2M3 13h10"/>', P)
sym("pictures", '<rect x="2" y="3" width="12" height="10" rx="2"/><circle cx="5.7" cy="6.5" r="1.1"/><path d="m2.6 12 3.9-3.6 2.6 2.3 1.9-1.7 2.4 2.5"/>', P)
sym("videos", '<rect x="2" y="3.4" width="12" height="9.2" rx="2"/><path d="M7 6.1v3.8L10.3 8z"/>', P)
sym("music", '<path d="M6.5 11.6V4.2l6-1.6v7.4"/><circle cx="5" cy="11.6" r="1.6"/><circle cx="11" cy="10" r="1.6"/>', P)
sym("bookmark", '<path d="M4.6 2.8h6.8a1 1 0 0 1 1 1v9.7L8 10.7l-4.4 2.8V3.8a1 1 0 0 1 1-1z"/>', P)
sym("drive", '<rect x="2" y="8" width="12" height="5" rx="1.8"/><path d="m3.5 8 1.5-4.5h6L12.5 8"/>' + dot(11, 10.5, 0.7), P)
sym("removable", '<rect x="4" y="5.2" width="8" height="8" rx="1.8"/><path d="M6.4 5.2V2.6M9.6 5.2V2.6"/>', P)
sym("network", '<rect x="2.5" y="2.8" width="11" height="4" rx="1.5"/><rect x="2.5" y="9.2" width="11" height="4" rx="1.5"/>' + dot(5, 4.8, 0.75) + dot(5, 11.2, 0.75), P)
sym("cloud", '<path d="M4.9 12.5a3 3 0 0 1-.5-5.95A4 4 0 0 1 12 6.8a2.85 2.85 0 0 1-.7 5.7z"/>', P)
sym("tag", '<path d="M2.5 8.4V3.6a1 1 0 0 1 1-1h4.8l5.2 5.2a1 1 0 0 1 0 1.4l-4.4 4.4a1 1 0 0 1-1.4 0z"/><circle cx="5.6" cy="5.6" r="1"/>', P)
sym("server", '<rect x="2.5" y="2.8" width="11" height="4" rx="1.5"/><rect x="2.5" y="9.2" width="11" height="4" rx="1.5"/>', P)

# ---- file & folder icons (64px, full colour) ----
PAGE = ("<defs>"
        '<linearGradient id="p" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#e9edf4"/></linearGradient>'
        "</defs>"
        '<path d="M15 8a4 4 0 0 1 4-4h19l13 13v39a4 4 0 0 1-4 4H19a4 4 0 0 1-4-4z" fill="url(#p)" stroke="#1b2430" stroke-opacity=".14" stroke-width="1.2"/>'
        '<path d="M38 4v9a4 4 0 0 0 4 4h9z" fill="#d3d9e4" stroke="#1b2430" stroke-opacity=".10" stroke-width="1" stroke-linejoin="round"/>')

def file_icon(name, glyph):
    write("mimetypes", f"fm-{name}", f'<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">{PAGE}{glyph}</svg>\n')

def stroke(c, inner, w=2.6):
    return f'<g fill="none" stroke="{c}" stroke-width="{w}" stroke-linecap="round" stroke-linejoin="round">{inner}</g>'

file_icon("file", stroke("#b3bccb", '<path d="M24 36h16M24 43h16M24 50h10"/>', 2.4))
file_icon("file-document", stroke("#5f8fe8", '<path d="M24 32h16M24 39h16M24 46h10"/>'))
file_icon("file-image", '<circle cx="39" cy="35" r="3.2" fill="#f2735f"/><path d="M21 51l8.5-10 5 5.5 4-4L45 51z" fill="#f2735f" opacity=".92"/>')
file_icon("file-audio", stroke("#8b6cf0", '<path d="M29 48V35l12-3v13"/><circle cx="26.5" cy="48" r="3" fill="#8b6cf0"/><circle cx="38.5" cy="45" r="3" fill="#8b6cf0"/>'))
file_icon("file-video", '<rect x="21" y="32" width="22" height="17" rx="4" fill="#f2a33a"/><path d="M30.2 36.4v8.2l6.6-4.1z" fill="#fff"/>')
file_icon("file-archive", stroke("#2bb3a0", '<path d="M32 22v2M32 27v2M32 32v2"/><rect x="26.5" y="37" width="11" height="12" rx="3" fill="#2bb3a0" fill-opacity=".18"/><path d="M32 41v3"/>'))
file_icon("file-code", stroke("#5b6b84", '<path d="m27 36-5 6 5 6M37 36l5 6-5 6M33.5 33l-3 18"/>'))

write("places", "fm-folder", '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><defs>'
      '<linearGradient id="b" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#5b8bea"/><stop offset="1" stop-color="#4a73d2"/></linearGradient>'
      '<linearGradient id="f" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#a4c4ff"/><stop offset="1" stop-color="#7aa5f6"/></linearGradient></defs>'
      '<path d="M5 16a5 5 0 0 1 5-5h14.2a4 4 0 0 1 3 1.4l3.3 3.8H54a5 5 0 0 1 5 5V47a5 5 0 0 1-5 5H10a5 5 0 0 1-5-5z" fill="url(#b)"/>'
      '<path d="M5 26a5 5 0 0 1 5-5h44a5 5 0 0 1 5 5v21a5 5 0 0 1-5 5H10a5 5 0 0 1-5-5z" fill="url(#f)"/>'
      '<path d="M10.5 21.6h43" stroke="#fff" stroke-opacity=".55" stroke-width="1.4" stroke-linecap="round"/></svg>\n')
print("icons written to", os.path.abspath(ROOT))
