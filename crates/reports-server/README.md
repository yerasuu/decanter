# decanter-reports

The server that receives decanter's error reports, re-validates them, and publishes the ones that pass as public
pages. One Rust binary, no database (everything in an S3-compatible bucket through `s3db`, encrypted at rest), no
accounts, no JavaScript. It speaks plain HTTP on loopback; a tunnel in
front exposes the port and terminates TLS (see `docs/deploy.md`).

Design decisions live in decanter's board: #985, #986, #987, #988, #989, #991, #1033, #1034, #1047, #1049, #1055,
#1060 (tasks #993, #1048, #1056, #1064). `docs/design.md` has the data model.

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
| `PUT /v1/reports/<id>/attachment` | owner, header `X-Delete-Code`, rate limited | 204; one per report, up to 5 MiB, private; 409 if there is one or the report is resolved; also builds the report's public clean log |
| `GET /` | public | reports grouped by fingerprint, both partitions merged; `?product=decanter\|psport` reads one; `&title=CUSA00000&resolved=0\|1&page=N` |
| `GET /r/<id>` | public | one report: its message as readable text, every field as escaped text, a summary of its clean log with a link, plus the raw JSON |
| `GET /r/<id>/log` | public | the report's clean log: the attachment's lines that match a template of the report vocabulary, rebuilt from template and slots, with line numbers and the count of omitted lines; 404 without an attachment ever sent |
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
| `GET /admin/audit?after=<record id>` | `admin` role only | HTML "Registro de cambios": every change, login and logout and every `user` command, newest first, 100 per page (When / Who / Action / Target); 400 for a malformed cursor |
| `GET /admin/audit/records?after=<record id>` | `admin` role only | the same page as JSON, `{"records": [{user, role, action, target, count, at}], "next": <record id or null>}` |
| `GET /admin/logs?product=&title=&day=&kind=&fingerprint=&page=` | admin | HTML list of the visible reports with an attached log, newest first, 50 per page: id, product, title and game, day, kind, template, size, links to the viewer, the report and the download, and A/B choices for a comparison; filters as the admin list plus `kind` (lowercase letters), 400 otherwise |
| `GET /admin/logs/<id>?page=N` | admin | the log as numbered text, `logs.page_lines` lines per page, ERROR / WARN / fault / `Calls: summary` / failed-stub-unimplemented lines highlighted with a text marker and listed at the top; 404 for an unknown, hidden or unattached id |
| `GET /admin/logs/search?q=&title=&fingerprint=&product=&day=&kind=` | admin | lines holding `q` (1 to 200 characters, ASCII case ignored) with 2 lines of context, in the logs of one title or fingerprint (400 without one), newest first, within `logs.search_max_*`; 429 while the same session runs another search |
| `GET /admin/logs/diff?a=<id>&b=<id>&normalize=1` | admin | the two logs side by side: changed, removed and added lines with both line numbers and counts; `normalize` ignores times, addresses and long numbers; 404 unless both logs are readable |

Public pages (everything but `/healthz` and `/admin`) are rate limited per client: 429 with `Retry-After`.

Hidden reports answer 404 on the public pages, their clean log too. Resolved reports stay public, marked, and lose
their attachment but keep their clean log.
"admin" means a maintainer account's key in `Authorization: Bearer`, or a browser session from `/admin/login` (see
"Security model"); the accounts are made with `decanter-reports user add` ("Maintainer accounts" below). Both roles
reach every `GET` above but the audit trail, and the logout; the hide, restore, resolve and delete routes and the
audit trail need the `admin` role, and a
`viewer`'s call answers 403 `{"error":"role"}` and changes nothing. A session request that changes something (any method but GET and HEAD) also needs the session's CSRF token,
from a form field `csrf` or the header `X-CSRF-Token` (sent back on every session answer), and an `Origin` (or
`Referer`) of this server, else 403. A form action answers 303 back to the admin page instead of 204.
Without a valid key or session every `/admin` route but the login form answers 404, not 401, and a wrong method on any path also answers 404 (with no `Allow` header). Every error is a small JSON `{"error": ...}`, also the framework's own (a repeated query key is `400 bad_request`).

### Try it

