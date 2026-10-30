# Sensitive data in logs, reports and process state: audit

Recorded on 2026-10-30 for #1313. Read-only review of decanter `develop` at `416bb6a` and psport
`develop` at `3c995aef2b` (`~/Projects/dev/psport/dev`, paths below prefixed `psport:`). Nothing
was built, run or connected. Local logs were only counted with `grep -c` per file; no matching
text was printed or copied.

This extends `docs/secure-credential-storage-design.md` (inventory 1, gaps 1.3, threat model 2,
phases 4). Items already there are referenced, not repeated: shadNet secrets in the environment of
the launch tree (#716, gap 1), the launcher's plaintext password (gap 2), `KeyringError: Debug`
(gap 7), `sent.jsonl` without an ACL on Windows (gap 9). "Unverified" marks what reading could not
settle.

Readers used below: **user** (same uid, incl. same-user malware), **local** (other uids on the
machine), **root**, **operator** (whoever maintains the report server), **paste** (anyone who
receives a copied or attached log), **vendor** (Valve, through SteamOS crash collection).

## 1. Findings

### F1. psport logs HTTP request header values, the shadNet bearer token included

- psport: `gpu/net/shadps4/core/libraries/network/http.cpp:2647-2648` (`sceHttpAddRequestHeader`
  "called ... value={}"), `:2675-2676` ("added header ... {}: {}"), `:549` (every header of a
  request dumped on send).
- The token reaches that function from psport itself:
  `np/np_web_api/np_web_api_internal.cpp:876-880` and
  `np/np_web_api2/np_web_api2_context.cpp:493-497` add `Authorization: Bearer <token>`
  (`network/http2.cpp:30-37` forwards to `sceHttpAddRequestHeader`). Game-supplied headers
  (session ids, `X-*` auth of community servers) go the same way.
- Gate: `LOG_INFO`; psport's threshold is `PS_GPU_LOG`, default `warning`
  (`gpu/shim/common/logging/log.cpp:16-26`). Off by default; on with `PS_GPU_LOG=info|debug|trace`,
  the documented debug lever, settable per game through the launch overrides
  (`crates/ps/src/bbport/mod.rs:348`).
- Where it lands: psport's stderr, captured into decanter's run log
  (`<library>/decanter/logs/<target>-game-*.log`), `failures.jsonl` context lines
  (`crates/ps/src/failures.rs:115-116`), the debug-mode `gpu.log` (`log.cpp:43-49`), the report
  attachment (F4), the clipboard (F5).
- Readers: user, operator, paste. Severity **sev-high** (account bearer token, reusable until the
  server expires it), cwe-532.
- Fix (psport): log header names always, values never for `Authorization`, `Proxy-Authorization`,
  `Cookie`, `Set-Cookie` and any name containing `token|auth|key|secret|session`; log
  `<redacted N bytes>` instead. One helper in `http.cpp` used by all three sites.

### F2. Crash dumps carry the shadNet secrets; SteamOS queues minidumps for upload

- `/proc/sys/kernel/core_pattern` on this machine pipes every core to
  `steamos_log_submitter.hooks.coredump`, which feeds both `systemd-coredump` (full core under
  `/var/lib/systemd/coredump`, `0640 root`) and breakpad's `core_handler`, whose minidump is moved
  to the submitter's `pending/minidump` for upload
  (`/usr/lib/python3.14/site-packages/steamos_log_submitter/hooks/coredump.py:51-66,68-106`).
  Only AppImage mounts, Flatpak, `~/.local/share/Steam/` and `/homebrew/` are skipped
  (`should_collect`): psport from the library is collected.
- psport's process holds `PS_SHADNET_PASSWORD` and `PS_SHADNET_TOKEN` in its initial environment
  block for its whole life (`psport: gpu/net/include/core/user_manager.h:41-43` reads, never
  clears) and the WebAPI bearer token in heap. A full core has all three; breakpad's Linux
  minidump writes `/proc/<pid>/environ` as its `MD_LINUX_ENVIRON` stream (from breakpad's
  minidump writer; not checked against this machine's build: **unverified**).
- Whether uploading is enabled here: **unverified** (`local.cfg` is readable only by
  `steamos-log-submitter`). No psport or decanter core is in `/var/lib/systemd/coredump` today.
- Readers: root, vendor (when submission is on), user through `coredumpctl` (unverified for
  this ACL). Severity **sev-high** (password to a third party with no user action), cwe-214,
  cwe-528 (core dump exposure), cwe-312.
- Checked on the owner's machine (2026-10-30, read only): `steamos-log-submitter status` says
  "Log submission is currently enabled", `helpers.minidump` is `enable = on`, and
  `uploaded/minidump` holds minidumps already sent for other processes. `core_handler` carries
  the strings `cmdline` and `environ`.
- Mitigated on Linux (psport #1362, owner decision #1360): a psport run handed shadNet secrets
  sets itself non-dumpable (`PR_SET_DUMPABLE=0`) before reading them and stays so until it ends,
  so neither the kernel nor SteamOS's `core_pattern` pipe gets a core, and other processes of the
  user cannot ptrace it or read its memory
  ([prctl(2)](https://man7.org/linux/man-pages/man2/prctl.2.html),
  [core(5)](https://man7.org/linux/man-pages/man5/core.5.html)). Runs without secrets keep their
  dumps. Windows has no such switch: a WER minidump may still carry them (open).
- Fix: the hand-off of design 3.4 (phases 5-6) removes the variables. Until then, and also after
  it, psport overwrites the bytes of each secret in the initial environment block after reading
  it (`unsetenv` only drops the pointer: `/proc/<pid>/environ` and the core still show the
  original bytes), keeps the secrets in one `mmap`'d page marked `MADV_DONTDUMP`, and the
  `run.sh`/`online.py` stages never receive them. Optional `prctl(PR_SET_DUMPABLE, 0)` during
  online sessions only (it also blocks same-user `ptrace` and `/proc/<pid>/environ`), for the
  owner to decide since it stops debugging with gdb.

### F3. Run log header: secret filter too narrow, arguments unfiltered

- `crates/ps/src/run_log.rs:193` writes every argument; `:197-205` every variable decanter set,
  hiding only names that end in `SHADNET_PASSWORD` or `SHADNET_TOKEN`
  (`crates/ps/src/bbport/online.rs:251-253`).
- Anything else passes in clear: user launch overrides (`bbport/mod.rs:348`), a URL with
  credentials or a query (F11), a future secret variable with another name. psport's own debug
  `env.txt` already filters wider (`psport: src/runtime/runtime_debug.c:66-81`: names containing
  `TOKEN|PASS|SECRET|KEY|CREDENTIAL|COOKIE|AUTH`, values starting `eyJ` or containing `Bearer `).
- Readers: user, operator, paste. Severity **sev-medium**, cwe-532.
- Fix: one `decanter_ps::redact` policy shared by the header, `failures.jsonl` and the copy paths:
  psport's name and value rules, plus URL userinfo and query values stripped from values and
  arguments. `is_secret_var` becomes that policy's name rule.

### F4. Report attachment is scrubbed for identity only

- `app/src/report/mod.rs:67-87` reads the last 5 MiB of the run log and passes it through
  `decanter_report::scrub_log` (`crates/report/src/lib.rs:99-108`), which replaces identity
  strings (`app/src/report/identity.rs:16-56`: user, host, home folders, profile and account names,
  NPID, pad names). It does not look for secret shapes, IP addresses, server host names, MACs or
  GOG account ids. `docs/error-reports.md` calls it best effort; that holds for names, not for
  F1/F3 content.
- The public report itself is safe by construction (closed schema, vocabulary, `clean.rs`
  allow-list). Only the private attachment is concerned.
- Readers: operator (stored encrypted, one per report). Severity **sev-medium**, cwe-532, cwe-359.
- Fix: in `scrub_log`, after identity, the F3 policy plus token shapes (`Bearer \S+`, JWT
  `eyJ…\.…`, `key=value` with secret names), non-loopback IPv4/IPv6 and MAC addresses; drop
  `# env:` lines outside the `PS_*` allow-list the report already uses; the confirm dialog says
  how many values were replaced.

### F5. Copy buttons put unscrubbed logs on the clipboard

- `app/src/views/ps/log.rs:112-124` (whole job log), `app/src/views/ps/failures.rs:75-87`
  (`Failure::report`, `crates/ps/src/failures.rs:293-329`, Markdown with a code block: made to be
  pasted into an issue or chat).
- Carries home paths with the user name, NPID, the game folder, and any F1/F3 content; clipboard
  managers keep it.
- Readers: paste, user. Severity **sev-medium**, cwe-359, cwe-532.
- Fix: pass both through `scrub_log` with the F4 policy before copying; the "copied" state says
  that names were hidden.

### F6. psport logs cookies and URL queries at levels that are on by default

- psport `network/http.cpp:1100-1101`: `sceHttpAddCookie` logs the cookie value at `LOG_ERROR`
  (above the default threshold: always written). `:3508-3509` logs `url` and `relativeUri` at
  `LOG_ERROR`. URLs with queries also at `LOG_INFO` (`:500-501`, `:1197`, `:1203`, `:1319`,
  `:2990`).
- A game's session cookie or a signed query lands in the run log and its copies (F1 path).
- Readers: user, operator, paste. Severity **sev-medium**, cwe-532.
- Fix: cookie length only; URLs logged as `scheme://host[:port]/path` with the query reduced to
  its parameter names.

### F7. psport launcher password can be copied into a settings backup

- `LauncherSettings::edit_title` (`crates/ps/src/bbport/settings/title.rs:96-99`) reaches
  `title_file` -> `create` -> `port_ini::back_up_shared` (`title.rs:188-191`,
  `crates/ps/src/bbport/port_ini.rs:195-210`) without `LauncherSettings::load` first, and the
  backup is a raw `fs::copy`. If psport's GTK launcher wrote `online_password` since decanter's
  last load, the plaintext password is kept in
  `<library>/psport/backups/per-game-settings-<date>/settings.json` indefinitely, outside the drop
  in `settings.rs:39-58,102-104`. Once per library (first title only).
- Local check: the existing backup (`per-game-settings-2026-10-29`) holds only `bbport.ini` and
  `psport.ini`, 0 matches; the shared `settings.json` has 0 `online_password` keys.
- Readers: user, local (F8 modes), anyone with a library backup or sync. Severity **sev-medium**,
  cwe-312.
- Fix: back up `settings.json` through read, remove `online_password`, write; or call `load`
  before `title_file` in `edit_title`. Folds into design phase 4 (migrate the password to the
  keyring before any copy).

### F8. Library files are created world-readable

- `app/src/logs.rs:189-194` (day log), `crates/ps/src/run_log.rs:80-85` (run logs),
  `app/src/debug_logs.rs:140-160,225-233` (debug copies), the database
  (`crates/core/src/db/mod.rs:222`), port settings (`crates/ps/src/files.rs:5-13`): default mode,
  so `0644` files in `0755` folders with the usual umask. Observed: `decanter.db` 644, `logs/` and
  `logs/debug/` 755, 29 of 29 top-level logs 644, `psport/bbport/settings.json` 644.
- Protected here only because `/home/deck` is `0700`. A library on another disk
  (`/run/media/deck/<disk>`, an SD card) is readable by every local uid (this machine has 32
  `nixbld` users besides `deck`); on exFAT/NTFS the mount options decide for every file. Windows:
  `restrict_to_owner` is a no-op (`app/src/platform/windows.rs:32`).
- Content today: identifiers, paths, account ids, NPIDs; secrets only through F1/F3/F6/F7.
  Severity **sev-low**, cwe-276, cwe-532.
- Fix: `logs/`, `logs/debug/`, `reports/` and the database created `0700`/`0600` on Unix (a
  `std::fs` twin of `platform::restrict_to_owner`); on Windows the ACL of design phase 8 for the
  same files.

### F9. Debug mode copies unscrubbed logs to a folder meant for syncing

- `app/src/debug_logs.rs:1-4` (decision #620: a folder "the user may sync between machines"),
  `:199-233` (`keep_job` copies each job log and `.calls`), `app/src/logs.rs:61-66` (day log
  mirrored). psport writes `gpu.log`, `faults.txt`, `env.txt` and `config.txt` there too
  (`env.txt` and settings redacted, `config.txt` command line not).
- The sync tool and its peers become readers of everything in F1/F3/F6. Severity **sev-low**,
  cwe-532, cwe-359.
- Fix: apply the F4 scrub to the copies (the library keeps the raw ones), files `0600`, and say
  in Ajustes that the folder holds raw logs.

### F10. Identifiers in logs: NPIDs of friends, GOG account ids

- decanter: `account_id` in 14 of 14 local day logs (count only); NPID at info on every keyring
  operation (`crates/core/src/credentials.rs:147-178`), already noted in the inventory.
- psport: own NPID at info (`shadnet/client.cpp:224,762,833`, `np/np_handler.cpp:219,327`);
  other players' NPIDs at info on friend events (`np_handler.cpp:819-1087`), debug in
  `client.cpp:900-942`. Third-party identifiers, off at the default threshold. 22 local log files
  hold an NPID-like line (count only).
- Readers: user, operator (NPID is scrubbed; friends' NPIDs and account ids are not), paste.
  Severity **sev-low**, cwe-359.
- Fix: friends' NPIDs at debug and never in the attachment (F4 can drop `npid='…'` values that are
  not the user's); `account_id` added to the identity list for the attachment.

### F11. Game arguments in the day log and in `/proc/<pid>/cmdline`

- `crates/runtime/src/os/linux/mod.rs:153-160,270-275`, `crates/runtime/src/os/native.rs:100-104`
  log `args = ?launch.args` at debug, which the day log keeps for decanter's targets
  (`app/src/logs.rs:77-86`). `launch.env` is deliberately not logged (`linux/mod.rs:148-152`).
- Arguments of every child (umu-run, Proton, psport, `run.sh`, `online.py`) are readable by any
  local uid: `/proc` is mounted without `hidepid` (checked in `/proc/mounts`). Today they are
  paths (user name) and game options. User launch options of external games may carry a
  launcher's auth argument: **unverified** whether any supported launcher needs one.
- Severity **sev-low**, cwe-214, cwe-532.
- Fix: the F3 policy on `args` before logging; never pass a secret as an argument (design 3.4
  rule, already followed by decanter's own code).

### F12. Online server addresses keep userinfo and query

- `crates/ps/src/bbport/online.rs:151-160` (`http_address`) accepts `http://user:pass@host/?k=v`;
  the address is logged at info (`:428-433`, `:472-474`), exported to the launch environment and
  written in the run log header (F3). psport refuses userinfo for `PS_SHADNET_WEBAPI` only
  (`psport: gpu/net/psport_net_tls.cpp:122`).
- Severity **sev-low**, cwe-532.
- Fix: refuse userinfo (`url.username()`/`url.password()` non-empty) and a query in all three
  addresses at the edge, with the field named in the error.

### F13. Predictable temporary folders in `/tmp`

- `crates/runtime/src/os/linux/umu.rs:192` `decanter-umu-run-<pid>`,
  `crates/gog/src/steam_export.rs:49` `decanter-steam-artwork-<account_id>-<game_id>`:
  `create_dir_all` in the shared temp dir accepts a folder another uid made first, and the second
  name publishes the GOG account id to every local uid. The umu archive is hash-checked
  (`umu.rs:197-201`); the artwork is not.
- Severity **sev-low**, cwe-200, cwe-377.
- Fix: a private random folder (`O_EXCL`/`mkdtemp` semantics, `0700`) under
  `$XDG_RUNTIME_DIR` or decanter's cache; no account id in the name.

### F14. Latent: `ChunkSource` derives `Debug` over a signed CDN URL

- `crates/gog/src/install/download.rs:57-65`. Not formatted anywhere today (no `?chunk`); errors
  are already stripped with `without_url` (`:520-547`).
- Severity **sev-low**, cwe-532.
- Fix: manual `Debug` printing host and path only.

### F15. Opt-in memory dumps on a fault

- `psport: src/main/signals.c:88-130` (`PS_FAULT_DUMP`): up to 64 KiB of chosen guest memory
  written to stderr and `faults.txt` on a fault. Opt-in; the ranges can hold game session data.
- Severity **sev-low**, cwe-532.
- Fix: say in the variable's documentation that the dump is raw memory; never attach
  `faults.txt` hex blocks (F4 can drop them).

## 2. Checked and found clean

- GOG API client: URLs with the client secret, refresh token or login code never reach an error
  (`crates/gog/src/api/client/error.rs:39-48`, `without_url`); bodies of token answers are never
  quoted (`auth.rs:26-39`, `has_secrets`); `Token` has no `Debug`. Cloud-saves token exchange and
  chunk downloads strip URLs (`cloud_saves.rs:425-440`, `install/download.rs:520-547`). The
  download journal stores no URL (`install/journal.rs`).
- tracing calls carrying `token`, `secret`, `password`, `code`, `key`: none print a value; shadNet
  helpers log the secret's kind (`?secret` is the enum).
- `Online` (`online.rs:255`) and the reports server's credentials have no `Debug`.
- Galaxy hook (`crates/hook`): records unlocks and diagnostics only; no sign-in data.
- `systemd-run --scope` (`crates/core/src/scope.rs:47-61`) and the toolchain shell
  (`crates/nix/src/lib.rs:161-195`) pass secrets only through the inherited environment, never in
  arguments. Windows git runs with `env_clear` and an allow-list (`winbuild/git.rs:97-125`).
- Public report JSON: closed schema, closed vocabulary, `clean.rs` allow-list; `PS_*` values by
  allow-list only. The day log is never attached (`app/src/cli/ps.rs:208-217`).
- psport debug `env.txt` and `config.txt` settings: redacted by name and value
  (`runtime_debug.c:66-97`). psport logs the bearer token's length only (`client.cpp:833`).
- Proton log (`PROTON_LOG=1` on every non-captured launch, `linux/mod.rs:127-137`): UMU-Proton
  10.0-4 writes the command, options, `PATH`, `WINEDLLOVERRIDES` and `WINEDEBUG`, not the
  environment (`proton:2032-2074`). Its `+debugstr` channel records the game's
  `OutputDebugString`: content per game, **unverified**. No `steam-*.log` was found locally.
- Screenshots and frame dumps: psport writes them only with `PS_DUMP_*` variables to
  `PS_DUMP_DIR` (`vk_temporal_upscaler.cpp:62-92`); the image content is the game's, file names
  carry no identifier.
- Cloud saves: the save files themselves go to GOG, which is their purpose; nothing of decanter's
  is added.

## 3. Local logs, counted

`grep -cE` per file, never printing a match, for: bearer tokens, `Authorization` headers,
`refresh_token`, `access_token`, `client_secret`, `password=`/`password:` with a value,
`SHADNET_(PASSWORD|TOKEN)=` not followed by `<hidden>`, JWTs, `Cookie:` values, delete codes,
`token|sig|signature|X-Amz-Signature|Expires` query parameters, and psport's header-dump lines.

| Set | Files | Matches |
|---|---|---|
| `/home/deck/data/Decanter/steamapps/decanter/logs/` (day, run, debug, mangohud) | 193 | 0 |
| `/home/deck/data/psport-runs/*/` (`*.log`, `*.txt`, `*.jsonl`, `*.calls`) | 453 `.log` plus the rest | 1 file, 2 lines: `n1021/tmp/check-deps-fix2.log` lines 52 and 54 match `client_secret`. They are source diff lines quoting the public Galaxy client constant (inventory row "Galaxy client id/secret"), not a user secret |
| Proton `steam-*.log` under `/home/deck/data`, `/run/media/deck/Datos` | 0 found | - |

No `Lib_Http` line exists in any local log, so F1 and F6 have not fired here. **No real leak
confirmed.**

## 4. Prioritized table

| # | Finding | Repo | Severity | CWE | Readers |
|---|---|---|---|---|---|
| F2 | Secrets in crash dumps, SteamOS minidump upload | psport (+decanter hand-off) | sev-high | cwe-214, cwe-528, cwe-312 | root, vendor |
| F1 | HTTP header values (bearer token) logged | psport | sev-high | cwe-532 | user, operator, paste |
| F6 | Cookies and URL queries logged at default levels | psport | sev-medium | cwe-532 | user, operator, paste |
| F3 | Run log header: narrow env filter, args in clear | decanter | sev-medium | cwe-532 | user, operator, paste |
| F4 | Attachment scrub is identity only | decanter | sev-medium | cwe-532, cwe-359 | operator |
| F5 | Copy buttons copy unscrubbed logs | decanter | sev-medium | cwe-359, cwe-532 | paste |
| F7 | Launcher password into settings backup | decanter | sev-medium | cwe-312 | user, backups |
| F8 | Library files `0644`/`0755` | decanter | sev-low | cwe-276, cwe-532 | local |
| F9 | Debug folder: raw logs for syncing | decanter | sev-low | cwe-532, cwe-359 | sync peers |
| F10 | Friends' NPIDs, account ids in logs | psport, decanter | sev-low | cwe-359 | operator, paste |
| F11 | Game args logged and in `cmdline` | decanter | sev-low | cwe-214, cwe-532 | local, paste |
| F12 | Server addresses keep userinfo/query | decanter | sev-low | cwe-532 | paste |
| F13 | Predictable `/tmp` folders, account id in name | decanter | sev-low | cwe-200, cwe-377 | local |
| F14 | `ChunkSource: Debug` over signed URL (latent) | decanter | sev-low | cwe-532 | - |
| F15 | `PS_FAULT_DUMP` raw memory (opt-in) | psport | sev-low | cwe-532 | user, paste |

Count: 2 sev-high, 5 sev-medium, 8 sev-low.

## 5. Proposed tasks

| Title | Repo | Files | Model |
|---|---|---|---|
| Redact header values, cookies and URL queries in libhttp logs (F1, F6) | psport | `gpu/net/shadps4/core/libraries/network/http.cpp`, `http2.cpp` | opus (C++, security) |
| Wipe shadNet secrets from the environment block and keep them out of dumps (F2) | psport | `gpu/net/include/core/user_manager.h`, `gpu/net/shadps4/shadnet/client.{h,cpp}`, `run.sh` | opus (C++, security) |
| Owner decision: `PR_SET_DUMPABLE` during online sessions, and SteamOS crash submission (F2) | decanter (board) | none | user decision |
| Shared redaction policy for run log header, failures and copies (F3, F11, F12) | decanter | `crates/ps/src/run_log.rs`, `crates/ps/src/bbport/online.rs`, new `crates/ps/src/redact.rs`, `crates/ps/src/failures.rs`, `crates/runtime/src/os/{linux/mod.rs,native.rs}` | opus (security) |
| Secret and address pass in `scrub_log`, count shown before sending (F4, F10) | decanter | `crates/report/src/lib.rs`, `crates/report/src/identity.rs`, `app/src/report/{mod,identity}.rs`, `app/src/views/ps/report.rs`, locales | opus (security) |
| Scrub log and failure copies to the clipboard (F5) | decanter | `app/src/views/ps/log.rs`, `app/src/views/ps/failures.rs`, locales | sonnet |
| Back up shared settings without the launcher password (F7) | decanter | `crates/ps/src/bbport/port_ini.rs`, `crates/ps/src/bbport/settings/title.rs` | opus (credential data) |
| Owner-only modes for logs, reports, debug folder and database (F8, F9) | decanter | `app/src/logs.rs`, `app/src/debug_logs.rs`, `crates/ps/src/run_log.rs`, `crates/core/src/db/mod.rs`, `app/src/platform/*` | sonnet |
| Scrub debug-folder copies and say so in Ajustes (F9) | decanter | `app/src/debug_logs.rs`, `app/src/views/settings/*`, locales | sonnet |
| Friends' NPIDs to debug level (F10) | psport | `gpu/net/shadps4/core/libraries/np/np_handler.cpp` | sonnet |
| Private temp folders without account ids (F13) | decanter | `crates/runtime/src/os/linux/umu.rs`, `crates/gog/src/steam_export.rs` | sonnet |
| Manual `Debug` for `ChunkSource` (F14) | decanter | `crates/gog/src/install/download.rs` | sonnet |
| Document `PS_FAULT_DUMP` as raw memory; drop its blocks from attachments (F15) | psport, decanter | psport `docs/`, `crates/report/src/lib.rs` | sonnet |

The design's phases 5 and 6 (secrets hand-off by file or stdin) stay the structural fix for F2
and #716; the psport task above is the part that also helps once the variables are gone.

## 6. Not verified

- Breakpad's `MD_LINUX_ENVIRON` stream in SteamOS's `core_handler` build (F2); crash submission
  is on here (checked 2026-10-30).
- Whether `coredumpctl` gives `deck` its own psport cores on this system (F2).
- Content of Proton's `+debugstr` channel for the games in use.
- Whether any supported launcher passes auth in arguments (F11).
- Where decanter's stderr ends up when started from the desktop entry or Game Mode (journal,
  Steam's console log): same INFO lines as the day log, unverified destination.
- Windows ACLs of every file above (only the code was read).
