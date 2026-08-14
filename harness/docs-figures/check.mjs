// Do the numbers in the report still match the artefacts?
//
//   node harness/docs-figures/check.mjs
//
// Three revisions running, a figure was updated in one place and left stale in
// another: 1,030 against 1,026 corpus files, then 419 against 448.8 KB in three
// separate paragraphs, then a section saying there was no documentation site
// while another struck that row through as done. Every one was caught by a
// reader rather than by us, and a report whose whole value is that it does not
// round things off cannot keep doing that.
//
// This does not check that the figures are *right*. It checks that a figure
// appearing more than once appears with the same value, and that figures with a
// measurable source match the measurement. The rest is still a human's job.

import { existsSync, readFileSync, statSync } from 'node:fs'
import { gzipSync } from 'node:zlib'

const report = readFileSync('docs/report.md', 'utf8')
let failures = 0

const check = (label, ok, detail = '') => {
  if (ok) console.log(`  ok    ${label}`)
  else {
    console.error(`  FAIL  ${label}${detail ? ` -- ${detail}` : ''}`)
    failures += 1
  }
}

/** Every distinct value a pattern with one capture group matches. */
function values(pattern) {
  return [...new Set([...report.matchAll(pattern)].map((m) => m[1]))]
}

// --- figures with a measurable source ---------------------------------------
//
// Only these. A first version also asserted that any figure appearing twice
// appeared with the same value, which was wrong: "123 KB gzip" is the object
// layer alone, "628 corpus files" is the subset frame inference ran over, and
// "41 tests" is the JavaScript suite. Context makes them legitimately
// different, and a check that cannot tell the difference between a
// contradiction and a distinction trains its reader to ignore it.
//
// What is checkable is a figure with an artefact behind it, and a claim that
// contradicts another claim by name. Both of those have caught real problems.

const gzipFigures = values(/\*\*([\d,]+\.?\d*) KB\*\*/g)

const wasm = 'target/pkg/web/rasura_wasm_bg.wasm'
if (existsSync(wasm)) {
  const gz = gzipSync(readFileSync(wasm), { level: 9 }).length / 1024
  const claimed = Number((gzipFigures[0] ?? '0').replace(/,/g, ''))
  check(
    `the report's gzip figure matches the built module (${gz.toFixed(1)} KB)`,
    Math.abs(gz - claimed) < 5,
    `report says ${claimed}, module is ${gz.toFixed(1)}`,
  )

  const raw = statSync(wasm).size / 1024
  const rawClaimed = Number((values(/\| ([\d,]+\.\d) KB \|/g)[0] ?? '0').replace(/,/g, ''))
  check(
    `the report's raw figure matches the built module (${raw.toFixed(1)} KB)`,
    Math.abs(raw - rawClaimed) < 5,
    `report says ${rawClaimed}, module is ${raw.toFixed(1)}`,
  )
} else {
  console.log(`  skip  module sizes -- ${wasm} not built`)
}

// --- claims that contradict each other --------------------------------------
//
// Prose and tables drift apart. These are the pairs that have actually done so.

const contradictions = [
  {
    label: 'the documentation site is not described as missing',
    bad: /no (?:tutorial|API reference) site/i,
  },
  {
    label: 'rustybuzz is not described as having no replacement',
    bad: /no maintained pure-Rust replacement/i,
  },
  {
    label: 'the package is not described as unpublished',
    bad: /\*\*Nothing is published\.\*\*/,
  },
]

for (const { label, bad } of contradictions) {
  const hit = report.match(bad)
  check(label, !hit, hit ? `found ${JSON.stringify(hit[0])}` : '')
}

console.log(failures === 0 ? '\nthe report agrees with itself.' : `\n${failures} problem(s)`)
process.exit(failures === 0 ? 0 : 1)