```sh
B=http://127.0.0.1:8787
curl -s -X POST -H 'Content-Type: application/json' --data-binary @report.json $B/v1/reports          # keep the delete_code: it is shown once
curl -s -X DELETE -H "X-Delete-Code: $CODE" $B/v1/reports/$ID
curl -s -X PUT -H "X-Delete-Code: $CODE" --data-binary @game.log $B/v1/reports/$ID/attachment
curl -s $B/r/$ID/log                                      # the public clean version of that log

A="Authorization: Bearer $KEY"                             # an admin account's key, drk_<8 hex>_<64 hex>
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

In a browser: open `$B/admin/login`, enter the key ("Clave de acceso"), and `/admin` lists the reports with their
actions (none for a viewer); its "Logs adjuntos" link opens the log viewer (`docs/runbook.md`, "Reading the logs").

## Maintainer accounts

| Command | Does |
|---|---|
| `decanter-reports user add <name> --role viewer\|admin [--force-reuse]` | creates the account and prints its key once on standard output (nothing else goes there); a name removed less than 24 hours ago needs `--force-reuse` |
| `decanter-reports user list` | name, role, created and last login day, state; never a key or a hash |
| `decanter-reports user reset <name>` | a new key, printed once; the old key and its sessions stop within 15 s |
| `decanter-reports user role <name> --role viewer\|admin` | changes the role; open sessions follow within 15 s |
| `decanter-reports user remove <name>` | removes the account; its key and sessions stop within 15 s |

Every command that changes an account leaves an audit record by `cli` (see `/admin/audit`).

Names are `[a-z0-9-]{1,32}` and unique; `remove` and `role` refuse to leave no admin. The commands read the same
configuration and secrets as the server (`DECANTER_REPORTS_CONFIG` and the environment); with the `local` driver the
server must be stopped. Keys are `drk_<8 hex id>_<64 hex>`, 256 random bits, kept by the server only as SHA-256 in
the encrypted `accounts/` store. `docs/runbook.md` has the first admin, lost keys and recovery.

## Configuration

`decanter-reports [--check-config] [config.toml]`, or the path in `DECANTER_REPORTS_CONFIG`. With neither, the
defaults plus the environment apply. `deploy/config.example.toml` lists every key.
`decanter-reports inspect <decanter|psport> ls [prefix] [limit] | get <key>` lists or decrypts the objects of one
partition, read-only, with the config from `DECANTER_REPORTS_CONFIG` (`docs/runbook.md`, "Looking inside").

| Key | Default | Environment override |
|---|---|---|
| `listen` | `127.0.0.1:8787` | `DECANTER_REPORTS_LISTEN` |
| `public_url` | empty (the `url` is a path) | `DECANTER_REPORTS_PUBLIC_URL` |
| `trusted_proxies`, `client_ip_header` | none | `DECANTER_REPORTS_TRUSTED_PROXIES`, `_CLIENT_IP_HEADER` |
| `retention_days` | 365 | `DECANTER_REPORTS_RETENTION_DAYS` |
| `resolved_retention_days` | 90 (0 keeps resolved reports for good) | `DECANTER_REPORTS_RESOLVED_RETENTION_DAYS` |
| `audit_retention_days` | 180 (0 keeps audit records for good) | `DECANTER_REPORTS_AUDIT_RETENTION_DAYS` |
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
| `admin.max_sessions` | 16 (a login past it ends the oldest session of the account holding the most) | `DECANTER_REPORTS_ADMIN_MAX_SESSIONS` |
| `admin.max_sessions_per_user` | 4 (an account's login past it ends that account's oldest session) | `DECANTER_REPORTS_ADMIN_MAX_SESSIONS_PER_USER` |
| `logs.page_lines` | 2000 lines of a log per page (100 to 20000) | `DECANTER_REPORTS_LOGS_PAGE_LINES` |
| `logs.search_max_logs` / `search_max_mib` / `search_max_matches` | 50 / 64 / 500 per search (1 to 1000 / 1024 / 10000) | `DECANTER_REPORTS_LOGS_SEARCH_MAX_LOGS`, `_LOGS_SEARCH_MAX_MIB`, `_LOGS_SEARCH_MAX_MATCHES` |
| `logs.diff_max_lines` | 20000 lines of both logs together, above which a comparison is positional (100 to 200000) | `DECANTER_REPORTS_LOGS_DIFF_MAX_LINES` |
| `storage.driver` | `local` (`s3`, `r2`) | `DECANTER_REPORTS_STORAGE_DRIVER` |
| `storage.dir` | `data` (local driver only; partitions in `<dir>/decanter` and `<dir>/psport`) | `DECANTER_REPORTS_STORAGE_DIR` |
| `storage.bucket`, `endpoint`, `region`, `account_id` (r2) | empty | `DECANTER_REPORTS_STORAGE_BUCKET`, `_STORAGE_ENDPOINT`, `_STORAGE_REGION`, `_STORAGE_ACCOUNT_ID` |
| `storage.prefix` | empty: the optional common root of the partitions, `<prefix>/decanter/` and `<prefix>/psport/` (s3 and r2 only) | `DECANTER_REPORTS_STORAGE_PREFIX` |
| `storage.path_style`, `allow_insecure_http` | false, false | `DECANTER_REPORTS_STORAGE_PATH_STYLE`, `_STORAGE_ALLOW_INSECURE_HTTP` |
| `storage.access_key_id_env` / `_file` | `DECANTER_REPORTS_S3_ACCESS_KEY_ID` / none | `DECANTER_REPORTS_ACCESS_KEY_ID_ENV`, `_ACCESS_KEY_ID_FILE` |
| `storage.secret_access_key_env` / `_file` | `DECANTER_REPORTS_S3_SECRET_ACCESS_KEY` / none | `DECANTER_REPORTS_SECRET_ACCESS_KEY_ENV`, `_SECRET_ACCESS_KEY_FILE` |
| `storage.passphrase_env` / `_file` | `DECANTER_REPORTS_PASSPHRASE` / none | `DECANTER_REPORTS_PASSPHRASE_ENV`, `_PASSPHRASE_FILE` |

`decanter-reports secret` prints a new passphrase (64 hex characters from the operating system's random source) and
nothing else; the serving process never makes one. The server refuses to start without a storage passphrase of at
least 32 characters, naming `secret` in the error (plus the bucket's access key id and secret access key for `s3`
and `r2`), and refuses a secret file that
group or others can read (a path starting with `$CREDENTIALS_DIRECTORY`, from systemd's `LoadCredential=`, and the
environment variables are exempt). No secret is ever printed or logged. `--check-config` validates them and makes
one read-only round trip per partition, printing one line for each: it reads the partition's encryption config and
says whether it is reachable, whether it is initialized and whether the passphrase opens it (a local partition not
created yet is not opened, since opening it would create it). The server never creates one partition's encryption
while the other partition is initialized and the passphrase does not open it: it stops naming that partition, and
`--check-config` says so on the uninitialized partition's line. `--check-config` also prints warnings for settings that let a client choose its own address (see `docs/deploy.md`), and, outside a container, for secrets read from the environment. Logs go to stderr through `tracing`
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
  only as a SHA-256 hash; admin keys only as hashes. Rejected reports are not stored and their values
  are not echoed or logged. Attachments are the one private, optional exception: kept in the
  bucket as encrypted blobs under generated names, never served except to the admin. The server reads them once,
  at upload, through `decanter_report::clean_log` to build their public **clean log**: only lines that are instances
  of the report vocabulary's templates, kept as template id and typed slots (with stricter slot rules than reports,
  `docs/design.md`), never text of the log; it is re-validated by `validate_clean_log` on every read and not shown
  if it fails.
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
  any other peer the header is ignored, since anyone can write it, and the log says `client_ip_header ignored` (at
  most every 10 minutes, naming only the peer): the sign that the proxy's address no longer matches. Addresses are
  compared in canonical form, so a dual-stack listener's `::ffff:a.b.c.d` is the IPv4 client `a.b.c.d`.
- **Retention.** Unresolved reports received more than `retention_days` ago, and resolved reports resolved more
  than `resolved_retention_days` ago (unless 0), are deleted with their attachments by a sweep of each partition at
  startup and every 24 hours. The same sweep removes blobs no report names and repairs index entries left by failed writes.
- **Audit trail.** Every change a maintainer makes (and every login, logout and `user` command that changed an
  account) leaves one record in the encrypted accounts store: name, role (`cli` for the command line), action,
  target, count and the minute in UTC; never an address, key, hash or attachment content. Written after the change,
  a failed record is a logged warning and never undoes it. Logins and logouts, which any account can repeat, are
  thinned: one `login` record per account per 5 minutes, a `logout` only when a session ended, and at most 6 of the
  two per account per minute; the rest are counted in the sweep's log line. Changes are always recorded. The daily
  sweep deletes records older than `audit_retention_days` (180, 0 keeps them); admins read them at `/admin/audit`.
- **Connections.** The process serves with hyper directly: at most `max_connections` at once (the next is closed
  at once), a request head must arrive within `header_timeout_secs` (slow headers and idle keep-alive connections
  are closed), and on SIGTERM open connections get `shutdown_grace_secs` before they are dropped.
- **Failures.** Wrong admin keys and wrong delete codes share one per-address bucket (`failures_per_ip` in
  `failure_window_secs`, IPv6 by /48), and so does any `/admin` request without a valid key or session and a
  failed login. An expired or unknown session cookie alone answers the same 404 without counting (a session id
  cannot be guessed, and counting it would lock out a maintainer who left a page open); past it every `/admin` path and every delete answers 429 with
  `Retry-After` for that address, known route or not, so the 429 names no route. Successes do not count. A
  maintainer locked out this way waits out the window or comes from another address.
- **Storage.** Every object in the bucket is encrypted by s3db under a key derived from the passphrase (each store,
  the two partitions and `accounts/`, has its own encryption config, opened by the same passphrase); the bucket keys
  and the passphrase are credentials, read from owner-only files or the environment. The `local` driver's directory
  is 0700 and its files 0600 (one per store); the process sets umask 077.
- **Admin.** One account per maintainer, with a role (`viewer` reads, `admin` also changes) and a server-generated
  key of 256 random bits kept only as its SHA-256; a key is found by its id and compared whole in constant time, and
  an unknown id, a wrong secret and a removed or disabled account answer the same 404 after the same work and count
  the same failure. The server re-reads the accounts every 15 s and then ends the sessions of removed, disabled or
  re-keyed accounts. Without an admin account the log says so hourly. For a browser, `/admin/login` trades the key
  for a session: a 256-bit random id kept in memory only as its SHA-256 (so a restart logs everyone out), ended
  after `admin.session_idle_secs` without a request or `admin.session_max_secs` after login, at most
  `admin.max_sessions_per_user` per account and `admin.max_sessions` in all, swept when expired. The cookie is
  `__Host-session` with `Secure; HttpOnly; SameSite=Strict; Path=/` and no `Domain`; a browser returns a `Secure`
  cookie only over HTTPS (or to `localhost`), so the login works only behind the TLS tunnel or on loopback.
  Every state-changing session request needs the session's own random CSRF token (constant-time compared) and an
  `Origin` equal to `public_url`'s (loopback origins only while `public_url` is empty or loopback); Bearer requests
  need neither. The admin pages send `Cache-Control: no-store` and `Referrer-Policy: same-origin` (with
  `no-referrer` a browser would post `Origin: null`). Keys, session ids and CSRF tokens are never logged; every
  admin log line names the account and its role, never an address.
- **Log viewer.** The raw logs stay private: only the admin pages under `/admin/logs` show them, as escaped text
  with control and bidirectional characters shown as visible markers, lines cut at 2000 characters, pages of
  `logs.page_lines` lines, every pass over a log linear and the heavy ones off the async workers. Searches are
  confined to one title or fingerprint and capped in logs, bytes and matches, one per account at a time; a
  comparison runs a diff bounded in lines and edits. Hidden reports' logs stay out of it (their download remains).

Not covered by the server: a report that passes validation is public. What that guarantee is worth is the schema's
(see #1033 and #1034), not this server's.

## License

AGPL-3.0-or-later, like `s3db` (the storage engine it links) and gitquarry. See `LICENSE`.

## In decanter's repository

This crate lives in decanter's repository for now (decisions #1051 and #1053), outside its workspace. It is the one
crate there that carries tests, because it is a public service that receives data from strangers: the rule of the
rest of the repository (no tests) does not apply to it. Its licence is AGPL-3.0-or-later (`LICENSE` in this
directory), because of `s3db`; the rest of decanter is GPL-3.0-or-later.
