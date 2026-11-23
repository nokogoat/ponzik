#!/usr/bin/env bash
# Builds dist/Ponzik-x86_64.AppImage: the release program, an AppDir around it,
# then appimagetool (downloaded once into dist/tools if it is not installed).
#
#   scripts/build-appimage.sh            # cargo build --release, then package
#   SKIP_BUILD=1 scripts/build-appimage.sh   # package target/release/ponzik as is
#
# The program talks to X11 over its socket and loads nothing at run time but
# libasound (ALSA) and the C library: the script checks that `ldd` lists only
# libraries every desktop Linux has, so the AppImage bundles no library at all.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-$ROOT/dist}"
APPDIR="$OUT/AppDir"
BIN="${PONZIK_BIN:-$ROOT/target/release/ponzik}"
NAME="Ponzik-x86_64.AppImage"

cd "$ROOT"
mkdir -p "$OUT"
if [ -z "${SKIP_BUILD:-}" ]; then
    cargo build --release --locked -p ponzik
fi
[ -x "$BIN" ] || { echo "error: $BIN not found" >&2; exit 1; }

# Only system libraries present on every desktop distribution.
echo "== runtime dependencies"
ldd "$BIN"
allowed='linux-vdso|ld-linux-x86-64|libc\.so|libm\.so|libgcc_s\.so|libasound\.so|libpthread\.so|libdl\.so|librt\.so'
unexpected="$(ldd "$BIN" | grep -vE "$allowed" | grep -v '^\s*statically linked' || true)"
if [ -n "$unexpected" ]; then
    echo "error: unexpected runtime dependencies:" >&2
    echo "$unexpected" >&2
    exit 1
fi
glibc="$(objdump -T "$BIN" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 || true)"
echo "== newest glibc symbol: ${glibc:-unknown}"

echo "== AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps" "$APPDIR/usr/share/icons/hicolor/scalable/apps"
install -m 755 "$BIN" "$APPDIR/usr/bin/ponzik"
if command -v strip >/dev/null; then
    strip --strip-unneeded "$APPDIR/usr/bin/ponzik"
fi
install -m 755 packaging/linux/AppRun "$APPDIR/AppRun"
install -m 644 packaging/linux/ponzik.desktop "$APPDIR/ponzik.desktop"
install -m 644 packaging/linux/ponzik.desktop "$APPDIR/usr/share/applications/ponzik.desktop"
install -m 644 assets/icons/ponzik-256.png "$APPDIR/ponzik.png"
install -m 644 assets/icons/ponzik-256.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/ponzik.png"
install -m 644 assets/icons/ponzik.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/ponzik.svg"
ln -sf ponzik.png "$APPDIR/.DirIcon"

echo "== appimagetool"
TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [ -z "$TOOL" ]; then
    TOOL="$OUT/tools/appimagetool-x86_64.AppImage"
    if [ ! -x "$TOOL" ]; then
        mkdir -p "$OUT/tools"
        curl -fsSL -o "$TOOL" \
            https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
        chmod +x "$TOOL"
    fi
fi
# No FUSE needed to run the tool itself (CI containers have none).
export APPIMAGE_EXTRACT_AND_RUN=1
ARCH=x86_64 "$TOOL" --no-appstream "$APPDIR" "$OUT/$NAME"
chmod +x "$OUT/$NAME"
ls -l "$OUT/$NAME"
