#!/bin/bash
# Gets this Mac a "Developer ID Application" signing identity, the one thing a Zone.dmg that
# anyone can open needs and that no API key can make. (Apple answers the App Store Connect
# API with "This operation can only be performed by the Account Holder", and xcodebuild's
# cloud signing with an API key, even an Admin one, with "Cloud signing permission error".)
#
#   mac/scripts/developer-id.sh request   Makes the private key (in the login keychain) and
#                                         the certificate request, and opens Apple's page.
#   mac/scripts/developer-id.sh install [FILE.cer]
#                                         Adds the certificate Apple gave back (by default
#                                         the newest developerID_application*.cer in
#                                         Downloads) to the login keychain, beside its key.
#   mac/scripts/developer-id.sh status    Says which Developer ID identities this Mac has.
#
# Between the two, the Account Holder makes the certificate on developer.apple.com (five clicks and the file) (see the
# README, "Releasing"). `build-app.sh --notarize` runs `install` by itself when it finds the
# downloaded certificate, so after the clicks one command is left.
#
# Options for request: --no-open (do not open the page), --out FILE (where the request goes,
# default ~/Desktop/Zone Developer ID.certSigningRequest), --name, --email.
set -euo pipefail

openssl=/usr/bin/openssl
keychain="$HOME/Library/Keychains/login.keychain-db"
page="https://developer.apple.com/account/resources/certificates/add"

identities() { security find-identity -v -p codesigning 2>/dev/null | grep '"Developer ID Application' || true; }

command="${1:-status}"
shift || true
case "$command" in
request)
    open_page=1
    out="$HOME/Desktop/Zone Developer ID.certSigningRequest"
    name="$(git config user.name 2>/dev/null || id -F)"
    email="$(git config user.email 2>/dev/null || echo "")"
    while [ $# -gt 0 ]; do
        case "$1" in
            --no-open) open_page=0 ;;
            --out) out="$2"; shift ;;
            --name) name="$2"; shift ;;
            --email) email="$2"; shift ;;
            *) echo "Unknown option: $1" >&2; exit 1 ;;
        esac
        shift
    done
    if [ -n "$(identities)" ]; then
        echo "This Mac already has a Developer ID Application identity:"
        identities
        exit 0
    fi
    # The key is made in a folder only this user can read, goes into the login keychain,
    # and the file is removed. Only the request (which holds the public key) stays.
    work="$(mktemp -d)"
    chmod 700 "$work"
    trap 'rm -rf "$work"' EXIT
    subject="/CN=${name}"
    if [ -n "$email" ]; then subject="/emailAddress=${email}${subject}"; fi
    "$openssl" req -new -newkey rsa:2048 -nodes -keyout "$work/key.pem" -out "$work/request.csr" -subj "$subject" 2>/dev/null
    # PKCS#1, the form `security import` reads as an RSA private key.
    "$openssl" rsa -in "$work/key.pem" -out "$work/key.rsa.pem" -traditional 2>/dev/null \
        || "$openssl" rsa -in "$work/key.pem" -out "$work/key.rsa.pem" 2>/dev/null
    security import "$work/key.rsa.pem" -k "$keychain" -t priv -f openssl -T /usr/bin/codesign -T /usr/bin/security >/dev/null
    cp "$work/request.csr" "$out"
    echo "The private key is in the login keychain. The request is at:"
    echo "  $out"
    echo
    echo "On developer.apple.com, as the Account Holder:"
    echo "  1. Developer ID Application, then Continue"
    echo "  2. Profile Type: G2 Sub-CA (the one already chosen)"
    echo "  3. Choose File: the request above, then Continue"
    echo "  4. Download"
    echo "Then: mac/scripts/build-app.sh --notarize   (it adds the downloaded certificate)"
    if [ "$open_page" = 1 ]; then open "$page"; fi
    ;;
install)
    file="${1:-}"
    if [ -z "$file" ]; then
        file="$(ls -t "$HOME"/Downloads/developerID_application*.cer 2>/dev/null | head -1 || true)"
    fi
    [ -n "$file" ] && [ -f "$file" ] || { echo "No certificate to add. Download it from $page first (see the README, Releasing)." >&2; exit 1; }
    subject="$("$openssl" x509 -inform DER -in "$file" -noout -subject 2>/dev/null || "$openssl" x509 -in "$file" -noout -subject)"
    case "$subject" in
        *"Developer ID Application"*) ;;
        *) echo "$file is not a Developer ID Application certificate ($subject)." >&2; exit 1 ;;
    esac
    security import "$file" -k "$keychain" >/dev/null 2>&1 || true
    if [ -z "$(identities)" ]; then
        cat >&2 <<EOF
The certificate is in the keychain, but this keychain has no private key that belongs to it,
so it cannot sign. It was made from a request on another Mac (or the key was deleted).
Make a new request here (mac/scripts/developer-id.sh request) and a new certificate from it.
EOF
        exit 1
    fi
    echo "Ready to sign:"
    identities
    ;;
status)
    found="$(identities)"
    if [ -n "$found" ]; then echo "$found"; else echo "No Developer ID Application identity on this Mac."; exit 1; fi
    ;;
*)
    sed -n '2,24p' "$0"
    exit 1
    ;;
esac
