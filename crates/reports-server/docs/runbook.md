# Maintainer runbook

Commands for the people running the server. `B` is the base URL, `A` the admin header. Every maintainer has an
account and a key of their own ("Maintainer accounts" below); there is no key in the configuration. Without a valid
key every `/admin` route answers 404; an address that fails a key (or a delete code) `failures_per_ip` times in
`failure_window_secs` gets 429 on all `/admin/` paths until the window passes.

```sh
B=https://reports.example.org            # or http://127.0.0.1:8787 on the host
A="Authorization: Bearer $KEY"             # KEY: your account's key, drk_<8 hex>_<64 hex>
```

## Maintainer accounts

Accounts live encrypted in the bucket (`accounts/`) and are managed with the binary, with the server's own
configuration and secrets. Under Compose prefix each command with `docker compose run --rm --no-deps
decanter-reports`; under systemd run it as root with the credential overrides of `--check-config`
(`docs/deploy.md`). With the `local` driver the server must be stopped first (the command says so when the
database is locked); with `s3` or `r2` it may keep running.

```sh
decanter-reports user add ana --role viewer     # prints ana's key once on stdout; give it to her privately
decanter-reports user list                      # name, role, created, last login, state; never a key or hash
decanter-reports user role ana --role admin     # viewer or admin
decanter-reports user reset ana                 # a new key, printed once; the old one stops working
decanter-reports user remove ana
decanter-reports user add ana --role viewer --force-reuse   # within 24 h of removing an ana: see below
```

A name is 1 to 32 characters among `a-z`, `0-9` and `-`, and unique. A removed name is held for 24 hours: `user add`
refuses it in that time, because the audit trail keeps naming the old account by it and records of the two would
read as one person's. Pass `--force-reuse` when it is the same person coming back, or the mix-up does not matter. Only `add` and `reset` write to standard output,
and only the key, so `KEY=$(decanter-reports user add ...)` captures it whole; the key is never logged and the
server keeps only its SHA-256. `remove` and `role` refuse to leave no admin. The server re-reads the accounts every
15 s: a new account works at once, and after `remove`, `reset` or `role` the old key and the sessions opened with it
(or, for `role`, their rights) change within 15 s, without a restart.

- **`viewer`** reads everything under `/admin`: the lists, the rows, attachments, the log viewer, search and diff.
  Its page shows no buttons; a change it sends answers 403 `{"error":"role"}`.
- **`admin`** also hides, restores, resolves and deletes.

Every `user add`, `remove`, `reset` and `role` that changes something leaves an audit record by `cli` (see "The
audit trail"); if that record cannot be written the command says so on stderr and the change stands.

**Lost key.** Of one person: an admin runs `user reset <name>` and hands over the new key. Of every admin: whoever
holds the configuration and secrets (`.env`, or the credential files) runs `user add <new name> --role admin`, then
removes or resets the old accounts. **Lost passphrase:** the bucket, accounts included, is unreadable; there is no
recovery.

## In a browser

Open `$B/admin/login`, paste the key and send: the server sets the session cookie and opens `/admin`, the list with
filters (product, title, received day, fingerprint, page) and, per report, buttons to hide or restore, resolve and
delete (delete asks for its checkbox first). Filtering by a fingerprint adds the same buttons for the whole group.
A viewer gets the same page without the buttons. The page says whose session it is ("Sesión de ana (lectura)" or
"(administración)"). "Cerrar sesión" ends the session. A session ends after `admin.session_idle_secs` (30 min) without
a request, after `admin.session_max_secs` (8 h) in any case, when the same account logs in past
`admin.max_sessions_per_user` (4) or, past `admin.max_sessions` (16), the account holding the most sessions does, when
its account is removed or its key reset, and on every restart of the server; then any `/admin` page answers the
plain 404: go back to `/admin/login`. Those 404s do not count against the failure budget (a wrong key or a wrong
Bearer does).

The cookie is `Secure`: a browser returns it only over HTTPS, so through the public URL it works behind the tunnel
and on the host only as `http://127.0.0.1:8787` or `http://localhost:8787` (a browser treats loopback as secure).
Buttons that change something need `public_url` to be the address in the browser's bar (else 403 `cross_origin`).

The same session works with curl, which helps when checking it:

```sh
J=$(mktemp)                                                       # the cookie jar
curl -s -c "$J" -H "Origin: $B" --data-urlencode "key=$KEY" $B/admin/login
T=$(curl -s -b "$J" -D - -o /dev/null $B/admin | sed -n 's/^x-csrf-token: //Ip' | tr -d '\r')
curl -s -b "$J" -H "Origin: $B" -H "X-CSRF-Token: $T" -X POST $B/admin/reports/$ID/hide
curl -s -b "$J" -H "Origin: $B" -d "csrf=$T" $B/admin/logout; rm -f "$J"
```

## The audit trail

Who changed what, and when: every hide, restore, resolve and delete (of a report or a whole group), every session
login and logout, and every `user` command that changed an account leaves one record in the accounts store, with
the account's name and role (`cli` for the command line), the action, its target (report id, fingerprint or account
name) and the number of reports, at the minute in UTC. No address, key, hash or attachment content is ever in a
record. Admins read it at `$B/admin/audit` ("Registro de cambios", linked from `/admin`), newest first, 100 per page
("Anteriores" goes back); a viewer gets 403. For a script:

