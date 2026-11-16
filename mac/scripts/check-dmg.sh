#!/bin/bash
# Checks a Zone.dmg the way a stranger's Mac will see it after a download: a copy gets the
# quarantine mark a browser puts on downloads, then Gatekeeper is asked about the disk image
# and about the app inside it, and both signatures and stapled tickets are checked.
#
#   mac/scripts/check-dmg.sh [path/to/Zone.dmg]     (default mac/build/Zone.dmg)
#
# Exits non-zero when any check fails. Nothing is opened in the Finder; the image is
# mounted read-only, out of sight, and ejected again.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
dmg="${1:-$(dirname "$here")/build/Zone.dmg}"
[ -f "$dmg" ] || { echo "No disk image at $dmg" >&2; exit 1; }

work="$(mktemp -d)"
mount="$work/mount"
mkdir -p "$mount"
cleanup() {
    hdiutil detach "$mount" -quiet 2>/dev/null || hdiutil detach "$mount" -force -quiet 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

failed=0
check() {
    # check "what" command...: runs it, shows what it said, and remembers a failure.
    local what="$1"
    shift
    echo
    echo "== $what"
    echo "\$ $*"
    if "$@" 2>&1; then
        echo "-> ok"
    else
        echo "-> FAILED"
        failed=1
    fi
}

copy="$work/Zone.dmg"
cp "$dmg" "$copy"
# What Safari writes on a download: flags, the time, the app that downloaded it.
xattr -w com.apple.quarantine "0081;$(printf %x "$(date +%s)");Safari;" "$copy"
echo "Checking a quarantined copy of $dmg"
xattr -p com.apple.quarantine "$copy"

check "Gatekeeper on the disk image" spctl -a -vv -t open --context context:primary-signature "$copy"
check "The disk image's ticket" xcrun stapler validate "$copy"
check "The disk image's signature" codesign --verify --strict -vv "$copy"

if ! hdiutil attach "$copy" -readonly -nobrowse -noautoopen -mountpoint "$mount" -quiet; then
    echo "The disk image could not be opened." >&2
    exit 1
fi
app="$mount/Zone.app"
[ -d "$app" ] || { echo "There is no Zone.app in the disk image." >&2; exit 1; }

check "Gatekeeper on the app inside" spctl -a -vv "$app"
check "The app's signature, everything inside it included" codesign --verify --deep --strict -vv "$app"
check "The app's ticket" xcrun stapler validate "$app"
check "zone inside the app: hardened runtime and a secure timestamp" bash -c '
    info="$(codesign -dvv "$1/Contents/Helpers/zone" 2>&1)"
    echo "$info" | grep -E "^(Identifier|Authority|Timestamp|TeamIdentifier)=|flags="
    echo "$info" | grep -q "flags=.*runtime" && echo "$info" | grep -q "^Timestamp=" && echo "$info" | grep -q "^Authority=Developer ID Application"
' _ "$app"

echo
if [ "$failed" = 0 ]; then
    echo "Every check passed: a downloaded Zone.dmg opens on any Mac with a double click."
else
    echo "Some checks failed (above). People who download this disk image will be warned or blocked."
fi
exit "$failed"
