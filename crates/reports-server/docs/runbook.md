# Maintainer runbook

Commands for the person running the server. `B` is the base URL, `A` the admin header. Without the key every
`/admin` route answers 404; an address that fails the key (or a delete code) `failures_per_ip` times in
`failure_window_secs` gets 429 on all `/admin/` paths until the window passes.

```sh
B=https://reports.example.org            # or http://127.0.0.1:8787 on the host
A="Authorization: Bearer $(cat /etc/decanter-reports/admin.key)"
```

## In a browser

Open `$B/admin/login`, paste the key and send: the server sets the session cookie and opens `/admin`, the list with
filters (product, title, received day, fingerprint, page) and, per report, buttons to hide or restore, resolve and
delete (delete asks for its checkbox first). Filtering by a fingerprint adds the same buttons for the whole group.
"Cerrar sesión" ends the session. A session ends after `admin.session_idle_secs` (30 min) without a request, after
`admin.session_max_secs` (8 h) in any case, when a login past `admin.max_sessions` (16) pushes it out, and on every
restart of the server; then any `/admin` page answers the plain 404: go back to `/admin/login`. Those 404s count
against the failure budget like a wrong key, so do not reload an expired page in a loop.

The cookie is `Secure`: a browser returns it only over HTTPS, so through the public URL it works behind the tunnel
and on the host only as `http://127.0.0.1:8787` or `http://localhost:8787` (a browser treats loopback as secure).
Buttons that change something need `public_url` to be the address in the browser's bar (else 403 `cross_origin`).

The same session works with curl, which helps when checking it:

```sh
J=$(mktemp)                                                       # the cookie jar
curl -s -c "$J" -H "Origin: $B" --data-urlencode "key=$(cat /etc/decanter-reports/admin.key)" $B/admin/login
T=$(curl -s -b "$J" -D - -o /dev/null $B/admin | sed -n 's/^x-csrf-token: //Ip' | tr -d '\r')
curl -s -b "$J" -H "Origin: $B" -H "X-CSRF-Token: $T" -X POST $B/admin/reports/$ID/hide
curl -s -b "$J" -H "Origin: $B" -d "csrf=$T" $B/admin/logout; rm -f "$J"
```

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
  cut it: add a day, a kind or a product to see the rest. One search at a time per session (429 otherwise).
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

It needs the passphrase and the bucket keys but not the admin key, writes nothing, refuses a partition that is not
initialized (and, with the `local` driver, one whose directory the server has not created; stop the server first,
since its database file is locked while it runs). What it prints is decrypted bucket content (delete-code hashes,
attachment names): treat the output as private as the bucket. Attachments are blobs, read them through
`GET /admin/reports/<id>/attachment` instead.

Layout of each partition: `reports/<id>.json`, `groups/<fingerprint>.json`, `meta/totals.json`,
`meta/sequence.json`, `meta/sweep.json`, `attachments/` (blobs), `cleanlogs/` (blobs: the public clean logs),
`idx/` (the ordered lists) and `.conf/` (encryption config and transaction records).

## What to watch

The log has no addresses, codes or report contents, only counts; storage lines name their partition
(`product="decanter"` or `"psport"`). With `journalctl -u decanter-reports`:

| Look for | Means |
|---|---|
| `report store full: oldest unresolved reports evicted` (with `evicted=N`) | a flood or a cap that is too small; repeated lines need action |
| `attachment store full: oldest attachments evicted` | same, for attachments |
| `storage_full` / `503` at the tunnel | only resolved reports left; see above |
| `connection cap reached: connection closed` | more than `max_connections` open: a slow-connection flood or a tunnel that does not reuse connections |
| `shutdown grace elapsed` | connections were cut at restart |
| `loose permissions narrowed` | something widened the local driver's directory; find out what |
| `stale transactions recovered` | a transaction was cut off; repeated lines mean the bucket is failing |
| `retention sweep` with `orphans` or `repaired` above 0 | leftovers of failed writes cleaned up; a steady stream means writes keep failing halfway |
| `report rejected` counts | uploads failing validation: a client bug or probing |
| `stored clean log refused by the validator` | a stored clean log no longer passes the crate's rules (a vocabulary change, or tampering in the bucket); its page answers 500 and shows nothing |
| 429 at the tunnel (no log line: the process does not log them) | per-address, global or failure limits; a sustained rate means abuse or a wrong `client_ip_header` (everyone counted as the tunnel) |

Also: the bucket's usage against its quota, the number of unresolved groups on `/`, and that `GET /healthz`
answers 200.
Rejected-upload and 429 rates are best counted at the tunnel, which sees every request.
