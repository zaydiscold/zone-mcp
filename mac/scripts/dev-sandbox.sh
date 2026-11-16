#!/bin/bash
# Runs Zone for Mac in a sandbox folder, so nothing real is touched: a pretend home folder
# (with a few pretend AI clients in it), a made-up library, another port. The installed
# zone, its login item, the real library and real AI client settings are never read or
# written.
#
#   dev-sandbox.sh start [--fresh] [--show popover,welcome] [--light] [--front] [--mock-camera]
#   dev-sandbox.sh cmd popover:pair        steer the running app (see ZoneApp.swift)
#   dev-sandbox.sh phone [count]           a stand-in phone scans the code on screen
#   dev-sandbox.sh send [count]            that phone sends a few more items
#   dev-sandbox.sh clip "some text"        that phone sends a clip (text for the clipboard)
#   dev-sandbox.sh zone status             run the sandbox's zone with any arguments
#   dev-sandbox.sh stop
#
# The folder is $ZONE_MAC_SANDBOX_DIR, or /tmp/zone-mac-sandbox.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
mac="$(dirname "$here")"
box="${ZONE_MAC_SANDBOX_DIR:-/tmp/zone-mac-sandbox}"
port="${ZONE_PORT:-47991}"
app="${ZONE_MAC_APP:-}"
if [ -z "$app" ]; then
    for candidate in "$mac/build/Zone.app" "$mac/build/DerivedData/Build/Products/Debug/Zone.app"; do
        if [ -d "$candidate" ]; then app="$candidate"; break; fi
    done
fi
[ -d "$app" ] || { echo "No Zone.app found. Build it first (mac/scripts/build-app.sh)." >&2; exit 1; }
zone_binary="$app/Contents/Helpers/zone"

# The sandbox's zone: only these variables, so nothing from this shell leaks in. Once the
# app has made its own copy in the data folder, that copy is the one run, the same as the
# app runs (zone registers AI clients with the path of the program that was run).
zone() {
    local program="$zone_binary"
    if [ -x "$box/zone-home/bin/zone" ]; then program="$box/zone-home/bin/zone"; fi
    env -i HOME="$box/home" ZONE_HOME="$box/zone-home" ZONE_PORT="$port" ZONE_NAME="Demo Mac" PATH="$box/bin:/usr/bin:/bin" "$program" "$@"
}

# The sandbox app's process, whichever build it was started from.
running_pid() {
    local pid
    pid="$(cat "$box/app.pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && ps -p "$pid" -o command= 2>/dev/null | grep -q "Zone.app/Contents/MacOS/Zone"; then
        echo "$pid"
    fi
}

