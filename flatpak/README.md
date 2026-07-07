# Lolly — Flatpak packaging

Builds the Tauri desktop app as a Linux [Flatpak](https://flatpak.org/). **This cannot
be built on macOS** — Flatpak is Linux-only and Tauri does not cross-compile. Use the
CI workflow (`.github/workflows/flatpak.yml`, runs on `v*` tags) or a Linux machine.

## How it works

Rather than compile Lolly inside the offline flatpak-builder sandbox (which would need
every cargo crate and npm package vendored as a source), we build a **`.deb` first** —
where the network and the webkit2gtk toolchain are available — and the manifest just
unpacks that prebuilt binary into `/app`.

```
tauri build --bundles deb  ──►  Lolly_x.y.z_amd64.deb  ──►  flatpak-builder  ──►  Lolly.flatpak
   (network + webkit deps)          (staged as lolly.deb)      (offline unpack)
```

## Files

| File | Role |
|---|---|
| `tools.lolly.desktop.yml` | flatpak-builder manifest (app id = the Tauri `identifier`) |
| `tools.lolly.desktop.desktop` | desktop entry (exported to the host menu) |
| `tools.lolly.desktop.metainfo.xml` | AppStream metadata (id must match the app id) |
| `icon-{32,128,256}.png` | hicolor icons, copied from `../src-tauri/icons/` |
| `lolly.deb` | **not committed** — the built package, staged here before building |

The app id `tools.lolly.desktop`, the runtime (`org.gnome.Platform//47`, which provides
the `webkit2gtk-4.1` Tauri needs), and the binary name (`lolly-desktop`, the Cargo
package name) all have to stay in agreement. If you set `mainBinaryName` in
`tauri.conf.json`, update `command:` and the `install` path in the manifest to match.

## Build it locally (on Linux)

```bash
# 0) system deps (Ubuntu 24.04+): libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev
#    libayatana-appindicator3-dev libsoup-3.0-dev build-essential

# 1) build the .deb
cd shells/tauri-desktop
npm ci && npm run build:frontend && npm run tauri -- build --bundles deb

# 2) stage it next to the manifest
cp src-tauri/target/release/bundle/deb/*.deb flatpak/lolly.deb

# 3) build + install the Flatpak
cd flatpak
flatpak install -y flathub org.gnome.Platform//47 org.gnome.Sdk//47
flatpak-builder --user --install --force-clean build-dir tools.lolly.desktop.yml

# 4) run it
flatpak run tools.lolly.desktop

# (optional) export a shippable single-file bundle
flatpak-builder --user --force-clean --repo=repo build-dir tools.lolly.desktop.yml
flatpak build-bundle repo Lolly.flatpak tools.lolly.desktop
```

## First-run things to verify

Because this can't be smoke-tested on macOS, watch these on the first CI/Linux run:

- **Runtime has webkit2gtk-4.1.** If the window is blank or the app won't start, the
  GNOME runtime version and the `WEBKIT_DISABLE_DMABUF_RENDERER=1` finish-arg are the
  first knobs — try bumping the runtime (and the CI container tag) together.
- **`.deb` data member is gzip.** The manifest uses `tar -xzf data.tar.gz`. Tauri's
  bundler gzips it; if a future version switches to xz/zst, adjust the flag.
- **AppStream compose passes.** If `appstreamcli compose` errors, the metainfo is the
  cause — a missing screenshot is only a warning, but a bad id/launchable is fatal.