```sh
curl -s -H "Authorization: Bearer $KEY" $B/admin/audit/records             # {"records":[...],"next":"<id>"|null}
curl -s -H "Authorization: Bearer $KEY" "$B/admin/audit/records?after=$NEXT"  # the next 100
```

Logins and logouts are thinned so nobody can flood the trail by repeating them: one `login` record per account
every 5 minutes, a `logout` only when a session really ended, and at most 6 of the two per account per minute.
Changes are always recorded. The daily sweep deletes records older than `audit_retention_days` (default 180; 0 keeps
them for good) and logs `audit retention sweep` with `deleted=N` and `session_records_skipped=N`, the logins and
logouts left unrecorded since the previous sweep; a large number means someone logs in and out in a loop. `--check-config` prints how many records there are and the day of the
oldest on the accounts line. A record that could not be written is the warning `audit record not written; the
change itself stands`: the change happened, only its record is missing.

## Reading the logs

The logs players attach are private: read them under `/admin/logs` (the "Logs adjuntos" link of `/admin`), with
the session or the key. Logs of hidden reports are not listed there; download them by id
(`/admin/reports/<id>/attachment`).

- **Find the log of a game.** `/admin/logs?title=CUSA03173` lists that game's logs, newest first, with size, day
  and failure kind; narrow with `product`, `day`, `kind` (`crash`, `stop`, `error`, `exit`, `missing`) or a
  `fingerprint` (all reports of one failure, any game). "ver" opens a log: numbered lines, faults, errors, warnings,
  `Calls: summary` and failed or stub calls highlighted with a marker word and listed at the top with links,
  `logs.page_lines` (2000) lines per page. Control characters show as markers (`␛`, `<U+202E>`); a line longer than
  2000 characters is cut with a link to the download, which is the exact file.
- **Search.** From the list or a log page, or `/admin/logs/search?q=Guest+fault&title=CUSA03173`: the matching
  lines with two lines around, each linked to its line in the log. A search needs a title or a fingerprint; it
  reads at most `logs.search_max_logs` (50) logs, `logs.search_max_mib` (64) MiB and shows at most
  `logs.search_max_matches` (500) lines, and says "La búsqueda paró en el tope…: buscados N de M logs" when a cap
  cut it: add a day, a kind or a product to see the rest. One search at a time per account (429 otherwise).
- **Compare two runs.** In the list, pick A and B on two rows and "Comparar A con B", or on a log page enter the
  other id, or `/admin/logs/diff?a=$A&b=$B&normalize=1`. Changed (`~`), removed from A (`−`) and added in B (`+`)
  lines show with the line numbers of both logs and the counts on top; equal stretches fold to three lines of
  context. Keep "ignorar horas, direcciones y números largos" checked to compare two runs of the same game (times,
  addresses, long numbers and the `# ended`/`# duration`/`# exit` lines then compare equal). Two very long or very
  different logs (more than `logs.diff_max_lines` lines together, or more than 1000 differing lines) are compared
  line by line at the same position and the page says so.

```sh
curl -s -H "$A" "$B/admin/logs?title=CUSA03173"                       # HTML, like the browser
curl -s -H "$A" "$B/admin/logs/search?q=Guest%20fault&title=CUSA03173"
curl -s -H "$A" "$B/admin/logs/diff?a=$ID1&b=$ID2&normalize=1"
curl -s -H "$A" -o log.bin "$B/admin/reports/$ID1/attachment"         # the raw file
```

## List

```sh
curl -s -H "$A" "$B/admin/reports?limit=100"                              # newest first, hidden ones included
curl -s -H "$A" "$B/admin/reports?fingerprint=$FP&day=2026-10-26"         # one group, one received day
curl -s -H "$A" "$B/admin/reports?product=psport&title=CUSA00000&offset=100"
```

Filters are validated (`fingerprint` 16 hex, `day` `YYYY-MM-DD`, `product` `decanter|psport`, `title` `CUSA00000`);
anything else is `400 invalid_filter`. `limit` is at most 500. Without `product` the list merges both partitions,
newest received day first (within one day, decanter's before psport's); with it, only that partition is read. Ids
start with their product's letter (`d…` decanter, `p…` psport), so the id routes below never need the product.

