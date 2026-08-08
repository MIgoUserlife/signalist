#!/usr/bin/env bash
# Adds a plain-text first-run README next to the app icon inside the .dmg
# assets already published for a GitHub release. Runs after tauri-action's
# own publish step, so it never touches the updater signing/publish flow
# (latest.json only references .app.tar.gz, never the .dmg).
set -euo pipefail

TAG="${1:?usage: inject-dmg-readme.sh <release-tag> <readme-file>}"
README_SRC="${2:?usage: inject-dmg-readme.sh <release-tag> <readme-file>}"

if [ ! -f "$README_SRC" ]; then
  echo "README source not found: $README_SRC" >&2
  exit 1
fi

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

echo "Downloading .dmg assets for $TAG..."
gh release download "$TAG" --pattern "*.dmg" --dir "$WORKDIR" --clobber

shopt -s nullglob
DMGS=("$WORKDIR"/*.dmg)
if [ ${#DMGS[@]} -eq 0 ]; then
  echo "No .dmg asset found for $TAG, nothing to patch." >&2
  exit 1
fi

for DMG in "${DMGS[@]}"; do
  NAME="$(basename "$DMG")"
  BASE="${NAME%.dmg}"
  echo "Patching $NAME..."

  RW_DMG="$WORKDIR/${BASE}-rw.dmg"
  hdiutil convert "$DMG" -format UDRW -o "$RW_DMG"
  # Pad with room for the README; original image is sized tightly to its content.
  hdiutil resize -size 60m "$RW_DMG"

  MOUNT_DIR="$WORKDIR/mnt-${BASE}"
  mkdir -p "$MOUNT_DIR"
  hdiutil attach "$RW_DMG" -mountpoint "$MOUNT_DIR" -nobrowse -quiet -noautoopen

  cp "$README_SRC" "$MOUNT_DIR/README.txt"

  hdiutil detach "$MOUNT_DIR" -quiet

  FINAL_DMG="$WORKDIR/${BASE}-final.dmg"
  hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$FINAL_DMG"

  mv -f "$FINAL_DMG" "$DMG"
done

echo "Re-uploading patched .dmg asset(s) to $TAG..."
gh release upload "$TAG" "${DMGS[@]}" --clobber

echo "Done."
