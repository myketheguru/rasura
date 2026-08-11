// Measure the WASM bundle and fail on regression. Spec 12.3:
//
//   "Enforce budgets in CI with a size-limit gate that fails the build on
//    regression."
//
//   node harness/wasm-size/measure.mjs           # measure and gate
//   node harness/wasm-size/measure.mjs --json    # machine-readable
//
// Expects the probes to have been built and wasm-opt'd already; see
// harness/wasm-size/build.sh.

import { readFileSync, existsSync } from "node:fs";
import { gzipSync, brotliCompressSync } from "node:zlib";

// A path argument measures one artefact against the `core` budget instead of
// the four probes. That is what `crates/rasura-wasm/build.sh` passes: the
// probes measure a *floor* — what the layers cost with no API on top — and the
// shipped module is the number the budget is actually about.
const explicit = process.argv.slice(2).find((a) => !a.startsWith("--"));
if (explicit) {
  const bytes = readFileSync(explicit);
  const gzip = gzipSync(bytes, { level: 9 }).length;
  const budget = 900 * 1024;
  const kb = (n) => `${(n / 1024).toFixed(1)} KB`;
  console.log(
    `${explicit}\n  raw ${kb(bytes.length)}, gzip ${kb(gzip)}, ` +
      `brotli ${kb(brotliCompressSync(bytes).length)}\n` +
      `  budget ${kb(budget)} -- ${gzip > budget ? "OVER" : "ok"}, ` +
      `${((100 * gzip) / budget).toFixed(1)}% used`,
  );
  if (gzip > budget) {
    console.error(`\nBUDGET EXCEEDED: ${kb(gzip)} gzipped, ceiling ${kb(budget)}`);
    process.exitCode = 1;
  }
  process.exit(process.exitCode ?? 0);
}

// Spec 12.3 budgets the `core` chunk (cos + content + layout, and font beneath
// them) at 900 KB gzipped. That chunk now exists, so `core` is gated on the
// real budget rather than a stand-in.
//
// The three cos-only variants keep their own tighter ceilings. They are not
// approximations of the real budget: they exist to catch a regression in the
// object layer, which 900 KB would never notice.
const BUDGETS = {
  "open-only": 130 * 1024,
  "read-only": 130 * 1024,
  full: 180 * 1024,
  // Spec 12.3's actual number. Measured at 413 KB with rustybuzz, ttf-parser,
  // a Unicode script table and the generated AGL and metrics tables all in.
  core: 900 * 1024,
};

const CORE_BUDGET = 900 * 1024;

const variants = ["open-only", "read-only", "full", "core"];
const results = [];

for (const v of variants) {
  const path = `target/probe-${v}-oz.wasm`;
  if (!existsSync(path)) {
    console.error(`missing ${path} -- run harness/wasm-size/build.sh first`);
    process.exitCode = 1;
    continue;
  }
  const bytes = readFileSync(path);
  results.push({
    variant: v,
    raw: bytes.length,
    gzip: gzipSync(bytes, { level: 9 }).length,
    brotli: brotliCompressSync(bytes).length,
    budget: BUDGETS[v],
  });
}

if (process.argv.includes("--json")) {
  console.log(JSON.stringify(results, null, 2));
} else {
  const kb = (n) => `${(n / 1024).toFixed(1)} KB`;
  console.log("=== spec 18 Q6: WASM bundle floor ===\n");
  console.log(
    `${"variant".padEnd(12)}${"raw".padStart(11)}${"gzip".padStart(11)}` +
      `${"brotli".padStart(11)}${"budget".padStart(11)}${"  status"}`,
  );
  for (const r of results) {
    const over = r.gzip > r.budget;
    console.log(
      r.variant.padEnd(12) +
        kb(r.raw).padStart(11) +
        kb(r.gzip).padStart(11) +
        kb(r.brotli).padStart(11) +
        kb(r.budget).padStart(11) +
        (over ? "  OVER" : "  ok"),
    );
  }
  const full = results.find((r) => r.variant === "full");
  if (full) {
    const share = (100 * full.gzip) / CORE_BUDGET;
    console.log(
      `\nrasura-cos is ${share.toFixed(1)}% of the 900 KB 'core' budget ` +
        `(cos + content + layout).\n` +
        `Headroom for the content and layout layers: ${kb(CORE_BUDGET - full.gzip)}.`,
    );
  }
}

const over = results.filter((r) => r.gzip > r.budget);
if (over.length) {
  for (const r of over) {
    console.error(
      `\nBUDGET EXCEEDED: ${r.variant} is ${(r.gzip / 1024).toFixed(1)} KB gzipped, ` +
        `ceiling is ${(r.budget / 1024).toFixed(1)} KB`,
    );
  }
  process.exitCode = 1;
}