## Hide, resolve, delete one report

```sh
curl -s -H "$A" -X POST   $B/admin/reports/$ID/hide        # 404 on the public pages; undo with /restore
curl -s -H "$A" -X POST   $B/admin/reports/$ID/restore
curl -s -H "$A" -X POST   $B/admin/reports/$ID/resolve     # marks it, deletes its attachment, starts its retention
curl -s -H "$A" -X DELETE $B/admin/reports/$ID             # removes it, its attachment and its clean log
curl -s -H "$A" -o log.bin $B/admin/reports/$ID/attachment # the private log, as a download
```

A report with an attachment also has a public clean log at `/r/<id>/log` (`docs/design.md`, "The public clean
log"). Resolving keeps it, since it holds only allow-listed lines; hiding hides it with the report; deleting removes
it. There is no route to remove the clean log alone: if one ever shows something it should not, hide the report
(the page answers 404 at once) and delete it, then fix the vocabulary in `crates/report`.

## A flooded group, in bulk

The group routes act on every report with that fingerprint, in both partitions, and answer 204, or 404 when neither
holds one. A 500 halfway (the second partition failing) leaves the first one changed: repeat the call.

```sh
curl -s -H "$A" "$B/admin/reports?fingerprint=$FP&day=$DAY"   # look first
curl -s -H "$A" -X POST   $B/admin/groups/$FP/hide            # reversible
curl -s -H "$A" -X POST   $B/admin/groups/$FP/restore
curl -s -H "$A" -X POST   $B/admin/groups/$FP/resolve
curl -s -H "$A" -X DELETE $B/admin/groups/$FP                 # reports and attachments, not reversible
```

Hide first, delete when sure. Each call clears the cached index at once.

## Bucket full and 503

- `503 storage_full` on an upload means that product's partition (the caps count per partition) or its attachments
  are at their cap and nothing could be evicted: at `max_reports` the oldest *unresolved* reports of the partition
  are evicted in 1% batches, so only a partition made of resolved reports (never evicted) answers 503; the other
  product keeps accepting uploads. Delete or lower `resolved_retention_days`, or resolve nothing more
  until the sweep runs; then raise `limits.max_reports` if the volume is genuine.
- A full *bucket* (MinIO's quota, or its disk) shows as `500 internal` on uploads and `request failed` in the log
  (writes to the bucket failing). Look at the usage (`mc du home/decanter-reports`), delete the flooded group,
  or raise the quota (`mc quota set home/decanter-reports --size ...`). Size it for `max_reports` x 64 KiB plus
  `max_attachments` x 5 MiB, twice (the caps are per partition), plus small index objects and up to `max_reports`
  clean logs of a few kilobytes each (at most 1 MiB; a 2000-line log of failures is a few hundred kilobytes).
- An unreachable bucket shows the same way. `GET /healthz` answers 200 regardless (it does not touch the bucket);
  `decanter-reports --check-config` with the same credentials makes one read-only round trip per partition and says
  what fails, one line each.
- Never delete objects in the bucket by hand: a report, its group counters and the totals change together in one
  transaction, and the sweep removes what nothing refers to (blobs whose upload died, index entries that no longer
  match, the leftovers of a removal that was cut off). Deleting a report object by hand leaves its group counting it.
- `stale transactions recovered` in the log means a transaction was cut off (a crash, a lost connection to the
  bucket) and was rolled back or finished by the recovery that runs every minute. Until it runs, uploads that touch
  the same group or the totals fail with 500, so repeated lines point at the bucket's health.

## Backup

The bucket is the whole state. Two options, both with the objects still encrypted (a backup is useless without the
passphrase, and the passphrase is useless without a backup: keep both, apart):

- **Bucket replication** (MinIO's `mc mirror --watch home/decanter-reports backup/decanter-reports`
  or server-side replication to a second MinIO): an exact copy, including both partitions' `.conf/encryption.json`. A mirror taken while
  the server writes may catch a transaction half applied; s3db's transaction records travel with it, and the restored
  server's recovery finishes or rolls them back at its first start.
- **s3db's logical backup** (`s3db::backup::backup_collection` / `restore_collection`): per collection, fuzzy-snapshot
  consistency. This server has no command wrapping it yet; replication is the supported path.

Restore by pointing a stopped server (or a new bucket) at the copy with the same passphrase, then start it: the
startup recovery and the first sweep repair what the copy caught mid-change. Backups hold the delete-code hashes and
the private attachments: keep them as private as the bucket.

### Looking inside

Every key of the bucket is under a partition prefix (`decanter/`, `psport/`, behind the optional root), and s3db
seals each object with its key *inside* the partition as associated data. `s3db-inspect` knows no prefix: it can
list and count a partition's keys, never decrypt them. So the server carries its own read-only inspector, which
connects exactly as the server does (same config, same prefix layer) and opens the partition's cipher without ever
creating one:

```sh
export DECANTER_REPORTS_CONFIG=/etc/decanter-reports/config.toml
export DECANTER_REPORTS_PASSPHRASE_FILE=/etc/decanter-reports/storage.passphrase
export DECANTER_REPORTS_ACCESS_KEY_ID_FILE=/etc/decanter-reports/s3.access_key_id
export DECANTER_REPORTS_SECRET_ACCESS_KEY_FILE=/etc/decanter-reports/s3.secret_access_key
decanter-reports inspect psport ls groups/ 20          # keys and sizes, at most 1000; "(more after <key>)" if cut
decanter-reports inspect decanter get meta/totals.json # one object, decrypted, as JSON on stdout
decanter-reports inspect psport get reports/p<32 hex>.json
```

It needs the passphrase and the bucket keys but no admin key, writes nothing, refuses a partition that is not
initialized (and, with the `local` driver, one whose directory the server has not created; stop the server first,
since its database file is locked while it runs). What it prints is decrypted bucket content (delete-code hashes,
attachment names): treat the output as private as the bucket. Attachments are blobs, read them through
`GET /admin/reports/<id>/attachment` instead.

The accounts store holds `users/<id>.json`, `idx/name/<name>.json` (kept after a removal, as the name's tombstone),
`meta/roster.json` and `audit/<id>.json` (the audit trail), beside its own `.conf/`.

Layout of each partition: `reports/<id>.json`, `groups/<fingerprint>.json`, `meta/totals.json`,
`meta/sequence.json`, `meta/sweep.json`, `attachments/` (blobs), `cleanlogs/` (blobs: the public clean logs),
`idx/` (the ordered lists) and `.conf/` (encryption config and transaction records).

## What to watch

The log has no addresses, codes, keys or report contents, only counts; storage lines name their partition
(`product="decanter"` or `"psport"`), and every line of an admin request names the account and its role
(`admin{user=ana role=viewer}: ...`, and `user=` / `role=` on the session lines). With
`journalctl -u decanter-reports`:

| Look for | Means |
|---|---|
| `report store full: oldest unresolved reports evicted` (with `evicted=N`) | a flood or a cap that is too small; repeated lines need action |
| `attachment store full: oldest attachments evicted` | same, for attachments |
| `storage_full` / `503` at the tunnel | only resolved reports left; see above |
| `connection cap reached: connection closed` | more than `max_connections` open: a slow-connection flood or a tunnel that does not reuse connections |
| `shutdown grace elapsed` | connections were cut at restart |
| `loose permissions narrowed` | something widened the local driver's directory; find out what |
| `stale transactions recovered` | a transaction was cut off; repeated lines mean the bucket is failing |
| `no admin account` (hourly) | nobody can change reports; create one with `user add <name> --role admin` |
| `account refresh failed` | the accounts could not be re-read; the server keeps the previous list until it can |
| `account object unreadable` (with `account=<id>`, `kept=`) | one account's object does not decrypt; `kept=true` while its name entry vouches for it, `false` drops it (a removal or reset since). Repeats every 15 s until the account is reset or removed; `--check-config` counts them |
| `client_ip_header ignored` (with `peer=`) | a request carried the client header from an address not in `trusted_proxies`: if `peer` is the proxy, its address changed, every client is counted as it, and `trusted_proxies` needs the new one |
| `audit record not written; the change itself stands` | a change went unrecorded in the audit trail; repeated lines mean the bucket is failing |
| `audit retention sweep failed` | old audit records were not deleted this time; the next daily sweep tries again |
| `admin session ended: account removed, disabled or given a new key` | a `user remove` or `user reset` took effect |
| `retention sweep` with `orphans` or `repaired` above 0 | leftovers of failed writes cleaned up; a steady stream means writes keep failing halfway |
| `report rejected` counts | uploads failing validation: a client bug or probing |
| `stored clean log refused by the validator` | a stored clean log no longer passes the crate's rules (a vocabulary change, or tampering in the bucket); its page answers 500 and shows nothing |
| 429 at the tunnel (no log line: the process does not log them) | per-address, global or failure limits; a sustained rate means abuse or a wrong `client_ip_header` (everyone counted as the tunnel) |

Also: the bucket's usage against its quota, the number of unresolved groups on `/`, and that `GET /healthz`
answers 200.
Rejected-upload and 429 rates are best counted at the tunnel, which sees every request.
