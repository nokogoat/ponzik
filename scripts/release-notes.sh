#!/usr/bin/env bash
# Release notes, in Markdown on standard output: the commits since the previous
# release of the same channel, and the build marker the in-app updater reads.
#
#   scripts/release-notes.sh nightly 0.1.0 412 abc123456
#   scripts/release-notes.sh stable  0.2.0 530 def456789
#
# nightly: since the commit the `nightly` tag points to (before it moves).
# stable:  since the previous `v*` tag.
set -euo pipefail

channel="$1" version="$2" build="$3" hash="$4"
date="$(git log -1 --format=%cs HEAD)"

if [ "$channel" = nightly ]; then
    previous="$(git rev-parse -q --verify 'refs/tags/nightly^{commit}' 2>/dev/null || true)"
else
    previous="$(git describe --tags --abbrev=0 --match 'v*' HEAD^ 2>/dev/null || true)"
fi
if [ -n "$previous" ] && [ "$(git rev-parse "$previous^{commit}")" = "$(git rev-parse HEAD)" ]; then
    previous=""
fi
if [ -n "$previous" ]; then
    range="$previous..HEAD"
    heading="Changes since the previous release"
else
    range="-30 HEAD"
    heading="Latest changes"
fi

if [ "$channel" = nightly ]; then
    echo "Nightly build of Ponzik $version, build $build ($hash), $date."
else
    echo "Ponzik $version, build $build ($hash), $date."
fi
cat <<'EOF'

**Downloads**
- Linux: `Ponzik-x86_64.AppImage` (make it executable, then run it)
- Windows: `Ponzik-windows-x86_64.exe`, or the portable `Ponzik-windows-x86_64.zip`
- `SHA256SUMS`: checksums of the files above

Installed builds check for updates at startup and update themselves. See `docs/INSTALL.md`.
EOF
echo
echo "## $heading"
echo
# shellcheck disable=SC2086
git log --no-merges --format='- %s (%h)' $range | sed 's/—/-/g'
echo
echo "<!-- ponzik-build: $version+$build.$hash -->"
