#!/usr/bin/env bash
#
# Print the CHANGELOG section for one version, for use as a GitHub release body.
#
# Usage: changelog-section.sh <tag> <changelog-path>
#   e.g. changelog-section.sh v0.5.4 ../CHANGELOG.md
#
# The tag's leading "v" is optional: v0.5.4 and 0.5.4 both match a heading of
# the form "## [0.5.4] — 2026-08-10". Everything up to the next "## [" heading
# is printed, minus the "---" separators the file uses between releases and any
# blank lines at either end.
#
# Never fails the build: an unreleased/typo'd version, or a CHANGELOG that moved,
# prints a fallback line instead of exiting non-zero. A release with imperfect
# notes beats a release that didn't publish.

set -uo pipefail

TAG="${1:-}"
CHANGELOG="${2:-CHANGELOG.md}"

FALLBACK="Перегляньте [CHANGELOG](https://github.com/MIgoUserlife/signalist/blob/main/CHANGELOG.md) для деталей."

if [ -z "$TAG" ] || [ ! -f "$CHANGELOG" ]; then
    echo "$FALLBACK"
    exit 0
fi

VERSION="${TAG#v}"

# Blank lines are buffered rather than printed, so they only make it out when
# more text follows — that drops trailing blanks without a second pass (no `tac`
# on macOS runners) while keeping the blank lines between subsections intact.
SECTION=$(awk -v ver="$VERSION" '
    index($0, "## [" ver "]") == 1 { found = 1; next }
    found && index($0, "## [") == 1 { exit }
    found && $0 == "---" { next }
    found {
        if ($0 ~ /^[[:space:]]*$/) { if (started) pending++; next }
        started = 1
        for (; pending > 0; pending--) print ""
        print
    }
' "$CHANGELOG")

if [ -z "$SECTION" ]; then
    echo "$FALLBACK"
else
    printf '%s\n' "$SECTION"
fi
