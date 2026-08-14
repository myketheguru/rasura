# Releasing

Two registries, seven crates and one npm package. Everything below has been
dry-run except the two commands that need credentials, which are marked.

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
rejected, fix the problem and bump **every** crate to `0.1.1` rather than trying
to reuse `0.1.0`. The version is declared once in the workspace manifest.

## npm

```bash
npm login
npm publish ./js --access public --otp=123456   # a live authenticator code
```

npm requires two-factor authentication to publish and there is no way around it
from a script: without `--otp` it returns 403 before uploading anything. For CI,
create a granular access token with "bypass 2FA" enabled and use that instead.

The tarball is 459 KB packed, 1.3 MB unpacked, 11 files. Verified by packing it,
installing into an empty directory with `--ignore-scripts`, and editing a PDF
with the result. That check runs in CI too.

## After

- Tag the commit: `git tag v0.1.0 && git push --tags`
- The Pages site deploys from `main` automatically and needs nothing.

## Names

`rasura` was free on both registries when this was written. The previous name for
this project, `palimpsest`, was taken on crates.io by an unrelated tool three
weeks before anyone looked. Claiming a name converts it from available to yours,
and a placeholder `0.0.0` costs nothing if a full release is not ready.
