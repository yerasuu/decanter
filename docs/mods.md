# Third-party mods

What is built of [third-party-mods-design.md](third-party-mods-design.md): phases 1 to 4 for GOG, Amazon Games and Epic Games
games: loose folders and zip or 7z archives, DLL overrides and load order, update checks and the uninstall dialog. The code is `crates/mods` (data, import, archives, play tree) and `app/src/mods.rs` (launch);
the Mods tab of the game's detail view is `app/src/views/library/detail/mods/`.

## Using it

- The game's detail view → Mods → *Añadir carpeta de mod...*: pick a folder laid out like the game's own folder (its files go
  to the same relative paths). The folder is copied in and never changed; the mod is turned on for the account
  that added it.
- *Añadir archivo comprimido...* (or an archive dropped on the tab): a zip or 7z. It is hashed and every entry
  checked first, with a progress line and *Cancelar*; then a preview shows its name (editable), file count and
  size, where its files go (the game's folder or one of its folders, preselected from the archive's top level; a
  single wrapping folder that matches nothing of the game is left out), the game's files it replaces, and whether
  it carries an installer (FOMOD or setup program), which is never run. Nothing is written before *Añadir*; the
  same archive added twice points at the mod it already is. RAR is refused with a line saying so.
- The checkbox turns a mod on or off for this account. Each mod keeps its slot in the load order on or off (a
  disabled mod stays in the list, greyed, with its number, and turning it on again puts it back there); a mod new
  to the account goes last and wins a file over the ones already on. ↑ and ↓, or dragging a row's handle onto
  another row, move a mod in the load order, where the lower enabled one wins a file and a DLL. A move that hands a
  file or a DLL override to another mod asks first, listing the paths (at most 12 named) and DLLs with their old and
  new winner (`GameMods::preview_place`, the same merge as the conflict lists); a move that changes no winner applies
  at once. Changes apply at the next launch; nothing can be changed while the game runs.
