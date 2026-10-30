# Pausing and resuming downloads

Design proposal for kb #1277: let the user pause and resume any download, and get the same
resume after an exit nobody chose (crash, `kill -9`, power loss, reboot, full disk, network
drop part way through a chunk). This page covers what happens today, what the user would see,
the crash-safe persistence, the order of the work, the risks and the tests, and the decisions
that are the owner's.

Code references are to `develop` at `123e5ab` (2026-10-29). Versions pinned in `Cargo.lock`:
reqwest 0.13.5, tokio 1.54.1, md5 0.8.1.

**Status.** The owner accepted every recommendation in §6 (#1281). Phases 0 to 3 are
integrated. Phase 4 is built as described below (#1310), and phase 4b on top of it (#1315, #1316,
#1319, #1320); nothing of phases 3 and 4 has been run against GOG yet.

**Phase 3 as built.**

- Buttons. Downloads, the game's action bar and Storage share `download_actions`
  (`app/src/views/downloads.rs`): *Pausar* and *Cancelar* on a queued or running row, *Reanudar*
  and *Cancelar* on a paused one, nothing while it is cancelling, failed or run by another process.
  Downloads adds *Pausar todo* / *Reanudar todo* once it lists two or more rows. *Pausar* calls
  `decanter_gog::pause`; *Reanudar* sets the row to Queued at once (so neither a second press nor
  the watcher can start it twice) and runs `decanter_gog::resume`, which plans again from the
  journal's request with the existing engine and so keeps every guarantee of phase 2.
- Reasons. `PauseReason::AppClosed` is new (*Decanter se cerró*); the network reason now says it
  resumes on its own. A resume that cannot reach GOG while planning (session refresh or plan, a
  network error, a 5xx or a 429) stays `Paused{network}` instead of failing, so retrying never
  turns a row into a failure. `Paused` carries the library folder (`root`) the download is in.
- Clean quit. The window's ✕, the compositor's close, the tray's quit, a polite signal and the
  self-update restart all go through `decanter_gog::pause_for_quit`: every running download is
  stopped with `Stop::Quit`, recorded as `Paused{app closed}`, and the quit waits for that (a
  counter of downloads not yet settled) for at most 2 s. A download whose plan lands during the
  wait is paused before any byte moves.
- Automatic resume (`app/src/views/library/unfinished.rs`, one app-level task):
  - At startup, and whenever the active profile's set of present library folders changes (a disk
    plugged in), the journals are scanned. A row new to the list resumes when its reason is
    *app closed* or *closed unexpectedly* and `Reanudar las descargas al abrir Decanter` (Ajustes →
    General → Descargas, setting `resume_downloads_on_start`, on by default) is on, or when its
    reason is *library unavailable* (its folder is back, since it was found). Each found download
    resumes this way at most once per run.
  - A `Paused{network}` row is retried 15 s after it paused, then after 30 s, 60 s… capped at
    5 min; a `Paused{library unavailable}` row is retried as soon as its folder is a directory
    again, then on the same growing wait. The wait resets only once bytes move again. The retry
    is the resume itself (session refresh and plan), not a separate probe.
  - Never on their own: a user pause, a full disk, a repair.
- Another process. A journal whose lock another process holds is listed as *En curso en otro
  proceso de Decanter* with its progress and no buttons, scanned again every 30 s; when its folder
  is gone the row goes and the game's install state is read again.
- 14 days. A row paused 14 days or more (from the journal's last write, or the pause time in this
  run) shows *Descarga en pausa desde hace N días · tamaño*, and its *Cancelar* reads *Eliminar*,
  behind the same confirmation.
- Storage. Each library folder card has *Descargas sin terminar* with the size of its
  `downloading/` and the rows in that folder, with their progress and the same buttons.
- Uninstall. With a paused download of the game listed, the confirmation adds *Se borrará también
  la descarga en pausa (tamaño)*, and confirming discards it (`decanter_gog::discard`) before the
  uninstall.

**Phase 4 as built.**

- Queue. One download runs at a time (`tracking::wait_turn`, a fair one-permit semaphore taken by
  `install` and `repair` before the session refresh and the plan, so chunk URLs are signed only
  when the download is about to use them, and held until it ends, move included). The rest read
  *En cola* and start in the order they asked; the 4 files at once inside a download stay. A pause
  or cancel of a waiting download wakes it (`Notify`): it is planned and its journal opened without
  a turn, and the stop lands there before any byte moves, so a paused one always has a journal to
  resume from (phase 4b stops it without a plan instead). Quitting stops a waiting one at once. *Pausar* on the running one lets the next
  start. Restarts need nothing new: the startup scan resumes what it should, and those resumes
  queue in the order found.
- Build drift. When the journal's `plan` differs from the new plan, the old journal is first
  reconciled against its own plan (the phase 2 checks: truncate to committed, re-hash the last
  window after an unclean exit). Then each file of the new plan with the same path keeps the
  longest prefix of chunks equal in MD5 and size, in order: a finished file identical in chunks
  and mode stays finished, a finished one sharing a prefix (or changing only its mode) is renamed
  back to `.part` and cut there, a file in flight is cut to the shared prefix, and every other
  staged file is deleted. A file never holds chunks of two builds. Only cuts happen before the
  switch, and each is one the old log already copes with. The new log (`Begin` with the new plan,
  a `Commit` or `Done` per kept file, `Resumed`) is written whole as `progress.log.new` with one
  sync (`LineLog::create_all`), then the new header replaces the old one atomically, then the log
  is renamed into place and the folder synced. A crash before the header leaves the old journal
  valid (the stray log is dropped at the next open); a crash after it finishes the rename at the
  next open (`finish_swap`). The reused bytes are logged, not shown. A pinned build stays pinned; a
  pinned build GOG no longer offers fails and keeps the data, as before.
- Free space. Before the first byte of every start or resume, `remaining = total − on disk` plus
  the larger of 1 GiB and 2 % of `total` is compared with the free space of the staging folder's
  disk. Short, the download does not start: it pauses as *Disco lleno* and journals the
  `Room{needed, free}`. `ENOSPC` while writing pauses the same way and records the room measured
  then. The row reads *Disco lleno: faltan 3,2 GB libres en <biblioteca>*, and for an update
  *Disco lleno: la actualización necesita 41 GB libres en <biblioteca> mientras se descarga;
  faltan 3,2 GB*. A full disk still never resumes on its own; *Reanudar* checks again.
- Library unavailable. The row reads *La biblioteca <ruta> no está disponible*. *Reanudar* on a
  paused row whose library folder is not a directory keeps it paused with that reason instead of
  failing to find its journal.
- Repair. A paused repair has *Reanudar* (and *Reanudar todo* takes it): `decanter_gog::resume_repair`
  rebuilds the broken-file list from its journal, each file with the depot it came from
  (`HeaderFile.owner`, journalled for repairs only, so install plan hashes are unchanged; an older
  repair journal plans every file from the game's own depots), and repairs again at the build the
  game is on. Owners are planned in order (`BTreeMap`), so the same broken files give the same
  plan and the repair carries on as it was. Repairs still never resume on their own.

**Phase 4 follow-ups** (#1317, #1318).

- Staging folders without a journal (`decanter_gog::stray_staging`): a folder under
  `downloading/<account>/<id>` with no `download.json` and a free `lock` is listed in Storage's
  *Descargas sin terminar* with its size. *Eliminar* asks first, naming the folder, and
  `discard_stray` takes the lock and looks for the journal again under it, so a download that
  started meanwhile, or any folder with a journal, is never removed.
- Build drift is shown: `Progress.build_switch` (`BuildSwitch{kept_bytes, had_bytes}`, the numbers
  `carry_over` logs) rides every report of the resumed run, and the row reads *GOG publicó otro build
  mientras estaba parada: se reutilizaron X de los Y ya descargados*, also while it is paused again.
- Verify after an unclean resume: `resume_download` flags a row whose reason was *closed
  unexpectedly*; when that download finishes, Descargas shows a notice with *Verificar ahora*, which
  opens the game's Propiedades on Archivos locales. A chunk recovery after a clean pause is not
  flagged (the app has no signal for it), and repairs are not offered.

**Chunk recovery** (#1314: after any failure only the chunk in flight is lost).

- Process alive (network drop, retry window over, refused URL, pause, quit, full disk, a corrupt
  chunk, another of the 4 files failing): a chunk is fetched, inflated and checked whole in memory
  before it is written, and a stopped file makes a best-effort `commit` of every chunk written.
  Each chunk's write is now flushed before the file's cursor counts it, since tokio's `write_all`
  returns with its last piece still being written and an `ENOSPC` there would otherwise surface
  only later, after that commit had recorded a chunk that never landed.
- Unclean exit (`kill -9`, power cut) or a failed last commit: the `.part` can hold up to
  `COMMIT_BYTES` (64 MiB) or `COMMIT_INTERVAL` (5 s) of whole chunks past its last record. On
  resume `reconcile_file` first does the phase 2 checks (cut to the record, re-hash the last window
  after an unclean exit); only when the record held and its window re-hashed clean does
  `recover_tail` read the plan's next chunks from the `.part`, in order, hashing each (inflated
  size and MD5 from the header) through a 1 MiB buffer, and keep them up to the first one short of
  its size or with another MD5. It reads at most what is on disk past the record and never past the
  plan. The `.part` is cut there and synced, then one `Commit` per recovered file is appended before
  `Resumed`: no per-chunk sync while downloading, and still no record names unsynced bytes. On a
  build switch the recovered chunks go into the new log's `Commit`s instead. Each scan logs the
  recovered chunks and bytes, the discarded bytes, and the first rejected chunk and why.

**Phase 4b as built.**

- Queued downloads (#1319). `install` and `repair` enter their staging folder before waiting for
  their turn (`pending::enter`): they take its `lock` and hold it until they end, so another process
  lists them as running there, and a folder without a plan gets `pending.json`, the request (kind,
  account, product, parent, title, branch, build) and, for a repair, the files it repairs (`path`,
  `owner`), written whole with `write_atomic`. A folder with a plan already has the request in its
  header; entering it drops an earlier stop, since the download is asked for again.
- Stops before the plan. The wait for the turn and the plan (`tracking::until_stopped`) end as soon
  as a pause, a cancel or a quit is asked, with nothing asked of GOG: a cancel deletes the folder
  (`journal::discard_with`), a pause or a quit records `stop` (*user*, *app closed*) in
  `pending.json` and fails with `Paused`. The session refresh runs on its own task, so a renewed
  token is still stored when the plan waiting on it is dropped. A quit counts a queued download
  among those not yet settled from its start, so the 2 s wait covers its record too.
- Crash safety. `pending.json` never speaks for the bytes: a resume trusts what is staged by the
  log alone, so a stop recorded there can't hide an unclean exit from the re-hash. It is written
  only under the lock while nothing downloads, so its stop is newer than the log's last record, and
  the listing takes it over the log's reason (and drops the log's `room` then). Opening the journal
  for a download removes it, after the header is in place (`start_over` keeps it through the clear),
  so a crash between the two still lists the request, and a crash after it reads as one.
- Listing and resume. A folder with only `pending.json` is listed with no total; `resume` plans it
  again from that request, and `resume_repair` from its files. A plan GOG refuses (not offline) drops
  a folder that held only the request, as a failed fresh install left nothing before; one GOG
  couldn't be asked for stays queued. A plan that picks another library folder than the guessed one
  drops the guessed folder unless an earlier plan is staged there.
- DLC after the game (#1315). `queue_dlcs` enters each DLC ticked with a game as a queued download
  of its own (parent set), before the game's own install starts. A queued DLC never planned, with no
  stop of its own, is listed with its game's reason. `install` of a DLC whose game isn't installed
  pauses with that reason while the game has a download left, and is refused at once without one.
  The app doesn't resume such a DLC at startup, *Reanudar* on it carries the game on instead, and
  once the game lands from a resume its paused DLC rows resume. A cancelled game drops its queued
  DLC; a paused one lists them paused for the same reason.
- Uninstall (#1316). The confirmation names every paused download of the game and of its DLC with
  its size (*Se borrará también la descarga en pausa de <título> (tamaño)*), and confirming
  discards each before the uninstall.
- One slot across processes (#1320). After this process's turn, a download takes the lock file
  `<root>/downloading/lock` of the library folder every process using the library agrees on (the
  first present, in path order), polled every second while another process holds it and given up at
  once on a stop; it is held with the turn until the download ends, move included. A folder that
  can't be locked at all lets the download go on without it. `decanter install` says when it waits
  for another process and repeats it every 15 s until bytes move; `--no-wait` exits at once with a
  clear message instead. The app lists the CLI's download, queued or running, as *En curso en otro
  proceso de Decanter*.

- With the follow-ups: a folder holding `pending.json` has a journal for `stray_staging` and
  `discard_stray`, so a queued download is never offered as a stray; a resume of one never planned
  isn't flagged for *Verify*, since nothing was staged.

**Left for later** (deliberately): the slot is per library location, so two profiles with different
library folders can still download at once; cross-process turns are not in asking order (the lock is
polled); the DLC queued behind a game that failed are listed only after the next start.

## 1. Current behaviour

### 1.1 GOG installs (game, DLC, update, branch or build switch, repair)

**Pipeline.** `decanter_gog::install` (`crates/gog/src/install/mod.rs:89`) refreshes the token,
resolves a `Plan` (`plan.rs:592`): it picks a build, reads the depot manifests and signs one CDN
URL per chunk from the `secure_link` answer (`plan.rs:606-643`). Then `install_into`
(`mod.rs:179`) downloads into staging and moves the result into `common/<InstallDir>`.

**Staging layout.** `staging_root` (`staging.rs:16-36`) gives
`<root>/downloading/<account>/<product>/`, beside `common/` and on the same root so the final
move is a `rename`. The staged tree mirrors the game's own relative paths, with each file under
its final name and no `.part` suffix (`download.rs:182-186`). A repair stages into
`<root>/downloading/<account>/<product>-repair/` (`verify.rs:313-317`).

**Transfer.** `download_all` (`download.rs:114`) runs at most `CONCURRENT_FILES = 4` files at
once per install (`download.rs:30`), and each install has its own budget, so two installs mean 8
connections. A file is fetched chunk by chunk, in order. Each chunk is downloaded fully into
memory, inflated with zlib and appended to the file (`download.rs:189-209`, `260-297`). Nothing
is flushed to stable storage: there is a `flush()` (`download.rs:211`) and no `sync_data`.

**Checksums.** Each GOG depot chunk has `compressedMd5`, `md5` (of the inflated bytes) and
`size` (`crates/gog/src/api/client/content.rs:150-155`). **The download checks neither hash.**
`compressed_md5` is only used to build the chunk URL (`plan.rs:632-634`), and `ChunkSource`
keeps nothing but the URL and a `compressed` flag (`download.rs:41-45`). The only integrity
check on the way in is zlib's own Adler-32, which `inflate` (`download.rs:292`) runs implicitly.
The MD5s are used only by the separate *verify* (`verify.rs:241-269`), which reads the installed
file in `size` slices and compares each slice with `md5`.

**Network errors.** Every chunk has `CHUNK_ATTEMPTS = 3` tries with a linear backoff of 0.5 s
and then 1 s (`download.rs:35-38`, `235-258`). The client times out after 10 s connecting and
30 s idle (`download.rs:85-97`). Retries happen per chunk, but the unit restarted is the *whole
chunk*, never a byte range. Once the third try fails, `download_all` sets the shared cancel flag,
so every other file stops (`download.rs:145-151`), and the install fails. In practice an outage
longer than about 1.5 s of backoff plus the timeouts fails the install.

**On failure or cancel.** `download_into_staging` deletes the whole staging directory
(`mod.rs:369-382`). The app shows `InstallStatus::Failed(text)`
(`app/src/views/library/install.rs:125-133`). The module documents this as deliberate: "a retry
always starts from nothing and there is no partial state to reconcile" (`download.rs:3-8`).
`decanter_core` repeats the point: "decanter itself has no partially-installed state"
(`crates/core/src/library.rs:66-70`).

**Cancel.** The Downloads tab's *Cancelar* (`app/src/views/downloads.rs:129-140`) calls
`cancel_and_forget` (`app/src/views/library/install.rs:215-219`), which calls
`decanter_gog::cancel` (`tracking.rs:37-56`) and removes the row. The cancel is a flag checked
between chunks and between body reads (`download.rs:47-68`, `269`), so it lands within one
chunk. A cancel during the queued phase (token refresh, plan) is recorded in `cancel_requested`
and honoured before any byte moves (`mod.rs:157-160`). The download then "fails", and the
failure path deletes staging. Cancelling today therefore means "stop and delete everything", with
no confirmation.

**App close.** `close_main_window` (`app/src/views/mod.rs:178-196`) stops running games and quits
unless decanter closes to the tray. `on_window_closed` quits the same way (`mod.rs:145-157`).
Neither one waits for, pauses or records installs: the tokio tasks die with the process. The
staging directory **stays on disk**, holding a mix of complete files and one partial file per
in-flight transfer, with nothing recording which is which.

**Crash, `kill -9`, power loss, reboot.** Same as an app close, with one difference: because
nothing is `fsync`ed, the tail of each partial file may be torn or zero-filled after a power
loss. The leftovers:
- are not shown anywhere. Storage does not size `downloading/`; it only knows the legacy
  leftovers (`app/src/views/storage.rs:201`).
- are deleted only when the same `(account, product)` is installed again (`mod.rs:218-225`).
  A product that is never retried keeps its gigabytes in `downloading/` for good.
- leave an `Installing` row that no longer exists, because `Background.installing` lives only in
  memory (`app/src/state/background.rs:50`).

**Crash during the move.** `move_into_library` (`staging.rs:45-101`) renames file by file and
merges into the existing directory. A crash part way through an **update** leaves `common/` with
some files from the new build and some from the old one. The appmanifest still names the old
build, because it is written after the move (`mod.rs:284-309`). The next verify then compares
against the *old* build and repairs the new files back to old ones. That works, but the user's
update has silently vanished.

**Full disk.** `ENOSPC` from `write_all` (`download.rs:192-196`) is an ordinary I/O error: the
install fails and staging is deleted. No free space is checked before starting.

**Updates re-download everything.** An update is a plain `install` of the new build
(`app/src/views/library/properties/updates.rs:93-102`). `plan` lists every file of the build and
`move_into_library` merges the result over the old directory. There is no delta, so a 60 GB game
re-fetches 60 GB for a small patch. That is out of scope here, but it is the case where resume
matters most.

**What the user sees.** Only installs started through `install::install` (library action,
`app/src/views/library/actions.rs:119`) report into `Background.installing` and so appear in the
Downloads tab. The other GOG downloads pass `|_| {}` as their progress callback and never touch
the map: updates (`properties/updates.rs:93`), the DLC tab (`properties/dlc.rs:99-110`), branch
and build switches (`properties/releases.rs:83`, `236`), repair (`properties/local_files.rs:120`,
which also passes `Cancel::default()` and discards progress at `verify.rs:359-362`), and DLC
installed after a game (`app/src/views/library/install.rs:182-191`, shown as `Queued` with no
progress). None of these can be seen in Downloads or cancelled from there. The map is also keyed
by product id alone (`background.rs:50`), so the same game on two accounts collides there, even
though staging already separates them by account.

### 1.2 Other downloads

| Download | Code | Size | Now: error / cancel | Now: crash leftovers |
|---|---|---|---|---|
| psport Linux bundle | `crates/ps/src/os/linux/bundle.rs:98-145` via `crates/ps/src/download.rs:115` → `decanter_runtime::download::fetch` (`crates/runtime/src/download.rs:91-152`) | ~50 MB (cap 512 MiB, `ps/download.rs:24`) | Restarts from zero. The archive and staging (`.{tag}.{pid}.tar.zst`, `.{tag}.{pid}`) are removed after any error (`bundle.rs:136-137`) | pid-named leftovers in `<root>/psport/…/bundles/` that **nothing sweeps** |
| psport Windows package | `crates/ps/src/os/windows/package.rs:350-430` | ≤512 MiB | Same, with a lock file per pid (`package.rs:388-398`) | `sweep_stale` (`package.rs:510-555`) removes another dead process's leftovers on the next install |
| Port prebuilts (Linux) | `crates/ps/src/os/linux/prebuilt.rs` (lookup only) | n/a | nix substitutes from the binary cache; nix owns atomicity | nix's own store; nothing of ours |
| nix-portable | `crates/nix/src/portable.rs:38-96` | ~70 MB | `part-<pid>` is **not** removed on a network error, only on a checksum mismatch (`portable.rs:83`) | `nix-portable.part-<pid>` stays |
| GE-Proton (FSR recording) | `crates/runtime/src/os/linux/ge_proton/mod.rs:140-213` | ~0.5 GB (cap 1 GiB, `:42`) | `Leftovers` drop guard removes the archive and staging even when the future is dropped (`:263-287`) | `.{tag}.{pid}.*`, swept under the lock by `sweep_stale` (`:232-260`) |
| umu-run | `crates/runtime/src/os/linux/umu.rs:180-219` | small (cap 64 MiB) | Temp dir removed on error | `$TMPDIR/decanter-umu-run-<pid>`, **outside the library** |
| Raven | `crates/runtime/src/os/linux/raven/build.rs:78-87` (`git fetch --depth`), toolchain via nix | source + nix | git and nix own it; the checkout is swept by `sweep_stale` (`raven/mod.rs:436-490`) | as left by git/nix |
| decanter self-update | `app/src/self_update.rs:300-395` | ≤512 MiB (`:30`) | `.{ASSET}.{pid}.part` removed on error (`:385-391`) | the pid `.part` stays until… nothing |
| GOG cloud saves | `crates/gog/src/cloud_saves.rs:573-630` | KB–MB | Not a pausable download | **Writes the save in place with `tokio::fs::write` (`:618`)**: a crash mid-write truncates the player's save. A separate fix (write `.part`, `sync_data`, rename) worth its own task |

All the single-file downloads go through `decanter_runtime::download::fetch`, or code shaped like
it. That function streams into a file it truncates (`runtime/src/download.rs:120`) and hashes as
it goes, and it is documented as leaving a partial file for the caller to remove (`:84-86`).
None of them sends `Range`.

## 2. User-facing behaviour

### 2.1 States and actions

`InstallStatus` (`app/src/state/background.rs:12-25`) gains a `Paused` state, and the map is
keyed by `(account, product)`:

```text
Queued ──start──► Downloading ──done──► Moving ──► (row disappears, game installed)
   │  ▲                 │  ▲
 pause resume        pause resume / auto
   ▼  │                 ▼  │
 Paused{reason, done, total}   ──cancel & delete (confirm)──► gone
                        │
                     Failed{text}  (only for errors a resume cannot fix)
```

The Downloads tab gets one row per download (game, DLC, update, branch switch, repair), now
including the ones that are invisible today:

- **Downloading:** progress bar, `12,3 GB / 40,1 GB · 18 MB/s · faltan 25 min`, buttons
  **Pausar** and **Cancelar**.
- **Paused:** the bar frozen with `En pausa · 12,3 GB de 40,1 GB` and a reason line:
  - `La pausaste tú` (user).
  - `Sin conexión; se reanudará sola cuando vuelva la red` (network).
  - `Decanter se cerró` (clean exit with downloads running).
  - `Decanter se cerró de forma inesperada` (crash, kill, power loss).
  - `Disco lleno: faltan 3,2 GB en <ruta>` (disk full).
  - `La biblioteca <ruta> no está disponible` (unplugged disk; see #1257).
  - `GOG ha publicado una versión nueva` (build changed, only when a decision is needed; §2.4).

  Buttons: **Reanudar** and **Cancelar**.
- **Queued:** **Pausar** (= "don't start this one") and **Cancelar**.
- A header with **Pausar todo** / **Reanudar todo** once there are two or more rows.

The game's own detail pane keeps reading the same status through `progress_lines`
(`downloads.rs:29-65`), so the two places show the same thing.

### 2.2 Across restarts and reboots

A paused or interrupted download is recorded in the library (§3), not in memory. At startup,
decanter scans `downloading/` in every root of the active profile's libraries and fills
`Background.installing` with `Paused` rows before the first frame. A crash, power loss or reboot
behaves exactly like *Decanter se cerró de forma inesperada*. The only difference from a clean
close is the deeper check at resume (§3.5).

On a clean quit (`close_main_window`, `on_window_closed`, tray quit), decanter pauses every
download with reason *app closed*. It waits up to about 2 s for in-flight chunks to stop and for
the journals to sync, then quits. A download that doesn't stop within that bound is treated as an
unclean exit, which is still safe.

### 2.3 Automatic resume (recommendation)

- Downloads paused **by the user** never resume on their own.
- Downloads paused **by the app closing, a crash, the network or a reconnected library disk**
  resume on their own: at startup for the first two, and when the cause clears for the last two
  (a network probe with backoff, starting at 15 s and capped at 5 min; a library root that
  reappears on the existing library refresh).
- One setting, `Reanudar las descargas al abrir decanter` (default on), turns off the startup
  case for people on metered links. Network and disk recovery always resume, since the user never
  asked for those pauses.
- A full disk does **not** resume on its own. The user frees space and presses *Reanudar*, which
  re-checks first.

Steam behaves this way; "ask once at startup" adds a dialog to every boot after a crash, for an
answer that is almost always yes.

### 2.4 GOG publishes a new build while paused

The signed chunk URLs expire (`expires_at` in the secure-link parameters, `content.rs:190`), so
every resume has to plan again anyway. At resume:

1. If the download was **pinned** to a build (rollback, `build_id` in `releases.rs:236`), plan
   that same build. If GOG no longer offers it, the row becomes `Failed` with
   `GOG ya no ofrece la versión <id>`. Nothing is deleted.
2. Otherwise plan the branch's current build. If its id equals the journal's, carry on.
3. If it differs, **keep what still matches**:
   - A finished file whose chunk list (`md5`, `size` in order) is identical in the new plan is
     kept as is.
   - An in-progress file keeps the longest *prefix* of chunks that match, and is truncated to
     that offset.
   - Everything else is deleted from staging. The row shows
     `Versión nueva de GOG: se aprovechan 9,8 GB de 12,3 GB` for one refresh and carries on.
   - This never asks. The download was always "install the branch's current build", and the
     update check would offer the new build straight after anyway.
4. A DLC follows its base game's build, as now (`mod.rs:122-137`). If the base game changed
   build while the DLC was paused, the DLC re-plans against the build on disk.

### 2.5 Disk space

- Before the first byte, and again at each resume, compare `remaining = total − committed` plus a
  margin with the free space of the staging root (`decanter_core::disk::disk_usage`,
  `crates/core/src/disk.rs:24`). The margin is the larger of 1 GiB and 2 % of `total`.
- If space is short, the download does not start: `Paused{disk full}` with the shortfall.
- For an **update**, `remaining` is the whole new build, because staging holds a full second
  copy until the move. Say so in the message:
  `La actualización necesita 41 GB libres mientras se descarga`.
- No preallocation. `fallocate` on a 50 GB install is quick on ext4 and btrfs but not on exFAT or
  NTFS (Deck SD cards and Windows), and it would make staging's size on disk lie about progress.
  `ENOSPC` while writing turns into `Paused{disk full}`, not `Failed` (§3.6).

### 2.6 Cancel and delete partial data (#1133)

- **Cancelar** on a running, queued or paused download opens a confirmation:
  `¿Cancelar la descarga de <juego>? Se borrarán 12,3 GB ya descargados.` with
  `Cancelar y borrar` / `Seguir`.
- The confirmation is per #1133: removing anything decanter manages asks first. Partial data that
  decanter itself created in `downloading/` is the **one** thing removed without a further prompt,
  and only as the result of that confirmed user cancel. Nothing deletes partial data on its own:
  no expiry, no "failed so we clean up", no "started again so we wipe".
- A download that is *not* cancelled keeps its data indefinitely. After 14 days paused (owner's
  decision, §6), Storage and Downloads show a notice
  (`Descarga en pausa desde hace 14 días · 12,3 GB`) with **Reanudar** and **Eliminar**, the
  latter behind the same confirmation.
- Storage gets a line *Descargas sin terminar* per root, with the total size of `downloading/`
  and a list of the rows.
- Uninstalling a game with a paused update asks once for both
  (`Se borrará también la actualización en pausa (12,3 GB)`).

### 2.7 Bandwidth limit

There is no limiter today; a grep for one finds nothing. Proposal (phase 5, independent):

- One process-wide token bucket shared by every download's body reads, set in Ajustes →
  Descargas as `Límite de velocidad` (off, or MB/s). It is stored in the database's settings
  table, since it is a preference of the machine, not of the library.
- Pausing is not a limit of 0. A pause stops the tasks and drops the connections, because the
  signed URLs expire and an idle connection gets nothing.
- A paused download frees its share of the budget at once.
- The rate shown stays the measured one (`rate.rs`), so it reads as the limit while the limit
  binds.

### 2.8 One download at a time

Today every install runs at once, with 4 files each. Pause and resume make a queue natural:
**one active download, the rest Queued** (owner's decision, §6), with *Pausar* on a running row
letting the next one start. The 4-file concurrency inside a download stays.

## 3. Crash-safe persistence

### 3.1 Where

Everything stays on the same root as the bytes, inside the existing staging folder, so a library
move takes it along, a different disk is never needed for the final rename, and Storage can size
it:

```text
<root>/downloading/<account>/<product>/        one per (account, product, kind)
  download.json        plan header, written once per plan (tmp + fsync + rename + fsync dir)
  progress.log         append-only records, fsynced at commit points
  progress.snap        optional compacted snapshot (tmp + rename), see 3.3
  lock                 flock while a process owns this download (pattern of package.rs:388-398)
  files/               the staged tree, final relative paths
    Game_Data/level1.assets.part      in progress
    Game_Data/level0.assets           complete and hash-checked
```

A repair keeps its `<product>-repair` folder, with the same layout under it.

`files/` is what `move_into_library` walks, so the journal is never moved into the game. The
location the task suggested, `<root>/decanter/downloads/…`, would split the journal from the bytes
it describes and put two folders in play for one download. `downloading/` is already documented as
decanter's staging (`docs/data-in-library.md`, "Already inside the library").

### 3.2 What is stored where

| Data | Where | Why |
|---|---|---|
| The plan: kind (install / dlc / update / switch / repair), account, product, parent, title, branch, pinned build id, resolved build id, install dir name, DLC still to install after the game (the `dlcs` of `InstallRequest`), and per file: relative path, executable flag, chunks (`md5`, `compressed_md5`, `size`) | `download.json` | Has to sit beside the bytes it describes and survive a lost or moved database. Chunk URLs are **not** stored: they expire and are re-signed at resume |
| Progress: committed chunks per file, finished files, pause reason, phase (`downloading` / `moving` / `recording`) | `progress.log` | Append-only and cheap to sync |
| User pause flag | `progress.log` (`Paused{user}`) | It belongs to the download; a reinstall of the database must not resume something the user paused |
| Auto-resume setting, bandwidth limit, queue concurrency | database settings (`crates/core/src/db/settings.rs`) | Machine preferences |
| The list of downloads | none: derived by scanning `downloading/*/*/download.json` in each root | One source of truth; no row that can disagree with the folder |

`download.json` for a 50 GB game: GOG chunks are up to about 1 MiB inflated (as gogdl reads the
manifests; GOG does not document it, so confirm against one real manifest). That gives roughly
50 000 chunks × ~110 bytes of JSON ≈ 5–6 MB, written once per plan. That is acceptable. A
compact binary form is not worth it.

### 3.3 The progress log

- JSON Lines, one record per line, each ending in a CRC32 of the line (`{…}\t<crc>`), so a torn
  last line is detected and dropped at read time.
- Records:
  - `Commit{file, chunks: n, bytes: offset}`: the first `n` chunks of `file` (`offset` bytes) are
    on disk and synced.
  - `FileDone{file}`.
  - `Paused{reason, at}`.
  - `Resumed{at, build}`.
  - `Replanned{from_build, to_build}`.
  - `Moving`.
  - `Recorded`.
- Compaction: after about 10 000 records, write `progress.snap` (the folded state) with
  tmp + fsync + rename, then truncate the log. Reading folds the snapshot first, then the log.

### 3.4 Writing order and fsync points

Per file in flight:

1. Open `files/<path>.part`. On resume, truncate it to the last committed `bytes` (§3.5), then seek
   to the end.
2. For each chunk:
   1. Download the raw bytes and check `md5(raw) == compressed_md5`. This catches transport
      corruption before any work is spent on the chunk.
   2. Inflate, check `inflated.len() == size` and `md5(inflated) == md5`.
   3. Append to the file.

   A mismatch is a transport error for the retry policy (§3.6). This adds the per-chunk
   verification missing today (§1.1).
3. **Commit point:** every 64 MiB written to the file, or every 5 s, whichever comes first:
   1. `File::sync_data` on the `.part` file.
   2. Then append `Commit{file, n, offset}` to `progress.log` and `sync_data` it.

   The order guarantees that no record ever names bytes that are not durable.
4. On the last chunk: `sync_data`, rename `.part` → final name, fsync `files/<dir>`, then append
   `FileDone` and sync the log.

Cost: one `fsync` per 64 MiB per file, plus one on the log. For 50 GB that is about 800 data syncs
and 800 log syncs, compared with 50 000 if every chunk were synced. A crash loses at most about
64 MiB × 4 files of work.

The task suggested writing every chunk as its own `.part` and renaming it after the hash check.
That is not proposed here: 50 000 extra files and renames per install, a second copy of every
byte to assemble the file, and no gain over "truncate to the last committed offset", given that
chunks are written in order.

### 3.5 Resume and how much to re-validate

At resume, under the `lock` file:

1. Fold `progress.snap` and `progress.log`, discarding a torn tail.
2. **Clean pause** (the last record is `Paused{user|app closed|network|disk}`): trust the log.
   Truncate each `.part` to its committed `bytes`. A `.part` shorter than that means the bytes
   were lost despite the sync (an unsafe unplug, a lying disk), so drop that file's commits.
   Check each `FileDone` file's size against the plan; a wrong size reopens it from zero. No
   hashing.
3. **Unclean exit** (no final `Paused`): do everything in step 2, and also re-hash the last
   committed window of every in-flight `.part` (the chunks since the previous commit point,
   ≤ 64 MiB each) and the last chunk of each file finished in the last minute. A mismatch moves
   that file back to the last chunk that still matches.
4. Re-plan (§2.4), then continue.

**Cost model** for a full re-hash of 50 GB, estimated, not measured. MD5 runs at roughly
0.5–0.8 GB/s per core on the Deck's Zen 2, so 4 files in parallel are bounded by the disk:

| Storage | Read rate | Full re-hash of 50 GB | Window re-hash (≤ 4 × 64 MiB) |
|---|---|---|---|
| Deck NVMe | ~1.5–3 GB/s (CPU-bound ~2 GB/s with 4 threads) | ~25–40 s | < 0.2 s |
| microSD (UHS-I) | ~80–100 MB/s | ~9–10 min | ~3 s |
| USB 3 HDD | ~120–150 MB/s | ~6–7 min | ~2 s |

A full re-hash at every resume would cost minutes on SD and HDD, where most Deck games live, for
data that was hash-checked chunk by chunk when written (§3.4) and synced before being recorded.
**Recommendation:** use the window re-hash only. The existing *Verify* in the game's Local files
stays the full check, and can be offered once after an install that resumed from an unclean exit.

### 3.6 Errors, mapped to pause or fail

| Error | Today | Proposed |
|---|---|---|
| Connect or read timeout, reset, 5xx, 429, hash mismatch | 3 tries, ~1.5 s backoff, then fail and delete | Per chunk, exponential backoff 1 → 30 s, for up to 60 s. Then `Paused{network}` with auto-resume (§2.3) |
| 401/403 on a chunk (signed URL expired) | Fail | Re-sign once (a new `secure_link`, which needs a token refresh), retry. A second refusal → `Failed` |
| Token refresh refused (session gone) | Fail | `Paused{network}`, plus the existing session-renewal prompt (`app/src/views/session_renewal.rs`) |
| `ENOSPC` / quota | Fail and delete | Commit what is synced, then `Paused{disk full}` |
| Library root gone (`ENOENT`/`EIO` on staging, root no longer a dir) | Fail and delete | `Paused{library unavailable}`. Resume when the root comes back, consistent with #1257 (no switching libraries in silence) |
| Build gone, depot gone, unsafe manifest path | Fail | `Failed`, data kept until the user cancels |
| User cancel (confirmed) | Fail and delete | Remove the download folder |

`download_into_staging` stops deleting staging on failure (`mod.rs:369-382`). The staging wipe at
the start of `install_into` (`mod.rs:218-225`) becomes "resume if there is a journal for this
plan's kind, otherwise refuse to start over it until the user cancels the old one". A new
install of the same game while an update is paused offers *Reanudar* instead.

### 3.7 Moving into the library

1. Append `Moving` and sync.
2. `move_into_library` renames what remains under `files/`. It is idempotent, because a moved
   file is no longer in staging.
3. Write the appmanifest, append `Recorded`, deploy the hook, then remove the download folder.

A resume that finds `Moving` without `Recorded` finishes the move and records the manifest. It
never re-downloads, and never leaves an update half-applied under the old build id (§1.1). A
`.part` never gets as far as the move; `move_into_library` skips any it finds and logs a warning.

### 3.8 Accounts, roots, unplugged disks

- The key is `(account, product, kind)`, and staging is already per account (`staging.rs:31`).
  The UI map is re-keyed the same way (phase 0).
- Each library root has its own `downloading/`. The startup scan covers every root of the active
  profile (`Library::roots`, `library.rs:634`). A download for another profile is listed only when
  that profile is active.
- Two accounts installing the same `InstallDir` at once would race on `common/<dir>` today, with
  or without this change. The queue (§2.8) makes it impossible in practice; a guard can come later.
- An unplugged disk mid-download gives `Paused{library unavailable}` (§3.6). Bytes already synced
  survive. Bytes in the page cache may not, which is what the truncate-to-committed check at
  resume handles. The row stays listed, with the reason, until the disk returns or the user
  cancels. Cancelling while the disk is away only forgets the row; the folder is removed on the
  next start that sees the disk, after the user confirms again.

## 4. What goes first, and what is shared

1. **GOG installs, updates, DLC, branch switches and repairs.** These are tens of GB, and the
   chunk is the natural unit. GOG chunks are small (≤ ~1 MiB inflated), so an interrupted chunk
   is simply fetched again: no `Range` request against the CDN.
2. **psport Windows package and Linux bundles.** Up to 512 MiB and ~50 MB, single files from
   GitHub releases.
3. **GE-Proton, nix-portable, umu-run, the self-update.** Small or rare. The self-update
   verifies a minisign stream, so a resume re-feeds the existing prefix from disk into the
   verifier before appending.

Out of scope: port prebuilts (nix substitution), Raven (`git fetch` plus nix), cloud saves. Cloud
saves need the separate atomic-write fix noted in §1.2.

**Shared code.** There are two components, because the units differ:

- `crates/gog/src/install/journal.rs` (new): the header, log, snapshot and fold, used only by GOG.
- `decanter_runtime::download` (`crates/runtime/src/download.rs`) gains a resumable `fetch`:
  - a stable `.part` name plus `lock` (the `package.rs` pattern), replacing the pid name;
  - a sidecar `<name>.part.json` with the URL, the server's `ETag` / `Last-Modified`, and the
    expected digest and size;
  - on resume, re-hash the existing prefix, which is needed anyway because the digest is over the
    whole file and costs < 1 s for these sizes, then send `Range: bytes=<len>-` with
    `If-Range: <etag>`. A `206` appends; a `200` starts over;
  - `sync_data` every 64 MiB and on pause.

  `crates/ps/src/download.rs` and the runtime callers keep their signatures and gain only a
  cancel token. The rule is in [RFC 9110 §14 (Range requests)](https://www.rfc-editor.org/rfc/rfc9110#section-14)
  and [§13.1.5 (If-Range)](https://www.rfc-editor.org/rfc/rfc9110#section-13.1.5), and reqwest
  sends arbitrary headers ([reqwest 0.13 `RequestBuilder::header`](https://docs.rs/reqwest/0.13.5/reqwest/struct.RequestBuilder.html#method.header)).
- Durability uses [`tokio::fs::File::sync_data`](https://docs.rs/tokio/1.54.1/tokio/fs/struct.File.html#method.sync_data).

**Range support is unverified.** Agents may not touch the network. GitHub serves release assets
from `release-assets.githubusercontent.com` / `objects.githubusercontent.com` (the hosts allowed
in `runtime/src/download.rs:14-18`); both are expected to honour `Range` and send an `ETag`, but
that has to be checked. The owner can check with:

```sh
curl -sSIL -H 'Range: bytes=0-0' https://github.com/GloriousEggroll/proton-ge-custom/releases/download/<tag>/<asset>.tar.gz
```

Expect `206 Partial Content`, a `Content-Range` and an `ETag` on the last hop. If one host
answers `200`, the component already handles it by starting over, and this whole section degrades
to today's behaviour. GOG's CDN needs no check, since chunks are the unit.

## 5. Risks, phases and tests

### 5.1 Risks

- **Silent corruption from trusting the log.** Mitigated by per-chunk MD5 at write (new), sync
  before record, truncate-to-committed at resume, the window re-hash after an unclean exit, and
  the existing full *Verify* as the backstop.
- **Disks that lie about fsync** (some USB bridges, SD cards). Synced bytes can still be lost on
  power loss. The `.part` length check and the window re-hash catch the common shape (a short or
  zero tail). A lie deeper than 64 MiB would only be caught by *Verify*.
- **Build drift.** Keeping matching chunk prefixes across builds relies on GOG's MD5s per chunk.
  These are what `verify` already trusts, so this adds no new trust.
- **Two decanter processes** (GUI plus `decanter install` from the CLI, `app/src/cli/install.rs`)
  could resume the same download. The `flock` on `lock` decides; the loser shows the row as
  "en curso en otro proceso".
- **Old leftovers.** Staging folders from before this change have no `download.json`. Treat them
  as unknown partial data: list them in Storage with their size and **Eliminar** (with
  confirmation). Never resume or delete them on your own.
- **More disk held for longer.** Paused updates hold a full second copy of the game. Mitigated by
  Storage showing it (§2.6) and the space message (§2.5).
- **Behaviour change in `decanter_core`.** The "no partially-installed state" note
  (`library.rs:66-70`) stays true for `common/`, but not for `downloading/`. Update the comment
  with phase 2.

### 5.2 Phases (each shippable alone)

| # | Phase | Files | Size (est.) |
|---|---|---|---|
| 0 | Every GOG download through one runner, visible and cancellable in Downloads, keyed by `(account, product)`; DLC-after-game with progress | `app/src/state/background.rs`, `app/src/views/library/install.rs`, `app/src/views/downloads.rs`, `app/src/views/library/properties/{updates,dlc,releases,local_files}.rs`, `crates/gog/src/install/verify.rs` (cancel and progress for repair) | 250–350 lines |
| 1 | Per-chunk `compressed_md5` / `md5` / `size` checks while downloading | `crates/gog/src/install/{plan.rs,download.rs}` | 60–100 lines |
| 2 | Journal and resume engine: layout, header, log, commit points, `.part` files, resume, `Moving` phase, network errors → pause, keep staging on failure, `pause(account, product)` / `resume(...)` API | `crates/gog/src/install/{journal.rs (new), download.rs, mod.rs, staging.rs, tracking.rs, verify.rs}`, `crates/core/src/library.rs` (comment) | 700–1000 lines |
| 3 | UI: Pausar/Reanudar/Pausar todo, `Paused{reason}`, startup scan, clean-quit pause, auto-resume setting, cancel confirmation, Storage line, en/es strings | `app/src/state/background.rs`, `app/src/views/{downloads.rs,storage.rs,mod.rs}`, `app/src/views/settings/…`, `crates/core/src/db/settings.rs`, `app/locales/*` | 500–700 lines |
| 4 | Build drift at resume, free-space check, `ENOSPC` and unplugged root → pause, single-active-download queue | `crates/gog/src/install/{mod.rs,plan.rs,journal.rs}`, `app/src/views/library/install.rs` | 250–400 lines |
| 5 | Bandwidth limit (token bucket plus setting) | `crates/gog/src/install/download.rs`, `crates/runtime/src/download.rs`, settings view and db | 150–250 lines |
| 6 | Resumable single-file `fetch` (stable `.part`, sidecar, `Range`/`If-Range`, lock, sweep), adopted by psport package and bundle, then GE-Proton, nix-portable, umu-run (moved into the library) and the self-update | `crates/runtime/src/download.rs`, `crates/ps/src/{download.rs,os/linux/bundle.rs,os/windows/package.rs}`, `crates/runtime/src/os/linux/{ge_proton,umu}.rs`, `crates/nix/src/portable.rs`, `app/src/self_update.rs`, `app/src/state/ps/package.rs` (pause and cancel in the PS section) | 400–600 lines |

Phase 0 and phase 1 are worth shipping even if the rest waits: today an update, a DLC toggle or a
repair cannot be seen or stopped, and the download trusts bytes it never checked.

### 5.3 Test plan

Agents never touch online services. Everything below is either the owner's hand test or runs
against `127.0.0.1`.

**Local fake content server, no Internet.** Add a dev override, in the spirit of
`DECANTER_PS_BUNDLES` (`crates/ps/src/download.rs:21`), for example `DECANTER_GOG_CONTENT`. It
points the builds, build-manifest, depot-manifest, secure-link and chunk hosts at a local server
that serves a folder laid out like the content system, generated by a script from any directory:
it builds zlib chunks of ~1 MiB, the MD5s and the manifests. The server injects faults on command:

- drop the connection at byte N of chunk K;
- answer 500 or 403 once;
- stall for 40 s;
- serve one corrupted chunk;
- throttle to X KB/s;
- switch to "build 2" (a second tree sharing most chunks).

A script then loops: start `decanter install` on the CLI against a scratch library, `kill -9` it
at a random moment, start it again (resume), and repeat 50 times. At the end, compare `common/`
byte for byte with the source tree, and check that `downloading/` is gone. The same harness
checks:

- an `ENOSPC` pause, using a small loop-mounted or tmpfs library root;
- an unplugged root, by unmounting the loop device mid-download;
- build drift, by switching to build 2 while paused and checking that only the changed chunks are
  fetched again (the server counts requests);
- the bandwidth limit, by measuring the rate.

For phase 6, the same server must honour `Range` and `If-Range` and be able to change its ETag.

**Owner's hand tests** (real GOG and GitHub, on this machine by day only if no game run is
needed; nothing here runs a game):

1. Install a ~5–10 GB game, *Pausar* at ~30 %, wait 2 min, *Reanudar*: the bytes carry on from
   ~30 %, and *Verify* afterwards is clean.
2. Same game: `kill -9` decanter mid-download, start it again: the row shows *se cerró de forma
   inesperada* and resumes on its own (setting on) or waits (setting off).
3. Reboot the machine (or pull power on a test box) mid-download, then *Verify* after it
   finishes.
4. Unplug the network for 30 s (the retry window): the download carries on alone. Then for 5 min:
   the row shows *Sin conexión*, and resumes when the network comes back.
5. Install onto a nearly full SD card or USB disk: the free-space message appears before the
   start. Fill the disk during a download: *Disco lleno*, then free space and *Reanudar*.
6. Unplug the USB library disk mid-download: *La biblioteca … no está disponible*. Plug it back
   in: it resumes.
7. Start an update, pause it, wait for GOG to publish a newer build (or switch a beta branch):
   *Reanudar* reports the reused GB.
8. *Cancelar* on a paused download: the confirmation shows the size, the folder is gone from
   `downloading/`, and Storage no longer counts it.
9. Quit decanter cleanly with a download running and restart it: *Decanter se cerró*, then an
   automatic resume.
10. Phase 6: pause and kill a GE-Proton download and a psport Windows package download, resume,
    check the logged `206`, and check that the checksum passes.

## 6. Decisions for the owner

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | Automatic resume | (a) resume everything not user-paused at start; (b) ask once at start; (c) never | **(a)** plus a setting (default on). Network and disk recovery always resume |
| 2 | How long partial data is kept | (a) forever, with a notice after N days; (b) auto-delete after N days; (c) delete on any failure as now | **(a), N = 14.** Deletion only after a confirmed user action (#1133); auto-delete contradicts it |
| 3 | Verify depth at resume | (a) trust the log after a clean pause, re-hash the last window after an unclean exit; (b) re-hash everything every time; (c) sample N random chunks too | **(a)**, plus per-chunk MD5 when written (phase 1). Full *Verify* stays a button. (b) costs 6–10 min per resume on SD or HDD for a 50 GB game |
| 4 | Pause while Queued | yes / no | **Yes**: it means "skip until I say", and costs nothing |
| 5 | Concurrent downloads | (a) one active, the rest queued; (b) all at once as now | **(a)**, 4 files inside it as now |
| 6 | Retry window before pausing on network errors | 30 s / 60 s / 5 min | **60 s** |
| 7 | New GOG build while paused | (a) carry on with the new build, reusing matching chunks; (b) ask; (c) discard and restart | **(a)**; pinned builds stay pinned |
| 8 | Bandwidth limit | now (phase 5) / later / never | **Later**: after phases 0–4 |
| 9 | Where the journal lives | `<root>/downloading/<account>/<product>/` (beside the bytes) / `<root>/decanter/downloads/…` | **`downloading/`**, as documented in `docs/data-in-library.md` |
| 10 | Old staging leftovers without a journal | list in Storage with *Eliminar* (confirmation) / delete on upgrade | **List**, never delete on our own |
