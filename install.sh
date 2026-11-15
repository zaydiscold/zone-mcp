#!/bin/sh
# Installs zone, the computer side of the Zone iPhone app, on macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/zaydiscold/zone-mcp/main/install.sh | sh
#
# It downloads the latest release for this computer from GitHub, checks it against the
# release's SHA256SUMS, and puts the one program, `zone`, in ~/.local/bin. No admin rights,
# nothing else is changed. Run it again to update. `zone update` runs this same script.
#
# Settings, all optional:
#   ZONE_VERSION              install this version instead of the latest (for example 0.2.0)
#   ZONE_INSTALL_DIR          put zone here instead of ~/.local/bin
#   ZONE_INSTALL_BASE_URL     download from here instead of github.com/zaydiscold/zone-mcp/releases
#   ZONE_INSTALL_NO_RESTART   set to 1 to leave a running zone alone

set -eu

REPO="zaydiscold/zone-mcp"
LAUNCHD_LABEL="wtf.zayd.zone.link"
SYSTEMD_UNIT="zone.service"

say() {
    printf '%s\n' "$*"
}

fail() {
    printf 'zone install: %s\n' "$*" >&2
    exit 1
}

# The whole script is one function, called on the last line, so that a download cut off
# halfway runs nothing at all.
main() {
    base="${ZONE_INSTALL_BASE_URL:-https://github.com/$REPO/releases}"
    base="${base%/}"
    dir="${ZONE_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}"
    version="${ZONE_VERSION:-}"

    command -v curl >/dev/null 2>&1 || fail "curl is needed to download zone. Install curl and run this again."
    command -v tar >/dev/null 2>&1 || fail "tar is needed to unpack zone."

    # Which release file is for this computer.
    os="$(uname -s)"
    machine="$(uname -m)"
    case "$os" in
        Darwin)
            platform="macos-universal"
            ;;
        Linux)
            case "$machine" in
                x86_64 | amd64) platform="linux-x86_64" ;;
                aarch64 | arm64) platform="linux-aarch64" ;;
                *) fail "there is no ready-made zone for Linux on $machine. Build it from source: https://github.com/$REPO#from-source" ;;
            esac
            ;;
        *)
            fail "there is no ready-made zone for $os. On Windows use install.ps1. Otherwise build it from source: https://github.com/$REPO#from-source"
            ;;
    esac

    # Which version is the latest: GitHub sends /releases/latest on to /releases/tag/v<version>.
    if [ -z "$version" ]; then
        latest="$(curl -fsSL -o /dev/null -w '%{url_effective}' "$base/latest")" || fail "could not reach $base/latest. Is this computer online?"
        version="${latest##*/}"
    fi
    version="${version#v}"
    case "$version" in
        [0-9]*) ;;
        *) fail "could not tell which version is the latest (got \"$version\"). No release may have been published yet: see https://github.com/$REPO/releases" ;;
    esac

    asset="zone-$version-$platform.tar.gz"
    work="$(mktemp -d "${TMPDIR:-/tmp}/zone-install.XXXXXX")"
    trap 'rm -rf "$work"' EXIT
    trap 'rm -rf "$work"; exit 130' INT TERM

    say "Downloading zone $version for $platform"
    curl -fsSL -o "$work/$asset" "$base/download/v$version/$asset" || fail "could not download $base/download/v$version/$asset"
    curl -fsSL -o "$work/SHA256SUMS" "$base/download/v$version/SHA256SUMS" || fail "could not download the checksums of zone $version, so the download cannot be checked"

    # The download must be the file the release says it is.
    expected="$(awk -v file="$asset" '$2 == file || $2 == "*" file { print $1; exit }' "$work/SHA256SUMS")"
    [ -n "$expected" ] || fail "the checksums of zone $version do not list $asset"
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$work/$asset" | awk '{ print $1 }')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$work/$asset" | awk '{ print $1 }')"
    else
        fail "neither sha256sum nor shasum is on this computer, so the download cannot be checked"
    fi
    [ "$actual" = "$expected" ] || fail "the download does not match its checksum (expected $expected, got $actual). Nothing was installed."

    mkdir "$work/unpacked"
    tar -xzf "$work/$asset" -C "$work/unpacked" || fail "could not unpack $asset"
    [ -f "$work/unpacked/zone" ] || fail "$asset does not hold a zone program"
    chmod 755 "$work/unpacked/zone"
    # It must run here before it replaces anything.
    installed_version="$("$work/unpacked/zone" --version 2>/dev/null)" || fail "the downloaded zone does not run on this computer. Nothing was installed. Build it from source instead: https://github.com/$REPO#from-source"

    mkdir -p "$dir" || fail "cannot create $dir"
    target="$dir/zone"
    updating=""
    [ -e "$target" ] && updating="yes"

    # Put the new program beside the old one, take the old one away, then move the new one
    # into its place. Writing over a running zone would get it killed by macOS, whose code
    # signing check does not allow a running program's file to change.
    staged="$dir/.zone-new.$$"
    cp "$work/unpacked/zone" "$staged" || fail "cannot write to $dir"
    chmod 755 "$staged"
    rm -f "$target"
    mv "$staged" "$target" || fail "cannot put zone in $dir"

    say "Installed $installed_version in $target"

    # A zone that starts at login keeps running the old program until it is started again.
    restarted=""
    if [ -n "$updating" ] && [ "${ZONE_INSTALL_NO_RESTART:-}" != "1" ]; then
        case "$os" in
            Darwin)
                plist="$HOME/Library/LaunchAgents/$LAUNCHD_LABEL.plist"
                if [ -f "$plist" ] && grep -q -F "<string>$target</string>" "$plist" && launchctl print "gui/$(id -u)/$LAUNCHD_LABEL" >/dev/null 2>&1; then
                    launchctl kickstart -k "gui/$(id -u)/$LAUNCHD_LABEL" >/dev/null 2>&1 && restarted="yes"
                fi
                ;;
            Linux)
                unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$SYSTEMD_UNIT"
                if [ -f "$unit" ] && grep -q -F "$target" "$unit" && systemctl --user is-active --quiet "$SYSTEMD_UNIT" 2>/dev/null; then
                    systemctl --user restart "$SYSTEMD_UNIT" >/dev/null 2>&1 && restarted="yes"
                fi
                ;;
        esac
    fi
    if [ -n "$restarted" ]; then
        say "Restarted zone, so the new version is the one running."
    elif [ -n "$updating" ]; then
        say "If zone is running (\`zone status\` says), stop it and start it again to run the new version."
    fi

    # Is the folder somewhere the shell looks for programs? (`zone update` was found there.)
    case ":${PATH:-}:${ZONE_INSTALL_UPDATING:+$dir:}" in
        *":$dir:"*) ;;
        *)
            say ""
            say "$dir is not on your PATH yet, so the shell will not find \`zone\`. Add it:"
            case "${SHELL:-}" in
                */zsh) say "  echo 'export PATH=\"$dir:\$PATH\"' >> ~/.zshrc && exec zsh" ;;
                */fish) say "  fish_add_path \"$dir\"" ;;
                *) say "  echo 'export PATH=\"$dir:\$PATH\"' >> ~/.bashrc && exec bash" ;;
            esac
            ;;
    esac

    if [ -z "${ZONE_INSTALL_UPDATING:-}" ]; then
        say ""
        say "Next:"
        say "  zone install    start zone every time you log in, and let your AI agents use it"
        say "  zone pair       show the code to scan with Zone on your iPhone"
        say "  zone open       see your transcriptions in the browser"
    fi
}

main "$@"
