#!/bin/bash
# Earlybolt installer for macOS
#   curl -fsSL https://raw.githubusercontent.com/tony1010-dev/earlybolt/main/install.sh | bash
set -euo pipefail

REPO="tony1010-dev/earlybolt"

if [ "$(uname)" != "Darwin" ]; then
  echo "This installer is for macOS. On Windows run in PowerShell:"
  echo "  irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex"
  exit 1
fi

echo "⚡ Installing Earlybolt…"

# No -f here: a missing release returns 404, which we explain below instead of failing silently
release=$(curl -sSL "https://api.github.com/repos/$REPO/releases/latest" || true)
url=$(printf '%s' "$release" | grep -o '"browser_download_url": *"[^"]*\.dmg"' | head -1 | sed -E 's/.*"(https[^"]+)"/\1/' || true)
if [ -z "$url" ]; then
  echo "No macOS download is published yet (the release may still be building)."
  echo "Check https://github.com/$REPO/releases and try again in a few minutes."
  exit 1
fi

tmp=$(mktemp -d)
cleanup() { hdiutil detach "$tmp/mnt" -quiet 2>/dev/null || true; rm -rf "$tmp"; }
trap cleanup EXIT

curl -fL --progress-bar "$url" -o "$tmp/Earlybolt.dmg"
hdiutil attach "$tmp/Earlybolt.dmg" -nobrowse -quiet -mountpoint "$tmp/mnt"

# /Applications is writable for admin accounts; otherwise install for this user only
dest="/Applications"
[ -w "$dest" ] || { dest="$HOME/Applications"; mkdir -p "$dest"; }

pkill -x earlybolt 2>/dev/null || true
rm -rf "$dest/Earlybolt.app"
cp -R "$tmp/mnt/Earlybolt.app" "$dest/"
# Downloaded with curl, so no quarantine flag is set — remove it anyway in case one exists
xattr -dr com.apple.quarantine "$dest/Earlybolt.app" 2>/dev/null || true

open "$dest/Earlybolt.app"
echo "✓ Installed to $dest/Earlybolt.app — look for ⚡ in the menu bar."
