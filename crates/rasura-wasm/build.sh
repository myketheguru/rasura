#!/usr/bin/env bash
# Build the shipped WASM artefact with spec 12.3's flags, then check it runs.
#
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --locked
#   npm i -g wasm-opt
#   ./crates/rasura-wasm/build.sh
#
# Two bindgen targets come out of one .wasm:
#
#   pkg/web/   --target web    the thing that ships (§12.4: ESM primary)
#   pkg/node/  --target nodejs the thing CI drives, because a binding that
#                             compiles and cannot open a file passes every
#                             Rust test and fails this one
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

OUT=target/pkg
CRATE=rasura_wasm

# Same flags as the size probe: -C opt-level=z, lto = "fat", codegen-units = 1,
# panic = "abort", all set by the `wasm-release` profile.
echo "building $CRATE for wasm32"
cargo build --profile wasm-release --target wasm32-unknown-unknown -p rasura-wasm

RAW="target/wasm32-unknown-unknown/wasm-release/$CRATE.wasm"

for target in web nodejs; do
    dir="$OUT/$target"
    rm -rf "$dir"
    mkdir -p "$dir"
    echo "binding for $target"
    wasm-bindgen "$RAW" --out-dir "$dir" --target "$target" --omit-default-module-path
done

# wasm-opt after wasm-bindgen, not before: bindgen rewrites the module and
# would undo some of what -Oz did. The enable flags are not optional -- LLVM
# emits bulk-memory operations for wasm32 by default and binaryen rejects a
# module using them unless told they are allowed.
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

for target in web nodejs; do
    file="$OUT/$target/${CRATE}_bg.wasm"
    before=$(wc -c < "$file")
    wasm-opt "${OPT_FLAGS[@]}" "$file" -o "$file.opt"
    mv "$file.opt" "$file"
    after=$(wc -c < "$file")
    printf '%-8s %9d -> %9d bytes\n' "$target" "$before" "$after"
done

echo
echo "gzipped, against spec 12.3's 900 KB core budget"
node harness/wasm-size/measure.mjs "$OUT/web/${CRATE}_bg.wasm"

echo
echo "driving the real artefact from node"
node harness/wasm-size/api.mjs "$OUT/nodejs"

# Stage the asset into the npm package. Not committed — it is a build output,
# and `.gitignore` excludes `*.wasm` — but `package.json`'s `files` list ships
# it, which is what makes spec 12.4's "no postinstall scripts, no native build
# step" true for a consumer: the binary is in the tarball.
echo
echo "staging into js/wasm"
mkdir -p js/wasm
cp "$OUT/web/$CRATE.js" "$OUT/web/${CRATE}_bg.wasm" js/wasm/
ls -l js/wasm
