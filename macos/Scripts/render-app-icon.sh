#!/usr/bin/env bash
#
# Renders Resources/AppIcon.svg into the multi-resolution AppIcon.icns that
# Scripts/build-app.sh copies into the bundle (Dock / Finder / app switcher).
#
# The SVG is the source of truth; `sips` rasterizes it (ImageIO reads SVG on
# current macOS) and `iconutil` assembles the ten required representations.
# The menu bar mark stays vector: Resources/MenuBarIcon.svg is loaded by
# StatusItemController as a template NSImage.
#
# Usage:  Scripts/render-app-icon.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"     # macos/
RESOURCES="${ROOT}/Resources"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

rasterize() { # <svg> <pixels> <out-png>
    local svg="$1" pixels="$2" out="$3"
    local master="${WORK}/master.png"
    rm -f "${master}"
    # ImageIO reads SVG on current macOS; Quick Look is the fallback for older
    # systems that cannot decode SVG through sips.
    if ! sips -s format png "${svg}" --out "${master}" >/dev/null 2>&1; then
        local dir
        dir="$(mktemp -d "${WORK}/raster.XXXXXX")"
        qlmanage -t -s "${pixels}" -o "${dir}" "${svg}" >/dev/null 2>&1 || true
        master="$(find "${dir}" -maxdepth 1 -type f -name '*.png' | head -n 1)"
    fi
    [[ -n "${master}" && -f "${master}" ]] || { echo "error: could not rasterize ${svg}" >&2; exit 1; }
    sips -z "${pixels}" "${pixels}" "${master}" --out "${out}" >/dev/null
}

# MARK: - App icon (.icns)

APP_SVG="${RESOURCES}/AppIcon.svg"
APP_ICNS="${RESOURCES}/AppIcon.icns"
[[ -f "${APP_SVG}" ]] || { echo "error: missing ${APP_SVG}" >&2; exit 1; }

rasterize "${APP_SVG}" 1024 "${WORK}/AppIcon-1024.png"
ICONSET="${WORK}/AppIcon.iconset"
mkdir -p "${ICONSET}"

# "pixels name" pairs: iconutil's required representations.
for spec in \
    "16 icon_16x16" \
    "32 icon_16x16@2x" \
    "32 icon_32x32" \
    "64 icon_32x32@2x" \
    "128 icon_128x128" \
    "256 icon_128x128@2x" \
    "256 icon_256x256" \
    "512 icon_256x256@2x" \
    "512 icon_512x512" \
    "1024 icon_512x512@2x"; do
    set -- ${spec}
    sips -z "$1" "$1" "${WORK}/AppIcon-1024.png" --out "${ICONSET}/$2.png" >/dev/null
done

iconutil -c icns "${ICONSET}" -o "${APP_ICNS}"
echo "==> ${APP_ICNS}"
