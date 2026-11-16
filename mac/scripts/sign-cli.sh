#!/bin/bash
# Signs the standalone `zone` program for macOS with Developer ID, has Apple notarize it, and
# packs the release download again.
#
#   mac/scripts/sign-cli.sh zone-0.1.1-macos-universal.tar.gz --sums SHA256SUMS --formula zone.rb
#   mac/scripts/sign-cli.sh path/to/zone [--version 0.1.1]        (a program built here)
#
# The result is zone-<version>-macos-universal.tar.gz in mac/build. Signing changes the
# tarball's SHA-256, and install.sh and `zone update` refuse any download that does not match
# the release's SHA256SUMS. So when the signed tarball replaces the release's download, the
# release's SHA256SUMS and zone.rb must be replaced with it: pass them with --sums and
# --formula and updated copies are written beside the tarball, ready for
# `gh release upload v<version> --clobber` (all three together).
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
sums=""
formula=""
while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="$2"; shift ;;
        --sums) sums="$2"; shift ;;
        --formula) formula="$2"; shift ;;
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
name="zone-$version-macos-universal.tar.gz"
out="$build/$name"
files=(zone)
[ -f "$work/stage/LICENSE" ] && files+=(LICENSE)
tar -C "$work/stage" -czf "$out" "${files[@]}"
sha="$(shasum -a 256 "$out" | cut -d' ' -f1)"
echo "== Done: $out"
echo "sha256 $sha"

if [ -n "$sums" ]; then
    grep -q "  $name\$" "$sums" || { echo "$sums has no line for $name" >&2; exit 1; }
    awk -v sha="$sha" -v name="$name" '$2 == name { print sha "  " name; next } { print }' "$sums" > "$build/SHA256SUMS"
    echo "== Updated $build/SHA256SUMS"
fi
if [ -n "$formula" ]; then
    grep -q '# macos-universal$' "$formula" || { echo "$formula has no macos-universal sha256 line" >&2; exit 1; }
    sed -E "s/sha256 \"[0-9a-f]{64}\" # macos-universal$/sha256 \"$sha\" # macos-universal/" "$formula" > "$build/zone.rb"
    echo "== Updated $build/zone.rb"
fi
if [ -n "$sums" ] && [ -n "$formula" ]; then
    echo "Upload the three together: gh release upload v$version \"$out\" \"$build/SHA256SUMS\" \"$build/zone.rb\" --clobber"
else
    echo "Warning: the release's SHA256SUMS and zone.rb still name the unsigned tarball. Replacing only the tarball" >&2
    echo "breaks install.sh and zone update on every Mac. Run again with --sums and --formula." >&2
fi
