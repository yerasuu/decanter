# GOG cloud saves: behaviour specification for the clean-room rewrite

Step A of kb #1525. `crates/gog/src/cloud_saves.rs` was ported from heroic-gogdl (GPL-3.0, no "or later"
notice). This document says what the protocol does and what decanter's sync must do, so that the file can be
written again from scratch by someone who never reads heroic-gogdl, the current `cloud_saves.rs` or its history,
and released under GPL-3.0-or-later with the rest of the crate. It follows the method and layout of
`docs/gog-api-behaviour-spec.md` and the record kept in `crates/gog/src/api/VENDOR.md`.

Whoever writes the new file reads this document, the public items named in section 5.1, and decanter's own
modules (`session.rs`, `depot.rs`, `api/client/`, `decanter_core::secrets`, `decanter_core::platform`). They do
not read the file being replaced, nor heroic-gogdl, nor their history.

How facts are marked:

- **READ**: seen in a named public source with a permissive licence (the table in section 7 gives repository,
  licence, path). Sources are cited by id: `L` lgogdownloader, `G2` gogg, `R` GogDownloader.
- **READ (from gogdl, unconfirmed elsewhere)**: only heroic-gogdl (GPL-3.0) states it. Used to confirm facts the
  permissive sources leave silent; a rewrite may rely on them only as UNVERIFIED until the test plan of
  section 6 settles them.
- **UNVERIFIED**: no source states it, or sources disagree. Nothing here was tested against GOG: no request to GOG
  or to any account was made while writing this.

GOG publishes no documentation for `cloudstorage.gog.com`. Everything below is what other clients do.

## Contents

1. Endpoints and the per-game token
2. The object store protocol
3. Timestamp semantics
4. Decanter's sync design
5. Requirements to preserve
6. Open questions and a test plan for the owner's account
7. Sources

## 1. Endpoints and the per-game token

The bucket does not answer to the user's login token. It answers to a token issued for the game build's own
Galaxy OAuth client, obtained in four steps.

| Step | Request | Answer used |
| --- | --- | --- |
| 1. Builds | `GET https://content-system.gog.com/products/<product id>/os/<os>/builds?generation=2`, bearer login token. `<os>` is `windows`, `osx` or `linux` | `items[]`, each with `link` (G2, L). When `link` is empty, `urls[0].url` (G2) |
| 2. Build manifest | `GET <link>` of the chosen item, no authorisation (G2) | JSON `clientId` and `clientSecret` of the build. Generation 2 manifests are zlib-compressed, older ones plain (G2); L accepts generation 2 only and skips builds of another generation |
| 3. Token exchange | `GET https://auth.gog.com/token` with query `client_id=<build client id>`, `client_secret=<build client secret>`, `grant_type=refresh_token`, `refresh_token=<user's refresh token>`, `without_new_session=1` (G2, R, L) | `access_token` and `user_id` (G2). `expires_in` too (R). Authorisation is the query, no header |
| 4. Bucket | `https://cloudstorage.gog.com/v1/<user_id>/<build client id>` (G2, L, R) | see section 2 |

Facts and caveats:

- `user_id` of the bucket path is the one the token exchange returns (G2). R and L take it from the account's own
  data; whether the two always agree is UNVERIFIED. The rewrite uses the token answer's `user_id`.
- `without_new_session=1` makes the exchange not start a new login session, so the user's own refresh token stays
  valid (G2, R, L, and gogdl). A request without it is not described by any source for this exchange. L sends it
  for game clients and omits it otherwise (a flag it calls "new session"). What GOG does on omission is
  UNVERIFIED; always send it.
- The exchange uses the user's refresh token and answers a token valid for the build's client only. Whether the
  answer carries a new refresh token that replaces the user's is not read anywhere; decanter must keep using its
  own stored session and never store the answer's tokens (requirement, section 5.2).
