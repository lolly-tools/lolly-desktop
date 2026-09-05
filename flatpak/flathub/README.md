# Flathub submission

The manifest here builds Lolly **entirely from source, offline**, which is what Flathub
requires. The manifest one directory up unpacks a prebuilt `.deb` instead - fine for the
bundles we hand out ourselves, but Flathub forbids prebuilt binaries, so the two cannot
be the same file.

Keep the two in agreement on **app id, runtime version, `command`, `finish-args`** and
the freedesktop metadata they install. Only the build strategy should differ.

## Files

| File | Role |
|---|---|
| `tools.lolly.Desktop.yml` | The manifest. Filename must equal the app id. |
| `cargo-sources.json` | **Generated.** ~600 crates. |
| `node-sources.json` | **Generated.** Both npm lockfiles, merged. |

## Regenerating the sources

Needed whenever `Cargo.lock` or either `package-lock.json` changes.

```bash
pip install tomlkit aiohttp 'PyYAML>=6.0.2'
git clone https://github.com/flatpak/flatpak-builder-tools

# cargo
python3 flatpak-builder-tools/cargo/flatpak-cargo-generator.py \
  shells/tauri-desktop/src-tauri/Cargo.lock -o cargo-sources.json

# npm - TWO lockfiles. The desktop shell is deliberately not a workspace member, so
# its deps are not in the root lockfile. Generate both and merge.
pip install ./flatpak-builder-tools/node
flatpak-node-generator npm package-lock.json \
  -o /tmp/node-root.json --node-sdk-extension node24
cd shells/tauri-desktop && flatpak-node-generator npm package-lock.json \
  -o /tmp/node-desktop.json --node-sdk-extension node24
```

Merge the two JSON arrays with the repository helper. It deduplicates identical
destinations and fails if two sources would write different content to one path:

```bash
node scripts/merge-flatpak-node-sources.ts \
  /tmp/node-root.json /tmp/node-desktop.json \
  shells/tauri-desktop/flatpak/flathub/node-sources.json
```

## Building and linting locally

```bash
flatpak install -y flathub org.flatpak.Builder
flatpak run org.flatpak.Builder --force-clean --install --user \
  build-dir tools.lolly.Desktop.yml
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest tools.lolly.Desktop.yml
```

The build is heavy - ~700 npm packages, a Vite build, and ~600 crates including a
statically linked ONNX Runtime. Budget accordingly.

## The commit pin lags by one

`sources[0].commit` points at a commit of this repo. Because the manifest lives *in*
the repo it pins, the pin necessarily refers to the **previous** commit. That is fine:
the build never reads the manifest from the checkout, only the source. On Flathub the
manifest lives in the `flathub/tools.lolly.Desktop` repo instead, so the cycle
disappears entirely - but **the pin must be bumped to a pushed commit** before any
build, or you are testing stale source.

`brands/suse` is `update = none` in `.gitmodules`, so git skips that private pack and a
Flathub builder resolves to the public `lolly-start` profile. Do not "fix" this.

## Submitting

1. Fork [flathub/flathub](https://github.com/flathub/flathub), **unchecking** "Copy the
   master branch only".
2. `git clone --branch=new-pr git@github.com:<you>/flathub.git`
3. `git checkout -b lolly new-pr`
4. Copy `tools.lolly.Desktop.yml`, `cargo-sources.json` and `node-sources.json` in.
5. PR against **`new-pr`** (never `master`), titled `Add tools.lolly.Desktop`.
6. A reviewer will run `bot, build`. Push fixes to the same PR.
7. On merge you get a repo under the Flathub org and a write invite - **accept within a
   week, with 2FA enabled**. Publishes 1-2 hours after merge.

## Still outstanding

- **Screenshots.** The metainfo has none. Flathub expects at least one, and the store
  listing looks broken without them.
- **GNOME 50.** The linter suggests it (warning, not an error). Bump both manifests and
  the CI container tag together.
