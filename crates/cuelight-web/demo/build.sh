#!/bin/sh
# Build the demo page: the player compiled to WebAssembly plus show folders
# with their manifests, all under demo/. Serve it with any static file
# server, e.g. `python3 -m http.server -d crates/cuelight-web/demo`.
#
#   demo/build.sh [show folder]...
#
# Without arguments the bundled beacon show is used. Needs the wasm32
# target (`rustup target add wasm32-unknown-unknown`) and a wasm-bindgen
# CLI of the version in Cargo.lock (`cargo install wasm-bindgen-cli`).
#
# This is a plain build, kept to two tools. A site that ships the player
# will want a smaller download: build with a size-tuned cargo profile
# (opt-level "s" or "z", lto, codegen-units = 1), run wasm-opt (binaryen)
# over the .wasm, and serve it compressed.
set -eu

demo=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$demo/../../.." && pwd)
[ $# -gt 0 ] || set -- "$root/crates/cuelight/examples/shows/beacon"

cargo build --manifest-path "$root/Cargo.toml" -p cuelight-web \
    --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir "$demo/pkg" \
    "$root/target/wasm32-unknown-unknown/release/cuelight_web.wasm"

rm -rf "$demo/shows"
mkdir -p "$demo/shows"
names=
for show in "$@"; do
    name=$(basename "$show")
    cp -r "$show" "$demo/shows/$name"
    names="$names\"$name\","
done
cargo run --manifest-path "$root/Cargo.toml" -q -p cuelight-loader \
    --bin cuelight-manifest -- "$demo"/shows/*
echo "[${names%,}]" > "$demo/shows/index.json"
