#!/usr/bin/env bash
# Build a portable Windows bundle: the application and both protocol helpers, in one directory.
#
# There is no installer yet, and this is deliberately not one. An installer has to answer where the
# helpers go, and `helper_path` looks for them beside the running executable -- so until that decision
# is made for `%PROGRAMFILES%`, one directory with everything in it is both correct and the whole of
# what is needed to run the thing.
#
# Two cargo workspaces, because IronRDP and russh cannot share a dependency graph; see
# helpers/rdp/Cargo.toml. That is why this exists rather than a single `cargo build`.
#
# NASM is required, by `aws-lc-sys`, for the release profile on Windows. Its absence surfaces as a
# failure deep inside a build script, so it is checked here where the message can say so.
#
# BESTTERM_TOOLCHAIN names a rustup toolchain, for a machine whose default host is not MSVC. On an
# ordinary Windows install with rustup the default already is, and this can be left unset.
set -euo pipefail

toolchain=()
if [ -n "${BESTTERM_TOOLCHAIN:-}" ]; then
    toolchain=("+${BESTTERM_TOOLCHAIN}")
fi

cd "$(dirname "$0")/.."

if ! command -v nasm >/dev/null 2>&1; then
    echo "nasm is not on PATH. aws-lc-sys needs it to assemble its crypto primitives for" >&2
    echo "x86_64-pc-windows-msvc, and without it the release build fails inside a build script." >&2
    echo "Install NASM 2.16 or later and put nasm.exe on PATH." >&2
    exit 1
fi

version=$(grep -m1 '^version' Cargo.toml | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
bundle="dist/BestTerm"
archive="dist/BestTerm-${version}-windows-x64-portable.zip"

echo "== building the application =="
cargo "${toolchain[@]}" build --workspace --release

echo "== building the RDP helper (its own workspace) =="
cargo "${toolchain[@]}" build --manifest-path helpers/rdp/Cargo.toml --workspace --release

echo "== assembling ${bundle} =="
rm -rf "$bundle" "$archive"
mkdir -p "$bundle"
cp target/release/bestterm.exe "$bundle/"
cp target/release/bestterm-vnc.exe "$bundle/"
cp helpers/rdp/target/release/bestterm-rdp.exe "$bundle/"
cp LICENSE "$bundle/"
cp docs/portable-readme.txt "$bundle/README.txt"

echo "== ${archive} =="
powershell -NoProfile -Command "Compress-Archive -Path '${bundle}' -DestinationPath '${archive}' -Force"
sha256sum "$archive"