- Which build supplies the client id: the build decanter already selects for the game on the current platform
  (`depot::get_builds`, `depot::select_build`); sources take the first item (G2) or the item matching a chosen
  build id (L). Whether different builds of one game ever carry different client ids is UNVERIFIED.
- Platform spelling: the content system says `osx` where decanter says "mac" (G2).
- Quota and enablement live elsewhere: `GET https://remote-config.gog.com/components/galaxy_client/clients/<client id>?component_version=<ver>`
  (R uses `2.0.43`, L `2.0.51`). R reads `content.cloudStorage.quota` and treats `> 0` as "game supports cloud
  saves". L reads `content.<Windows|...>.cloudStorage.enabled` and `.locations[]` with `name` and `location`.
  That answer drives the save-location detection (`crates/gog/src/save_location.rs`), not the sync, and is out
  of scope here beyond the fact that a location's `name` is the first path component of bucket names (section 2.6).
- Client id and secret of a build are credentials: the secret is a `Secret`, never logged and never put in an
  error (section 5.2).

## 2. The object store protocol

Base: `https://cloudstorage.gog.com/v1/<user_id>/<client id>`. Every request carries
`Authorization: Bearer <game token>` (G2, R, L, gogdl).

### 2.1 List

`GET <base>` with `Accept: application/json` (G2, R, L). The answer is a JSON array of objects (G2, R, L):

| Field | Meaning |
| --- | --- |
| `name` | the object's name, `/`-separated, qualified by location (section 2.6) |
| `hash` | md5 in lowercase hex of the stored (compressed) body (G2, R; see 2.3). May be null (R types it nullable) |
| `last_modified` | RFC 3339 timestamp of the object (G2 parses RFC 3339; L parses ISO 8601 extended) |
| `bytes` | stored size (R, L) |
| `content_type` | present (R) |

- The listing is of the whole bucket; there is no paging parameter in any source. Whether a very large bucket is
  truncated is UNVERIFIED.
- An entry with an empty `name` is ignored (G2). An entry whose `last_modified` does not parse is kept with an
  unknown time (G2); decanter treats an unparseable time as "changed" (section 4).
- Deleted objects stay in the listing as tombstones (2.5).
- 404 on the listing means no saves yet (gogdl, G2); G2 also treats 400 the same way and says "the probe against
  the live service showed" so (a comment, not reproduced here). Both UNVERIFIED for a rewrite (section 6, Q3).

### 2.2 Download

`GET <base>/<name>` (G2, R, L). Observed behaviour:

- R asks `Accept-Encoding: deflate, gzip` and gunzips the body itself, hashing the raw body: the stored object
  is gzip and its md5 is the listing's `hash` (R).
- G2 gunzips a body that still starts with the gzip magic `1f 8b` after the HTTP stack has had its chance to
  decode `Content-Encoding`. A client must therefore not assume which of the two happened: inspect the first two
  bytes.
- The object's original modification time comes back in the response header `X-Object-Meta-LocalLastModified`
  (G2, gogdl). When absent, G2 falls back on the listing's `last_modified`. L takes the time of the HTTP
  response's `Last-Modified` instead (curl's file time); it does not read the metadata header.
- Anything but 200 is a failure (G2). Whether the body can be partial (206) is not stated; L resumes with Range
  against a `.~incomplete` file (L). Decanter does not resume.

### 2.3 Upload

`PUT <base>/<name>` with the body and these headers (gogdl; L is the only permissive source and differs, below):

| Header | Value |
| --- | --- |
| `Authorization` | `Bearer <game token>` |
| `Content-Encoding` | `gzip` |
| `Etag` | md5, lowercase hex, of the compressed body that is sent |
| `X-Object-Meta-LocalLastModified` | the local file's modification time, UTC, ISO 8601, see section 3 |

- gogdl also sends the same `User-Agent` string under `X-Object-Meta-User-Agent` (from gogdl, unconfirmed
  elsewhere). Whether it is needed is UNVERIFIED.
