#!/bin/bash
# Builds the universal (Apple silicon + Intel) `zone` program that the app carries inside it.
#
#   mac/scripts/build-zone-binary.sh [--committed] [output-file]
#
# The result goes to mac/build/zone unless another path is given. Cargo builds into
# mac/build/cargo, not the repository's own target folder, so this never waits on (or
# disturbs) a build someone else is running in the same checkout.
#
# --committed builds the last commit instead of the files as they are on disk, for when
# someone is in the middle of changing the Rust code.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
mac="$(dirname "$here")"
repo="$(dirname "$mac")"
source="$repo"
if [ "${1:-}" = "--committed" ]; then
    shift
    source="$mac/build/zone-source"
    if [ -d "$source" ]; then mv "$source" "$source.previous.$$"; fi
    mkdir -p "$source"
    git -C "$repo" archive HEAD -- Cargo.toml Cargo.lock src assets | tar -x -C "$source"
    echo "Building zone as committed at $(git -C "$repo" rev-parse --short HEAD)"
fi
out="${1:-$mac/build/zone}"
targets=(aarch64-apple-darwin x86_64-apple-darwin)

command -v cargo >/dev/null || { echo "cargo is not installed. Get Rust from https://rustup.rs" >&2; exit 1; }

installed="$(rustup target list --installed 2>/dev/null || true)"
for target in "${targets[@]}"; do
    if ! grep -qx "$target" <<<"$installed"; then
        echo "Adding the Rust target $target"
        rustup target add "$target"
    fi
done

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$mac/build/cargo}"
# The app runs on macOS 14 and later, so the program inside it must too.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

slices=()
for target in "${targets[@]}"; do
    echo "Building zone for $target"
    cargo build --release --locked --manifest-path "$source/Cargo.toml" --target "$target"
    slices+=("$CARGO_TARGET_DIR/$target/release/zone")
done

mkdir -p "$(dirname "$out")"
# Written beside the result and moved into place, so a copy that is running is never
# overwritten (macOS kills a signed program whose file changes under it).
tmp="$out.tmp.$$"
lipo -create "${slices[@]}" -output "$tmp"
mv -f "$tmp" "$out"
echo "Universal zone: $out"
lipo -info "$out"
"$out" --version
