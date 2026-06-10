#!/usr/bin/env python3
"""Fallback sin dependencias: genera los 10 PNG de iconos del tray.

Solo usa la stdlib de Python 3 (zlib, struct). No requiere PIL, rsvg-convert,
ImageMagick ni Inkscape. Dibuja iconos flat simples con un mini rasterizador
(supersampling 4x para antialiasing) y los codifica como PNG RGBA a mano.

Se usa cuando NO hay ningun rasterizador SVG instalado en el sistema. Para
regenerar desde los SVG fuente con calidad vectorial, ver scripts/build-icons.sh
(requiere rsvg-convert).

Uso:  python3 scripts/gen-icons.py
Salida: assets/<nombre>-22.png y assets/<nombre>-24.png  (10 ficheros)
"""

import math
import os
import struct
import zlib

SS = 4  # factor de supersampling para antialiasing

# Paleta (coherente con assets/svg/*.svg)
GREY = (122, 122, 122, 255)
LIGHT = (232, 232, 232, 255)
GREEN = (67, 181, 129, 255)
RED = (240, 71, 71, 255)
DARK = (26, 26, 26, 255)  # contorno de la barra diagonal


class Canvas:
    """Lienzo RGBA en coordenadas de viewBox 24x24, renderizado a SS*size px."""

    def __init__(self, size):
        self.size = size
        self.w = size * SS
        self.h = size * SS
        self.scale = self.w / 24.0  # viewBox 24 -> pixeles
        self.buf = bytearray(self.w * self.h * 4)  # transparente

    def _px(self, x, y, color):
        if x < 0 or y < 0 or x >= self.w or y >= self.h:
            return
        i = (y * self.w + x) * 4
        sr, sg, sb, sa = color
        if sa == 0:
            return
        a = sa / 255.0
        dr, dg, db, da = self.buf[i], self.buf[i + 1], self.buf[i + 2], self.buf[i + 3]
        # composicion alpha "over"
        out_a = sa + int(da * (1 - a))
        if out_a == 0:
            return
        self.buf[i] = int((sr * a + dr * (da / 255.0) * (1 - a)) * 255 / out_a) if False else int(sr * a + dr * (1 - a))
        self.buf[i + 1] = int(sg * a + dg * (1 - a))
        self.buf[i + 2] = int(sb * a + db * (1 - a))
        self.buf[i + 3] = max(da, sa)

    def _fill_test(self, fn, color):
        """Pinta cada pixel cuyo centro (en coords viewBox) cumple fn(x,y)."""
        for py in range(self.h):
            vy = (py + 0.5) / self.scale
            for px in range(self.w):
                vx = (px + 0.5) / self.scale
                if fn(vx, vy):
                    self._px(px, py, color)

    # ---- primitivas en coordenadas de viewBox (0..24) ----

    def rounded_rect(self, x0, y0, x1, y1, r, color):
        def inside(x, y):
            if not (x0 <= x <= x1 and y0 <= y <= y1):
                return False
            cx = min(max(x, x0 + r), x1 - r)
            cy = min(max(y, y0 + r), y1 - r)
            return (x - cx) ** 2 + (y - cy) ** 2 <= r * r
        self._fill_test(inside, color)

    def disc(self, cx, cy, r, color):
        self._fill_test(lambda x, y: (x - cx) ** 2 + (y - cy) ** 2 <= r * r, color)

    def thick_segment(self, x0, y0, x1, y1, half, color):
        """Segmento de grosor 2*half con extremos redondeados."""
        dx, dy = x1 - x0, y1 - y0
        L2 = dx * dx + dy * dy

        def inside(x, y):
            if L2 == 0:
                t = 0.0
            else:
                t = ((x - x0) * dx + (y - y0) * dy) / L2
                t = min(1.0, max(0.0, t))
            px, py = x0 + t * dx, y0 + t * dy
            return (x - px) ** 2 + (y - py) ** 2 <= half * half
        self._fill_test(inside, color)

    def arc_band(self, cx, cy, r, half, a0, a1, color):
        """Banda anular (anillo grueso) entre angulos a0..a1 (radianes)."""
        rin, rout = r - half, r + half

        def inside(x, y):
            d = math.hypot(x - cx, y - cy)
            if not (rin <= d <= rout):
                return False
            ang = math.atan2(y - cy, x - cx)
            if ang < 0:
                ang += 2 * math.pi
            aa0 = a0 % (2 * math.pi)
            aa1 = a1 % (2 * math.pi)
            if aa0 <= aa1:
                return aa0 <= ang <= aa1
            return ang >= aa0 or ang <= aa1
        self._fill_test(inside, color)

    def downsample(self):
        """Reduce de SS*size a size con promedio de cajas (antialiasing)."""
        s = self.size
        out = bytearray(s * s * 4)
        n = SS * SS
        for oy in range(s):
            for ox in range(s):
                ar = ag = ab = aa = 0
                for j in range(SS):
                    for i in range(SS):
                        sx = ox * SS + i
                        sy = oy * SS + j
                        k = (sy * self.w + sx) * 4
                        a = self.buf[k + 3]
                        ar += self.buf[k] * a
                        ag += self.buf[k + 1] * a
                        ab += self.buf[k + 2] * a
                        aa += a
                o = (oy * s + ox) * 4
                if aa == 0:
                    out[o] = out[o + 1] = out[o + 2] = out[o + 3] = 0
                else:
                    out[o] = ar // aa
                    out[o + 1] = ag // aa
                    out[o + 2] = ab // aa
                    out[o + 3] = aa // n
        return bytes(out)


def write_png(path, size, rgba):
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filtro None
        raw.extend(rgba[y * size * 4:(y + 1) * size * 4])
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # RGBA 8-bit
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", ihdr)
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def draw_headset(c, color):
    # diadema (arco superior) + dos auriculares
    c.arc_band(12, 12, 7, 1.0, math.radians(200), math.radians(340), color)
    c.rounded_rect(3, 13, 7, 19, 1.5, color)
    c.rounded_rect(17, 13, 21, 19, 1.5, color)


def draw_mic(c, color):
    # capsula + soporte (arco) + pie
    c.rounded_rect(9, 3, 15, 14, 3, color)
    c.arc_band(12, 11, 6, 1.0, math.radians(20), math.radians(160), color)
    c.thick_segment(12, 16, 12, 21, 1.0, color)
    c.thick_segment(8, 21, 16, 21, 1.0, color)


def draw_slash(c, color):
    # barra diagonal con halo oscuro para destacar sobre el icono
    c.thick_segment(4, 4, 20, 20, 2.0, DARK)
    c.thick_segment(4, 4, 20, 20, 1.0, color)


ICONS = {
    "discord-closed": lambda c: draw_headset(c, GREY),
    "idle": lambda c: draw_headset(c, LIGHT),
    "voice-on": lambda c: draw_mic(c, GREEN),
    "voice-muted": lambda c: (draw_mic(c, RED), draw_slash(c, RED)),
    "voice-deafened": lambda c: (draw_headset(c, RED), draw_slash(c, RED)),
}


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out_dir = os.path.join(os.path.dirname(here), "assets")
    os.makedirs(out_dir, exist_ok=True)
    for name, fn in ICONS.items():
        for size in (22, 24):
            c = Canvas(size)
            fn(c)
            rgba = c.downsample()
            path = os.path.join(out_dir, f"{name}-{size}.png")
            write_png(path, size, rgba)
            print(f"generado {path}")
    print("Listos los 10 PNG.")


if __name__ == "__main__":
    main()
