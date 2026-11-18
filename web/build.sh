#!/bin/sh
# Builds the web page's program into docs/try/pkg/, which the page loads. Needs Rust's
# WebAssembly target and the wasm-bindgen tool of the version web/Cargo.toml names:
#
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.130 --locked

set -eu
cd "$(dirname "$0")/.."
cargo build --profile web --locked -p correctsound-web --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir docs/try/pkg \
    target/wasm32-unknown-unknown/web/correctsound_web.wasm
ls -l docs/try/pkg
