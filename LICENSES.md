# Licenses

decanter's own code is GPL-3.0-or-later. The report server in `crates/reports-server` is AGPL-3.0-or-later. Every
other component keeps the terms of its own authors. A release is a collection of components with these terms, not
one licence applied to every file. [LICENSES.json](LICENSES.json) lists the components machine-readably, and
`python3 scripts/licenses.py check` verifies that it matches the tree.

## Components

| Component | Terms and source |
| --- | --- |
| decanter application and its crates (`app`, `crates/amazon`, `crates/core`, `crates/epic`, `crates/galaxy`, `crates/gog`, `crates/mods`, `crates/modpack`, `crates/nix`, `crates/ps`, `crates/runtime`, `crates/report`, `crates/thunderstore`, `crates/nexus`, `crates/modio`), scripts and tools | GPL-3.0-or-later; root `LICENSE`. The GOG web API client in `crates/gog/src/api` began as `gog` 0.5.0 (gogapi-rs) by nicohman, GPL-3.0 (`https://git.sr.ht/~nicohman/gogapi-rs`), and holds none of its code any more: see `crates/gog/src/api/VENDOR.md` |
| Galaxy64.dll achievement proxy (`crates/hook`) | GPL-3.0-or-later; `crates/hook/Cargo.toml`. A Windows DLL built from this crate and embedded in decanter; it runs inside the Wine or Proton prefix of a GOG game |
| GalaxyCommunication stand-in service (`crates/galaxy-service`) | GPL-3.0-or-later; `crates/galaxy-service/Cargo.toml`. A dependency-free Windows executable built from this crate and embedded in decanter; it runs inside the Wine or Proton prefix of a game that uses the Galaxy SDK |
| Report server (`crates/reports-server`) | AGPL-3.0-or-later; `crates/reports-server/LICENSE`. A separate binary with its own workspace; it links s3db (AGPL-3.0-or-later) and `decanter-report` |
| Inter, JetBrains Mono and Oswald fonts (`app/assets/fonts`) | SIL Open Font License 1.1; copyright lines in LICENSES.json |
| Twemoji bitmaps (`app/assets/emoji`) | CC-BY-4.0; Copyright Twitter, Inc and other contributors |
| Bloodborne patch definitions (`crates/ps/data/bloodborne/Bloodborne.xml`) | No licence stated by their authors (Kyo, auser1337, Lance McDonald, illusion, emoose and others, named in the file's Author attributes); each keeps their rights and the file is redistributed as it is with their names |
| Galaxy protocol definitions (`crates/galaxy/proto`) | CC0-1.0; four `.proto` files copied verbatim from [Yepoleb/gog_protocols](https://github.com/Yepoleb/gog_protocols) at commit `923e8a0008603365d29002db611559190da5bf99`, with its `LICENSE`. Reverse-engineered by their authors, not GOG's; `crates/galaxy/build.rs` generates Rust from them |
| decanter logo and icon (`app/assets`) | GPL-3.0-or-later, assumed; the owner has not recorded a separate licence yet (review: pending) |
| Rust dependencies | Their own SPDX expressions, inventoried by `scripts/licenses.py inventory` |
| psport and the PS4 ports | Separate works, see below |

## Rust dependencies

About 850 crates build the decanter application and about 280 the report server. Almost all are MIT or Apache-2.0
(often both, so the recipient may take either), the rest Unicode-3.0, BSD-2/3-Clause, ISC, Zlib, CC0-1.0 and
similar permissive terms. Exceptions this review found:

- `gog` (gogapi-rs, GPL-3.0-only) is no longer a dependency nor part of the tree. It was incorporated as the `api`
  module of `decanter-gog` on 2026-10-29; its files were removed and the last four derived from it were rewritten
  clean-room from `docs/gog-api-behaviour-spec.md` (kb #1228). While they remained, the binaries were distributed under
  GPL-3.0-only terms (kb #1073); with nothing of it left, the whole application can be offered under GPL-3.0-or-later.
- `crates/gog/src/cloud_saves.rs` had been ported from heroic-gogdl (`gogdl/saves.py`, `gogdl/auth.py`), which declares
  only "GPL-3" (the GPLv3 text, no "or later" notice in any file: kb #1530). It was written again clean-room (kb #1525):
  one agent described the protocol and decanter's sync design in `docs/gog-cloud-saves-behaviour-spec.md`, and a
  different agent removed the file and wrote the replacement from that specification, decanter's own neighbouring files
  and the callers of its public items, without reading heroic-gogdl, the removed file or its history (commit
  `refactor(gog): rewrite cloud saves from a specification`). It carries `SPDX-License-Identifier: GPL-3.0-or-later`
  without a based-on line; what carried over is GOG's own interface (endpoints, header and JSON field names) and the
  module's public items, which the specification documents. While the port remained, the binaries that linked it,
  v0.4.0 among them, carried GPL-3.0-only terms for the combined work; they can now be offered under GPL-3.0-or-later.
- `crates/galaxy` adds prost, prost-derive, prost-types and prost-build (Apache-2.0), protox, protox-parse and
  prost-reflect (MIT OR Apache-2.0) and their build-time helpers (logos, logos-codegen, logos-derive, petgraph,
  fixedbitset, hashbrown, multimap, prettyplease, beef, unicode-width: MIT OR Apache-2.0; miette and miette-derive:
  Apache-2.0; foldhash: Zlib), plus sha2 and getrandom (MIT OR Apache-2.0) for the service's digests and per-process
  key. All are compatible with GPL-3.0-or-later (Apache-2.0 is one-way compatible with GPLv3).
- `crates/amazon` adds rsa (MIT OR Apache-2.0) and its RustCrypto helpers (pkcs1, pkcs8, spki, der, const-oid,
  pem-rfc7468, signature: Apache-2.0 OR MIT; num-bigint-dig 0.8: MIT OR Apache-2.0) to verify the Amazon Games
  manifest's signature, and lzma-rust2 (Apache-2.0) to decompress it (kb #1611). Both are pure Rust and compatible
  with GPL-3.0-or-later.
- `crates/epic` is written clean-room from the protocol facts of `docs/epic-api-behaviour-spec.md`, with no code of
  legendary-gl (GPL-3.0-or-later). The only values copied from legendary-gl 0.21.1 (`legendary/api/egs.py`) are the
  official Epic Games Launcher's OAuth client id and secret, Epic's own identifiers, pinned as constants (decision
  D3 of `docs/epic-legendary-integration-analysis.md`). It adds no dependency.
- `crates/mods` adds zip 9.1.3 (MIT) and sevenz-rust2 0.24.1 (Apache-2.0) to read mod archives (decision #1234),
  with deflate64 (MIT), ppmd-rust (CC0-1.0 OR MIT-0), typed-path (MIT OR Apache-2.0) and lzma-rust2 0.23 (Apache-2.0,
  beside the 0.25 crates/amazon uses). All are pure Rust and compatible with GPL-3.0-or-later. bzip2 support is left
  out because libbz2-rs-sys is `bzip2-1.0.6`, a licence the inventory's allow-list does not hold.
- `crates/modpack` holds the mod pack format, its validator and resolver from
  `docs/mod-packs-and-search-design.md` and adds no dependency: serde, thiserror, toml and uuid are already in the
  workspace.
- `crates/nexus` reads Nexus Mods' v1 API with the user's own key from the facts of `docs/mod-platforms-analysis.md`
  and adds no dependency: md5, reqwest, serde, serde_json, sha2, tokio, toml and url are already in the workspace.
- `crates/thunderstore` reads Thunderstore's public API from the facts of `docs/mod-platforms-analysis.md` and adds
  no dependency: reqwest, serde, serde_json, sha2, tokio, toml and url are already in the workspace.
- `crates/modio` reads mod.io's REST API v1 with the user's own key from the facts of
  `docs/mod-platforms-analysis.md` and adds no dependency: md5, reqwest, serde, serde_json, sha2, tokio, toml and
  url are already in the workspace.
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

decanter does not link or ship the programs it runs. Wine/Proton (through umu-run), psport, AnyPS5, Raven and any
other runtime are separate programs started as their own processes, built or downloaded on the user's machine. They
keep their own terms: psport is GPL-2.0-only (https://github.com/yerasuu/psport, its own LICENSES.md) and Raven, built
from a pinned commit only when the user asks for it in its section, is GPL-3.0-or-later
(https://github.com/mirkobrombin/raven), with the guest components its build compiles from its own MIT sources;
decanter neither relicenses them nor includes their source or binaries in a release.

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
