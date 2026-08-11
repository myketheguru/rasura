#!/usr/bin/env bash
# Build the WASM size probes with spec 12.3's flags, then measure.
#
#   rustup target add wasm32-unknown-unknown
#   npm i -g wasm-opt
#   ./harness/wasm-size/build.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# wasm-opt needs these enabled explicitly: LLVM emits bulk-memory operations
# (memory.copy) by default for wasm32, and binaryen rejects them otherwise.
OPT_FLAGS=(
    -Oz
    --strip-debug
    --strip-producers
    --enable-bulk-memory
    --enable-nontrapping-float-to-int
    --enable-sign-ext
    --enable-mutable-globals
    --enable-reference-types
)

# `core` is the variant spec 12.3's 900 KB budget actually applies to: cos +
# content + layout + font, which is everything a caller has before a lazy chunk
# loads. It is measured last so a regression in it is the final line of output.
for variant in open-only read-only full core; do
    echo "building $variant"
    cargo build --profile wasm-release --target wasm32-unknown-unknown \
        -p rasura-wasm-size --no-default-features --features "$variant"
    cp target/wasm32-unknown-unknown/wasm-release/rasura_wasm_size.wasm \
        "target/probe-$variant.wasm"
    wasm-opt "${OPT_FLAGS[@]}" "target/probe-$variant.wasm" -o "target/probe-$variant-oz.wasm"
done

echo
node harness/wasm-size/measure.mjs

echo
echo "verifying the module actually runs"
node harness/wasm-size/run.mjs target/probe-full-oz.wasm corpus/files
