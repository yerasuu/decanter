# Maintainer runbook

Commands for the person running the server. `B` is the base URL, `A` the admin header. Without the key every
`/admin` route answers 404; an address that fails the key (or a delete code) `failures_per_ip` times in
`failure_window_secs` gets 429 on all `/admin/` paths until the window passes.

```sh
B=https://reports.example.org            # or http://127.0.0.1:8787 on the host
A="Authorization: Bearer $(cat /etc/decanter-reports/admin.key)"
```

## List

```sh
curl -s -H "$A" "$B/admin/reports?limit=100"                              # newest first, hidden ones included
curl -s -H "$A" "$B/admin/reports?fingerprint=$FP&day=2026-10-26"         # one group, one received day
curl -s -H "$A" "$B/admin/reports?product=psport&title=CUSA00000&offset=100"
```

Filters are validated (`fingerprint` 16 hex, `day` `YYYY-MM-DD`, `product` `decanter|psport`, `title` `CUSA00000`);
anything else is `400 invalid_filter`. `limit` is at most 500.

## Hide, resolve, delete one report

```sh
curl -s -H "$A" -X POST   $B/admin/reports/$ID/hide        # 404 on the public pages; undo with /restore
curl -s -H "$A" -X POST   $B/admin/reports/$ID/restore
curl -s -H "$A" -X POST   $B/admin/reports/$ID/resolve     # marks it, deletes its attachment, starts its retention
curl -s -H "$A" -X DELETE $B/admin/reports/$ID             # removes it and its attachment
curl -s -H "$A" -o log.bin $B/admin/reports/$ID/attachment # the private log, as a download
```

## A flooded group, in bulk

The group routes act on every report with that fingerprint and answer 204, or 404 when the group has none.

```sh
curl -s -H "$A" "$B/admin/reports?fingerprint=$FP&day=$DAY"   # look first
curl -s -H "$A" -X POST   $B/admin/groups/$FP/hide            # reversible
curl -s -H "$A" -X POST   $B/admin/groups/$FP/restore
curl -s -H "$A" -X POST   $B/admin/groups/$FP/resolve
curl -s -H "$A" -X DELETE $B/admin/groups/$FP                 # reports and attachments, not reversible
```

Hide first, delete when sure. Each call clears the cached index at once.

## Bucket full and 503

- `503 storage_full` on an upload means the store or the attachments are at their cap and nothing could be
  evicted: at `max_reports` the oldest *unresolved* reports are evicted in 1% batches, so only a store made of
  resolved reports (never evicted) answers 503. Delete or lower `resolved_retention_days`, or resolve nothing more
  until the sweep runs; then raise `limits.max_reports` if the volume is genuine.
- A full *bucket* (MinIO's quota, or its disk) shows as `500 internal` on uploads and `request failed` in the log
  (writes to the bucket failing). Look at the usage (`mc du home/decanter-reports`), delete the flooded group,
  or raise the quota (`mc quota set home/decanter-reports --size ...`). Size it for `max_reports` x 64 KiB plus
  `max_attachments` x 5 MiB, plus small index objects.
- An unreachable bucket shows the same way. `GET /healthz` answers 200 regardless (it does not touch the bucket);
  `decanter-reports --check-config` with the same credentials makes one read-only round trip and says what fails.
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
  or server-side replication to a second MinIO): an exact copy, including `.conf/encryption.json`. A mirror taken while
  the server writes may catch a transaction half applied; s3db's transaction records travel with it, and the restored
  server's recovery finishes or rolls them back at its first start.
- **s3db's logical backup** (`s3db::backup::backup_collection` / `restore_collection`): per collection, fuzzy-snapshot
  consistency. This server has no command wrapping it yet; replication is the supported path.

Restore by pointing a stopped server (or a new bucket) at the copy with the same passphrase, then start it: the
startup recovery and the first sweep repair what the copy caught mid-change. Backups hold the delete-code hashes and
the private attachments: keep them as private as the bucket.

### Looking inside

`s3db-inspect` (from the s3db repository at the pinned tag: `cargo build --release --bin s3db-inspect`) lists,
counts and decrypts objects by hand. It takes the passphrase from `S3DB_PASSPHRASE` (never an argument) and the
bucket from `--config <file.json>`, a serialized s3db `BackendConfig` that holds the secret access key: create it
with `umask 077` in a private temporary directory and delete it afterwards. It only opens buckets the server already
initialized and changes nothing without `--yes`/`--apply`; never use those on the live bucket.

```sh
S3DB_PASSPHRASE="$(cat /etc/decanter-reports/storage.passphrase)" s3db-inspect --config "$TMP/bucket.json" ls groups/ 20
S3DB_PASSPHRASE="$(cat /etc/decanter-reports/storage.passphrase)" s3db-inspect --config "$TMP/bucket.json" get meta/totals.json
```

It knows nothing of this server's `storage.prefix`, and each object is sealed with its key (without the prefix) as
associated data: on a bucket with a prefix it can list and count (`ls reports/idx/`), not decrypt. That is why the
example config uses a bucket of its own with no prefix. Its `--local` reads the deprecated `files` engine, not the
local driver's database file. Layout: `reports/<id>.json`, `groups/<fingerprint>.json`, `meta/totals.json`,
`attachments/` (blobs), `idx/` (the ordered lists) and `.conf/` (encryption config and transaction records).

## What to watch

The log has no addresses, codes or report contents, only counts. With `journalctl -u decanter-reports`:

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
| 429 at the tunnel (no log line: the process does not log them) | per-address, global or failure limits; a sustained rate means abuse or a wrong `client_ip_header` (everyone counted as the tunnel) |

Also: the bucket's usage against its quota, the number of unresolved groups on `/`, and that `GET /healthz`
answers 200.
Rejected-upload and 429 rates are best counted at the tunnel, which sees every request.