- L sends `Authorization`, `X-Object-Meta-LocalLastModified`, `Etag` (md5 of the body it sends), `Content-Type:
  Octet-Stream` and `Content-Length`, and does not set `Content-Encoding` nor compress anything (L). So the
  service accepted, at least at some time, an upload whose `Etag` is the md5 of the body as sent. Whether such an
  object then reads back as plain or as gzip is UNVERIFIED; decanter uploads gzip only, like Galaxy.
- A successful upload answers 2xx (G2 does not write; gogdl treats `ok` as success). The body of the answer is
  not read by any source. L retries on timeouts and 5xx-class transport errors and not on 400, 416 or 422.
- The `Etag` is what the service compares against the received body; a mismatch is presumably refused
  (UNVERIFIED, no source says what status).

Why the compression must be reproducible. The listing's `hash` is the md5 of the compressed bytes, not of the
file. Two clients that gzip the same content differently produce two hashes for equal content. gogdl compresses
with level 6 and the gzip header's modification time set to 0, so equal content gives equal bytes and an equal
md5 on every upload (from gogdl, unconfirmed elsewhere); the point of a fixed header time is that gzip stores
the compression time by default, which would change the md5 on every run. Consequences for decanter:

- Level 6 and header time 0 are what to produce when uploading (UNVERIFIED that the service requires them; they
  are what makes a re-upload of unchanged content byte-identical).
- The byte stream of a deflate encoder depends on the implementation, not only on the level: a Rust encoder and
  zlib at "level 6" need not emit the same bytes. So a local md5 computed by decanter equals the hash GOG holds
  only for objects decanter itself uploaded with the same encoder, never for objects Galaxy or another client
  wrote. Decanter therefore never concludes "unchanged" or "changed" from a local md5 against the cloud's hash
  alone (section 4.3).

### 2.4 Delete

`DELETE <base>/<name>` with the bearer header (L, gogdl). No body, no other header. A 2xx is success. After a
delete the listing may keep an entry for the name as a tombstone (2.5); whether a DELETE produces that entry,
or the entry only appears when Galaxy deletes a file, is UNVERIFIED (Q4).

### 2.5 Tombstone

A listing entry whose `hash` is `aadd86936a80ee8a369579c3926f1b3c` means the file is deleted (R names it
"deleted"; G2 calls it an empty placeholder written by Galaxy and skips it on backup; gogdl drops it from
downloads). Decanter treats such an entry as absent, never downloads it and never writes it to disk.

- It is the md5 of the stored form of an empty object as Galaxy writes it. It is not the md5 of zero bytes
  (`d41d8cd98f00b204e9800998ecf8427e`). Python's gzip of empty input at levels 1, 6 and 9 with header time 0
  gives other md5s too (`6a632241...`, `163be0a8...`, `fa9ac5a2...`), so how the constant is produced is
  UNVERIFIED (it may be a different header, a stored block or a different OS byte). Decanter compares against
  the constant and never tries to compute it.
- Whether decanter should ever write a tombstone itself (to propagate a local deletion) instead of using DELETE
  is UNVERIFIED (Q4); the design uses DELETE (the verb two sources use).

### 2.6 Names

- A bucket name starts with the cloud save location's name (the `name` of an entry of `locations` in the
  remote-config answer; `__default` when the game declares none), then `/`, then the path inside the location,
  always `/`-separated (G2 example `__default/save1.dat`; L builds names as location name plus relative path and
  falls back to `__default` when a game has no locations; gogdl filters listings by that first component, from
  gogdl, unconfirmed elsewhere). Local `\` separators are converted to `/` before use (gogdl, from gogdl,
  unconfirmed elsewhere).
- Percent-encoding in the URL, as the sources do it: G2 escapes each `/`-separated segment as a path segment
  (separators kept); R only replaces `#` with `%23`; L inserts names raw; gogdl percent-encodes the whole name
  except `/` (from gogdl, unconfirmed elsewhere). Decanter percent-encodes each segment with the RFC 3986
  path-segment rule and keeps `/`. Whether the service decodes `%2F` is UNVERIFIED; names never contain an
  encoded separator on decanter's side.
