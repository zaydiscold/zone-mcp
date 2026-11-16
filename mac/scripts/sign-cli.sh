#!/bin/bash
# Signs the standalone `zone` program for macOS with Developer ID, has Apple notarize it, and
# packs the release download again.
#
#   mac/scripts/sign-cli.sh zone-0.1.0-macos-universal.tar.gz    (from the draft release)
#   mac/scripts/sign-cli.sh path/to/zone [--version 0.1.0]        (a program built here)
#
# The result is zone-<version>-macos-universal.tar.gz in mac/build, and its SHA-256 for the
# Homebrew formula (packaging/homebrew), which has to change with it.
#
# Why bother, when the release workflow's ad hoc signature runs fine:
# - A tarball downloaded with a browser is quarantined, and so is the zone unpacked from it.
#   macOS then refuses to run an ad hoc signed zone until the person finds "Open Anyway" in
#   System Settings. Signed and notarized, it simply runs. (curl, the install script and
#   Homebrew formulae do not quarantine, so those were never affected.)
# - macOS remembers a Local Network answer for a program by its signature. An ad hoc
#   signature is new with every build, so `zone install`'s login item can be asked again
#   after each update. Developer ID keeps one identity across versions (Apple's TN3179).
#
# A bare program cannot have its ticket stapled (only apps, disk images and installers can),
# so Gatekeeper fetches the ticket online the first time.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
build="$(dirname "$here")/build"
input="${1:?a zone program or a zone-<version>-macos-universal.tar.gz}"
shift
version=""
while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="$2"; shift ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
    shift
done

identity="$(security find-identity -v -p codesigning | grep '"Developer ID Application' | head -1 | sed -E 's/^[^"]*"([^"]+)".*/\1/' || true)"
[ -n "$identity" ] || { echo "No Developer ID Application identity here. See mac/scripts/developer-id.sh." >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/stage"
case "$input" in
    *.tar.gz)
        tar -xzf "$input" -C "$work/stage"
        [ -n "$version" ] || version="$(basename "$input" | sed -E 's/^zone-(.+)-macos-universal\.tar\.gz$/\1/')"
        ;;
    *)
        cp "$input" "$work/stage/zone"
        cp "$(dirname "$build")/../LICENSE" "$work/stage/LICENSE" 2>/dev/null || true
        ;;
esac
zone="$work/stage/zone"
[ -f "$zone" ] || { echo "No zone program in $input" >&2; exit 1; }
[ -n "$version" ] || version="$("$zone" --version | awk '{print $2}')"

echo "== Signing zone $version as $identity"
codesign --force --options runtime --timestamp --identifier wtf.zayd.zone --sign "$identity" "$zone"
codesign --verify --strict -vv "$zone"

echo "== Notarizing"
ditto -c -k "$zone" "$work/zone.zip"
if command -v asc >/dev/null && [ -z "${NOTARY_KEY:-}${NOTARY_PROFILE:-}" ]; then
    asc notarization submit --file "$work/zone.zip" --wait
elif [ -n "${NOTARY_PROFILE:-}" ]; then
    xcrun notarytool submit "$work/zone.zip" --keychain-profile "$NOTARY_PROFILE" --wait
else
    xcrun notarytool submit "$work/zone.zip" --key "${NOTARY_KEY:?}" --key-id "${NOTARY_KEY_ID:?}" --issuer "${NOTARY_ISSUER:?}" --wait
fi

mkdir -p "$build"
out="$build/zone-$version-macos-universal.tar.gz"
tar -C "$work/stage" -czf "$out" zone $( [ -f "$work/stage/LICENSE" ] && echo LICENSE )
echo "== Done: $out"
echo "sha256 $(shasum -a 256 "$out" | cut -d' ' -f1)  (put this in packaging/homebrew for macos-universal)"
