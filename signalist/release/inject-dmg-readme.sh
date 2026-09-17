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
MOUNT_DIR=""

# Detach before deleting the work directory. Without this, an abort while the
# image is attached sends `rm -rf` straight into the mounted read-write volume,
# deleting the app bundle inside it before it fails on the mount point.
cleanup() {
  if [ -n "$MOUNT_DIR" ] && [ -d "$MOUNT_DIR" ]; then
    hdiutil detach "$MOUNT_DIR" -force -quiet 2>/dev/null || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

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
  # No -nobrowse: Finder must see the volume so AppleScript below can position the icon.
  hdiutil attach "$RW_DMG" -mountpoint "$MOUNT_DIR" -quiet -noautoopen

  cp "$README_SRC" "$MOUNT_DIR/README.txt"

  # A file copied in with plain `cp` has no entry in the existing .DS_Store, so Finder
  # parks it at a fixed off-window coordinate (observed: {325, 462} in a 660x400 window)
  # instead of tiling it into view — it's on disk but invisible to the user. Set its
  # icon position explicitly, below the app/Applications row (180,170) / (480,170).
  # Finder automation is the least reliable step on a hosted runner, and it is
  # purely cosmetic — the README is already on the image. A failure here must
  # not fail a release that tauri-action has already published, the same way
  # changelog-section.sh deliberately never exits non-zero.
  osascript <<APPLESCRIPT || echo "Warning: could not position the README icon; continuing." >&2
tell application "Finder"
    set tgt to (POSIX file "$MOUNT_DIR") as alias
    open tgt
    delay 1
    set position of item "README.txt" of tgt to {330, 290}
    close window of tgt
    open tgt
    update tgt without registering applications
    delay 2
    close window of tgt
end tell
APPLESCRIPT

  hdiutil detach "$MOUNT_DIR" -quiet
  MOUNT_DIR=""

  FINAL_DMG="$WORKDIR/${BASE}-final.dmg"
  hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$FINAL_DMG"

  mv -f "$FINAL_DMG" "$DMG"
done

echo "Re-uploading patched .dmg asset(s) to $TAG..."
gh release upload "$TAG" "${DMGS[@]}" --clobber

echo "Done."
