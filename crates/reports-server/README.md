# decanter-reports

The server that receives decanter's error reports, re-validates them, and publishes the ones that pass as public
pages. One Rust binary, no database (everything in an S3-compatible bucket through `s3db`, encrypted at rest), no
accounts, no JavaScript. It speaks plain HTTP on loopback; a tunnel in
front exposes the port and terminates TLS (see `docs/deploy.md`).

Design decisions live in decanter's board: #985, #986, #987, #988, #989, #991, #1033, #1034, #1047, #1049, #1055
(tasks #993, #1048, #1056). `docs/design.md` has the data model.

## How it works

A report is not a log. decanter builds a closed JSON structure from an allow-list with the `decanter-report`
crate and sends it after the player previews it. This server runs the **same crate's** `validate` on the bytes it
receives: if anything does not fit the schema, the whole report is refused with the path and rule that failed (never
the value) and nothing is stored. A report that passes is stored exactly as validated and published at once, with no
human review. Because that is the last barrier, every value is treated as untrusted and every page escapes it.

The bucket is split by product (#1055): the reports of `decanter` and of `psport` live in two partitions,
`decanter/` and `psport/`, each its own s3db store with its own encryption config, reports, groups, indexes,
attachments and totals. A report id is the product's letter (`d` or `p`) followed by 32 hex characters, so every
route that takes an id goes straight to its partition; anything else is an unknown id.

`decanter-report` is a path dependency (`../report`), a crate of the same repository. This crate is excluded from
decanter's workspace and has its own `Cargo.lock`, so build and test it from this directory
(`cargo test` inside `crates/reports-server`). `s3db` is a git dependency pinned by
tag; `.cargo/config.toml` makes cargo fetch it with the system git.

## Endpoints

| Method and path | Who | Answer |
|---|---|---|
| `POST /v1/reports` | anyone, rate limited, `Content-Type: application/json` | 201 `{id, delete_code, url, fingerprint}`; 422 `{rejections:[{path, rule}]}`; 413 above 64 KiB; 415 without the JSON content type; 429 with `Retry-After` |
| `DELETE /v1/reports/<id>` | owner, header `X-Delete-Code` | 204; 404 for an unknown id or a wrong code (identical); 429 once the address has made too many wrong codes |
| `PUT /v1/reports/<id>/attachment` | owner, header `X-Delete-Code`, rate limited | 204; one per report, up to 5 MiB, private; 409 if there is one or the report is resolved |
| `GET /` | public | reports grouped by fingerprint, both partitions merged; `?product=decanter\|psport` reads one; `&title=CUSA00000&resolved=0\|1&page=N` |
| `GET /r/<id>` | public | one report: every field as escaped text, plus the raw JSON |
| `GET /g/<fingerprint>` | public | the reports of one group, from every partition that holds it |
| `GET /healthz` | public | `ok` |
| `GET /admin/login` | public, rate limited like a page | the login form (no script) |
| `POST /admin/login` | form field `key`, same-site `Origin` | 303 to `/admin` with the cookie `__Host-session` (`Secure; HttpOnly; SameSite=Strict; Path=/`); the uniform 404 otherwise, counted as a failure |
| `POST /admin/logout` | session (with CSRF) or key | 303 to `/admin/login`; ends the session and clears the cookie |
| `GET /admin?product=&title=&day=&fingerprint=&page=` | admin | HTML list of reports, hidden ones included, with forms to hide, restore, resolve and delete each report and, when filtered by fingerprint, its whole group |
| `GET /admin/reports?limit=&offset=&fingerprint=&day=&product=&title=` | admin | JSON list including hidden reports, both partitions merged unless `product` is given; the filters (16 hex, `YYYY-MM-DD` received day, `decanter\|psport`, `CUSA00000`) are validated, 400 `invalid_filter` otherwise |
| `POST /admin/reports/<id>/hide`, `/restore`, `/resolve` | admin | 204 |
| `DELETE /admin/reports/<id>`, or `POST /admin/reports/<id>/delete` for a form | admin | 204 |
| `POST /admin/groups/<fingerprint>/hide`, `/restore`, `/resolve` | admin | 204 for every report of the group, in both partitions; 404 if neither holds one |
| `DELETE /admin/groups/<fingerprint>`, or `POST .../delete` for a form | admin | 204; deletes the group's reports and their attachments |
| `GET /admin/reports/<id>/attachment` | admin | the file as `application/octet-stream`, `Content-Disposition: attachment` |

Public pages (everything but `/healthz` and `/admin`) are rate limited per client: 429 with `Retry-After`.

Hidden reports answer 404 on the public pages. Resolved reports stay public, marked, and lose their attachment.
"admin" means a valid key in `Authorization: Bearer`, or a browser session from `/admin/login` (see "Security
model"); a session request that changes something (any method but GET and HEAD) also needs the session's CSRF token,
from a form field `csrf` or the header `X-CSRF-Token` (sent back on every session answer), and an `Origin` (or
`Referer`) of this server, else 403. A form action answers 303 back to the admin page instead of 204.
Without a valid key or session every `/admin` route but the login form answers 404, not 401, and a wrong method on any path also answers 404 (with no `Allow` header). Every error is a small JSON `{"error": ...}`, also the framework's own (a repeated query key is `400 bad_request`).

### Try it

```sh
B=http://127.0.0.1:8787
curl -s -X POST -H 'Content-Type: application/json' --data-binary @report.json $B/v1/reports          # keep the delete_code: it is shown once
curl -s -X DELETE -H "X-Delete-Code: $CODE" $B/v1/reports/$ID
curl -s -X PUT -H "X-Delete-Code: $CODE" --data-binary @game.log $B/v1/reports/$ID/attachment

A="Authorization: Bearer $DECANTER_REPORTS_ADMIN_KEY"
curl -s -H "$A" $B/admin/reports
curl -s -H "$A" -X POST $B/admin/reports/$ID/hide
curl -s -H "$A" -X POST $B/admin/reports/$ID/restore
curl -s -H "$A" -X POST $B/admin/reports/$ID/resolve      # also deletes the attachment
curl -s -H "$A" -X DELETE $B/admin/reports/$ID
curl -s -H "$A" -o log.bin $B/admin/reports/$ID/attachment

# bulk clean-up of a flooded group
curl -s -H "$A" "$B/admin/reports?fingerprint=$FP&day=2026-10-26"
curl -s -H "$A" -X POST $B/admin/groups/$FP/hide          # also /restore and /resolve
curl -s -H "$A" -X DELETE $B/admin/groups/$FP
```

In a browser: open `$B/admin/login`, enter the key, and `/admin` lists the reports with their actions.

## Configuration

`decanter-reports [--check-config] [config.toml]`, or the path in `DECANTER_REPORTS_CONFIG`. With neither, the
defaults plus the environment apply. `deploy/config.example.toml` lists every key.
`decanter-reports inspect <decanter|psport> ls [prefix] [limit] | get <key>` lists or decrypts the objects of one
partition, read-only, with the config from `DECANTER_REPORTS_CONFIG` (`docs/runbook.md`, "Looking inside").

| Key | Default | Environment override |
|---|---|---|
| `listen` | `127.0.0.1:8787` | `DECANTER_REPORTS_LISTEN` |
| `public_url` | empty (the `url` is a path) | `DECANTER_REPORTS_PUBLIC_URL` |
| `admin_key_env` / `admin_key_file` | `DECANTER_REPORTS_ADMIN_KEY` / none | `DECANTER_REPORTS_ADMIN_KEY_ENV`, `_ADMIN_KEY_FILE` |
| `trusted_proxies`, `client_ip_header` | none | `DECANTER_REPORTS_TRUSTED_PROXIES`, `_CLIENT_IP_HEADER` |
| `retention_days` | 365 | `DECANTER_REPORTS_RETENTION_DAYS` |
| `resolved_retention_days` | 90 (0 keeps resolved reports for good) | `DECANTER_REPORTS_RESOLVED_RETENTION_DAYS` |
| `index_cache_secs` | 15 (0 turns the cache off) | `DECANTER_REPORTS_INDEX_CACHE_SECS` |
| `limits.per_ip_hour` / `per_ip_day` / `global_hour` | 10 / 50 / 500 | `DECANTER_REPORTS_LIMIT_PER_IP_HOUR`, `_PER_IP_DAY`, `_GLOBAL_HOUR` |
| `limits.get_per_ip_minute` | 120 | `DECANTER_REPORTS_LIMIT_GET_PER_IP_MINUTE` |
| `limits.failures_per_ip` / `failure_window_secs` | 10 / 600 | `DECANTER_REPORTS_LIMIT_FAILURES_PER_IP`, `_LIMIT_FAILURE_WINDOW_SECS` |
| `limits.max_reports` / `max_attachments` | 100000 / 2000, per partition | none |
| `max_connections` | 512 | `DECANTER_REPORTS_MAX_CONNECTIONS` |
| `header_timeout_secs` | 10 (also the idle keep-alive limit) | `DECANTER_REPORTS_HEADER_TIMEOUT_SECS` |
| `shutdown_grace_secs` | 5 | `DECANTER_REPORTS_SHUTDOWN_GRACE_SECS` |
| `log_level` | `info` | `DECANTER_REPORTS_LOG` |
| `admin.session_idle_secs` / `session_max_secs` | 1800 / 28800 | `DECANTER_REPORTS_ADMIN_SESSION_IDLE_SECS`, `_ADMIN_SESSION_MAX_SECS` |
| `admin.max_sessions` | 16 (a login past it ends the oldest session) | `DECANTER_REPORTS_ADMIN_MAX_SESSIONS` |
| `storage.driver` | `local` (`s3`, `r2`) | `DECANTER_REPORTS_STORAGE_DRIVER` |
| `storage.dir` | `data` (local driver only; partitions in `<dir>/decanter` and `<dir>/psport`) | `DECANTER_REPORTS_STORAGE_DIR` |
| `storage.bucket`, `endpoint`, `region`, `account_id` (r2) | empty | `DECANTER_REPORTS_STORAGE_BUCKET`, `_STORAGE_ENDPOINT`, `_STORAGE_REGION`, `_STORAGE_ACCOUNT_ID` |
| `storage.prefix` | empty: the optional common root of the partitions, `<prefix>/decanter/` and `<prefix>/psport/` (s3 and r2 only) | `DECANTER_REPORTS_STORAGE_PREFIX` |
| `storage.path_style`, `allow_insecure_http` | false, false | `DECANTER_REPORTS_STORAGE_PATH_STYLE`, `_STORAGE_ALLOW_INSECURE_HTTP` |
| `storage.access_key_id_env` / `_file` | `DECANTER_REPORTS_S3_ACCESS_KEY_ID` / none | `DECANTER_REPORTS_ACCESS_KEY_ID_ENV`, `_ACCESS_KEY_ID_FILE` |
| `storage.secret_access_key_env` / `_file` | `DECANTER_REPORTS_S3_SECRET_ACCESS_KEY` / none | `DECANTER_REPORTS_SECRET_ACCESS_KEY_ENV`, `_SECRET_ACCESS_KEY_FILE` |
| `storage.passphrase_env` / `_file` | `DECANTER_REPORTS_PASSPHRASE` / none | `DECANTER_REPORTS_PASSPHRASE_ENV`, `_PASSPHRASE_FILE` |

The server refuses to start without an admin key of at least 32 characters and a storage passphrase of at least 32
characters (plus the bucket's access key id and secret access key for `s3` and `r2`), and refuses a secret file that
group or others can read (a path starting with `$CREDENTIALS_DIRECTORY`, from systemd's `LoadCredential=`, and the
environment variables are exempt). No secret is ever printed or logged. `--check-config` validates them and makes
one read-only round trip per partition, printing one line for each: it reads the partition's encryption config and
says whether it is reachable, whether it is initialized and whether the passphrase opens it (a local partition not
created yet is not opened, since opening it would create it). `--check-config` also prints warnings for settings that let a client choose its own address (see `docs/deploy.md`). Logs go to stderr through `tracing`
and hold no report contents and no addresses.

## Security model

- **Validated.** Every upload goes through `decanter_report::validate`: size (64 KiB, checked on the declared length
  before reading and again while reading), closed JSON shape, every field's rule, the closed message vocabulary.
  The stored text is the exact validated bytes.
- **Public.** What the schema allows: product, versions and commits, OS family and version, GPU, the game's title id
  and name, settings from a closed list, the failure kind, a message template id with typed slots, failing call
  names and counts, relative fault offsets, performance figures, and days. Pages render it all as text with one
  `escape_html`; there is no script, no external resource, and the headers are `Content-Security-Policy:
  default-src 'none'; style-src 'self' 'unsafe-inline'`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`.
- **Never stored.** IP addresses, user agents, request headers, times finer than a day. The delete code is stored
  only as a SHA-256 hash; the admin key only as a hash in memory. Rejected reports are not stored and their values
  are not echoed or logged. Attachments are the one private, optional exception: kept in the
  bucket as encrypted blobs under generated names, never served except to the admin, never parsed.
- **Rate limits.** In memory only, per client address; restarting resets them. Uploads and attachment uploads
  share one per-address counter (`per_ip_hour`, `per_ip_day`; IPv6 by /48) that counts every request, rejected or
  not. The `global_hour` cap for the whole server is spent only by reports that passed validation and by
  attachments sent with the right delete code, so rejected traffic cannot use up what real players share; a
  full global cap answers 429 without counting. Page reads have their own bucket (`get_per_ip_minute`, IPv6 by
  /64) with 429 and `Retry-After`; `/healthz` and `/admin` are outside it, except `GET` and `POST /admin/login`.
- **Reads.** Every change is one s3db transaction over the reports, their group and the totals of one partition;
  reads take no lock, so a flood of reads cannot delay an upload. The index reads one `Group` row per group shown,
  never the reports (without a product filter from both partitions, merged up to the end of the page),
  and is still rendered once per `index_cache_secs` for each validated filter combination (at most 128 cached keys;
  when full, new combinations are rendered without caching); hiding, resolving or deleting anything clears it, a new
  upload does not, so a new group can take up to that long to appear. The day range and some per-filter figures of
  a group row differ slightly from a full recount (`docs/design.md`, "Data model").
- **Groups.** The fingerprint ignores the game, so a group can span titles; its row shows the title most of its
  reports name, with that title's most frequent game name and `(+N más)` for the other titles (32 titles and 8
  names per title are tracked; more count as at least one more).
- **Caps.** Each partition counts its own totals against `max_reports` and `max_attachments`, so the bucket holds up
  to twice each figure, and a flood of one product never evicts the other's reports. At `max_reports` an upload evicts the oldest unresolved reports (1% of the cap per batch, with their
  attachments, taken from an oldest-first list without reading the rest) instead of failing; resolved reports are never evicted, and if nothing can be freed the answer is
  503. At `max_attachments` the oldest attachments of unresolved reports are removed (their reports stay). Only
  the count is logged. A flood can still push genuine unresolved reports out, which is why the per-address
  limits matter and why the maintainer resolves what must be kept.
- **Trusted proxy.** The client address is the socket's, unless the socket's address is in `trusted_proxies` and
  `client_ip_header` is set: then it is the rightmost entry of that header that is not itself a trusted proxy. From
  any other peer the header is ignored, since anyone can write it.
- **Retention.** Unresolved reports received more than `retention_days` ago, and resolved reports resolved more
  than `resolved_retention_days` ago (unless 0), are deleted with their attachments by a sweep of each partition at
  startup and every 24 hours. The same sweep removes blobs no report names and repairs index entries left by failed writes.
- **Connections.** The process serves with hyper directly: at most `max_connections` at once (the next is closed
  at once), a request head must arrive within `header_timeout_secs` (slow headers and idle keep-alive connections
  are closed), and on SIGTERM open connections get `shutdown_grace_secs` before they are dropped.
- **Failures.** Wrong admin keys and wrong delete codes share one per-address bucket (`failures_per_ip` in
  `failure_window_secs`, IPv6 by /48), and so does any `/admin` request without a valid key or session (an
  expired session cookie included) and a failed login; past it every `/admin` path and every delete answers 429 with
  `Retry-After` for that address, known route or not, so the 429 names no route. Successes do not count. A
  maintainer locked out this way waits out the window or comes from another address.
- **Storage.** Every object in the bucket is encrypted by s3db under a key derived from the passphrase (each partition
  has its own encryption config, opened by the same passphrase); the bucket keys and the passphrase are credentials
  like the admin key. The `local` driver's directory is 0700 and its file
  0600 (one per partition); the process sets umask 077.
- **Admin.** One key, compared in constant time after hashing both sides; 404 without it. For a browser,
  `/admin/login` trades the key for a session: a 256-bit random id kept in memory only as its SHA-256 (so a
  restart logs everyone out), ended after `admin.session_idle_secs` without a request or `admin.session_max_secs`
  after login, at most `admin.max_sessions` at once (the oldest ends first), swept when expired. The cookie is
  `__Host-session` with `Secure; HttpOnly; SameSite=Strict; Path=/` and no `Domain`; a browser returns a `Secure`
  cookie only over HTTPS (or to `localhost`), so the login works only behind the TLS tunnel or on loopback.
  Every state-changing session request needs the session's own random CSRF token (constant-time compared) and an
  `Origin` equal to `public_url`'s (loopback origins only while `public_url` is empty or loopback); Bearer requests
  need neither. The admin pages send `Cache-Control: no-store` and `Referrer-Policy: same-origin` (with
  `no-referrer` a browser would post `Origin: null`). The key, session ids and CSRF tokens are never logged.

Not covered by the server: a report that passes validation is public. What that guarantee is worth is the schema's
(see #1033 and #1034), not this server's.

## License

AGPL-3.0-or-later, like `s3db` (the storage engine it links) and gitquarry. See `LICENSE`.

## In decanter's repository

This crate lives in decanter's repository for now (decisions #1051 and #1053), outside its workspace. It is the one
crate there that carries tests, because it is a public service that receives data from strangers: the rule of the
rest of the repository (no tests) does not apply to it. Its licence is AGPL-3.0-or-later (`LICENSE` in this
directory), because of `s3db`; the rest of decanter is GPL-3.0-or-later.
