#!/bin/bash
# Builds Zone for Mac and the disk image people download.
#
#   mac/scripts/build-app.sh [options]
#
#   --identity NAME     The signing identity, as `security find-identity -v -p codesigning`
#                       lists it (or its 40 character hash). "-" signs ad hoc.
#                       Default: a "Developer ID Application" identity if the keychain has
#                       one, else "Apple Development", else ad hoc.
#   --zone-binary FILE  Use this zone program instead of building the universal one.
#   --committed         Build zone from the last commit, not from the files as they are on
#                       disk (for when the Rust code is in the middle of a change).
#   --notarize          Send the app and the disk image to Apple, staple the tickets and
#                       check both the way Gatekeeper will. Needs a Developer ID identity.
#                       Uses `asc notarization` when asc is installed, else notarytool with
#                       NOTARY_KEY (a .p8 file), NOTARY_KEY_ID and NOTARY_ISSUER.
#   --notary-profile P  Notarize with this notarytool keychain profile instead (made once
#                       with `xcrun notarytool store-credentials P`). Implies --notarize.
#                       The NOTARY_PROFILE variable does the same.
#   --no-dmg            Stop after the app.
#   --version X.Y.Z     The version to stamp (default: the one in project.yml).
#
# Results: mac/build/Zone.app and mac/build/Zone.dmg.
#
# What "signed" means for who can open it:
#   Developer ID + notarized   anyone, on any Mac, with a double click
#   Apple Development          this Mac (and Macs registered to the team)
#   ad hoc                     this Mac only
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
mac="$(dirname "$here")"
build="$mac/build"
identity=""
zone_binary=""
notarize=0
make_dmg=1
version=""
committed=""

while [ $# -gt 0 ]; do
    case "$1" in
        --identity) identity="$2"; shift ;;
        --zone-binary) zone_binary="$2"; shift ;;
        --committed) committed="--committed" ;;
        --notarize) notarize=1 ;;
        --notary-profile) NOTARY_PROFILE="$2"; notarize=1; shift ;;
        --no-dmg) make_dmg=0 ;;
        --version) version="$2"; shift ;;
        -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
        *) echo "Unknown option: $1 (see --help)" >&2; exit 1 ;;
    esac
    shift
done

say() { printf '\n== %s\n' "$*"; }
need() { command -v "$1" >/dev/null || { echo "$1 is not installed. $2" >&2; exit 1; }; }

need xcodebuild "Install Xcode."
need xcodegen "Install it with: brew install xcodegen"

# ------------------------------------------------------------------ the identity

identities="$(security find-identity -v -p codesigning 2>/dev/null || true)"
if [ -z "$identity" ]; then
    for kind in "Developer ID Application" "Apple Development"; do
        found="$(grep "\"$kind" <<<"$identities" | head -1 | sed -E 's/^[^"]*"([^"]+)".*/\1/' || true)"
        if [ -n "$found" ]; then identity="$found"; break; fi
    done
    identity="${identity:--}"
fi
case "$identity" in
    "Developer ID Application"*) kind="developer-id" ;;
    -) kind="ad-hoc" ;;
    *) kind="development" ;;
esac
# A hash was given: look up what it is.
if [[ "$identity" =~ ^[0-9A-Fa-f]{40}$ ]] && grep -i "$identity" <<<"$identities" | grep -q "Developer ID Application"; then
    kind="developer-id"
fi
say "Signing as: $identity ($kind)"

if [ "$notarize" = 1 ] && [ "$kind" != "developer-id" ]; then
    cat >&2 <<'EOF'
Apple only notarizes apps signed with a "Developer ID Application" certificate, and this
keychain has none. Only the Account Holder of the developer team can make one:

  Xcode > Settings > Accounts > (the team) > Manage Certificates > + > Developer ID Application

Then run this script again with --notarize.
EOF
    exit 1
fi

# ------------------------------------------------------------------ the zone program

if [ -z "$zone_binary" ]; then
    say "Building the universal zone program"
    "$here/build-zone-binary.sh" $committed "$build/zone"
    zone_binary="$build/zone"
fi
[ -x "$zone_binary" ] || { echo "No zone program at $zone_binary" >&2; exit 1; }

# ------------------------------------------------------------------ the app

