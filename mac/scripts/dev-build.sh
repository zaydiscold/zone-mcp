#!/bin/bash
# A quick debug build for this Mac only (Apple silicon or Intel, whichever it is), for
# working on the app. The app that people download is made by build-app.sh.
set -euo pipefail
mac="$(cd "$(dirname "$0")/.." && pwd)"
cd "$mac"
xcodegen generate >/dev/null
# Client settings in this shell's environment must not reach the build's scripts.
env -u CLAUDE_CONFIG_DIR xcodebuild -project Zone.xcodeproj -scheme Zone -configuration Debug \
    -derivedDataPath build/DerivedData -destination "platform=macOS,arch=$(uname -m)" \
    ONLY_ACTIVE_ARCH=YES ARCHS="$(uname -m)" build 2>&1 \
    | grep -E "^\*\* BUILD|\.swift:[0-9]+:[0-9]+: (error|warning)|error:" | sort -u
echo "$mac/build/DerivedData/Build/Products/Debug/Zone.app"
