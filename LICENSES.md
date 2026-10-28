# Licenses

decanter's own code is GPL-3.0-or-later. The report server in `crates/reports-server` is AGPL-3.0-or-later. Every
other component keeps the terms of its own authors. A release is a collection of components with these terms, not
one licence applied to every file. [LICENSES.json](LICENSES.json) lists the components machine-readably, and
`python3 scripts/licenses.py check` verifies that it matches the tree.

## Components

| Component | Terms and source |
| --- | --- |
| decanter application and its crates (`app`, `crates/core`, `crates/gog`, `crates/nix`, `crates/ps`, `crates/runtime`, `crates/report`), scripts and tools | GPL-3.0-or-later; root `LICENSE` |
| Galaxy64.dll achievement proxy (`crates/hook`) | GPL-3.0-or-later; `crates/hook/Cargo.toml`. A Windows DLL built from this crate and embedded in decanter; it runs inside the Wine or Proton prefix of a GOG game |
| Report server (`crates/reports-server`) | AGPL-3.0-or-later; `crates/reports-server/LICENSE`. A separate binary with its own workspace; it links s3db (AGPL-3.0-or-later) and `decanter-report` |
| Inter, JetBrains Mono and Oswald fonts (`app/assets/fonts`) | SIL Open Font License 1.1; copyright lines in LICENSES.json |
| Twemoji bitmaps (`app/assets/emoji`) | CC-BY-4.0; Copyright Twitter, Inc and other contributors |
| decanter logo and icon (`app/assets`) | GPL-3.0-or-later, assumed; the owner has not recorded a separate licence yet (review: pending) |
| Rust dependencies | Their own SPDX expressions, inventoried by `scripts/licenses.py inventory` |
| psport and the PS4 ports | Separate works, see below |

## Rust dependencies

About 850 crates build the decanter application and about 280 the report server. Almost all are MIT or Apache-2.0
(often both, so the recipient may take either), the rest Unicode-3.0, BSD-2/3-Clause, ISC, Zlib, CC0-1.0 and
similar permissive terms. Exceptions this review found:

- `gog` 0.5.0 (`https://git.sr.ht/~nicohman/gogapi-rs`) declares **GPL-3.0** and is linked into `decanter-gog`.
  The combined decanter binaries are therefore distributed under GPL-3.0 terms; the "or later" option cannot be
  offered for the combination, although decanter's own files keep their notices. Confirm with the crate's author
  whether "GPL-3.0" means "only" before the next release.
- `cbindgen`, `dwrote` and `option-ext` are MPL-2.0, used unmodified; their notices travel with the source.
- `r-efi` offers LGPL-2.1-or-later as a third alternative; `self_cell` offers GPL-2.0-only as a second one. Neither
  alternative is taken: the MIT or Apache-2.0 option applies.
- `libfuzzer-sys` is `(MIT OR Apache-2.0) AND NCSA` and is used only by fuzz targets.
- `webpki-root-certs` ships Mozilla's root certificate list under CDLA-Permissive-2.0, a permissive data licence that
  requires its notice to be kept.

`scripts/licenses.py check` fails when a dependency's expression has no alternative made only of allowed licences
(MIT, MIT-0, Apache-2.0 with or without the LLVM exception, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0,
Unlicense, CC0-1.0, 0BSD, BSL-1.0, NCSA, MPL-2.0, CDLA-Permissive-2.0, GPL-3.0 and GPL-3.0-or-later; AGPL-3.0-or-later only in the report
server). A new licence is an owner decision: it is added to the allow-list in the script and recorded here.

## Attribution required

- Twemoji graphics: © Twitter, Inc and other contributors, licensed under CC-BY 4.0
  (<https://creativecommons.org/licenses/by/4.0/>). Changes: the two bitmaps used (fire, U+1F525; wastebasket,
  U+1F5D1) were extracted unmodified from `twemoji.ttf` as PNG. The application has no About or notices screen
  yet, so the attribution lives here, in `usr/share/licenses/decanter/` of every package and in LICENSES.json;
  a notices screen is a pending task.
- Fonts: the copyright notices embedded in each font file stay intact; the fonts are not sold by themselves.

## What decanter is not

decanter does not link or ship the programs it runs. Wine/Proton (through umu-run), psport, AnyPS5 and any other
runtime are separate programs started as their own processes, built or downloaded on the user's machine. They keep
their own terms: psport is GPL-2.0-only (https://github.com/yerasuu/psport, its own LICENSES.md); decanter neither
relicenses them nor includes their source or binaries in a release.

## Release contents

A release is built from one tag and consists of:

| File | What it carries |
| --- | --- |
| `decanter-VERSION-ARCH.AppImage` and the `.deb`, `.rpm` and pacman packages | The decanter binary and the libraries it links from the Nix store, **glibc and its loader included**, with `usr/share/licenses/decanter/` (LICENSE, LICENSES/, LICENSES.json, LICENSES.md and `BUNDLED-LIBRARIES.txt`, the store path of every bundled library) |
| `decanter.exe` and `decanter-cli.exe` (Windows, from the tag's CI run) | The Windows binaries; the release page links LICENSES.md and the matching source archive |
| Source (the tag's archive on GitHub) | The corresponding source of decanter and the pinned `Cargo.lock`; the bundled Nix libraries' source is named by their store paths and the pinned nixpkgs revision |

See [docs/packaging.md](docs/packaging.md) for how a release is assembled and checked.

License texts are in [LICENSES](LICENSES). File notices remain authoritative; a manifest does not replace them or
grant permission over third-party work.

- [GPLv3, corresponding source and section 6](https://www.gnu.org/licenses/gpl-3.0.html)
- [GNU license compatibility](https://www.gnu.org/licenses/license-compatibility.html)
- [SPDX license list](https://spdx.org/licenses/)