- A listed `name` is data from the service: it can hold `..`, an absolute path, a drive letter or a `\`. G2
  refuses entries whose name escapes the target directory; decanter does the same (section 5.2).
- v1 of decanter syncs a manually chosen local folder against the whole bucket, flat by the part after the
  location name (the current behaviour, kept). Whether names of several locations of one game must be kept
  apart is a later step and out of scope.

### 2.7 Commit

After a sync that changed the bucket, gogdl sends `POST <base>` with the bearer header and no body, and logs a
failure if the answer is not 2xx (from gogdl, unconfirmed elsewhere). Neither L, G2 nor R makes such a request:
L and R write and delete without it (R never writes). What the commit does (makes uploads visible to Galaxy's own
clients, closes a session) and whether skipping it loses data is UNVERIFIED (Q1). Decanter sends it once after
every sync that uploaded or deleted at least one object, and treats a failed commit as a warning that does not
undo the sync.

### 2.8 Headers common to all calls

- `Authorization: Bearer <access token>` on every bucket call (G2, R, L).
- `User-Agent`. G2 says the service refuses other agents and sends
  `GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) dont_sync_marker/true installation_source/gog` on
  every call, including auth and content-system ones (G2's comment, not reproduced by testing). gogdl sends the
  same string (from gogdl, unconfirmed elsewhere). R sends `RikudouSage/GogDownloader dont_sync_marker/true` on
  list and download and works with it (R, no word on whether it was ever refused), which contradicts "other
  agents are refused" as G2 states it; L sends a configurable agent, default not read. So: UNVERIFIED whether any
  `User-Agent` is required; the token that decides it is probably `dont_sync_marker/true` (R and G2 both contain
  it; what it means, for instance "this client is not Galaxy's own sync engine", is UNVERIFIED). Decanter sends
  `<decanter product token> dont_sync_marker/true` unless Q2 shows it needs the Galaxy string.
- Timeouts, redirects and size caps follow `api/client/http.rs`: HTTPS only, 30 s total, at most 5 redirects, only
  on GOG hosts. The cloudstorage host must be added to that allow-list if it is not already there. G2 caps small
  answers at 64 MiB. Decanter caps a listing at a few MiB and a single object by the quota it knows (Q5).

## 3. Timestamp semantics

| Where | Form | Source |
| --- | --- | --- |
| Listing `last_modified` | RFC 3339 / ISO 8601 extended with a zone | G2, L |
| Header `X-Object-Meta-LocalLastModified`, written | UTC, seconds precision, ISO 8601 with an explicit `+00:00` offset, e.g. `2026-11-03T10:15:30+00:00` | gogdl (from gogdl, unconfirmed elsewhere); L writes ISO 8601 extended of a time-of-day type, with no zone suffix, subtracting one second from the file's mtime |
| Header, read back | RFC 3339 (G2); an unparseable header is ignored and the file keeps the time it was given (G2, gogdl) | G2 |

- Precision: seconds. A client that wrote fractional seconds is not described. Decanter writes whole seconds and
  accepts fractions when reading.
- Zone: write UTC with `Z`-equivalent explicit offset; accept any RFC 3339 offset when reading. A timestamp with no
  zone (as L writes) is read as UTC (UNVERIFIED that this is what the service means).
- After a download the local file's modification time is set to the cloud's recorded time (the header, else the
  listing's `last_modified`) (G2, gogdl, L). This is what keeps the next sync from seeing the file as changed.
- L subtracts one second from the local mtime when it compares and when it uploads, presumably to absorb
  rounding between seconds and fractions; no other source does. Decanter truncates every time to whole seconds
  before comparing and treats equal seconds as equal.
- Whether the listing's `last_modified` is the upload's server time or the `LocalLastModified` metadata is
  UNVERIFIED (Q6). The design below uses the listing's value as "when the cloud copy last changed" and the
  header as "the file's own time", and never needs the two to be the same.

## 4. Decanter's sync design

This section is decanter's own design, stated as behaviour. It uses one piece of persistent state per game, the
`last_sync` instant in Unix seconds that decanter's own bookkeeping already records (`game.cloud_save_last_sync`),
and no per-file memory.

### 4.1 Inputs

- The local root, a folder the user chose; every regular file below it, with its path relative to the root
  (`/`-separated) and its modification time in whole seconds. Files whose name marks them as temporary (a
  partial download of this very sync) are ignored, and so are files that are not regular files.
- The cloud listing (2.1) with tombstones (2.5) removed. Entries whose names are unsafe (2.6) are dropped with a
  warning and counted, never fatal.
- `last_sync`, absent for a game never synced, and an optional forced direction.

### 4.2 Definitions

For a path present in a side:

- *local changed* = local mtime is later than `last_sync` (whole seconds), or `last_sync` is absent;
- *cloud changed* = the listing's `last_modified` is later than `last_sync`, or is unparseable, or `last_sync` is
  absent.

### 4.3 Per-path decision

Compute the union of paths of both sides, in a stable order.

| Local | Cloud | Decision |
| --- | --- | --- |
| present | absent (or tombstone) | local changed: **upload** (new file or a cloud deletion that the user overrode by editing). Not changed: the cloud lost it and the local copy is only the stale remainder, a **lone deletion**: delete the local file |
| absent | present | cloud changed: **download**. Not changed: the local side lost it, a **lone deletion**: delete the cloud object |
| present | present, same content (4.4) | nothing |
| present | present, different | only local changed: **upload**; only cloud changed: **download**; both changed: **conflict**; neither changed: **conflict** (the sides disagree without either having moved since the last sync, so the record cannot say which is right) |

- A lone deletion needs `last_sync`: without it nothing is ever deleted, only uploaded and downloaded.
- A path that exists on both sides and is unchanged on both and equal is not listed anywhere.

### 4.4 "Same content"

The cloud `hash` is the md5 of compressed bytes (2.3), so local plaintext cannot be compared to it in general.
Decanter treats content as the same when any of these hold, in this order of cost:

1. local mtime equals the cloud's recorded time in whole seconds and local size equals the decompressed size
   (known only after a download, so used only for files this sync just moved);
2. the local file, gzipped by decanter's own encoder at the fixed level and header time, has the md5 the cloud
   reports (true for objects decanter itself uploaded);
3. only when the decision would otherwise be *conflict*: the cloud object is downloaded to memory and compared
   byte for byte with the local file. Equal means no conflict and no transfer (the local mtime is then set to
   the cloud's time).

Never decide anything else from a hash alone.

### 4.5 Whole-sync outcome

- If any path is a *conflict*, nothing is moved at all, no file is written, uploaded or deleted, and the outcome
  is a conflict. `last_sync` in the outcome is not meaningful (the caller does not record it). The user then
  picks a direction.
- Forced direction **upload**: every local file that differs from the cloud (4.4) is uploaded; cloud objects the
  local side lacks are left alone. Forced **download**: every cloud file that differs is downloaded, overwriting
  the local copy; local-only files are left alone. Under force no path is deleted on either side, and the
  outcome's action is that direction. (A design choice; the existing behaviour for deletions under force must be
  compared with it before writing, and the owner decides if they differ. They are the one place this spec does
  not take the current file's behaviour as given, because it is not read here.)
- Otherwise the outcome's action is *upload* if only uploads and cloud deletions happened, *download* if only
  downloads and local deletions happened, *none* if nothing moved, and when both directions occurred (distinct
  files changed on each side) both are performed and the action reported is upload. Counts of uploaded and
  downloaded files are in the outcome.
- `last_sync` of the outcome is the instant the sync began (read before scanning and listing), in Unix seconds,
  so a file the user saves during the sync is still seen as changed next time.
- The first failure aborts the sync (no retry loop, no skipping). Files already moved stay moved; the outcome is
  an error and `last_sync` is not recorded.
- After a sync with at least one upload or cloud deletion, send the commit (2.7).

### 4.6 Order of work

1. Validate the product id and make the local root.
2. Exchange the game token (section 1). A refusal of the login session by GOG is told apart from GOG not
   answering (section 5.2, as `exchange_game_token` already does for its other caller).
3. Scan local files and list the cloud, then decide every path (4.3) before changing anything. A conflict ends
   here with no change.
4. Do the transfers and deletions, one at a time, aborting on the first failure.
5. Commit, report.

## 5. Requirements to preserve

### 5.1 Public surface other code imports

Confirmed by grep across the repository:

| Item | Used by |
| --- | --- |
| `cloud_saves::sync(account_id: &str, product_id: &str, local_root: &Path, last_sync: Option<i64>, force: Option<ForceDirection>) -> Result<SyncOutcome>` (async) | `app/src/launch/mod.rs` (before and after a launch), `app/src/views/library/properties/cloud_saves.rs` |
| `SyncOutcome { action, uploaded, downloaded, last_sync }` (`#[non_exhaustive]`, `last_sync` in Unix seconds, meaningless on a conflict) | same two callers; the caller records `last_sync` through `record_cloud_save_sync` unless the action is `Conflict` |
| `SyncAction { Upload, Download, Conflict, None }` | same |
| `ForceDirection { Upload, Download }` (`#[non_exhaustive]`) | `cloud_saves.rs` view |
| `CloudFile { relative_path, md5, last_modified: DateTime<Utc> }` (`#[non_exhaustive]`) | public, no other importer found; keep |
| `exchange_game_token(session: &Token, game_id: i64) -> Result<GameToken>` (`pub(crate)`, async) | `crates/gog/src/user_stats.rs` |
| `GameToken { access_token: Secret, user_id: String, client_id: String }` | `user_stats.rs` |
| `resolve_build_client(access_token: &str, game_id: i64) -> Result<BuildClient>` (`pub(crate)`, async) | `crates/gog/src/save_location.rs` |
| `BuildClient { client_id: String, client_secret: Secret }` | `save_location.rs` |
| `Result<T>` = `Result<T, anyhow::Error>` | module |

