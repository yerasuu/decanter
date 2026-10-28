# Packaging decanter

A release is built from one tag. Which files it carries, and what each one has to carry for the licences, is in
[LICENSES.md](../LICENSES.md), section "Release contents". This page is the checklist.

## Before the tag

1. `python3 scripts/licenses.py check` inside `nix-shell`: the manifest matches the tree, every licence has its text
   in `LICENSES/`, and every Rust dependency has an allowed licence. It needs the crates in the local registry
   (`--no-deps` skips that part).
2. A new component (a vendored file, a font, an image, a crate that is not on the allow-list) is added to
   `LICENSES.json` and `LICENSES.md` in the same commit that brings it, with its licence text in `LICENSES/` when
   the licence is new. A licence outside the allow-list is an owner decision.
3. Items marked `"review": "pending"` in `LICENSES.json` are either resolved or called out in the release notes.

## What every package carries

- **AppImage, `.deb`, `.rpm` and pacman packages** (`scripts/appimage`, `scripts/packages`): the AppDir includes the
  libraries the binary links from the Nix store, glibc and its loader among them. `scripts/appimage` copies
  `LICENSE`, `LICENSES/`, `LICENSES.json` and `LICENSES.md` into `usr/share/licenses/decanter/` and writes
  `BUNDLED-LIBRARIES.txt` there: one line per bundled file with its store path, whose package name and version
  identify the licence and the source that apply. The packages install the whole AppDir, so they carry it too.
- **Windows** (`decanter.exe`, `decanter-cli.exe`, from the tag's CI run): the release page links `LICENSES.md` and
  the tag's source archive. Nothing third-party is bundled besides what Cargo links.
- **Source**: the tag's archive, with the pinned `Cargo.lock` and `shell.nix`. The Nix libraries in an AppDir are
  rebuildable from the nixpkgs revision that `shell.nix` resolves; name that revision in the release notes.

Publish the binaries and the source in the same release with equivalent access. A user needs only the binary to
run decanter.

## What is never in a release

psport, AnyPS5, Wine, Proton, umu-run and Raven. decanter builds or downloads them on the user's machine and runs
them as separate programs (see "What decanter is not" in LICENSES.md). The report server (`crates/reports-server`)
is released on its own, as AGPL-3.0-or-later, with the source of the version it runs.
