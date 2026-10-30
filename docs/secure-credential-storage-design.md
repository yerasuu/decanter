# Secure credential storage: design

Recorded on 2026-10-30. Read-only analysis of `develop` at `6bae95f`: nothing was
built, run or connected, and no stored value was read. Paths are relative to this
repository unless they name psport (`~/Projects/dev/psport/dev`) or a crate in
`~/.cargo/registry/src` (`<registry>/<crate>-<version>`). "Unverified" marks what
reading the code could not settle.

## 1. Inventory

Every credential-like item decanter keeps, sends or hands to another process.
"Keyring" is the `keyring` crate's store, service `dev.decanter.Decanter`
(`crates/core/src/credentials.rs:40`); the entry's user part is in the column.

| Item | Where it lives today | Format | Who reads it | Exposure | Code |
|---|---|---|---|---|---|
| GOG session (access token, refresh token, `session_id`, `user_id`, expiry) | Keyring `<account_id>/STOREFRONT_GOG` | `Token` as JSON (`expires_in`, `scope`, `token_type`, `access_token`, `user_id`, `refresh_token`, `session_id`, `updated_at`) | `decanter_gog` only: `read_session`/`write_session`/`clear_session`, rewritten after every call because GOG rotates the refresh token | Process memory as plain `String` (no zeroize); `Token` has no `Debug` (good). Sent as bearer header; never logged (only `found`, byte count). Size on Windows: see 1.2 | `crates/gog/src/api/client/auth.rs:51-68`, `crates/gog/src/lib.rs:88-125`, `crates/gog/src/session.rs:131-150`, `credentials.rs:51-118` |
| GOG cloud-saves game token | Memory only, per sync | `GameToken { access_token, user_id, client_id }` | `cloud_saves.rs` | The exchange is a GET whose query holds the build's client secret and the account's refresh token; reqwest errors are stripped of the URL before logging (`without_url`). GOG's own access logs see the query (outside decanter) | `crates/gog/src/cloud_saves.rs:86-95,392-450` |
| GOG build client id/secret | Memory, fetched per sync from build metadata | strings | `resolve_build_client` | As above | `cloud_saves.rs:97-130` |
| Galaxy client id/secret | Compiled constants | `&str` | token and refresh requests | Public (community-documented Galaxy client); in the binary and source. Not a user secret; its URLs are kept out of logs | `crates/gog/src/api/client/auth.rs:15-18,100-117` |
| GOG OAuth login code | UI text field, then memory | pasted redirect URL | `decanter_gog::log_in` | Field not masked; passes through the system clipboard (clipboard managers may keep it). Single-use and short-lived. Never logged | `app/src/views/gog_login.rs:38,70-75`, `crates/gog/src/auth.rs:61-65` |
| GOG signed CDN links | Memory, per download | URL with a signature in the query | install/download | Stripped from errors (`without_url`) | `crates/gog/src/install/download.rs:520-529`, `crates/gog/src/api/client/error.rs:45` |
| shadNet password | Keyring `shadnet/<NPID>/password` | UTF-8, <= 1024 bytes, no NUL | Ajustes (presence only), PS launch | Environment variable `PS_SHADNET_PASSWORD` (or the port's prefix) of the whole launch tree: toolchain shell, `gamemoderun`, `run.sh`, `online.py`, the local game server, psport (#716). Readable in `/proc/<pid>/environ` by the same user (and root) for the game's lifetime; on Windows by any process of the same user. Hidden in the run log header | `credentials.rs:120-182`, `app/src/ps_play/launch.rs:277-317`, `crates/ps/src/bbport/online.rs:234-309`, psport `gpu/net/include/core/user_manager.h:41-43` |
| shadNet token | Keyring `shadnet/<NPID>/token` | as above | as above | as above (`PS_SHADNET_TOKEN`) | same |
| shadNet password typed in psport's own launcher | psport's `settings.json` in the port's data dir, key `online_password`, plain text | JSON string | psport's GTK launcher (`launcher/psport_launcher.py:420`) | decanter deletes it on load and on every save, without moving it to the keyring: the credential is dropped (logged at info) and the user has to type it again | `crates/ps/src/bbport/settings.rs:16,39-58,102-104` |
| NPID | `settings.json` `online_npid` | 3-16 of `[A-Za-z0-9_-]` | launch, Ajustes, report identity | Identifier, not a secret; logged at info by `save_shadnet`; scrubbed from reports as an identity name | `online.rs:30,212-232`, `credentials.rs:161`, `app/src/report/identity.rs:52-53` |
| SteamGridDB API key | Keyring `steamgriddb/STOREFRONT_UNSPECIFIED` (reuses the storefront-session API with `Storefront::Unspecified`) | string | external-game art search, bearer header | Shown unmasked and read back into the Ajustes field in clear | `app/src/external/art.rs:22-24,67-83,181`, `app/src/views/external/mod.rs:77-84` |
| Report delete code | `<library>/decanter/reports/sent.jsonl`, one line per sent report | 32 hex | `X-Delete-Code` header to delete a report or attach its log | Bearer for one report. File `0600` on Unix; on Windows default ACL of wherever the library is (another drive can grant `Users`). Inside the library: travels with library backups and syncs | `app/src/report/sent.rs:1-56`, `app/src/report/send.rs:335-365` |
| Report attachment (run log) | Uploaded on request | scrubbed text | reports server | `scrub_log` removes identity strings (user, host, home paths, account names, NPID) only; it is not a secret filter. Secrets are kept out of run logs at the source (header hides `*SHADNET_PASSWORD`/`*SHADNET_TOKEN`); command arguments are not filtered | `app/src/report/mod.rs:67-82`, `crates/report/src/lib.rs:105-108`, `crates/ps/src/run_log.rs:194-205` |
| Report text fields | Built report JSON | allow-list of token shapes | reports server | Allow-list design; known token prefixes (`ghp_`, `sk-`, `steam<digits>`, ...) refused | `crates/report/src/text/mod.rs:1-6`, `crates/report/src/text/shapes.rs:10,119-130,159-161` |
| Day log | `<library>/decanter/logs/decanter-<day>.log` | tracing text | the user, report builder | No secrets by construction where read (above); default file mode (umask; unverified). Account ids and NPID appear | `app/src/report/mod.rs:200-202`, `credentials.rs:61-117` |
| Database | `<library>/decanter/decanter.db` | SQLite | decanter | Account ids, display names, caches; no secret columns | `crates/core/src/bootstrap.rs:70-77`, `crates/core/src/db/mod.rs:42-93` |
| Per-title and shared PS settings | `<port data>/settings.json`, `titles/<id>.json` | JSON | decanter, psport's launcher | Servers, NPID, switches; no secret once the import above has dropped `online_password` | `settings.rs:20-30,93-105` |
| Game launch environment (umu-run/Proton, native) | `Launch.env` | pairs | the game | No decanter secret passes today; `launch.env` is deliberately not logged | `crates/runtime/src/os/linux/mod.rs:148-152`, `crates/runtime/src/os/native.rs:91-99` |
| Headless `ps run` | CLI | | | Removes every online variable, the shell's included, so no secret reaches a headless run | `app/src/cli/ps_run.rs:46-59,288-297` |
| Reports server (separate workspace, AGPL, runs on a host) | `reports.toml` plus env/`*_file`: storage passphrase, S3/R2 access key pair; maintainer keys `drk_<8 hex>_<64 hex>` stored as SHA-256 in the bucket; browser session ids and CSRF tokens in memory as SHA-256 | | the server, its CLI | Owner-only files (`umask 077`, permission check), `$CREDENTIALS_DIRECTORY` for systemd credentials; `StorageCredentials` without `Debug`; constant-time compare. Not part of the desktop store below | `crates/reports-server/src/config.rs:9-17,160-196,470-500`, `secrets.rs`, `accounts/mod.rs:1-20`, `session.rs:1-6`, `perms.rs` |

No GOG token, Galaxy session or account secret is written into a Wine prefix or
handed to a game: the Galaxy hook only records unlocks (`crates/hook/README.md`).
No signing key exists on the client: self-update verifies with a minisign public
key (`app/src/self_update.rs`).

### 1.1 The keyring backend as built

- `keyring` 4.2.0 with default feature `v1` (`<registry>/keyring-4.2.0/Cargo.toml`
  `[features]`). On Linux `v1` picks `zbus-secret-service-keyring-store` 1.0.1
  (`crypto-rust`), on Windows `windows-native-keyring-store` 1.1.0, on macOS
  `apple-native-keyring-store` 1.0.2 keychain (`keyring-4.2.0/src/v1.rs:109-129`).
- `crates/core/Cargo.toml:19-26` also enables `dbus-secret-service-keyring-store`
  and its comment says it is the backend in use. Under 4.2.0 it is compiled
  (with vendored libdbus) but never selected: `set_credential_store` only builds
  the zbus store. The comment describes keyring 3.
- The store is built once per process in a `LazyLock` (`v1.rs:107`). If the
  Secret Service cannot be reached the first time (no session bus, no provider),
  every later call in that process fails with `NoDefaultStore`, even after the
  user starts or unlocks KWallet. Whether SteamOS Game Mode runs a Secret
  Service provider: unverified.
- KWallet (Plasma) and GNOME Keyring both serve the Secret Service API
  (`<registry>/secret-service-5.2.0/src/lib.rs:10`); no separate KWallet backend
  is needed.
- `keyring::Error` (`keyring_core::Error`) derives `Debug` with the raw bytes in
  `BadEncoding(Vec<u8>)` and `BadDataFormat(Vec<u8>, _)`; its `Display` omits
  them (`<registry>/keyring-core-1.0.0/src/error.rs:25-50,81-111`).
  `KeyringError` derives `Debug` over it (`credentials.rs:42-47`): a `?err` in
  a log would print a stored value. Today every log uses `%err`.

### 1.2 Windows specifics

- Persistence defaults to `Enterprise` (roams with a domain profile)
  (`<registry>/windows-native-keyring-store-1.1.0/src/store.rs:116-121`).
  `keyring` v1's `Entry::new` takes no modifiers, so `Local` needs
  `keyring_core::Entry::new_with_modifiers` (`keyring-core-1.0.0/src/lib.rs:152`).
- A password is stored as UTF-16 and refused above 2560 bytes
  (`utils.rs:79-95`). The shadNet secrets fit (`docs/windows-online-plan.md`
  2.7). Whether a serialized GOG `Token` always fits (1280 UTF-16 code units):
  unverified; `docs/windows-support.md` reports GOG login working there.

### 1.3 Gaps found

1. #716: shadNet secrets in the environment of the whole launch tree; psport
   never `unsetenv`s them (no hit in `gpu/net`; the rest of psport unverified).
2. The psport launcher's plaintext password is deleted, not migrated.
3. SteamGridDB key shown in clear and filed as a fake storefront session.
4. `delete_profile` forgets the accounts' rows but leaves their keyring entries
   (`crates/core/src/profiles.rs:92-125`); no app caller today.
5. No way to see or forget everything decanter keeps in the keyring.
6. One failed Secret Service connection disables the keyring for the session.
7. `KeyringError: Debug` can print stored bytes.
8. Secrets live as plain `String`s, not zeroized; `TextInput` keeps typed
   secrets until cleared (`app/src/views/ps/online.rs:199-200` clears them).
9. Windows: Credential Manager entries roam (`Enterprise`); `sent.jsonl` has no
   ACL restriction (`app/src/platform/windows.rs:32-37` is a no-op).
10. `crates/core/Cargo.toml` builds an unused D-Bus backend with vendored libdbus.

## 2. Threat model

| Adversary | What the OS keyring gives | What it does not |
|---|---|---|
| Malware running as the same user | Nothing durable. Secret Service has no per-application access control: any process of the user can ask the unlocked collection. Credential Manager likewise (`CredRead` by any process of the user). macOS Keychain does check the calling application | It can also read `/proc/<pid>/environ` and process memory (ptrace scope permitting), so it gets the shadNet secrets during a game and the GOG token while decanter runs |
| Another local user | Protection: the collection and Credential Manager are per user | Today: none needed beyond file modes; `sent.jsonl` on Windows depends on the library's ACL |
| Disk theft, powered off | Protection when the collection is encrypted with the login password (GNOME Keyring default, KWallet with a password, DPAPI on Windows). A KWallet or GNOME collection with an empty password, as some autologin setups have (SteamOS unverified), is effectively plain text | Library files (`sent.jsonl`, logs) unless the disk is encrypted |
| Accidental leak: logs, run logs, report bundles, screenshots, library backups and cloud syncs, clipboard | Keeping secrets out of the library and out of `Debug`/`Display` is what this design is mostly about | The clipboard path of the GOG login code |
| Network attacker | Out of scope here: HTTPS (rustls) for GOG, SteamGridDB and reports; shadNet's transport is psport's | |

Honest summary: the realistic gains are against accidental leaks, other users
and offline theft. Same-user malware is not stopped by any of the stores on
Linux or Windows; the design only shortens how long secrets sit where such
malware looks first (environment, files).

## 3. Design

### 3.1 One module, `decanter_core::secrets`

A directory module replacing `credentials.rs` (it has outgrown one concern):

```
crates/core/src/secrets/
  mod.rs        public API, re-exports
  key.rs        SecretKey and its checked parts
  secret.rs     Secret, redacting wrapper
  store.rs      Store: backend choice, retry, errors
  native.rs     keyring-core stores per platform
  file.rs       fallback (only if the owner accepts one, 3.3)
  handoff.rs    secrets to child processes (3.4)
```

```rust
pub enum SecretKey {
    GogSession { account_id: AccountId },   // decimal digits, checked once
    ShadnetPassword { npid: Npid },         // online::check_npid rules
    ShadnetToken { npid: Npid },
    SteamGridDbKey,
}

pub struct Secret(Zeroizing<String>);       // no Clone, Serialize, Display
impl Secret {
    pub fn new(value: String) -> Result<Self, SecretError>; // size and NUL checks per kind
    pub fn expose(&self) -> &str;
}
impl fmt::Debug for Secret { /* "Secret(<redacted>)" */ }

pub struct Store { /* backend + status */ }
impl Store {
    pub fn status(&self) -> Status;                       // Ready, Locked, Missing{reason}, Fallback{kind}
    pub fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), StoreError>;
    pub fn get(&self, key: &SecretKey) -> Result<Option<Secret>, StoreError>;
    pub fn delete(&self, key: &SecretKey) -> Result<(), StoreError>;  // absent is Ok
    pub fn list(&self) -> Result<Vec<SecretKey>, StoreError>;          // keys only
    pub fn forget_all(&self) -> Result<Forgotten, StoreError>;
}
```

- Entry names stay as today (`<id>/STOREFRONT_GOG`, `shadnet/<NPID>/{password,token}`),
  so the native store needs no migration; `SteamGridDbKey` moves to
  `steamgriddb/api_key` (migrated, 3.5). `SecretKey` parses its own entry
  names back for `list`, so a stray entry under the service is reported, never
  guessed at.
- `list` and `forget_all` use `keyring_core::Entry::search` on the service
  attribute; the three stores implement `search` (`zbus .../store.rs:112`,
  `windows .../store.rs:135` behind its default `search` feature,
  `apple .../keychain.rs:254`). The search spec each store accepts: unverified.
- `StoreError` has no `#[from] keyring_core::Error`. Each error is mapped to a
  closed set (`Unavailable`, `Locked`, `Denied`, `TooLarge`, `Corrupt`, `Io`)
  carrying the key's kind and a short reason string built from `Display` only,
  so neither `Debug` nor `Display` can carry stored bytes.
- Logs name the key's kind and subject (`account_id`, `npid`) and the outcome,
  never the value, as `credentials.rs` already does.
- Blocking, as today; async callers keep `spawn_blocking`.
- The redaction is typed, not a deny-list: anything holding a `Secret` cannot
  print it, and structs that carry one (`Online`, `GameToken`, a future
  `Token`) keep `Secret` fields, so `#[derive(Debug)]` on them is safe. GOG's
  `Token` keeps `String` on the wire (`serde`) and wraps its two tokens in
  `Secret` at the `decanter_gog` boundary; that change is phase 3.

### 3.2 Backends

The `keyring` v1 wrapper is replaced by `keyring-core` plus the store crates,
as `keyring`'s own docs advise for applications that choose their stores
(`<registry>/keyring-4.2.0/src/lib.rs:17-28`). That gives:

| Platform | Store | Notes |
|---|---|---|
| Linux, BSD | `zbus-secret-service-keyring-store` (GNOME Keyring, KWallet via its Secret Service, KeePassXC) | Built lazily and retried on the next call after a failure, instead of once per process. The unused `dbus-secret-service-keyring-store` goes |
| Windows | `windows-native-keyring-store`, persistence `Local` | Owner's choice (5); `Local` means the secret stays on this machine |
| macOS | `apple-native-keyring-store` keychain | Per-application ACL; `protected` (data-protection keychain) needs signing entitlements, unverified for decanter's bundle |
| Any, no store | Fallback (3.3) | Never silent |

### 3.3 When there is no store

Detected by `Store::status`: no session bus, no Secret Service provider, a
locked collection the user declined to unlock, or `NoDefaultStore`.

Options, for the owner (5):

| Option | Protection | Cost |
|---|---|---|
| A. Session only (recommended default) | Secrets kept in memory for the session; the user is told, once, that they will be asked again next time. Nothing written | GOG re-login each session; on Game Mode, unusable if typing is hard |
| B. Encrypted file, key from a passphrase (Argon2id, then XChaCha20-Poly1305) in `<config>/decanter/secrets.bin`, `0600`/`0700` | Real against disk theft and other users, as strong as the passphrase. None against same-user malware once unlocked | A passphrase prompt per session; two new crates |
| C. Encrypted file, key derived from machine data (`/etc/machine-id`, uid) | Obfuscation only: equal to a `0600` plain file against everyone who can read the file; stops casual `grep` and accidental copying of the library | One crate; must be labelled as unprotected in the UI |
| D. Linux kernel keyring (`linux-keyutils-keyring-store`) | Per user session, gone at logout/reboot; not on disk | Same UX as A across reboots; one crate |

A plain-text file is never written without a visible, persistent warning in
Ajustes, and only if the owner wants option C at all. The fallback file never
lives in the library (it would travel with backups and syncs).

### 3.4 Secrets to child processes

Rule: no secret in an environment variable or argument. decanter passes a
reference, the child reads the secret once.

| Child | Today | Proposed |
|---|---|---|
| psport through the toolchain and `run.sh` (Linux) | `PS_SHADNET_PASSWORD`, `PS_SHADNET_TOKEN` | A per-launch file `$XDG_RUNTIME_DIR/decanter/secrets/<random>` (dir `0700`, file `0600`, `O_EXCL`, tmpfs), path in `PS_SHADNET_SECRETS_FILE`; psport reads it at `UserManager` construction and unlinks it; decanter removes it when the job exits, and sweeps the directory at start. Works through `nix`/`gamemoderun`/`run.sh`, which keep the environment but may not keep extra descriptors (unverified for the toolchain wrapper). Falls back to the library-free `<cache>/decanter/secrets/` when `XDG_RUNTIME_DIR` is unset |
| psport.exe direct (Windows) | same variables | stdin is free (`crates/ps/src/process.rs:190`, `Stdio::null()` today): decanter writes `npid\0password\0token\n` to a piped stdin and closes it; psport reads with a size cap when `PS_SHADNET_SECRETS=stdin`. No `unsafe` needed (the workspace forbids it); handle inheritance in std needs it. Alternative: the file form under `%LOCALAPPDATA%\decanter\secrets\` |
| `online.py` and the local game server | inherit the variables | Read nothing secret; with the file form they never see the values. Whether `online.py` needs the password: unverified (it only resolves hosts in `windows-online-plan.md`) |
| umu-run / Proton / native games | nothing secret | Stays so: `Secret` has no conversion to `OsStr`, so it cannot be put in `Launch.env` or an argument by mistake; `secrets::handoff` is the only way out of the process, and only psport uses it |

Both forms are a psport protocol change (`user_manager.h`, `run.sh` ignores
them). During the transition psport reads the file/stdin form first and the
old variables second; decanter stops exporting the variables once the pinned
psport has the new form (feature probe through `--list-features`, as
`online_module` is).

### 3.5 Migration, no user action

Run once per start, after the store is ready, each step logged with counts:

1. GOG sessions, shadNet secrets: already in the store; names unchanged.
2. SteamGridDB key: read `steamgriddb/STOREFRONT_UNSPECIFIED`, write
   `steamgriddb/api_key`, delete the old entry only after the write reads back.
3. psport launcher's `online_password`: before `LauncherSettings::load` drops
   it, `put` it under the file's `online_npid` when there is one and the store
   is ready; drop it from the file only after the put succeeded. With no NPID
   or no store, keep today's behaviour but say so in Ajustes ("the launcher's
   saved password was not kept; type it again").
4. Windows `Enterprise` entries: read the `persistence` attribute and, when it
   is `Enterprise`, write the value again through an entry built with
   `persistence=Local` (the modifier applies when a secret is written,
   `windows .../lib.rs:40-45`); read back before counting it done.
5. Fallback file to native store, when a store appears later.

A failed step leaves the old copy where it was and retries next start.

### 3.6 Controls in the UI

In Ajustes, a "Saved credentials" group:

- One row per `list` entry: kind, subject (account name, NPID), backend. No
  value is ever shown; fields for secrets are masked (the SteamGridDB field
  becomes masked and shows "saved" like the shadNet one).
- "Forget" per row and "Forget everything", each confirmed first, naming what
  goes (#1133). Forgetting a GOG session is a logout.
- Store status line: ready, locked (with "Unlock" retrying the store), missing
  (with what fallback is in use).
- Backup/restore: none. Secrets are not exported; after a restore or on a new
  machine the user logs in again. Library backups contain no secret once
  `sent.jsonl` is treated as below.
- `delete_profile` deletes its accounts' entries in the same step.

`sent.jsonl` delete codes stay in the library (losing one only means a report
cannot be withdrawn from this machine), with the file restricted to the owner
on Windows too (new `restrict_to_owner` with an ACL, or the codes moved to the
store if the owner prefers; 5).

### 3.7 Failure behaviour

- Store missing or locked: the app starts normally (#1258). The library shows
  the accounts with "session not available: keyring locked/missing" and a
  button to retry; nothing is deleted, no account row is forgotten, no logout
  is inferred from a read failure.
- `put` failure: the value stays in memory for the session, the UI says it was
  not saved, the next start asks again. Never a silent loss.
- `Corrupt` (an entry that does not parse): kept, reported once, user offered
  "forget" for that entry.
- PS launch with an NPID and no reachable secret: stops with the message it has
  today (`LaunchError::NoPassword`/`Keyring`), plus a prompt for this launch
  only when the store is missing (decision #715 already says "se piden al
  lanzar"; today the launch just stops).

## 4. Phases

Each phase ships on its own and leaves behaviour no worse.

| # | Repo | What | Files | Model | Verified by (no real account) |
|---|---|---|---|---|---|
| 1 | decanter | `secrets` module over `keyring-core` + stores, retrying store, redacting errors, same entry names; `credentials.rs` callers moved; unused D-Bus backend dropped | `crates/core/src/secrets/*`, `crates/core/Cargo.toml`, `Cargo.toml`, `crates/core/src/lib.rs`, `crates/gog/src/lib.rs`, `app/src/external/art.rs`, `app/src/views/ps/online.rs`, `app/src/ps_play/launch.rs` | opus (security, store semantics) | clippy (workspace lints) and fmt; a throwaway Secret Service (`gnome-keyring-daemon --unlock` in a private `dbus-run-session`, as the zbus store's docs describe) with dummy values: put/get/delete/list round trip, `secret-tool search service dev.decanter.Decanter` shows the same names as before; start with no session bus: app opens, status "missing", no panic |
| 2 | decanter | Ajustes "Saved credentials", forget per entry and all with confirmation, masked SteamGridDB field, `delete_profile` cleanup, status and retry | `app/src/views/settings/*`, `app/src/views/external/mod.rs`, `crates/core/src/profiles.rs`, locales | sonnet | clippy; with the throwaway keyring: dummy entries listed, forgotten, confirmation shown; strings in en and es |
| 3 | decanter | `Secret` through `decanter_gog` (`Token` fields wrapped at the boundary, `GameToken`), `Online` | `crates/gog/src/api/client/{auth,http}.rs`, `cloud_saves.rs`, `crates/ps/src/bbport/online.rs` | opus (auth paths) | clippy; serialized `Token` byte-identical before and after for a dummy token; no `Secret` reachable by `Debug` (grep for `derive(Debug)` on holders) |
| 4 | decanter | Migrations 3.5 (SteamGridDB name, launcher password, Windows persistence) | `secrets/mod.rs`, `crates/ps/src/bbport/settings.rs` | opus (data migration) | a fixture `settings.json` with a dummy `online_password` and NPID: the value lands in the throwaway keyring, then leaves the file; with the store missing: the file keeps it until a later start; Windows agent with a dummy Credential Manager entry: `cmdkey /list` shows it local after start |
| 5 | psport | Secrets from a file or stdin, unlinked after read; variables still accepted | `gpu/net/include/core/user_manager.h`, `run.sh`, `host_features.inc` (`secrets_handoff`) | opus (C++, security) | OpenOrbis homebrew or the existing net test on `127.0.0.1` with a stub shadNet: login reaches the stub with the file form; `/proc/<pid>/environ` holds no secret; the file is gone after start |
| 6 | decanter | Hand-off: file form on Linux, stdin on Windows, variables only for older psport | `secrets/handoff.rs`, `crates/ps/src/bbport/online.rs:288-309`, `app/src/ps_play/{launch,direct}.rs`, `crates/ps/src/process.rs` | opus (security) | with a dummy NPID/password and every server at `127.0.0.1`: `tr '\0' '\n' < /proc/<pid>/environ` on every process of the job shows no secret; the per-launch file is gone after exit; Windows agent: environment block dump of `psport.exe` without it |
| 7 | decanter | Fallback chosen by the owner (3.3) | `secrets/file.rs` or `secrets/native.rs` (keyutils) | opus | the file's mode and location, the warning in Ajustes; offline unit-free check: write with a dummy value, read back after restart |
| 8 | decanter | `sent.jsonl` on Windows restricted, or moved to the store | `app/src/report/sent.rs`, `app/src/platform/windows.rs` | sonnet | Windows agent: `icacls` on the file shows only the user, SYSTEM, Administrators |

Phases 5 and 6 replace phase 8 of `docs/windows-online-plan.md`.

## 5. Open decisions

1. Dependencies. Move from `keyring` (v1 wrapper) to direct dependencies, all
   already in `Cargo.lock`, MIT OR Apache-2.0 (read from each crate's
   `Cargo.toml` in the registry cache), repository
   `open-source-cooperative/*`: `keyring-core` 1.0.0,
   `zbus-secret-service-keyring-store` 1.0.1, `windows-native-keyring-store`
   1.1.0, `apple-native-keyring-store` 1.0.2. Add `zeroize` 1.9.0 (Apache-2.0
   OR MIT, RustCrypto; already in the lock). For a fallback B: `argon2` 0.6.0
   and `chacha20poly1305` 0.11.0 (both MIT OR Apache-2.0, RustCrypto; in the
   registry cache, not in this lockfile). Considered, metadata not available
   offline beyond versions in the index cache (licence and maintenance
   unknown): `secrecy` (latest cached 0.10.3; not needed, `Zeroizing<String>`
   plus a manual `Debug` covers it), `linux-keyutils-keyring-store` (1.0.0),
   `db-keystore` (0.5.2).
2. Fallback policy when there is no keyring: A (session only), B (passphrase
   file), C (machine-key file, labelled unprotected), D (kernel keyring), or a
   combination (A by default, B on request).
3. psport protocol change: accept a secrets file (Linux) and stdin (Windows)
   with the environment variables kept for a transition; and when decanter
   stops exporting them.
4. Windows Credential Manager persistence: `Local` (recommended) or the
   crate's `Enterprise`; and the rewrite of existing entries.
5. Report delete codes: stay in `sent.jsonl` with an owner-only ACL on Windows,
   or move to the store.
6. Whether Ajustes may show a GOG session's or shadNet secret's existence when
   the store is locked (requires an unlock prompt for `list`).

## 6. As built

### Phase 1

- `crates/core/src/secrets/` (`mod.rs`, `key.rs`, `secret.rs`, `store.rs`, `native.rs`) is the
  only code that touches the OS keyring: `native.rs` alone names `keyring_core`; no other crate
  depends on a keyring crate. `credentials.rs` is gone, and `keyring` with it.
- `secrets::store()` builds its backend on first use and drops it when the platform reports a
  failure, so the next call (at most every 5 s) connects again. `Status` is `Unknown`,
  `Ready(BackendKind)`, `Locked` or `Missing`; a fallback backend (decision #1307, phase 7) plugs
  in behind the private `Backend` trait as another `BackendKind`, chosen where `Missing` is set.
- Entry names unchanged: `<account_id>/STOREFRONT_GOG`, `shadnet/<NPID>/{password,token}`,
  `steamgriddb/STOREFRONT_UNSPECIFIED`.
- Call sites moved to `secrets`: `crates/gog/src/lib.rs` (`read_session`, `write_session`,
  `clear_session`, the only GOG path to the store; `session.rs` and `auth.rs` call them),
  `app/src/external/art.rs` (`api_key`, `set_api_key`), `app/src/views/ps/online.rs`
  (`stored_secrets`, `copy_secrets`, `forget`), `app/src/ps_play/launch.rs` (`online_play`).
  The reports server (`crates/reports-server`) and the hook never used the keyring.

### Phase 3

- `decanter_gog::api::client::Token`: `access_token` and `refresh_token` are `Secret`s,
  (de)serialized as the same strings (`serialize_with`/`deserialize_with`), so the stored JSON is
  byte-identical: a dummy token parsed and written back before and after the change gives the
  same text. `Token` has a hand-written `Clone` and still no `Debug`.
- `Client` keeps and hands out the access token as `Secret`; the content-system callers
  (`install`, `verify`, `updates`, `save_location`, `cloud_saves`) borrow it with `expose()`.
- `cloud_saves`: `GameToken::access_token` and `BuildClient::client_secret` are `Secret`s; a
  token answer that does not parse is reported without serde_json's message, which quotes the
  value. The stored session's parse error keeps only its category, line and column.
- `bbport::online::Online` holds the shadNet password and token as `Secret`s until `env()`
  exposes them for the launch (#716, phases 5 and 6).

### Phase 4

- SteamGridDB key: `SecretKey::STEAMGRIDDB` is `steamgriddb/api_key`. A read that finds nothing
  there reads `steamgriddb/STOREFRONT_UNSPECIFIED`, writes it under the new name, and deletes the
  old entry only once the new one reads back the same; any failure keeps the old entry, returns
  its value and is retried on the next read. A put or delete also removes the old entry.
- psport launcher's `online_password`: `LauncherSettings::load` tries once per run to put it
  under the file's `online_npid` and read it back (a password the keyring already holds for that
  NPID wins), then removes it from the file. With the store missing, locked or failing, the file
  keeps it (`write_at` no longer drops it) until a later start; with no usable NPID or an empty
  value it is dropped, as before, with a warning.
- Windows: every write goes through an entry built with `persistence=Local`; a read that finds
  an entry with `Enterprise` persistence writes it again as `Local` and checks it reads back,
  else leaves it and tries on a later read.
- `Db::delete_profile` deletes its GOG accounts' sessions; one the keyring cannot delete is
  counted in a warning and left, and the profile is deleted anyway.

### Phase 2

- `Store::list(known)` returns `Listing { keys, unrecognized }`, names only: the store's own
  search (`service` on the Secret Service and Keychain, a `pattern` over the target name on
  Windows, where the store cuts the dotted service at its last dot), else one probe per `known`
  key. `Store::forget_all(known)` deletes what `list` finds one by one and counts `removed` and
  `failed`. Both fail as `StoreError::Everything` (state `locked` or `unavailable`). The search
  opens its own connection in `native::search_names`, since the `Backend` trait is not changed
  here; phase 7 may move it into the trait.
- Ajustes › General › "Credenciales guardadas" (`app/src/views/settings/credentials.rs`): a row
  per entry (kind, account name and id, or NPID), "Olvidar" and "Olvidar todo" each behind a
  `confirm_panel`, the status line with "Reintentar" while the store is not ready. A GOG session
  with no account in any profile is marked as left behind. Entries of the service that no key
  parses are not listed or deleted.
- The `SteamGridDB` field and the GOG login field are masked; the saved key is never put back in
  the field, which says a key is saved. Saving an empty field no longer deletes it.

### Not in the keyring, left for later phases

- shadNet password and token in the launch environment of the whole launch tree (#716): phases
  5 and 6.
- Report delete codes in `<library>/decanter/reports/sent.jsonl` (no ACL on Windows): phase 8.
- The GOG OAuth login code still passes through the clipboard (the field is masked now).
- The reports server's own secrets (`reports.toml`, `*_file`, systemd credentials), the Galaxy
  client id/secret (public constants) and GOG signed CDN links (memory only, stripped from
  errors): outside this store by design.