The names, fields and signatures above must not change. The errors that `exchange_game_token` returns keep
carrying the typed `api::client::Error` somewhere in the `anyhow` chain, because its caller in `user_stats.rs`
tells "GOG refused the session" from "GOG unreachable" by it (the contract of `docs/gog-api-behaviour-spec.md`
section 2.1); the rewriter confirms by reading `user_stats.rs`.

### 5.2 Decanter-own behaviours

- **Name sanitising.** Every cloud entry name is checked before it becomes a local path: refuse an empty name, an
  absolute path, a drive or UNC prefix, any `..` component, a `\` (or convert it only if the cloud convention is
  that it is a separator, never as an escape), a NUL, and a name that resolves to the root itself. A refused
  entry is skipped with a warning and counted; it is not an error and never a download.
- **Atomic local writes with fsync.** A download is written to a temporary file in the destination's directory,
  flushed and `fsync`ed, then renamed over the target, and the directory is synced where the platform allows. The
  modification time (section 3) is set on the file before or at the rename so the file never exists with a wrong
  time. A failure leaves the old file untouched and removes the temporary file.
- **Secret handling.** Game access tokens, build client secrets and the user's refresh token are
  `decanter_core::secrets::Secret` and are exposed only at the point of the request. They are never logged, never
  in an error string, never in a `Debug`.
- **Redaction.** Errors from the HTTP stack are stripped of the request URL (it contains the user id and, on the
  token exchange, the client secret and refresh token in the query). A JSON decode error of a token answer
  reports only a position, never a fragment of the body (the `has_secrets` mode of `Call` in `api/client/http.rs`).
  Object names may be logged as data; bodies never.
- **Lone-deletion detection** when only one side changed since `last_sync` (4.3); no deletion without a
  `last_sync`; a deletion failure aborts the sync.
- **Bookkeeping.** `SyncOutcome` as in 5.1; `last_sync` is the instant the sync started; nothing is recorded for a
  conflict or an error.
- **Abort on first failure**, with a distinct, specific log line per failing step (token, listing, read, upload,
  download, delete, commit), the way the current structured events are named (`crates/report/src/vocab/decanter/gog.rs`
  already holds the vocabulary; reuse its messages and keep their set).
- **Errors by meaning**, as the rest of the client: GOG refusing a credential, an unreachable service, a
  malformed answer and a local file error are distinct causes, never one catch-all.
- **No panics** on any reachable path, no `unsafe`, clippy at the workspace level, no tests (repository rule),
  comments only where they explain why.
- **Licence header**: `// SPDX-License-Identifier: GPL-3.0-or-later`, no based-on line and no mention of
  heroic-gogdl in the new file or its comments.

