# AUR packaging: lolly-desktop-bin

Arch recipe for the official Lolly desktop binary. `PKGBUILD` repacks the
canonical `.deb` from lolli.li (a from-source Tauri build pulls ~500 crates -
not worth imposing on users when the official artifact exists); the deb's
`data.tar.gz` already contains every plans/174 integration file (mime XML,
thumbnailer, search-provider ini, D-Bus services, KDE service menu, icons),
so `package()` is a straight `bsdtar` extraction into `$pkgdir`.

## Caveat: no Arch box here yet

This recipe is syntax-validated (`bash -n`, sourced in a clean env, .SRCINFO
consistency-checked against the PKGBUILD by `tests/linux-desktop-integration.test.ts`)
but its **first real `makepkg` run is still pending** - do that on an Arch
machine (or a clean chroot via `extra-devel`'s `pkgctl build` / `makechrootpkg`)
before the first AUR push, and fix anything it surfaces.

## Publishing to AUR (first time)

AUR uses per-package git repos over SSH; pushing to a non-existent repo
creates it and claims the name.

```bash
# once: register your SSH key at https://aur.archlinux.org (account settings)
git clone ssh://aur@aur.archlinux.org/lolly-desktop-bin.git
cp PKGBUILD .SRCINFO lolly-desktop-bin/
cd lolly-desktop-bin
git add PKGBUILD .SRCINFO
git commit -m "lolly-desktop-bin 1.0.1-1"
git push origin master        # AUR's default branch is master
```

AUR rejects a push whose .SRCINFO is missing or stale, so both files always
travel together.

## Installing (users)

```bash
paru -S lolly-desktop-bin     # or: yay -S lolly-desktop-bin
# or by hand:
git clone https://aur.archlinux.org/lolly-desktop-bin.git && cd lolly-desktop-bin && makepkg -si
```

## Update ritual (each release)

1. Bump `pkgver` in `PKGBUILD` (the source URL interpolates it); reset
   `pkgrel=1` (bump `pkgrel` only for packaging-only changes within a release).
2. Update `sha256sums` for the new deb: `sha256sum lolly-desktop-<ver>_amd64.deb`
   (or `updpkgsums` on Arch).
3. Regenerate `.SRCINFO`. On Arch: `makepkg --printsrcinfo > .SRCINFO`.
   Editing it by hand (as this first version was) works too, but every expanded
   value must match the PKGBUILD exactly - the contract test checks that.
4. Copy both files into the AUR clone, commit (`lolly-desktop-bin <ver>-1`),
   push.
5. Mirror any change back into this directory - this repo stays the source of
   truth, the AUR clone is a publish target.