- *Mods activos*, at the top of Instalados, is the account's switch for every mod of the game
  (`GameMods::set_active`). Off, the next launch reconciles the play tree to the install alone (no mod file, no mod
  DLL override, the run log's `mods_active` says `no`), so what the game wrote there stays where it reads it, and
  each mod keeps its own box, slot and DLL modes: switching it on again restores the same selection. With no play
  tree yet the game starts from its install. While off the rows are greyed, their conflict and DLL lines say they
  have no effect, the Mods tab's label says *desactivados* and Compatibilidad's mod DLL line says it is not applied;
  the update checks and *attention* keep working.
- *Activar todos* and *Desactivar todos* set every mod's box in one write, each keeping its slot
  (`GameMods::set_all_enabled`). Turning all on asks first when it hands a file or a DLL a mod on now wins to
  another mod, listing them as a move does (`GameMods::preview_all_enabled`); turning all off asks nothing.
- *Activar al instalar* (on by default), in every install dialog (folder and archive adds, the Thunderstore, mod.io
  and Nexus Mods installs, and a pack's plan), says whether a new mod starts on. It is one choice per game and
  account, remembered in its set file. A newer file of a mod the game has still takes the old one's slot on or off as
  it was. In a pack's plan it decides the entries whose `enabled` the pack file leaves out; an entry that says keeps
  it.
- A provider's result or detail of a mod the game already has shows its box, which turns it on or off as Instalados
  does, and *Ver en Instalados*, which goes to its row and marks it.
- `GameMods::replace` puts a newer version of a mod in the old one's slot of every account, on or off as it was,
  with the old one's DLL modes for the DLLs both ask for, then removes the old one;
  `GameMods::replace_keeping_previous` does the same but keeps the old one, turned off, in the slot right after.
  Installing from a provider a newer file of a mod the game has goes through them (see Sections and search below). `decanter_mods::constraints` is
  where hard ordering edges between mods (provider dependencies, packs) will come from; it returns none yet, and the
  row of a mod placed before one it requires would show it.
- DLL proxies: a mod file named after a system DLL Wine has a builtin for (`dinput8`, `version`, `winmm`, `dxgi`,
  `d3d9`, `d3d11`, `dsound`, `xinput1_3`, `winhttp`) in a folder of the game that holds a `.exe` gets the override
  `n,b` at import, stored in its `mod.toml` (`[[dll_overrides]]`). The Mods tab lists each with a selector for its
  mode (`n,b`, `b,n`, `n`, `b`, not loaded) and says which enabled mod wins a DLL two of them ask for (the later
  one). Under Proton the enabled mods' overrides are appended to the launch's `WINEDLLOVERRIDES`, and the run's log
  header names them (`mod_dll_overrides`); Compatibilidad shows them read-only. On a Windows host they are ignored.
- Each enabled mod lists the game's files it replaces and the files it wins or loses against other mods.
- *Quitar* asks first, with the mod's size and how many accounts use it, then deletes it for every account.
- *Comprobar archivos* hashes a mod's files against its `mod.toml` and lists those that changed (a game that rewrote
  a config through its link writes the mod's copy).

## Updates

- When an account's set is applied, `profiles/<account>.toml` `[base]` records the build and, per game path a mod
  replaces or adds (folded), the install file's `sha256:` or `absent`. The comparison runs again only when the
  installed build differs from `[base].build_id`: at launch, when the Mods tab opens, and right after an update in
  Actualizaciones, which then says how many mods need attention.
- A changed file (or a path only a mod had that the update now ships) puts every enabled mod touching it in
  `[attention]`: it is left out of the play tree (the run log's `mods_left_out` says so) and the Mods tab lists the
  files with *Seguir usándolo* (records the new files) and *Desactivarlo*. Nothing is decided for the user.

## Sections and search

The Mods tab has a row of sections: *Instalados (N)* (the mod list with its order, credits and DLL overrides,
*Añadir archivo comprimido…* and *Añadir carpeta de mod...*), then *Thunderstore*, *mod.io* and *Nexus Mods* (the last
only with its experimental switch on); a later section (packs) is one more `search::Section` variant. Each provider
section is the whole width of the tab (`mods/search/` in the app; design
[mod-packs-and-search-design.md](mod-packs-and-search-design.md) sections 2.3 to 2.5, decision #1635): a top bar with
the provider's name, the mapped game or community with *Cambiar juego* (*Cambiar comunidad* for Thunderstore), the
account and request-budget lines; a search bar with the shared query box and that provider's sort and filter
controls; the results on the left (picture, name, author, summary, numbers, flags, platform label, adult badge,
*instalado <versión>* when the game has it, *Mostrar más*); the chosen mod on the right (description, versions or
files, dependencies, credits and licence, *Instalar*/*Descargar* with progress and *Cancelar*, *Ver en <plataforma>*);
the attribution line at the bottom. There is no merged list and no close button: a browser opens the first time its
section is shown and keeps its results and chosen mod while other sections are visited; leaving the game drops it and
stops its downloads and imports. An archive or folder dropped on the tab goes to *Instalados*.

Mapping a game: a game without a mapping shows the picker inside the section. Suggestions are only plausible matches
(`decanter_mods::likeness`: the same letters and digits, then one name starting with or mostly holding the other,
then at least two significant words in common); with none the picker says there is no game with that name on the
platform. What does not look like the game shows only after a search by hand, under *Otros resultados*. A choice is
written (`thunderstore.toml`, `modio.toml`, `nexus.toml`) only once the platform answered for it: the community's
listing, mod.io's `GET /games/{id}` with third-party access, Nexus' first list. A 404 for a mapped game or community
forgets the file and shows the picker again with a line saying so; mod.io logs the status and `error_ref` of a 404
like any other refusal.

- Thunderstore: the cached listing is searched in memory as you type: sort by relevance (name matches first, then
  downloads; no words: pinned first), downloads, rating, last update or name; *Ocultar obsoletos* and *Ocultar NSFW*
  (both on by default); a category selector when the listing's packages carry `categories` (read when present, not
  documented); 50 results, then 50 more per *Mostrar más* (`Listing::find`, `thunderstore::Query`).
- mod.io: a search runs on Enter or *Buscar* only: `_q`, `_sort` (`-downloads_total`, `-date_updated`, `-date_live`,
  `-ratings_weighted_aggregate`, `name`), `tags` from the game's `GET /games/{id}/tags` (read once with the game, entries
  read for `name`, `tags` and `hidden` only, shape unverified; a failure only hides the tag selector), and
  `maturity_option=0` while *Ocultar contenido maduro* is on (default). Pages of 30 with `_offset`; *Mostrar más* adds
  the next one while `result_total` says there are more. Each page is kept in memory for 10 minutes (at most 40), so
  going back to a query, sort or filter already asked shows it without a request. The 429 handling is the client's,
  unchanged.
- Nexus Mods: the latest added, trending and latest updated lists as before, filtered in memory by the query's words,
  ordered as listed or by endorsements, last update or name, *Ocultar contenido para adultos* on by default; any other
  mod opens by its number or page address. The `X-RL-*` counts are the budget line. `crates/nexus/src/search.rs` holds
  the v2 GraphQL `mods` search (request body with variables, answer reading) behind `SEARCH_V2 = false`: nothing sends
  it and the app does not use it until the design's spike and Nexus' answer (section 2.2).
- Updates take the slot: when an install from any provider is a newer file of a mod the game has (Thunderstore: the
  same package at a lower version; mod.io: another file of the same mod; Nexus: a file the author's `file_updates`
  chain or the newest main file names as the update of the installed one), the confirmation says so and the new mod
  takes the old one's slot in every account (`GameMods::replace`), with *Conservar la versión anterior, desactivada,
  justo después* (off by default) to keep the old one (`GameMods::replace_keeping_previous`). The new mod's credits
  are the ones the provider gave with the new file.

## Uninstall

- The confirmation says how much the game wrote into this account's play tree, which goes with the prefix
  (`play/<account>/` and its record; the set stays for a reinstall).
- When no other account still has the game installed, a box *also remove N mods (size)*, off by default, deletes the
  whole `mods/<store>/<game>/` folder; otherwise the mods are kept and the dialog says so.

## Thunderstore

The Thunderstore section is its browser (`crates/thunderstore`, `mods/thunderstore/` in the app;
[mod-platforms-analysis.md](mod-platforms-analysis.md) section 2). No account; nothing is asked of Thunderstore until
the user acts, requests are paced to one a second and name decanter in the User-Agent, HTTPS only, redirects only to
`*.thunderstore.io`.

- Community: the first time, the game's title is matched against `/api/experimental/community/` (cached a week in
  `<root>/decanter/thunderstore/communities.json`); the user confirms the match or picks one, with the layout, and the
  choice is kept in `mods/<store>/<game>/thunderstore.toml`. *Cambiar* picks again.
- Packages: `/c/<community>/api/v1/package/`, streamed to `<root>/decanter/thunderstore/listings/<community>.json`
  (6 hours, at most 1 GiB) and searched locally (see Sections and search): name, team, summary, downloads, rating, last update, icon. The detail
  lists versions and the latest one's dependencies. A package or version that does not parse is skipped and counted
  in the log; an answer that does not parse at all is a retryable error.
- *Instalar* plans the latest version and every `Team-Name-1.2.3` it needs, recursively (10 levels, 100 packages,
  cycle-safe; the higher version wins; a dependency already installed from Thunderstore is not downloaded again and
  is listed with the version asked for; one the community lacks refuses the install). The zips download into
  `mods/<store>/<game>/incoming/thunderstore-<pid>-<n>/` with progress and *Cancelar*, each matched against the size
  the listing gives, then each is read by the archive import above, `icon.png`, `README.md`, `manifest.json` and
  `CHANGELOG.md` at its root left out. Nothing is added before *Añadir*; dependencies go first in the load order.
- Layout, per game: *BepInEx* follows r2modman's convention as the analysis reads it (not verified against
  r2modman): a package with `BepInEx/` goes to the game's root, `plugins/`, `patchers/`, `config/` and `monomod/` go
  under `BepInEx/`, a package with none of them into `BepInEx/plugins/<Team-Name>/`; one mixing those folders with
  other files is placed as any archive and the preview says so. *Como cualquier archivo* places every package as any
  archive. Making BepInEx load under Proton (`winhttp` and its doorstop) is the DLL proxy rule above, nothing more.
- `mod.toml` gets `[origin]` (`platform = "thunderstore"`, `package`, `version`, `host`). Such a mod shows it with
  *Buscar actualizaciones*, which compares it with the latest version in the community's listing.

## mod.io

The mod.io section is its browser (`crates/modio`, `mods/modio/` in the app;
[mod-platforms-analysis.md](mod-platforms-analysis.md) section 3) on mod.io's REST API v1. decanter ships no API key
(decision #1629): the user makes one at `https://mod.io/me/access` and saves it in *Ajustes › General › mod.io*, which
also has *Probar clave* (`GET /games?_limit=1`) and *Borrar clave*. The key lives in the keyring
(`SecretKey::MODIO`, entry `modio/api_key`), travels only as the `api_key` query parameter to `api.mod.io`, and never
reaches a log, an error, `mod.toml` or a download host. HTTPS only, redirects (5 at most) only to `mod.io`,
`*.mod.io`, `*.modapi.io` and `*.modcdn.io`; one request every 1.5 s across the process (a user key gets 60 a minute);
a 429 holds every request back for its `retry-after` (60 s when 0 or missing), as the reference warns that ignoring
429s can revoke the key.

- Terms: before anything else, `GET /authenticate/terms` is shown with links to the Terms of Use and the Privacy
  Policy; only the user's own *Acepto* records the SHA-256 of that text in `<root>/decanter/modio/terms.toml`, so
  changed terms are shown again. A 403 with error 11074 on any call drops the agreement and says so. decanter makes no
  login call, so it never sends `terms_agreed`.
- Game: the title is searched with `GET /games?_q=`; the user picks one and it is kept in
  `mods/<store>/<game>/modio.toml` once its own record answered. The game's own record is read again each time the browser opens: without
  `api_access_options` bit 1 (third-party access) it is refused with its own line. The documentation does not say
  whether a user's key reaches a game that left it off, so decanter does not try. A game with bit 4 or 8 (downloads
  need a logged-in user) can be browsed but not downloaded from, since decanter has no mod.io login.
- Mods: pages of 30, searched, sorted and filtered as Sections and search says: name, author, summary, downloads, subscribers,
  rating, dates, logo (HTTPS on mod.io's hosts). The detail lists the latest files and `/dependencies` (read for `mod_id` and `name`
  only, shape unverified; shown, never installed on their own).
- *Instalar* asks for the file's record again right before (its `binary_url` may expire), refuses `virus_positive` 1
  and warns on 2 or an unscanned file, downloads into `mods/<store>/<game>/incoming/modio-<pid>-<n>/` with progress
  and *Cancelar*, cut at the listed `filesize` and decanter-mods' cap, then checks the size and `filehash.md5`. The
  archive goes to the import above unchanged; nothing is added before *Añadir*.
- `mod.toml` gets `[origin]` with `platform = "modio"`, `package = "<game id>-<mod id>"`,
  `version = "<file id>-<version>"` and the download's host. *Buscar actualizaciones* reads the game's record and the
  mod, and offers the file mod.io now calls current when it is another one.
## Nexus Mods

Off by default, behind Ajustes' experimental switch (see Credits and attribution).
The Nexus Mods section is its browser (`crates/nexus`, `mods/nexus/` in the app;
[mod-platforms-analysis.md](mod-platforms-analysis.md) section 1). decanter registers no application and ships no key
(decision #1629): each user pastes a personal API key in Ajustes › General, kept in the OS keyring
(`SecretKey::NEXUS`, `nexusmods/api_key`) and sent only to `api.nexusmods.com` in the `apikey` header. *Probar clave*
asks `/v1/users/validate.json` and shows the account and whether it is Premium. No `nxm://` handler is registered.

- Requests: only on a user action (no polling; *Buscar actualizaciones* only on its button), HTTPS only, one a
  second, `Application-Name: decanter` and `Application-Version` as the acceptable use policy asks. The API client
  follows redirects only on `api.nexusmods.com` and is the only one carrying the key; downloads go through a client
  without it, redirected only within `*.nexusmods.com` and `*.nexus-cdn.com`. The `X-RL-Daily-Remaining` and
  `X-RL-Hourly-Remaining` counts are shown; once both are 0, or on a 429, nothing is sent until the next full hour.
- Game: the first time, the game's title is matched against `/v1/games.json` (cached a week in
  `<root>/decanter/nexus/games.json`); the user confirms the match or picks one, kept in
  `mods/<store>/<game>/nexus.toml`. *Cambiar* picks again.
- Mods: the latest added, trending and latest updated lists, filtered and ordered locally (see Sections and search); v1 has no
  search by name, so any other mod is opened by its number or the address of its page. The detail lists the files with category
  (main, update, optional, …), version, size, date and whether Nexus gives an MD5.
- Premium accounts: *Descargar* asks the file's `download_link.json`, takes the first HTTPS mirror on Nexus' hosts and
  downloads into `mods/<store>/<game>/incoming/nexus-<pid>-<n>/` with progress and *Cancelar*, matched against the
  listed size in bytes and MD5 when they are given, then the archive import above reads it; nothing is added before
  *Añadir*. Free accounts get a 403 there, so the detail says Nexus only gives download links to Premium users
  through the key and offers the mod's page; the file then comes in through *Añadir archivo comprimido…*.
- `mod.toml` gets `[origin]` with `platform = "nexus"`, `package = "<domain>-<mod>-<file>"`, the file's version and
  the download host. *Buscar actualizaciones* reads the mod's file list and follows the author's `file_updates`
  links from that file, or offers the newest main file once it is no longer current.
- Unverified (the v1 reference was unreachable for the analysis): every endpoint and field name, the rate-limit
  header names, the premium flag's spelling, the download link's shape and CDN hosts, and whether `files.json`
  carries `md5` and `size_in_bytes`. Each is optional when read; a mod or file that does not parse is skipped and
  counted in the log, an answer that does not parse at all is a retryable error.

## Credits and attribution

What each platform asks of decanter, with its source, is in
[mod-platforms-attribution.md](mod-platforms-attribution.md) (rows marked unverified follow the conservative reading;
every text is in `app/locales/<lang>/mods_credits.<lang>.yml`, so wording can change).

- `mod.toml` `[origin]` also keeps the credits (`decanter_mods::ModCredits`), filled at install and refreshed by
  *Buscar actualizaciones*: `author`, `author_url`, `page_url`, `website_url`, `licence`, `permissions`, `adult` and
  `fetched_at`. Older files without them still read; the mod then shows its platform and says the credits come with
  the next update check. Texts are plain and bounded; `author_url` and `page_url` must be HTTPS without credentials
  or port on the platform's own hosts (`thunderstore.io`, `mod.io`, `nexusmods.com` and their subdomains),
  `website_url` HTTPS on any host; a link that fails is left out when attached and refuses the file when read.
- Thunderstore: the team, linked to its page under the package's page, the package page and the manifest's
  `website_url`; the package format has no licence, so the line says it is not stated and points to the page.
- mod.io: `submitted_by` (`display_name_portal`, else `username`) linked to its `profile_url`, the mod's
  `profile_url`, `credit_options` decoded to permission tokens (bit values recalled, not read in the reference;
  unknown bits are left out) and `maturity_option` (any bit set) as the adult flag.
- Nexus Mods: `author`, `uploaded_users_profile_url` when given, the mod's page and `contains_adult_content`; v1 gives
  no permissions text the analysis could confirm, so the line points to the mod's page. The detail and each
  installed Nexus mod acknowledge Nexus Mods and the mod's authors as the authors of the content (ToS s.10).
- The Mods tab: each card and detail names its platform as text (no logos: none was cleared, and for Nexus one could
  suggest association, ToS s.27), *por <autor>* linked to the profile, *Ver en <plataforma>* (installed mods and the
  provider sections' detail), the licence or permissions line and an adult badge; each provider section ends with a
  line saying the mods are their authors' and decanter is not affiliated with or endorsed by the platform. Ajustes ›
  mod.io links its Terms of Use and Privacy Policy beside the key.
- Nexus Mods is off by default behind *Usar Nexus Mods (experimental, solo para uso personal)* in Ajustes › General
  (settings key `nexus_experimental`, decision #1635), since the acceptable use policy forbids personal API keys in a
  public application and decanter's registration is pending. Off hides the key section, the Nexus Mods
  section and the Nexus update check; the saved key stays in the keyring and Nexus mods keep working and
  showing their credits. Both Nexus clients send `Application-Name: decanter` and `Application-Version`.

## Packs

A pack is a named, ordered list of mod references for one game, as
[mod-packs-and-search-design.md](mod-packs-and-search-design.md) sections 3 to 6 describe it: the format, validator,
resolver, diff and plan are `crates/modpack`, the section is `app/src/views/library/detail/mods/packs/`. The Mods tab
switches between *Instalados* and *Packs*; the Packs section lists the game's packs on the left and shows the chosen
pack's entries, or its apply plan and progress, on the right.

- *Nuevo pack con los mods instalados*: every mod of the game becomes an entry, in this account's load order with its
  on or off state (mods only other accounts have go last, off), `track = "pinned"`, its origin as structured ids
  (Thunderstore package and version, Nexus domain, mod and file, mod.io game, mod and file), its `target`, its DLL
  modes and its archive's `source_sha256` and size. A mod without an origin is a `local` entry named by its archive's
  file name; one imported from a folder has no hash, is marked *not shareable* and is recognised only through the
  pack's notes. The header records the game's title and its Thunderstore community, Nexus domain and mod.io game.
- Editing: rename, ↑ and ↓ (refused when a `requires` edge would break; a move that hands an installed mod's file or
  DLL to another asks first, computed with `GameMods::preview_order`), the checkbox (`enabled`), *Fijada* / *La más
  reciente* (`track`, not for local entries) and × (the entry and every edge naming it). Each save bumps `revision`
  and writes through `Pack::write`, which re-reads what it wrote; every pack file read goes through `Pack::parse`, and
  a file that does not hold up is listed with the reason so it can still be removed.
- *Aplicar...* first makes a plan, on that click: the Thunderstore listing when a Thunderstore entry is missing (its
  cache, refreshed when stale), and with the Nexus switch on and a Nexus entry missing, the account (Premium or not).
  The plan lists each step in the resolved sequence (the listing's dependencies between the pack's Thunderstore
  entries are hard edges): already installed, to download (with the total size), needs a file, not available,
  blocked by a requirement, and the winners that placing the pack's mods in its order changes among the mods on now.
  Nothing is downloaded before *Aplicar el pack*.
- Applying runs one entry at a time: Thunderstore entries download into `incoming/` (`decanter_thunderstore::Staging`,
  removed after the import), are checked against the entry's `sha256` when pinned, read with `preview_package` and
  imported with `import_archive` under the entry's `target`; a newer version replaces the installed one in its slot
  (`GameMods::replace`); the entry's DLL modes are set. After every entry the pack's present mods are placed as one
  block at the slot of the first of them already in the load order (else last), in the resolved order, on or off as
  the plan says, and `packs/<id>.local.toml` records which mod each entry is and whether the pack installed it.
  A conflict line per mod the new one wins over or loses to follows each import, and a summary ends the run.
- Failure is skip and flag (decision #1635): an entry that fails, waits for a file or is not available holds back only
  what `requires` it (blocked, left off, its slot kept); the rest goes on. The pack shows *Incompleto: N de M en
  espera*, and applying again recomputes the plan from what is installed, so it does only what remains. *Cancelar*
  stops the download or import in flight, which removes its staging and partial import; what was installed before stays
  placed.
- Manual steps: Nexus Mods entries always have one (*Abrir la página*, the files tab built from the ids, and *Elegir
  archivo...*); with the switch on and a Premium account a *Descargar* button downloads that one file (`files`,
  `mod_info`, `download_link`, `download`, which checks Nexus' size and MD5), one per click, since batch downloads are
  an open question for Nexus. mod.io entries are manual too (the mod and file ids, no page link: the pack carries no
  URL and mod.io's pages use names, not ids), since its download path needs the browser's terms dialog and access
  checks. Local entries ask for their archive, or for a folder when they were one. A picked file is checked against
  the entry (SHA-256, else MD5, else size, when pinned) and imported with the origin the provider's crate would write,
  so the next apply recognises it.
- *Quitar el pack* deletes `packs/<id>.toml` and `.local.toml`; the mods stay. Ticking *Quitar también los N mods que
  instaló este pack* also removes the mods the notes say the pack installed, except those another pack's notes name
  or another account has on (counted as kept). Without notes nothing is offered.
- Not yet (later phases): export and import of pack files, adding to a pack from search, `requires`/`after` editing,
  Thunderstore dependencies added as `auto` entries at creation, update checks per pack, the mapping adoption of
  section 6.4 (a different Thunderstore community is only said), *accept this file once* after a hash mismatch, and
  mod.io downloads.

## Where things are

Under the library root that holds the install (`<root>/common/<InstallDir>`):

```
<root>/decanter/mods/<store>/<game id>/    <store>: gog, amazon or epic
    <mod uuid>/mod.toml, files/    one mod, shared by every account that has the game
    profiles/<account>.toml        the account's mods in load order, each `[[mod]]` with `id` and `enabled`;
                                   `active`, `enable_on_install`
    play/<account>/                the play tree the game starts from
    play/<account>.toml            every link decanter placed in it, and its target
    incoming/                      imports and removals in progress
    packs/<pack uuid>.toml         a pack: the recipe, shareable
    packs/<pack uuid>.local.toml   this machine's notes: entry -> mod, and which mods the pack installed
```

- `profiles/<account>.toml` is `schema = 2`. A schema 1 file (`mods`, enabled in load order, and `disabled`) is read
  as the enabled list followed by the disabled one, so it loads to the same winners, and is written as schema 2 at
  its next change; a decanter older than that refuses a schema 2 file. `active = false` (the *Mods activos* switch
  off) and `enable_on_install = false` (*Activar al instalar* unticked) are written only when off, so a file without
  them reads as on and the schema stays 2.
- The play tree is real folders and one relative symbolic link per file, to the install's file or the winning
  mod's (`../../…/common/<InstallDir>/…`, `../../<mod uuid>/files/…`), so moving the library keeps it valid. On a
  Windows host the links are hard links, since a symbolic link needs a privilege there.
- Anything the game writes in its folder (configs, saves next to the executable) lands in the play tree as a real
  file and is never replaced. A cloud save folder inside the install is read from the play tree instead.
- The game starts from the play tree once an account has turned a mod on, and keeps starting from it with every mod
  off, since it may hold the game's files. With no mod ever on there is no play tree and nothing changes.
- Verify, repair and update read and write only `common/<InstallDir>`: a mod never writes into the install. This holds
  for all three stores, whose installs are all `common/<InstallDir>` with a manifest per account; an Epic DLC
  installed into the game's folder is mirrored at the next launch like any file of the install.
- When the library root is outside the folders the Steam Linux Runtime shares by default, the launch adds it to
  `PRESSURE_VESSEL_FILESYSTEMS_RW`.

## Rules an import keeps

- Only regular files and folders; a link, device or name that is not UTF-8 refuses the whole import (in an archive:
  zip symlink modes and Unix types, 7z reparse points, Unix types and anti-items).
- Archive names: `\` read as `/`, then no absolute path, drive letter, `..` or `.`; any bad entry refuses the
  archive. Encrypted archives are refused, and so are 7z methods decanter does not build (bzip2, zstd, brotli, lz4)
  and LZMA or PPMd dictionaries larger than 1 536 MiB.
- Archives: at most 200 000 entries, and each entry (each solid block in 7z) larger than 1 MiB at most 1 000 times
  its compressed size. Sizes are enforced again while extracting, since a header can lie; files are created
  `0644` and folders `0755` in a fresh `incoming/` folder, and a cancel or failure removes it.
- Paths: relative, at most 1 024 bytes and 64 components, no `.`, `..`, `\`, `:` or control characters.
- Two names that are the same file for a Windows game (differing only in case) refuse the import.
- `Galaxy64.dll` and `Galaxy64.orig.dll` are kept for decanter's Galaxy hook and refuse the import.
- At most 200 000 files and 64 GiB per mod, and 1 GiB left free on the library's disk.
- Files are copied into `incoming/<pid>-<n>/`, created new (never through an existing file), hashed (SHA-256),
  then renamed into place; a failure removes what was copied.

Every `mod.toml`, set file and play tree record is checked again when read; a mod file that is not a plain file
inside its mod's `files/` fails the launch instead of being linked.

## Not yet

RAR, external games, renaming a mod once added, a target folder for a loose folder, a mod.io login (subscriptions,
games whose downloads need one) and Nexus' `nxm://` links for free accounts.