## 6. Open questions and a test plan for the owner's account

Not run. The owner runs these against one game that has cloud saves enabled, with a throwaway save file, on the
owner's own account, and the answers go back into this document.

| Q | Question | How to settle it |
| --- | --- | --- |
| Q1 | Does the commit (2.7) matter? Is it needed for Galaxy's own client (or another machine's decanter) to see uploads? | Upload a new file without a commit, list from a second token exchange: is it listed? Then commit, list again. Repeat after a delete. Check a Windows Galaxy client for the same game (if available) |
| Q2 | Which `User-Agent` is required, if any? | List and download with no agent, with `dont_sync_marker/true` appended to a decanter agent, and with the Galaxy string. Note status of each |
| Q3 | What does a listing return on first use, for a game with cloud saves never used, and for a game without cloud saves? 404, 400, `[]`? | List the bucket of a game never synced and of a game with no cloud support; record status and body (redacted) |
| Q4 | Does `DELETE` remove the entry or leave a tombstone? How does a tombstone appear (hash, bytes, `last_modified`)? | Upload a file, delete it, list immediately and after a commit |
| Q5 | Limits: maximum object size, maximum number of files, bucket quota (compare `quota` of remote-config), answer to going over | Upload objects of growing size on a throwaway client id only if one exists; otherwise only read the quota and record what the service says. Do not fill a real game's bucket |
| Q6 | Is `last_modified` of the listing the server time or `LocalLastModified`? What precision does the header keep? | Upload with a header time far in the past (and with fractions) and list |
| Q7 | Does the service require `Content-Encoding: gzip` and a matching `Etag`? What status on a mismatch? Is level 6 / header time 0 required, or does any valid gzip with a correct `Etag` do? | Upload the same content twice with different gzip bytes; upload with a wrong `Etag`; upload uncompressed |
| Q8 | Is the tombstone md5 reproducible? What does an empty upload from decanter hash to, and does it read as a tombstone? | Upload an empty object and a one-byte object, list `hash` |
| Q9 | Percent-encoding: `#`, `%`, spaces, non-ASCII and `?` in names; does `%2F` decode to a separator? | Upload files with those names, list and download them |
| Q10 | Does the token exchange without `without_new_session=1` invalidate the user's login session? And does the answer's refresh token differ from the user's? | Do the exchange once each way and check the login session still refreshes. Use a throwaway login if possible |
| Q11 | Do two builds of one game give the same client id? Does the bucket name depend on it? | Compare `clientId` of builds on the owner's installs |
| Q12 | Is `user_id` of the token answer always the account's id decanter has stored? | Compare for the owner's account |
| Q13 | Does a very large listing page or truncate? | Only if a game with many files exists |