say "Building Zone.app"
cd "$mac"
xcodegen generate >/dev/null
settings=(ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO CODE_SIGNING_ALLOWED=NO)
if [ -n "$version" ]; then settings+=(MARKETING_VERSION="$version"); fi
log="$build/xcodebuild.log"
mkdir -p "$build"
# The build is run without this shell's environment for AI clients, and with the zone
# program handed to the "Embed zone" step.
if ! env -u CLAUDE_CONFIG_DIR ZONE_BINARY="$zone_binary" xcodebuild -project Zone.xcodeproj -scheme Zone -configuration Release \
        -derivedDataPath "$build/DerivedData" -destination "generic/platform=macOS" "${settings[@]}" ZONE_BINARY="$zone_binary" build >"$log" 2>&1; then
    grep -E "error|BUILD FAILED" "$log" | head -40 >&2
    echo "The build failed. The whole log is in $log" >&2
    exit 1
fi
built="$build/DerivedData/Build/Products/Release/Zone.app"
[ -d "$built" ] || { echo "xcodebuild said it worked but there is no $built" >&2; exit 1; }

app="$build/Zone.app"
if [ -d "$app" ]; then mv "$app" "$build/Zone.app.previous.$$"; fi
ditto "$built" "$app"
helper="$app/Contents/Helpers/zone"
[ -x "$helper" ] || { echo "The app has no zone program inside it ($helper)." >&2; exit 1; }

# ------------------------------------------------------------------ signing

say "Signing"
flags=(--force --options runtime)
if [ "$kind" = "developer-id" ]; then flags+=(--timestamp); else flags+=(--timestamp=none); fi
# Inside out: the zone program first, then the app around it.
codesign "${flags[@]}" --identifier wtf.zayd.zone.mac.zone --sign "$identity" "$helper"
codesign "${flags[@]}" --sign "$identity" "$app"
codesign --verify --deep --strict "$app"
echo "Zone.app:  $(lipo -archs "$app/Contents/MacOS/Zone")"
echo "zone:      $(lipo -archs "$helper"), $("$helper" --version)"
codesign -dv "$app" 2>&1 | grep -E "^(Identifier|TeamIdentifier|Authority|Timestamp|Runtime)" || true

notary() {
    # notary FILE: send it to Apple and wait for the answer.
    local file="$1"
    if command -v asc >/dev/null && [ -z "${NOTARY_KEY:-}${NOTARY_PROFILE:-}" ]; then
        asc notarization submit --file "$file" --wait
    elif [ -n "${NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$file" --keychain-profile "$NOTARY_PROFILE" --wait
    else
        : "${NOTARY_KEY:?Set NOTARY_KEY, NOTARY_KEY_ID and NOTARY_ISSUER, or NOTARY_PROFILE}"
        xcrun notarytool submit "$file" --key "$NOTARY_KEY" --key-id "${NOTARY_KEY_ID:?}" --issuer "${NOTARY_ISSUER:?}" --wait
    fi
}

if [ "$notarize" = 1 ]; then
    say "Notarizing the app"
    zip="$build/Zone.app.zip"
    ditto -c -k --keepParent "$app" "$zip"
    notary "$zip"
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    spctl -a -vv "$app"
fi

# ------------------------------------------------------------------ the disk image

if [ "$make_dmg" = 0 ]; then
    say "Done: $app"
    exit 0
fi

say "Making Zone.dmg"
dmg="$build/Zone.dmg"
if [ -e "$dmg" ]; then mv "$dmg" "$build/Zone.dmg.previous.$$"; fi
venv="$build/dmg-venv"
if [ ! -x "$venv/bin/dmgbuild" ]; then
    # dmgbuild lays out the window (icon places, background) without driving the Finder.
    python3 -m venv "$venv"
    "$venv/bin/pip" install --quiet "dmgbuild==1.6.7"
fi
"$venv/bin/dmgbuild" -s "$mac/dmg/settings.py" -D app="$app" -D here="$mac/dmg" "Zone" "$dmg" >/dev/null
if [ "$kind" != "ad-hoc" ]; then
    if [ "$kind" = "developer-id" ]; then
        codesign --force --timestamp --sign "$identity" "$dmg"
    else
        codesign --force --timestamp=none --sign "$identity" "$dmg"
    fi
fi
if [ "$notarize" = 1 ]; then
    say "Notarizing the disk image"
    notary "$dmg"
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
    spctl -a -vv --type open --context context:primary-signature "$dmg"
fi

say "Done"
echo "App:        $app"
echo "Disk image: $dmg ($(du -h "$dmg" | cut -f1 | tr -d ' '))"
case "$kind" in
    developer-id)
        if [ "$notarize" = 1 ]; then echo "Signed with Developer ID and notarized: anyone can open it."
        else echo "Signed with Developer ID but not notarized: run again with --notarize before sharing it."; fi ;;
    development) echo "Signed for development: it opens on this Mac. Other people need a Developer ID build (see --help)." ;;
    ad-hoc) echo "Signed ad hoc: it opens on this Mac only." ;;
esac
