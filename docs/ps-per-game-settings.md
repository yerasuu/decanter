# PS section: per-game settings

Status: accepted (kb #1275, D1-D9). Analysis of `develop` at `123e5ab` and of the bbport checkout
under `<library>/psport/ports/bbport` (psport with `psport.ini`). Phases 1 (per-title ini,
`crates/ps/src/bbport/port_ini.rs`), 2 (per-title launch settings,
`crates/ps/src/bbport/settings/title.rs`), 3 (online and system modules switches per game), 4
(mods and patches per game, `crates/ps/src/bbport/mods.rs`, `ModFiles`) and 5 (defaults for new
games, copy, export and import, reset, `decanter ps settings`; `crates/ps/src/bbport/settings/`) are
implemented, and phase 6a (below); phase 6b (layouts catalog, migration step 4, GTK leftovers) is not
yet.

Phase 2 as built: the shared keys are an allow-list (`SHARED` in `settings/title.rs`): E1-E3, plus
`mods_dir`/`patches_dir` (until phase 4, which made them each title's). A title's file is made from the shared one the first time
it is needed, after a backup of the shared one, and the shared file is not rewritten (step 4 below
waits for phase 5): its global keys are what a new title copies until the defaults page exists, and
what an older port's launches read (D6).

Phase 3 as built: `online`, `online_upnp`, `online_bbsc` and `online_bbsc_local_server` left
`SHARED` and are each title's; the account (`online_npid`, `online_server`, `online_webapi`,
`online_community`) and the dump folders stay shared (E1, E2). A title's file carries `format: 2`;
one made by phase 2 (no `format`) takes those four keys from the shared file once, the first time
it is read, where it lacks them, so each game plays online exactly as before. The switches sit in
the game's Online tab, now shown for every bbport title, not only Bloodborne; Ajustes keeps the
account and its servers. The system modules switch is `sys_modules` in the title's file, shown in
Graphics › Extras; absent, it is on for every title but Bloodborne (D7), so nothing changes until
the user turns it. An older port's launches, with no title file, still read the shared file for all
of these.

Phase 4 as built: everything about a game's mods and patches is the title's, the folders included
(`ModFiles::of_title`): `titles/<TITLE_ID>/mods.json` and `patches.json` are copies of the shared
files made the first time the title's Mods tab or launch asks for them, after a backup of the shared
pair into `backups/per-game-settings-<date>/` before the first title takes its copy (a shared file
that does not exist is not copied, and the title's is written on its first change); the shared files
are never rewritten. `mods_dir` and `patches_dir` left `SHARED`; a title's file carries `format: 3`,
and one of an earlier format (or made now) takes the shared folder setting once, or, when that was
never set, the shared data folder's `mods/` or `patches/` where it exists, so the mods a user already
had keep loading in every game that loaded them; otherwise a title's folders default to `mods/` and
`patches/` inside its own folder. Mods are not copied or moved: two titles can still point at one
folder and keep separate order and switches. `mods-effective/` and `mods-effective.json` are built in
the title's folder at each launch, and `PS_PATCHES_DIR`/`PS_PATCHES_CONFIG` come from the title. The
Mods tab shows the selected title's and says whose they are; an older port or an unusual title id
keeps the shared files, as before.

Phase 5 as built (kb #1311):

- **Defaults for new games** are an explicit template, `defaults/settings.json` and
  `defaults/psport.ini` (`settings/defaults.rs`, `Bbport::defaults_ini`). They are made the first
  time they are needed from what a new title copied until then: the shared file's keys a title
  inherits (not `SHARED`, not the GTK leftovers, not the keys that only ever meant one title, no
  `titles` map) and the shared ini without `menu_language`, so nothing changes that day. A title
  seen for the first time now copies them, with its own `titles.<TITLE_ID>` entries of the shared
  file on top and the former mods and patches folders as phase 4 gave them, and records
  `created_from: defaults`. Absent keys keep meaning the default: absent `online_allow_plaintext` is
  on (#1303), so the template made from a shared file without it allows the plain-text login. The
  shared files are still never rewritten.
- **Defaults page** in PS › Ajustes (`app/src/views/ps/defaults.rs`): the Graphics rows (the
  Graphics view in a defaults mode, without the title's effects list, frame generation or extras),
  the online switches (play online, UPnP, TLS, plain-text login), and "take the defaults from a game"
  for everything else (keyboard, pad layout, extras), which asks first and backs the defaults up.
  The defaults' mods and patches folders are never taken from a game, so a new game keeps its own.
- **Categories** (`settings/keys.rs`, `Category`), one per tab: graphics (every launch key and ini
  line not below, mouse and keyboard-as-stick included), keyboard (`key.*`), gamepad (`pad.*`,
  `gamepad`, `gamepad_name`, `button_icons`), mods (`mods_enabled`, `mods_dir`, `patches_dir`,
  `mods.json`, `patches.json`), online (`online`, `online_upnp`, `online_bbsc`,
  `online_bbsc_local_server`, `online_tls`, `online_allow_plaintext`, `local_player`), extras
  (`extra_env`, `sys_modules`). Never any: the `SHARED` keys (E1-E3), a file's own record
  (`format`, `created_from`, `created_on`), the GTK leftovers, `menu_language`.
- **Copy from another game** (`per_game::copy`), all of it or one category, on the game's Resumen
  (`app/src/views/ps/games/title_settings.rs`): the category's keys of the destination are replaced
  by the source's, so it ends as the source is; asks first. Copied, never linked.
- **Reset to the defaults** (`per_game::reset`): every category but mods becomes a copy of the
  defaults (the defaults hold no mod choices, so the game's mods and patches stay); asks first;
  records `created_from: defaults`.
- **Export and import** (`settings/transfer.rs`): one JSON file (`kind: decanter-ps-settings`,
  `version: 1`, `title_id`, `settings`, `ini`, `mods`, `patches`), never a `SHARED` key and never a
  shadNet secret (`extra_env` items named like one are dropped). An import is checked whole before
  anything is written: at most 256 KiB, no unknown field, `title_id` as `^[A-Z]{4}[0-9]{5}$` (or
  `defaults`), launch keys against an allow-list with typed values (the settings table's switches,
  numbers in range and choices among its options, frame generation, local server, local player,
  pad GUID, icon style, `extra_env` as `NAME=value` items with no shadNet secret), `mods_dir` and
  `patches_dir` only as a full path inside the library, ini keys of the port's shape
  (`[A-Za-z0-9_.]`, at most 64) with one-line values and the settings table's keys typed, mod and
  patch names that can't leave their folder. Another title's file asks "apply to this game" first
  (CLI: `--any-title`).
- **Backups**: every copy, reset or import backs the destination's `settings.json`, `psport.ini`,
  `mods.json` and `patches.json` up first into
  `backups/per-game-settings-<date>/<TITLE_ID or defaults>-<HHMMSS>/` (#1133), a new folder each
  time.
- **CLI** `decanter ps settings list|get|set|copy|reset|export|import` (`app/src/cli/ps_settings.rs`),
  `--title <TITLE_ID>` or `--title defaults`; `set` checks as an import does and an empty value
  removes the key; `copy`, `reset` and `import` need `--yes` and say what they would do without it;
  `export` never replaces a file. Exits 0 when done or shown, 1 when a file could not be read or
  written, 2 when the request was refused.

Phase 6a as built (kb #1343):

- **Apply to all games** (D9), on a game's Resumen and as `decanter ps settings copy --from <TITLE>
  --to all [--category ...]`: one category of the game's settings, or all, copied over every other
  game of the port the library lists (`per_game::plan_to_all`, `per_game::copy_to_all`; several
  folders of one title are one game). The question first says how many games and which categories;
  each game is backed up as a copy is (`<TITLE_ID>-<HHMMSS>/`), a game that fails is reported and the
  rest still get theirs. Never a `SHARED` key and never the destination's `mods_dir`/`patches_dir`
  (each folder holds its own game's mods); the online category goes only to games whose eboot names
  an NP online library (`online_support`, #1340), and the rest are listed as left without it. Its
  mod and patch choices go with the mods category, as a copy carries them.
- **Forget this game's settings** (`per_game::forget`, CLI `ps settings forget --title <TITLE>
  --yes`): asks, backs the title's files up, removes `titles/<TITLE_ID>/settings.json` and
  `psport.ini` (and the folder once empty), so the next open or launch makes them again from the
  defaults. Its `mods.json`/`patches.json` stay, as a reset keeps them; the shared file's
  `titles.<TITLE_ID>` entries go on top again, as for any title seen for the first time.
- **Copy settings from another game** on each tab: Graphics (with a second entry for Extras, which
  sit in that tab), Keyboard, Gamepad, Mods and Online open the Resumen's copy row inside the tab,
  with that tab's category picked; the tab reads its files again after a copy.
- **Older port note** on the defaults page (`per_game::port_reads_shared`): a checkout whose
  `run.sh` doesn't name `psport.ini` says that its games still read the shared files, neither their
  own nor these defaults.
- **Ini keys against the port's list**: psport lists no settings of its own (`--list-settings`
  doesn't exist; its ini keys are spread over `psport_settings.cpp` and `runtime_config.c`); what it
  lists is each title's own switches (`--list-effects --title`, the `effect_*` keys and
  `skip_intro`, `debug_camera`, `debug_menu`). `ps settings set` of a switch for a title is refused
  when the port is built here and doesn't list it (`Refusal::NotListed`); `import` names such keys
  but still imports them, since every title's ini began as the shared one, which carries
  Bloodborne's switches for every game, and a game's own export would otherwise be refused. Any
  other ini key keeps the shape checks and the settings table's typing, and the command says when
  only the shape was checked (`per_game::ini_check`, `IniCheck`).
- **Keyboard and controller for new games**: the defaults page shows the Keyboard view on
  `defaults/psport.ini` and the Gamepad view on the defaults (`GamepadView::show_defaults`; button
  icons and the controller go to `defaults/settings.json`); the layouts catalog stays shared (D4).
  Extras are still taken from a game.

Left for phase 6b: the layouts catalog, the GTK leftovers, and migration step 4 (rewriting the shared
file with the E1-E3 keys only), still waiting since an older port's launches read the shared file.

## The owner's rule

Every setting a user can change for a game belongs to that game alone: keyboard, controller,
graphics, audio, online, mods, patches, extra environment, language, anything on a game's page or in
the PS settings. No exceptions by category. Only what is not a game's configuration stays shared
(ports and toolchain, the library, the account identity), and each such exception is justified
below. A game seen for the first time starts from the user's chosen defaults, **copied once**, never
live-linked, so a later change to the defaults reaches no game that already has its settings.

## How it works today

All PS launch configuration lives in one folder per library, `<library>/psport/bbport/`
(`Root::bbport_data`, `crates/ps/src/paths.rs:79`, documented there as "Shared by every profile"):

| File | Written by | Read by |
|---|---|---|
| `settings.json` (`LauncherSettings`, `crates/ps/src/bbport/settings.rs:14`) | Graphics, Gamepad, Mods, Online, Extras tabs; PS Ajustes; port selection on Windows | `Bbport::game_env` turns it into `PS_*` variables (`crates/ps/src/bbport/mod.rs:376-473`) |
| `psport.ini` (`bbport.ini` for older ports) (`Bbport::ini_path`, `mod.rs:243`) | Graphics tab (`Store::Ini` rows, effects), Keyboard tab (`key.*`), Gamepad tab (`pad.*`), **the port's in-game menu** (`PsSettings::Save`, `gpu/port/psport_settings.cpp:405`), `forget_menu_language` | the port at start (`PS_CONFIG`, `src/runtime/runtime_config.c:34`), `run.sh` (`output_res`, `live_resolution`, l.169-215), decanter's launch (`aspect_ratio`, `app/src/ps_play/launch.rs:164-172`) |
| `mods.json`, `patches.json` (`crates/ps/src/bbport/tools.rs:185,224`) | Mods tab | `run.sh` / `direct::arguments` (`--mods-config`, `--patches-config`) |
| `mods-effective/`, `mods-effective.json` (`crates/ps/src/bbport/mods.rs:116,131`) | every Bloodborne launch, rebuilt | the port (`--mods`) |
| `learned.json` (`crates/ps/src/learned.rs:51`) | run scanner | launch; **already keyed by `TITLE_ID`** |
| `<library>/psport/layouts.json` (`crates/ps/src/layouts.rs:63`) | Keyboard and Gamepad tabs (named layouts) | same tabs |
| `<library>/psport/anyps5/<TITLE_ID>/anyps5-input.ini` | Keyboard tab for AnyPS5 games | AnyPS5; **already per title** |

`settings.json` has one per-title map, `titles.<TITLE_ID>`, holding only `frame_gen`
(`frame_gen.rs`), `local_player` (`local_player.rs:41`), `frame_stats` and `debug`
(`app/src/views/ps/games/runs.rs:146`). Everything else is one value for every game.

The in-game menu matters most: psport rewrites its keys in whatever file `PS_CONFIG` names on every
change (upscaler, preset, sharpness, effect switches, `model_lod`, `aspect_ratio`, `output_res`,
`live_resolution`, `menu_language`, `menu_pos`, ...). Because decanter never sets `PS_CONFIG`, that
file is the shared `psport.ini`. The port already works around it for one key only: it does not save
`frame_gen` when a launcher passed it, "since psport.ini is shared by every game and the value is
per game" (`psport_settings.cpp`, `Save`), and decanter always passes `PS_FRAME_GEN` for the same
reason (`mod.rs:439-445`).

## (a) Inventory

Scope today: **G** = one value for every game in the library, **T** = per `TITLE_ID`, **P** = per
decanter profile, **D** = decanter-wide (database or app settings). "Must be" applies the owner's
rule. Keys in `settings.json` are launcher keys (env at launch); keys in `psport.ini` are read by the
port at start and most are rewritten by its menu.

### Graphics tab (`app/src/views/ps/games/graphics.rs`, rows from `crates/ps/src/bbport/groups.rs`)

| Key | Stored | Today | Must be | Notes |
|---|---|---|---|---|
| `language` (game's system language, `PS_LANGUAGE`) | settings.json | G | T | `mod.rs:404` |
| `mods_enabled` | settings.json | G | T | `groups.rs:109`, `mods.rs:83,103` |
| `pc_model` | settings.json | G | T | `mod.rs:420`; ignored by single-memory-model ports |
| `output_res`, `aspect_ratio`, `live_resolution` | psport.ini | G | T | menu-written; `run.sh` reads the first and last; decanter reads `aspect_ratio` (`launch.rs:166`) |
| `fullscreen`, `present_mode`, `hdr`, `aniso` | settings.json | G | T | `mod.rs:407,409,446`, `optional_env` |
| `gamepad` (GUID) + `gamepad_name` | settings.json | G | T | `groups.rs:226`, saved with name `values.rs:148` |
| `game_mouse_sensitivity`, `mouse_sensitivity` | psport.ini | G | T | mouse group |
| `key_stick_opposite`, `key_stick_hold_ms` | psport.ini | G | T | keyboard-as-stick group (`groups.rs:297-330`) |
| `upscaler`, `preset`, `sharpness` (+ menu-only `sharpen`, `jitter`, `reactive*`, `object_motion`, `debug_view`, `show_fps`, `fsr4_*`) | psport.ini | G | T | menu-written |
| `model_lod`, `effect_*`, `skip_intro`, `debug_camera`, `debug_menu`, and every effect key the port lists for a title (`--list-effects`, `effects.rs`) | psport.ini | G | T | effect keys are free text from the port: two titles exposing the same key (`effect_ssr`, `effect_dof`) share one line |
| `intel_tonemap` | settings.json | G | T | |
| `frame_gen` | settings.json `titles.*` | T | T | already right; menu choice saved per title (`app/src/ps_play/run.rs:263-276`) |
| `fps_mode`, `fps_limit` | settings.json | G | T | `mod.rs:410,449` |
| `draw_pipe`, `readbacks`, `preupload` | settings.json | G | T | performance |
| `keep_logs`, `save_log`, `crash_diag`, `as_0_3`, `gpu_profile`, `vk_validation`, `camera_cuts`, `image_traps` | settings.json | G | T | development group |
| `frame_stats` | settings.json, global **and** `titles.*` | G ∪ T | T | `mod.rs:570` ORs them: the global switch forces it on in every game |
| `debug` | `titles.*` | T | T | plus decanter's own debug mode (D, `app/src/debug_logs.rs:67`): that one is the app's, kept |
| `extra_env` (Extras) | settings.json | G | T | `extras.rs:114`, applied to every launch `mod.rs:468` |
| `menu_language` | psport.ini | G | T | written by the menu; dropped on UI language change (`app/src/i18n.rs:186`) |
| `menu_pos` | psport.ini | G | T | overlay position, menu-written |
| `mangohud`, gamemode | decanter DB per game key (`launch.rs:342`) | T | T | already right; the `mangohud` key in settings.json is a leftover of the GTK launcher, read by nothing |
| `ui_language`, `game_dir`, `user_dir` | settings.json | — | drop | GTK-launcher leftovers, read by nothing (`game_dir`, `user_dir` are never read; `PS_USER_DIR` comes from the profile) |

### Keyboard, Gamepad, Mods, Online tabs

| Item | Stored | Today | Must be | Notes |
|---|---|---|---|---|
| Keyboard bindings, bbport (`key.<input>=`) | psport.ini | G | T | `app/src/views/ps/games/detail.rs:238`, `crates/ps/src/binds.rs:28` |
| Keyboard bindings, AnyPS5 | `anyps5/<TITLE_ID>/anyps5-input.ini` | T | T | already right |
| Gamepad layout (`pad.<input>=`) | psport.ini | G | T | `pad.rs:306` (`save_to_ini`), shown back by `pad::current` (`pad.rs:334`) |
| `button_icons` | settings.json | G | T | `gamepad/mod.rs:330-335`; also feeds the icons mod (`mods.rs:63`) |
| Named layouts catalog (`pad`, `bbport_keys`, `anyps5_keys`) | `psport/layouts.json` | G | T, see decision D4 | a catalog of templates; applying one copies its lines into the game's file, so editing a template changes no game. Owner's rule says per game; recommendation in D4 |
| `mods_dir`, `patches_dir` | settings.json | G | T | `app/src/views/ps/games/mods.rs:173` |
| Mod order and switches | `mods.json` | G | T | `tools.rs:185` |
| Patch switches | `patches.json` | G | T | `tools.rs:224`; keys are `<file>.xml/<patch>` so two copies or regions of one game share them |
| `mods-effective*` | data dir | G (regenerated) | T | generated; per title avoids two launches racing on it |
| `online` (on/off) | settings.json | G | T | set in PS Ajustes (`app/src/views/ps/online.rs:117`), read by every launch (`launch.rs:267`) |
| `online_upnp`, `online_bbsc`, `online_bbsc_local_server` | settings.json | G | T | per-game behaviour; `bbsc` is Bloodborne's extension |
| `local_player` | `titles.*` | T | T | already right |
| `online_npid`, `online_server`, `online_webapi`, `online_community`, keyring secrets | settings.json + keyring | G | **G (exception E1)** | identity |

### PS Ajustes and port data

| Item | Stored | Today | Must be | Justification |
|---|---|---|---|---|
| `sys_modules_dir`, `sys_fonts_dir` | settings.json | G (and `sys_modules_dir` only for non-Bloodborne, `mod.rs:425-431`) | folder **G (E2)**; whether a game uses it **T** | the folder is a resource (where the user's firmware dump is), like the toolchain; the choice to load it is the game's |
| Port build selection (`SELECTED_KEY`, `PINNED_KEY`, `app/src/state/ps/package.rs:414`), `psport_exe_dir` (`direct.rs:629`) | settings.json | G | **G (E3)** | ports and toolchain |
| Toolchain, checkouts, bundles, `fsr4_411/`, `icons/` (built icon cache) | `psport/` | G | **G (E3)** | resources and caches, no user choice |
| `learned.json` | data dir | T | T | already per title |
| Saves, trophies (`profiles/<id>/bbport/user`) | per profile | P (and per title inside: `savedata/1/<TITLE_ID>`) | unchanged | game data, not configuration |
| Game cache (`PS_CACHE_DIR`) | next to the dump | T | unchanged | |
| Logs (`bbport/logs`) | data dir | G | unchanged (E4) | output, not configuration |

Exceptions, justified:

- **E1 Online identity**: the NPID and its secrets are who the user is on shadNet, not how a game
  runs; per-game copies would put the same secret in several places and drift on a password change.
  The server addresses go with the identity (one account lives on one server). Whether a game plays
  online is per game.
- **E2 Console dump folders**: a path to the user's own firmware dump, with no per-game value; a
  game picks whether to load it.
- **E3 Ports, toolchain, builds, caches**: what runs games, not a game's configuration.
- **E4 Logs**: written by runs; already named per run and gathered per title by the run view.

## Where mixing happens today

Each: change X in game A, start game B, B gets X.

1. **Graphics rows saved to `settings.json`** (`graphics.rs:194-200` → `values::save`,
   `crates/ps/src/bbport/values.rs:140-152`). Set `present_mode=Fifo` or `fps_limit=30` on Ryza's page;
   Bloodborne starts with Fifo / 30 fps (`mod.rs:409,449`). The module doc says so outright:
   "every Bloodborne copy shares them" (`graphics.rs:1-4`).
2. **Graphics rows and effects saved to `psport.ini`** (`graphics.rs:204-210` → `values::save_ini`,
   `values.rs:155`). Turn `skip_intro` or `effect_ssr` on for one game; any game whose profile reads
   the same key gets it.
3. **The in-game menu** (`PsSettings::Save` writing to `PS_CONFIG`, defaulted by `run.sh:45` and
   `direct::run_sh_defaults`, `crates/ps/src/bbport/direct.rs:108`). In game A set upscaler FSR 4,
   preset Performance, aspect 21:9, output 2560x1080; game B starts with all four, and `run.sh`
   picks B's render path from A's `output_res`/`live_resolution`.
4. **Automatic aspect ratio** (`launch.rs:164-172`). A chose `aspect_ratio=16:9` in the menu: B no
   longer gets the display's ratio passed, because the shared ini no longer says `auto`.
5. **Keyboard bindings** (`detail.rs:238` → `binds::save`, `binds.rs:28-33`). Rebinding dodge on
   Bloodborne's page rebinds it for every bbport-run title. AnyPS5 games are not affected.
6. **Gamepad layout and pad choice** (`gamepad/mod.rs:270` → `pad.rs:306`; GUID through
   `values::save` at `gamepad/mod.rs:363`; `button_icons` at `gamepad/mod.rs:330-335`). A swapped
   layout, a chosen pad or Xbox icons in A apply to B.
7. **Mouse and keyboard-as-stick** (`groups.rs:249-330`, `Store::Ini`). Sensitivity tuned for one
   game applies to all.
8. **Extra environment** (`extras.rs:114`, applied at `mod.rs:468-472`). A workaround variable added
   for A (e.g. `PS_SUBMIT_AHEAD=0`) runs in every game.
9. **Online** (`app/src/views/ps/online.rs:117`, `launch.rs:267`). Turning online on to play
   Bloodborne online turns it on for every title the port has online data for; the game's own Online
   tab only displays the global switch (`games/online.rs:66`).
10. **Mods and patches** (`games/mods.rs:173`, `tools.rs:185,224`). Two Bloodborne editions or copies
    (`CUSA03173` and `CUSA00207`) share one mod order and one patch set.
11. **`frame_stats` global OR title** (`mod.rs:570`). Global on means on in every game; the per-title
    switch can only add.
12. **System modules** (`views/ps/settings.rs:107`, `mod.rs:429`). Every non-Bloodborne title loads
    the dump once its folder is set; a game that runs better on HLE alone can't opt out.
13. **Views keep a stale copy across games**: `GraphicsView::new` reads `Values` once
    (`graphics.rs:58-64`), the Extras FSR check reads the shared `output_res`/`preset`
    (`extras.rs`, `check_fsr`). With per-game files they must reload on each `show_title`.
14. **Language change** (`app/src/i18n.rs:186` → `Bbport::forget_menu_language`, `mod.rs:260`) acts
    on the one shared ini; with per-game files it must visit each.

## (b) Proposal

### Storage

One folder per title inside the library's PS folder, beside the shared data:

```
<library>/psport/titles/<TITLE_ID>/
    settings.json        game's launch settings (same JSON shape as LauncherSettings, no `titles` map)
    psport.ini           game's port ini: graphics, menu keys, key.*, pad.*, mouse, key-stick
    mods.json            mod order and switches (Bloodborne today)
    patches.json         patch switches
    mods-effective/      generated per launch (links only)
    mods-effective.json  generated per launch
<library>/psport/defaults/
    settings.json        the user's defaults for new games (launch keys)
    psport.ini           the user's defaults for new games (port keys)
<library>/psport/bbport/settings.json   only shared keys: online identity, dump folders, port selection
```

`titles/` (not `games/`, which holds dumps; not `bbport/`, which a port reset or rename should not
own). Keyed by `TITLE_ID`, validated at the edge as `^[A-Z]{4}[0-9]{5}$` before it becomes a path,
since it comes from `param.sfo`. Inside the library, so it moves with it and is covered by the
library's backups.

### Launch

- Read `titles/<TITLE_ID>/settings.json` instead of the shared one in `launch_inputs`
  (`launch.rs:164`), merged with the shared file for E1-E3 keys only (an explicit allow-list, so
  nothing new leaks by default).
- Set `PS_CONFIG=<library>/psport/titles/<TITLE_ID>/psport.ini` in `game_env` (`mod.rs:396`).
  `run.sh:45` and the port (`runtime_config.c:34`) already honour it, and `direct::run_sh_defaults`
  leaves a set value alone, so the Windows direct path gets it for free. The port's menu then writes
  into the game's own file: no port change needed.
- `PS_MODS_CONFIG`, `PS_PATCHES_CONFIG`, `PS_MODS_DIR`, `PS_PATCHES_DIR` from the title folder
  (`mod.rs:399-403`); `mods-effective` built inside it (`mods.rs:116,131`).
- `PS_DATA_DIR` stays the shared data folder: it holds `out/`, logs and `fsr4_411`, which are not
  configuration.
- Once every launch passes its own ini, `frame_gen` can move back into the ini, and the
  "always pass `PS_FRAME_GEN`" workaround (`mod.rs:439`) and the port's matching guard can go
  (later, port side; not needed for this change).
- Older ports (`Names::EarlyPsport`, `Names::Bbport`, `names.rs:15`) don't read `PS_CONFIG`.
  Recommendation (D6): no per-game ini for them; the Graphics tab says so when the checkout is on such
  a tag. The alternative, copying the title's ini over the shared one before launch and back after,
  breaks with two games running and is not worth it for tags decanter no longer builds by default.

### First use: copy, not link

When a title has no folder yet (first open of its page, or first launch, whichever comes first),
decanter creates it by copying `defaults/settings.json` and `defaults/psport.ini`. From then on the
title owns its files; editing the defaults never touches it. Decanter writes a `created_from`
field (`defaults`, `migration`, `copy:<TITLE_ID>`) and the date into the title's settings.json for
traceability. Keys absent from a title's files mean "the port's own default", as today; the defaults
file is not consulted at launch.

### The in-game menu

Covered by `PS_CONFIG` alone: `PsSettings::Save` rewrites only its keys in the file it was given and
keeps the rest (`key.*`, `pad.*`, comments), atomically through a `.tmp` rename. Two games running at
once each write their own file. Decanter's existing parse of the menu's `Frame generation setting:`
line (`run.rs:263`) keeps working, writing to the title's settings.json.

### Migration (one-shot, at the first start of a version with this change)

1. Back up: copy `bbport/settings.json`, `bbport/psport.ini`, `bbport/bbport.ini`, `mods.json`,
   `patches.json` and `layouts.json` into `<library>/psport/backups/per-game-settings-<date>/`
   (`Root::backups`, `paths.rs:89`). Nothing is deleted.
2. Defaults: `defaults/settings.json` = today's global keys minus E1-E3 and minus `titles`;
   `defaults/psport.ini` = today's `psport.ini` (or `bbport.ini` when that is all there is) minus
   `menu_language`.
3. Every title decanter knows (library scan, plus every `TITLE_ID` in `titles.*`, `learned.json` and
   `user/cache/`): its folder gets today's effective values: the global keys, then its own
   `titles.<TITLE_ID>` entries on top (`frame_gen`, `local_player`, `frame_stats` as
   `global || title`, `debug`), the shared ini, `mods.json`, `patches.json`. So each game starts
   exactly as it ran yesterday: nothing changes for the player on day one, the difference is that
   from then on edits stay in their game.
4. Shared `settings.json` is rewritten with E1-E3 keys only; the `titles` map is removed once every
   entry has been moved. The shared ini is left in place untouched (an older port tag still reads
   it), and marked in the log as no longer written by decanter.
5. Logged with counts (`titles migrated`, `keys per title`, `backup path`), visible in the PS log.
   A failure stops before step 4, leaving the old files as the source of truth.

### Backup, restore, export

- Per game: "Export settings" writes a zip (or a folder) of `titles/<TITLE_ID>/` minus generated
  files; "Import" validates it (known keys, size cap, `TITLE_ID` match or explicit "apply to this
  game") before writing.
- "Reset to my defaults" replaces the game's files with a fresh copy of `defaults/`, after a
  confirmation and a backup into `backups/`, since it removes what the user set (removing something
  decanter manages asks first).
- Removing a game from the library does not remove its settings folder; a "forget settings" action
  does, after asking.

### UI

- Each tab on a game's page (Keyboard, Gamepad, Graphics, Mods, Online, Extras) carries a line
  "These settings are for <game> only" and a "Copy settings from another game..." action (per tab,
  or whole-game), which copies, never links, and asks before overwriting.
- PS Ajustes gets a "Defaults for new games" page with the same Graphics/Keyboard/Gamepad/Online
  rows, labelled as defaults that already-configured games don't follow, with an explicit
  "Apply to all games..." action that asks first and backs up.
- Online in PS Ajustes keeps only the account (E1); the on/off switch moves to each game's Online
  tab.
- System modules in Ajustes keeps the folder (E2); each game's Graphics or Extras tab gets
  "Use my system modules dump".
- Views reload their values on `show_title` (fixes scenario 13).

### CLI

`decanter ps run <TITLE>` uses that title's settings exactly as Play does, including `PS_CONFIG`, and
creates the title's folder from the defaults if it has none (so a night test never edits the
shared defaults). `--env` stays a per-run override that is never saved. New, small:
`decanter ps settings list|get|set|copy|reset|export|import --title <TITLE>` (as built in phase 5,
above), the same operations as the UI, for night scripts and Windows tests.

### Edge cases

- **Several copies or versions of one title** (base dump and merged `-01.09`, two folders of one
  dump): same `TITLE_ID`, same settings. Different regions (`CUSA00207` vs `CUSA03173`) are
  separate games with separate settings; "copy settings from another game" covers the wish to share.
- **Profiles**: settings are per title for the library, not per decanter profile, as the files are
  today per library (D3). Saves stay per profile.
- **Windows direct launch**: same files, same `PS_CONFIG` (set before `run_sh_defaults`); the port
  build selection stays shared (E3).
- **Library moves**: everything is inside `<library>/psport/`, so it moves with it.
- **Hand-edited files**: a title file that doesn't parse fails the save rather than replacing it
  with defaults (as `LauncherSettings::edit` does today), and the launch says which file.
- **Unknown keys** in a title's ini (from a newer port) are kept, as `Ini` already does.
- **Two games running at once**: no shared writable file left except logs and `out/`.
- **PS5 / AnyPS5 games**: keyboard already per title; any future AnyPS5 setting goes in the same
  `titles/<TITLE_ID>/` folder.

## (c) Risks, phases, decisions, tests

### Risks

- **Migration loses or mis-assigns a value.** Mitigated by the backup, by giving every title the
  effective values it ran with, and by not deleting the old ini.
- **A user who wants one change everywhere** now has to repeat it. Mitigated by "Apply to all
  games..." and "Copy settings from another game".
- **Older port tags** keep the shared ini (D6).
- **`run.sh` reads `PS_CONFIG` for `output_res`/`live_resolution`** (l.169-215): it already uses
  `$PS_CONFIG`, so it follows; checked in the test plan.
- **Report and identity code** read the shared settings (`app/src/report/identity.rs:50-56` for
  `gamepad_name`): must read every title's file, or the shared pad names list, so redaction keeps
  working.
- **`Values` caching** in three views: forgetting one reload keeps a stale value on screen and
  writes it to the wrong title. Each write takes the `TITLE_ID` explicitly, never "the current one".

### Phases (each shippable alone)

1. **Per-title ini.** `titles/<TITLE_ID>/psport.ini` created from the shared ini on first use;
   `PS_CONFIG` set at launch; Graphics `Store::Ini` rows, effects, Keyboard (bbport) and Gamepad
   layout write it; aspect ratio read from it; `forget_menu_language` visits every title. Fixes
   scenarios 2-7 and the menu. Biggest win, smallest change.
2. **Per-title launch settings.** `titles/<TITLE_ID>/settings.json`; all `Store::Launcher` rows,
   `extra_env`, `button_icons`, `gamepad`, `frame_stats`, `frame_gen`, `local_player`, `debug` move
   there; the shared file keeps E1-E3; migration of the `titles` map. Fixes 1, 8, 11.
3. **Online and system modules per game.** Switches move to the game's tabs; Ajustes keeps the
   account and the folder. Fixes 9, 12.
4. **Mods and patches per game.** `mods.json`, `patches.json`, `mods-effective*` and the folders.
   Fixes 10.
5. **Defaults page, copy-from, export/import, reset, CLI `ps settings`.**
6. **Layouts catalog** per D4, and cleanup of GTK-launcher leftovers (`ui_language`, `game_dir`,
   `user_dir`, `mangohud` in settings.json).

Phases 1 and 2 each carry their own slice of the migration (ini first, then JSON) with its backup.

### Decisions for the owner

| # | Decision | Recommendation |
|---|---|---|
| D1 | Key per game: `TITLE_ID` or decanter's game key (folder) | `TITLE_ID`: copies and versions of one game share settings, saves and trophies already go by it |
| D2 | New games: copy of the user's defaults at first use, or port defaults | copy of the user's defaults (owner's statement); no live link |
| D3 | Per title for the library, or per title per profile | per title for the library; per profile can be layered later if a household asks |
| D4 | Named layouts (`layouts.json`): per game or a shared catalog of templates | shared catalog, as an exception: a template is never a game's configuration because applying it copies its lines into the game's ini and editing it changes no game. If the owner wants no exception, each game gets its own `layouts.json` and "copy from another game" covers reuse |
| D5 | Online identity and server shared (E1) | shared; on/off, UPnP and BBSC per game |
| D6 | Older ports without `PS_CONFIG`: per game unsupported (shown), or swap the shared ini at launch | unsupported, shown in the tab |
| D7 | System modules folder shared (E2) with a per-game "use it" switch, default as today (on for non-Bloodborne) | yes |
| D8 | What migration gives each existing title: today's effective values (no behaviour change), or the port defaults | today's effective values |
| D9 | "Apply to all games" action on the defaults page: offer it or not | offer it, with confirmation and a backup |

### Testing plan

Not by day on the owner's machine (game runs only at night or on wbc3); headless where possible;
no online services.

**Per-game isolation (night, Linux, and wbc3, Windows direct path)**, two titles A and B (Bloodborne
`CUSA03173` and one other PS4 title, e.g. Ryza):

1. With `decanter ps settings set --title A`, set `present_mode=Fifo`, `language=1`, `fps_limit=30`, an ini
   key (`upscaler=fsr3`, `skip_intro=1`), one `key.*` binding and `extra_env=PS_TEST_A=1`; on B set
   `present_mode=Immediate`, `language=3`, no limit, `upscaler=off`, `extra_env=PS_TEST_B=1`.
2. `decanter ps run A --timeout 60`, then `decanter ps run B --timeout 60`.
3. Check each run log's header (`run_log.rs:185-210`, lines `# env: NAME=value`):
   A has `PS_PRESENT_MODE=Fifo`, `PS_LANGUAGE=1`, `PS_FPS_LIMIT=30`, `PS_TEST_A=1`, no `PS_TEST_B`,
   and `PS_CONFIG=.../titles/CUSA03173/psport.ini`; B the reverse and its own `PS_CONFIG`.
4. Diff both `titles/*/psport.ini` before and after: each changed only in its own keys; the shared
   `bbport/psport.ini` unchanged.
5. Port side: the upscaler and effects each run starts with (from the port's log or the frame
   statistics) match that title's ini. Whether psport logs the config path it read is not checked
   yet; if not, one `Settings: loaded <path>` line in `runtime_config.c` makes this step direct.
6. Menu write: on wbc3 (interactive allowed there), open the menu in A, change the preset, quit,
   start B: B's preset is its own; A's ini has the new preset.

**Migration (night)**: copy the current `<library>/psport/bbport/` into a scratch library, run the
migration, check that every title in `titles.*` and in the scan has a folder, that each title's
effective env (from step 3's header) equals the pre-migration header for the same title, and that
the backup folder holds the five original files byte for byte.

**Defaults (by day, no game run)**: change a default after A exists; create C; C has the new value,
A keeps its old one (file diff).

**Windows (wbc3)**: same as isolation steps 1-4 through the direct path; check `PS_CONFIG` in the
header and that `psport.exe`'s working folder holds no `psport.ini` written by the run.

**Regression**: the existing regress schedule run once per phase; perf numbers unchanged since only
the source of values changes.