## 7. Sources

Licences read from each repository's licence file through the GitHub API on 2026-11-03.

| Id | Repository | Licence | Path |
| --- | --- | --- | --- |
| L | Sude-/lgogdownloader | WTFPL | `src/downloader.cpp` (the `cloudstorage.gog.com` upload, download, delete and listing code, around lines 2582 to 2930, 5095 to 5190, 5295 to 5337), `src/galaxyapi.cpp` (token refresh with `without_new_session`, builds and remote-config URLs) |
| G2 | habedi/gogg | MIT | `client/cloudsaves.go` (and its header comment) |
| R | RikudouSage/GogDownloader | MIT | `src/Service/CloudSavesManager.php`; `src/DTO/SaveGameFile.php` (listing fields, the deleted test), `src/Service/AuthenticationManager.php` (the game token exchange), `src/Service/GameMetadataManager.php` (client id and secret from the build) and `config/services.yaml` (the user agent string) |
| gogdl | Heroic-Games-Launcher/heroic-gogdl | GPL-3.0, no "or later" | `gogdl/saves.py`, `gogdl/auth.py`. Used only to confirm facts; each is tagged "from gogdl, unconfirmed elsewhere" |

Fact to source:

| Fact | Source |
| --- | --- |
| Base URL `cloudstorage.gog.com/v1/<user_id>/<client id>`, bearer on every call | G2, R, L (+ gogdl) |
| Game-scoped token: client id and secret from the build manifest, refresh-token exchange | G2, R, L (+ gogdl) |
| `without_new_session=1` on the exchange | G2, R, L (+ gogdl) |
| Builds URL `content-system.gog.com/products/<id>/os/<os>/builds?generation=2` | G2, L |
| Build `link`, fallback `urls[0].url`, zlib manifest, `clientId`/`clientSecret` | G2 (L only generation 2) |
| `osx` spelling | G2 |
| Listing fields `name`, `hash`, `last_modified` | G2, R, L |
| Listing fields `bytes`, `content_type` | R, L (`bytes`); R (`content_type`) |
| `Accept: application/json` on the listing | G2, R, L |
| Listing 404 means empty; 400 also (G2 only) | gogdl (404); G2 (404 and 400, with its own claim of a live probe) |
| Download `Accept-Encoding: deflate, gzip`, md5 of raw body equals `hash` | R |
| Gzip magic check on the body | G2 |
| `X-Object-Meta-LocalLastModified` on download, fallback to listing time | G2, gogdl |
| Upload `PUT`, `Etag` md5 of body sent, `X-Object-Meta-LocalLastModified` | L (uncompressed), gogdl (gzip) |
| `Content-Encoding: gzip` on upload; gzip level 6, header time 0 | gogdl only |
| `X-Object-Meta-User-Agent` header | gogdl only |
| Header time: UTC, seconds, `+00:00` | gogdl only (L: ISO extended without zone, minus one second) |
| Download sets the file's mtime to the cloud time | G2, gogdl, L (from HTTP `Last-Modified`) |
| Delete is `DELETE <base>/<name>` | L, gogdl |
| Tombstone `hash` `aadd86936a80ee8a369579c3926f1b3c` | G2, R (+ gogdl) |
| Tombstone is not md5 of empty input nor of gzip-empty at levels 1, 6, 9 with header time 0 | computed while writing this document, with Python's `gzip` and `hashlib` on empty input; no source states it |
| Commit `POST <base>` | gogdl only |
| Names `<location>/<path>`, `__default` | G2 (example), L (location names, `__default` fallback) |
| First path component filtering, `\` to `/` | gogdl only |
| URL percent-encoding per segment | G2; R (`#` only); L (raw) |
| `User-Agent` `GOGGalaxyCommunicationService/2.0.13.27 ... dont_sync_marker/true installation_source/gog` | G2 (claims required), gogdl |
| `User-Agent` `RikudouSage/GogDownloader dont_sync_marker/true` | R (`config/services.yaml`, `CloudSavesManager.php`) |
| remote-config URL, `cloudStorage.quota`, `enabled`, `locations[].name/location` | R (`quota`, version 2.0.43), L (`enabled`, `locations`, version 2.0.51) |
| Names escaping the target directory must be refused | G2 (behaviour); decanter requirement |
| Everything in sections 4 and 5 | decanter's own design and requirements |
