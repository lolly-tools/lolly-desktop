# lolly-desktop

The Tauri 2 desktop app (macOS, Windows, Linux, plus a Flatpak manifest under `flatpak/`).

## Read this first: there is no `src/` here

**This directory contains no application code.** The app *is* the web shell. `vite.config.js` sets `root` to `../web`, so `vite` builds `shells/web/index.html` and `shells/web/src/main.ts` exactly as the PWA does, and then substitutes four modules at build time.

Everything in this directory is therefore one of four things:

| Path | What it is |
|---|---|
| `vite.config.js` | The substitution mechanism, plus dev-server middleware for `/tools/` and `/catalog/` |
| `bridge-overrides/*.ts` | The four replacement modules |
| `src-tauri/` | The Rust side: the Tauri app, its permissions and native page capture |
| `package.json`, `flatpak/`, `dist/` | Scripts, packaging, build output |

If you are looking for a view, a style or an input control, it is in [`shells/web/src/`](../web/src/README.md). If you change something there, it changes here too.

Own repo `lolly-desktop`, mounted in the umbrella [`lolly`](https://github.com/lolly-tools/lolly) as a git submodule at `shells/tauri-desktop/`. See the [submodule caveat](#submodule-caveat).

## Entry point

Two of them, in sequence.

The **native** entry is `src-tauri/src/main.rs`, which calls `run()` in `src-tauri/src/lib.rs`. That builds the Tauri app, registers the `fs`, `shell` and `http` plugins, registers two invoke handlers (`capture::capture_page` and `capture::capture_page_pdf`) and runs.

The **frontend** entry is the web shell's, `shells/web/index.html` → `/src/main.js` → `shells/web/src/main.ts`. `src-tauri/tauri.conf.json` points `devUrl` at `http://localhost:5173` and `frontendDist` at `../dist`, and its `beforeDevCommand` and `beforeBuildCommand` run this package's `dev:frontend` and `build:frontend`, both plain `vite`.

## How the bridge gets composed: build-time module substitution

This is the single most confusing thing about this directory, and until now it was explained only inside the override files themselves.

The web shell's `src/bridge/index.ts` composes the host from **relative sibling imports**: `./state.ts`, `./capture.ts`, `./capabilities-provided.ts`, `./export.ts`. The `overrideBridgeModules` plugin in `vite.config.js` is a `resolveId` hook with `enforce: 'pre'` that intercepts those specifiers and returns a path in `bridge-overrides/` instead. The bridge index itself is unmodified and unaware.

Two details of that hook are hard-won and easy to break:

- It matches on the **extension-less basename** of the specifier, so it fires whether the bridge imports `./state.js` or `./state.ts`. An earlier version keyed on `.js`, and after the web shell's TypeScript migration every override silently stopped firing, so the desktop app shipped browser IndexedDB state and a throwing capture stub.
- It requires the **importer** to live in a `bridge/` directory. This is what makes it safe: it cannot be `resolve.alias`, because a path regex cannot match a relative specifier without also catching same-named files elsewhere in the tree. Matching on importer plus basename works for both the absolute filesystem importer that `vite build` passes and the root-relative URL importer the dev server passes.

### The four overrides, and why each exists

| Module | Replaced with | Why |
|---|---|---|
| `state` | `bridge-overrides/state.ts` | Filesystem state via `tauri-plugin-fs` instead of IndexedDB. Saved sessions become `$APPDATA/Lolly/saved-state/<slot>.json`. The API surface has to match the web original method for method, because nothing downstream knows which implementation is running, so a missing method crashes boot — as of the TS conversion that is enforced: `createFsStateAPI` returns the web module's own `WebStateAPI`, imported type-only, so a method added there and forgotten here fails `npm run typecheck` instead of a device boot. The logic (slot-name codec, legacy-filename migration, record shape, asset-ref collection) lives in `../tauri-shared/bridge-overrides/state-fs.ts`, shared with the mobile shell; this file is just the `tauri-plugin-fs` binding it is handed, which is also where desktop-only storage behaviour would go. It is an adapter rather than a plain import because the Tauri shells are not npm workspaces, so the parent repo cannot resolve `@tauri-apps/plugin-fs`. |
| `capture` | `bridge-overrides/capture.ts` | Real page capture instead of the web shell's throwing stub. `page(spec)` calls the native `capture_page` for a raster plus page geometry; `vector(spec)` calls `capture_page_pdf` and converts the vector PDF to a standalone SVG through the engine's PDF interpreter, then windows it so a vector shot frames identical content to a raster shot of the same spec. |
| `capabilities-provided` | `bridge-overrides/capabilities-provided.ts` | Declares a genuine superset: it spreads the web list, filters out `'screen'` and adds `'filesystem'` and `'capture'`. It spreads rather than re-lists so that a capability added on the web side can never silently go missing here and gate a tool off as "desktop only" on the desktop itself. `'screen'` is subtracted because display capture is `getDisplayMedia`, and wry's webviews do not grant it without the host app answering a permission delegate that this shell does not implement, so advertising it would un-grey the screen-capture tool and then fail at the tap. |
| `export` | `bridge-overrides/export.ts` | Delivery only. The web `download()` uses `URL.createObjectURL` plus an `<a download>` click. WKWebView hands that navigation to wry, which **cancels** it outright unless a native download handler is registered, and none is, so every desktop export was silently dropped. The override replaces `download` and `file` with a real save through `tauri-plugin-fs`, into a `Lolly` subfolder of the user's own Downloads, de-colliding rather than overwriting (`qr.png` → `qr (1).png`) because macOS `BaseDirectory.Download` is the shared directory. `render()` and the rasteriser are inherited unchanged. |

The `export` override does one thing worth calling out: it opens with `export * from '../../web/src/bridge/export.ts'`. The substitution replaces that module for **every** importer inside `bridge/`, not just the bridge index, so it has to carry the original's whole public surface or a sibling such as `export-pptx.ts` fails the build. The star re-export forwards live bindings, which matters because the web module assigns an `export let _host`, and the local `createExportAPI` shadows the starred one per ES module semantics.

The mobile shell uses the same mechanism with a different set of three overrides. Compare [`../tauri-mobile/README.md`](../tauri-mobile/README.md) before you change a shared pattern.

### `vite.config.js` also carries two other plugins

- **`jsToTsFallback`** maps a missing `.js` specifier to its sibling `.ts`. The web shell's `index.html` still names `/src/main.js`, and it pins `vite@^8`, which resolves that implicitly. This shell pins `vite@^5`, which does not. The plugin only fires when the `.js` is genuinely absent and the `.ts` exists, so it can never shadow a real `.js`.
- **`bundleRepoDirs`** serves `/tools/` and `/catalog/` from the repo root in dev, and copies them into `dist/` on build with `dereference: true`, because those paths are symlink farms built by `scripts/use-profile.ts` and the WebView needs real files.

`build.target` and `optimizeDeps.esbuildOptions.target` are both `esnext` because harfbuzzjs, the text-to-path WASM, uses top-level await, which the default `es2020` target rejects.

## The Rust side

`src-tauri/` is small and does exactly two jobs: host the WebView, and fulfil the `capture` capability.

- `Cargo.toml` declares `lolly-desktop`, edition 2021, with `tauri`, `tauri-plugin-fs`, `tauri-plugin-shell`, `tauri-plugin-http`, `serde`, `serde_json` and **`headless_chrome`**.
- `src/main.rs` (5 lines) and `src/lib.rs` (15 lines): plugin registration and the invoke handler list, nothing else.
- `src/capture.rs` (312 lines): the only substantial Rust in the project. It drives a **headless Chrome over the DevTools Protocol**, deliberately not the app's own WKWebView or WebView2, because Tauri 2 has no stable API for screenshotting arbitrary content with viewport and scroll control. `capture_page` uses `Page.captureScreenshot`, and its clip rect is **document-space** when `captureBeyondViewport` is true, so scroll depth resolves into `clip.y` rather than into a `window.scrollTo`. An earlier version scrolled and then clipped at `y = 0`, which silently framed the page top at every depth. `capture_page_pdf` uses `Page.printToPDF` under `screen` media emulation for a true vector print. Both run on `spawn_blocking`, because `headless_chrome` is blocking. Both require a Chrome or Chromium install. Only non-http(s) schemes are rejected: capturing localhost or a private dev server is a feature here, because the user runs the tool on their own machine.
- `src-tauri/capabilities/default.json` holds the Tauri permission set. Notably `fs:scope-appdata-recursive` plus `fs:scope-download-recursive`, `shell:allow-open`, and `http:default` scoped to `https://*:*`.

## Run it

```bash
npm run dev            # tauri dev, which starts vite via beforeDevCommand, then the app
npm run dev:frontend   # just vite, in a browser, with the desktop overrides active
```

`dev:frontend` is genuinely useful: it lets you exercise the override modules without a Rust build, though `capture` will fail because there is no `invoke` host.

## Build it

```bash
npm run build          # build:frontend (vite) then tauri build
npm run build:frontend # frontend only, into ./dist
```

Requires a Rust toolchain. Note that `dist/` here is this shell's own output, distinct from `shells/web/dist`.

`tsconfig.json` here typechecks `bridge-overrides/` only — the frontend is covered by `tsc -p shells/web`. It is reached from the umbrella's `npm run typecheck` through `scripts/typecheck-tauri.ts` rather than as a bare `tsc -p` step, because the overrides import `@tauri-apps/api` and `@tauri-apps/plugin-fs` and **this shell is not an npm workspace**, so a root `npm ci` never creates its `node_modules`. That script SKIPS with a logged reason when they are absent, so a plain clone is not punished; CI installs both Tauri shells (`--omit=dev`) and then re-runs it with `--strict`, which fails on a skip, so the gate cannot quietly become a no-op. To run it locally:

```bash
npm --prefix shells/tauri-desktop ci --omit=dev   # once
npm run typecheck:tauri
```

## Surprising things

- Everything under [How the bridge gets composed](#how-the-bridge-gets-composed-build-time-module-substitution), which is the whole point of this section existing.
- **A state-override file name must not begin with a dot.** `tauri-plugin-fs` defaults `require_literal_leading_dot` to `cfg!(unix)`, true on macOS, so the `$APPDATA/**` glob behind `fs:scope-appdata-recursive` cannot match a dotfile and every access to one is rejected as a forbidden path. As `.slotname-v1` the migration marker's rejection propagated out and failed every state call, so the app could not boot. It is `slotname-v1.marker` now.
- `vite.config.js` is still `.js` while `bridge-overrides/` is now `.ts`. The Vite config is a build-tool file (Biome excludes `**/*.config.js` repo-wide) and is not part of the shipped app; the overrides are.
- The desktop app cannot do screen capture even though it is native. See the `'screen'` subtraction above.

## Submodule caveat

This shell builds **inside the umbrella repo** and nowhere else, more strictly than any other. Its Vite root is `../web`, its overrides import `../../web/src/bridge/…`, it resolves `@lolly/engine` and `@tauri-apps/*` through the umbrella's workspaces and its own `package-lock.json`, and it copies the repo-root `tools/` and `catalog/` profile views into `dist/`. A standalone clone of `lolly-desktop` builds nothing at all.

```bash
git clone --recurse-submodules https://github.com/lolly-tools/lolly.git
# or, in an existing clone, BEFORE npm install:
git submodule update --init --recursive
```

Commit changes to files in this directory in the `lolly-desktop` repo, then commit the moved pointer in the umbrella. See [`CONTRIBUTING.md`](../../CONTRIBUTING.md) §4.
