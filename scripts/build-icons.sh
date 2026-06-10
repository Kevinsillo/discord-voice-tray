#!/usr/bin/env bash
#
# Rasteriza los 5 SVG fuente de assets/svg/ a PNG de 22px y 24px en assets/.
#
# Genera los 10 ficheros que el binario embebe con include_bytes!:
#   assets/<nombre>-22.png  y  assets/<nombre>-24.png
#
# Requiere rsvg-convert (paquete librsvg2-bin / librsvg-tools).
# Si rsvg-convert no esta disponible, ver scripts/gen-icons.py como fallback
# (genera PNGs simples sin dependencias de sistema, usando solo python3 stdlib).
#
# Uso:  ./scripts/build-icons.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
SVG_DIR="$ROOT/assets/svg"
OUT_DIR="$ROOT/assets"

if ! command -v rsvg-convert >/dev/null 2>&1; then
  echo "error: rsvg-convert no esta instalado." >&2
  echo "  Instalalo (Debian/Ubuntu: 'sudo apt install librsvg2-bin') o usa" >&2
  echo "  el fallback sin dependencias:  python3 scripts/gen-icons.py" >&2
  exit 1
fi

NAMES=(discord-closed idle voice-on voice-muted voice-deafened)
SIZES=(22 24)

for name in "${NAMES[@]}"; do
  src="$SVG_DIR/$name.svg"
  [ -f "$src" ] || { echo "error: falta $src" >&2; exit 1; }
  for size in "${SIZES[@]}"; do
    out="$OUT_DIR/$name-$size.png"
    rsvg-convert -w "$size" -h "$size" "$src" -o "$out"
    echo "generado $out"
  done
done

echo "Listos los 10 PNG en $OUT_DIR"
