# Q6 — the bundle floor

Spec §18, question Q6:

> What is the smallest `core` chunk that can still parse and extract? If it
> exceeds 900 KB gzipped, the layout engine may need to become a third lazy
> chunk.

**Answer: `rasura-cos` is 123 KB gzipped — 13.6% of the budget. The module
split in §12.3 stands, and the layout engine does not need to become a third
chunk.**

Reproduce with:

```bash
rustup target add wasm32-unknown-unknown
npm i -g wasm-opt
./harness/wasm-size/build.sh
```

---

## Measurements

`wasm32-unknown-unknown`, `opt-level = "z"`, `lto = "fat"`,
`codegen-units = 1`, `panic = "abort"`, `strip = true`, then
`wasm-opt -Oz --strip-debug --strip-producers`.

| variant | raw | gzip | brotli |
|---|---:|---:|---:|
| open only | 185.2 KB | 90.0 KB | 77.1 KB |
| open + decode streams | 186.1 KB | 90.4 KB | 77.3 KB |
| **open + decode + save** | **259.1 KB** | **122.7 KB** | **103.2 KB** |

`core` is cos + content + layout and is budgeted at 900 KB gzipped, so **777 KB
of headroom** remains for the two layers that do not exist yet.

Two things worth noting from the spread:

- **Decoding streams is free.** Read-only costs 0.4 KB more than open-only,
  because opening already reaches the filter chain: xref streams and object
  streams are Flate-compressed, so `Document::open` pulls in the decoder
  whether or not the caller ever asks for a content stream.
- **The writer is 32 KB gzipped**, a quarter of the total. If a text-extraction
  build ever matters, dropping the save path is the single biggest lever. Not
  worth acting on now — it would mean a feature gate through the public API for
  32 KB — but it is the shape of a future `rasura/readonly` entry point.

## The module actually runs

A size measurement of a module that never executes is worthless: the linker
strips whatever the exports do not reach, so a probe that exercised nothing
would report a wonderfully small number.

`harness/wasm-size/run.mjs` instantiates the built module in node and runs it
over `corpus/files`: **16 of 16**, including both encrypted fixtures decrypting
correctly and every adversarial recovery path behaving as it does natively.

That is the first evidence that `rasura-cos` works on `wasm32` at all, which
matters beyond the size question — it means nothing in the object layer depends
on something the browser target lacks. No filesystem, no clock, no randomness.
The decision in §5.6 to derive `/ID[1]` from content rather than an RNG is part
of why.

## What is not in the number

- **wasm-bindgen glue.** This probe uses a plain `cdylib` with `extern "C"`
  exports, because the question is what the Rust side costs. The real
  `rasura-wasm` crate adds bindgen's glue on top — typically 10–30 KB, and
  measurable separately once that crate exists.
- **Further savings that were not taken.** `std` is linked, bringing dlmalloc
  and the formatting machinery `thiserror` needs. `-Z build-std` with
  `panic_immediate_abort` would cut more, at the cost of a nightly toolchain.
  There is no reason to spend that with 777 KB of headroom.

## The risk to re-measure

Content and layout are mostly logic, and logic compresses well. The thing that
could move this number is **table data**, and Q1 identified exactly one such
table: the **Adobe Glyph List**, which turned out to be the load-bearing
component of §7.2's Unicode-derivation chain.

The AGL is roughly 4,500 name-to-codepoint entries — order 30 KB gzipped if
stored naively, less with a sorted-string table and binary search. That belongs
in `core`, because Unicode derivation is layout's job, not the font engine's.
Even at the naive end it is 4% of the budget.

Script-property tables (§8.3) belong to the `fonts` chunk and do not land here.

Re-measure at the end of Phase 3, when content and layout exist and the AGL is
real. The CI gate below will catch a regression before then.

## The CI gate

Spec §12.3 asks for a size-limit gate that fails the build on regression, and
`harness/wasm-size/measure.mjs` is it. The ceilings are cos-specific, set with
headroom above the measured size:

| variant | ceiling (gzip) |
|---|---:|
| open only | 130 KB |
| read only | 130 KB |
| full | 180 KB |

Gating on the real 900 KB figure would pass no matter how badly cos regressed,
which is the failure mode the gate exists to prevent. These get replaced with
the real budget once `core` is complete.