command="${1:-start}"
shift || true
case "$command" in
start)
    fresh=0; show=""; appearance="dark"; quiet=1; mock_camera=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --fresh) fresh=1 ;;
            --show) show="$2"; shift ;;
            --light) appearance="light" ;;
            --front) quiet=0 ;;
            # WebKit's stand-in camera for the Flash window: the whole camera path runs
            # without a real camera, and macOS never asks.
            --mock-camera) mock_camera=1 ;;
            *) echo "unknown option $1" >&2; exit 1 ;;
        esac
        shift
    done
    if [ -n "$(running_pid)" ]; then
        echo "The sandbox app is already running (pid $(running_pid)). Stop it first: $0 stop" >&2
        exit 1
    fi
    if [ "$fresh" = 1 ] && [ -d "$box" ]; then
        # Moved aside, not deleted: an old sandbox is harmless and /tmp empties itself.
        mv "$box" "$box.old.$(date +%s)"
    fi
    if [ ! -d "$box/home" ]; then
        mkdir -p "$box/home/.local/bin" "$box/zone-home"
        # Pretend AI clients: an empty settings folder is all it takes for zone to find one.
        mkdir -p "$box/home/.claude" "$box/home/Library/Application Support/Claude" "$box/home/.cursor" \
                 "$box/home/.gemini" "$box/home/.config/zed" "$box/home/.codex"
        : > "$box/home/.codex/config.toml"
        # A pretend `open`: links the app would show in the browser are written to the
        # file `opened` instead, so a test run never brings a browser to the front.
        mkdir -p "$box/bin"
        printf '#!/bin/sh\necho "$@" >> "%s/opened"\n' "$box" > "$box/bin/open"
        chmod +x "$box/bin/open"
        python3 "$here/dev-items.py" backup "$box/backup"
        zone import "$box/backup/Demo.zonebackup" >/dev/null
        echo "Made a sandbox in $box with $(zone status --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["items"]["total"])') made-up items."
    fi
    for leftover in command windows pair-url; do
        if [ -e "$box/$leftover" ]; then mv "$box/$leftover" "$box/$leftover.old"; fi
    done
    variables=(ZONE_MAC_SANDBOX="$box" ZONE_PORT="$port" ZONE_NAME="Demo Mac" ZONE_MAC_APPEARANCE="$appearance" ZONE_MAC_SHOW="$show")
    if [ "$quiet" = 1 ]; then variables+=(ZONE_MAC_QUIET=1); fi
    if [ "$mock_camera" = 1 ]; then variables+=(ZONE_MAC_MOCK_CAMERA=1); fi
    nohup env -i HOME="$HOME" PATH=/usr/bin:/bin "${variables[@]}" "$app/Contents/MacOS/Zone" >"$box/app.log" 2>&1 &
    echo "$!" > "$box/app.pid"
    echo "Started $app (pid $!). Its windows are listed in $box/windows."
    ;;
cmd)
    printf '%s\n' "$@" > "$box/command.tmp"
    mv "$box/command.tmp" "$box/command"
    ;;
phone)
    count="${1:-180}"
    [ -f "$box/pair-url" ] || { echo "No pair code is on screen. Open one first: $0 cmd popover:pair" >&2; exit 1; }
    python3 "$here/dev-items.py" items "$count" 11 > "$box/phone-items.jsonl"
    zone dev fake-phone --url "$(cat "$box/pair-url")" --identity "$box/phone.id" --name "Demo iPhone" \
        --file "$box/phone-items.jsonl" --batch 30 \
        | python3 -c 'import json,sys; r=json.load(sys.stdin); print("paired:", r["result"], "sent", r["sent"], "stored", r["stored"], "path", r["path"])'
    ;;
send)
    count="${1:-3}"
    python3 "$here/dev-items.py" items "$count" "$RANDOM" > "$box/phone-items.jsonl"
    zone dev fake-phone --identity "$box/phone.id" --name "Demo iPhone" --file "$box/phone-items.jsonl" \
        | python3 -c 'import json,sys; r=json.load(sys.stdin); print("sent", r["sent"], "stored", r["stored"], "path", r["path"])'
    ;;
clip)
    text="${1:-https://zayd.wtf/zone}"
    # In a test run nothing writes the real clipboard: what zone or the app would have
    # put there goes to the file `clipboard` in the sandbox folder.
    zone dev fake-phone --identity "$box/phone.id" --name "Demo iPhone" --clip "$text" \
        | python3 -c 'import json,sys; r=json.load(sys.stdin); print("sent a clip:", r.get("clips"), "path", r.get("path"))'
    ;;
zone)
    zone "$@"
    ;;
stop)
    pid="$(running_pid)"
    if [ -n "$pid" ]; then
        kill "$pid"
        for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
        echo "Stopped the sandbox app."
    else
        echo "The sandbox app was not running."
    fi
    # The app stops the zone it started. If the app was killed hard, stop that zone too.
    leftover="$(zone status --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin)["daemon"]; print(d.get("pid") or "")' 2>/dev/null || true)"
    if [ -n "$leftover" ]; then kill "$leftover" 2>/dev/null || true; fi
    ;;
*)
    sed -n '2,15p' "$0"
    exit 1
    ;;
esac
