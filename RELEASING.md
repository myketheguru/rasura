# Releasing

Two registries, seven crates and one npm package. Everything below has been
dry-run except the two commands that need credentials, which are marked.

## 0.1.2, and why it is not optional

**Every font embedded by 0.1.0 and 0.1.1 has an out-of-order sfnt table
directory.** OpenType requires directory entries sorted by tag, and readers take
it at its word: `read-fonts`, HarfBuzz and FreeType's fast path binary-search it.
Subsetting drops `cmap` and the writer adds one back, so `cmap` was written last
in every composed document those versions produced. An unsorted directory does
not fail to parse, it fails to *find* the table, so the symptom is text shaping
to notdef in someone else's reader with no error anywhere.

That is a defect in shipped output rather than a dependency refresh, which is
what makes this a release worth pushing rather than one that can wait. Documents
already written by 0.1.0 or 0.1.1 are not repaired by upgrading; they have to be
composed again.

Also in 0.1.2: the shaper moved from `rustybuzz` to `harfrust`, which clears
RUSTSEC-2026-0192 and -0206 and removes `ttf-parser`. Anyone running
`cargo audit` against 0.1.1 sees two advisories and after this sees none.

## Before anything

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
./crates/rasura-wasm/build.sh
cd js && npm test && cd ..
```

All of these run in CI on every push, so a green build on `main` is the same
evidence. Run them anyway before a release: CI checks the commit, and a release
is a claim about a moment.

## crates.io

The crates must go up in dependency order, because each one's manifest names the
version of the one below it and cargo will not accept a dependency that does not
exist yet. There is no way to publish them together.

```bash
cargo login            # needs your token, once per machine
```

Then, in this order, waiting for each to appear in the index before the next.
**crates.io rate-limits new crates**, so expect to be stopped partway through
and told when to resume; five went up before the limit hit on the first release.

```bash
cargo publish -p rasura-cos
cargo publish -p rasura-content
cargo publish -p rasura-font
cargo publish -p rasura-layout
cargo publish -p rasura-edit
cargo publish -p rasura-flow
cargo publish -p rasura
```

`rasura-wasm` is deliberately not published. It is a build artefact for the npm
package rather than a library anyone should depend on, and publishing it would
invite exactly that.

Only `rasura-cos` can be verified in advance, for the same reason the order
exists: the others cannot resolve their dependencies until the ones below them
are up. `cargo package -p rasura-cos --no-verify` passes and produces an 18-file,
96 KB tarball.

### If a publish is rejected

A published version can never be replaced, only yanked, and a yanked version
still cannot be reused. If `rasura-cos` goes up and `rasura-content` is then
rejected, fix the problem and bump **every** crate to the next patch rather than
trying to reuse the version that partly published. The version is declared once
in the workspace manifest, so the bump is one line plus the six dependency
entries under `[workspace.dependencies]`.

## npm

```bash
npm login
npm publish ./js --access public --otp=123456   # a live authenticator code
```

npm requires two-factor authentication to publish and there is no way around it
from a script: without `--otp` it returns 403 before uploading anything. For CI,
create a granular access token with "bypass 2FA" enabled and use that instead.

The tarball is 481 KB packed, 1.17 MB unpacked, 11 files. Verified by packing it,
installing into an empty directory with `--ignore-scripts`, and editing a PDF
with the result. That check runs in CI too.

**Build the module first.** `js/wasm/` is a build output and is gitignored, so a
publish from a clean checkout without `./crates/rasura-wasm/build.sh` ships a
package whose `files` list points at nothing. `npm pack --dry-run` shows 11
files when it is right.

## After

- Tag the commit: `git tag v0.1.2 && git push --tags`
- The Pages site deploys from `main` automatically and needs nothing.
- Check the registries agree with what you meant to publish:

```bash
cargo search rasura --limit 8
npm view rasura version
```

## Names

`rasura` was free on both registries when this was written. The previous name for
this project, `palimpsest`, was taken on crates.io by an unrelated tool three
weeks before anyone looked. Claiming a name converts it from available to yours,
and a placeholder `0.0.0` costs nothing if a full release is not ready.
