# Design of decanter-reports

What the server is for, how its parts fit, what it trusts and what it never does. The endpoints and the
configuration keys are in `README.md`, the deployment in `docs/deploy.md`, the day-to-day work in
`docs/runbook.md`. The decisions behind this design are on decanter's board: #985, #986, #987, #988, #989, #991,
#1033, #1034, #1037, #1038, #1041 to #1046, #1047, #1049, #1055, #1060 (tasks #982, #993, #994, #1039, #1040, #1048,
#1056, #1064).

## Purpose

A player of decanter (a game launcher) or of psport (the PS4 compatibility layer it runs) hits a failure. The
launcher turns it into a small, closed, sanitized report and, if the player asks, sends it here. The server makes
the report visible to everyone so that failures can be debugged in public, without a human in the loop and without
the report ever containing private details. Address: `https://reports.yerasuu.com`.

Non-goals: no accounts, no login for players, no telemetry, no crash upload in the background, no storage of IP
addresses, no raw logs in public, no moderation queue.

## The parts

```
 decanter (client)                         reports.yerasuu.com
 ----------------                          -----------------------------------------------
 failures.jsonl, run log,    preview and   Cloudflare ──> cloudflared ──> Traefik ──> decanter-reports
 day log, machine identity   confirmation  (TLS, DNS)     (tunnel)        (routes,     (this server,
        |                    by the player                                 timeouts)    127.0.0.1 or
        v                         |                                                    container network)
 decanter-report crate  ----------+--> POST /v1/reports (JSON, <= 64 KiB)
 (schema, vocabulary,                         |
  identity denylist)                          v
                                      decanter-report::validate  (same crate, same rules)
                                              |  fails: 422 {path, rule}, nothing stored
                                              v
                                      s3db on a bucket     ──>  public pages  GET / , /r/<id> , /g/<fp>
                                      (reports, groups,         admin API (key)   /admin/...
                                       private blobs)
```

- **The client** (decanter, `decanter ps report`, the «Reportar errores» button) builds the report from local files
  and applies the denylist of the machine's identity (user, host, home folders, accounts, profile names). The
  player sees the exact JSON before anything leaves the machine. The server never trusts that this happened.
- **The crate `decanter-report`** is shared: it holds the closed schema (v2), the closed message vocabulary (a
  message is a known template with typed slots, never free text), the shape rules and the validator. The server
  calls the very same `validate` on the received bytes.
- **The server** (this repository) validates, stores, publishes and cleans up. It is one Rust binary on tokio and
  axum with no database: all state lives in an S3-compatible bucket through `s3db` (see "Data model"), and its
  pages use no JavaScript.
- **The tunnel** (Cloudflare Tunnel into Traefik at the owner's home server) provides TLS, DNS and the public
  address; the server itself speaks plain HTTP on a private address and trusts only the proxy it is told to.

## What a report is

A JSON document of at most 64 KiB, `schema` 2, `product` `decanter` or `psport`. It holds: the date rounded to
the day, the versions and commit of decanter (and psport), the OS family and version, the GPU vendor, model and
driver, the failure kind, the message as a template id with typed slots, the failing calls (name, code and
count), the guest code frames of a fault as offsets inside images, performance figures, closed-set game settings
and, for psport, the title id, game name and version. It never holds paths, user or host names, profile or
account ids, display or pad names, window titles, addresses, exact times or any free text.

The server stores the validated JSON as received. It derives the fields it queries (`product`, `title_id`,
`failure_kind`, `template_id`, `created_day`, `fingerprint`) from the validated report, never from the request.
The **fingerprint** (16 hex characters of SHA-256 over kind, template, named slots and commit, computed by the
crate) groups duplicates; it ignores the game, so a group may span titles and the page shows the most frequent one.

An optional **attachment** is the player's raw run log, up to 5 MiB, one per report. It is private by design: the
client scrubs it with the same denylist as a best effort, the server stores it as an encrypted blob under a
generated name, only the admin can download it, and it is deleted when the report is resolved or deleted. What the
public sees of it is its **clean log** (next section), never the log itself.

## The public clean log

**Decision (#1060, task #1064).** When an attachment is accepted, and only then, the server builds a public, clean
version of it with `decanter_report::clean_log` and the vocabulary of the report's product (the id's letter picks
it): the attachment is read as text with lossy UTF-8 (one NUL byte marks it as not a text log, which gives an empty
clean log), and each line is matched against the same closed templates a report's `failure.message` must be. A
psport line is matched as it is; a decanter line only when it is a `WARN` or `ERROR` of decanter's own modules,
rebuilt as the day-log source reads it (message, `job=`, `exit=`, `: <error>`). A line is kept as its number, its
template id and its typed slots; every other line is dropped and only counted.

**Guarantee.** The clean log holds no text of the log by construction: what a page shows is rebuilt from the
template's literal pattern and the slot values, with `…` where the line had text a `{_}` reads and discards. The
slots of a kept line pass the vocabulary's own shape rules and a stricter public rule (`is_public_slot` in
`crates/report/src/clean.rs`), because the server has no identity list (#1033, #1034) and a shape cannot tell a
one-word user, host or device name from a technical word: a call name must not be a lone plain word (`deck`,
`Yerasuu`, `DELL`) unless it is a listed technical one (`Open`, `main`), nor a snake-case name of plain words with
an unlisted head (`john_smith`), nor carry id-like digits (a run of four, or more than two groups) or start a
`::` part with a digit (an IPv6 address); an image must be a listed system image, a `lib…` one or a lowercase name
with a version in it (`vulkan-1.dll`), never a plain executable name taken from a path; a constant needs an `_`
(`VK_ERROR_DEVICE_LOST`, not `DELL` or `S2522HG`); a valid-usage id must read `VUID-<vk…>-<words>-<5 digits>` with
listed or camel-case words. The stored form (`CleanLog`, closed JSON, unknown fields refused) is re-validated by
`decanter_report::validate_clean_log` every time a page reads it: shape, product, counts, increasing line numbers,
every template and slot against the vocabulary and the public rule. One that fails is not shown: the page answers
500 and the log line `stored clean log refused by the validator` names nothing of it.

**Limits.** At most 2000 kept lines; once reached, the rest of the log is not examined and the page says so. Only
the first 5 MiB are read (the attachment cap), a line longer than 4096 bytes is counted without being matched, and
the work is linear in the input (about half a second for 5 MiB of short lines; the cap stops a log of matching
lines at once); it runs on a blocking thread at upload. The stored form is at most 1 MiB. The residual, measured
with a seeded battery of user, host, display and pad names, paths, ids, addresses, tokens, times, MACs and Steam ids
in every slot and position: a private string *inside* a longer identifier (`sceYerasuuOpen`, `pthread_yerasuu`,
`yerasuu2`, a camel-case word of a valid-usage id) passes, as no shape rule can tell it apart; number slots (up to
nine digits) remain a small numeric channel. The client's denylist (#1041) scrubs the attachment before it is sent,
which is what covers that residual for an honest sender. A sender can also fill the clean log with lines of their
choosing in the allowed shapes, as with any report field.

**Storage and lifecycle.** The clean log is a blob of its own collection, `cleanlogs/<id>-<random>`, in the report's
partition, named by a field of the report (`clean_log`). Both blobs (attachment and clean log) are written before
the one transaction that names them, as attachments always were; a failed transaction removes both and the sweep
removes any blob of either collection no report names. A blob and not a field of `Report`, so the report object stays
small for the lists that load reports, and the 256 KiB object limit is never near. It follows the report: hidden
with it (404), kept when the report is resolved or its attachment is evicted at `max_attachments` (it holds only
allow-listed data, the decision #988 publishes), replaced if a new attachment is sent after an eviction, and removed
when the report is deleted, by its owner, the maintainer, the caps or the retention sweep. It counts in no cap of its
own: there is at most one per report.

**Pages.** `GET /r/<id>/log` shows the counts, a note explaining what is shown, and a table of line number, readable
text and template id; `GET /r/<id>` shows the counts and a link. Both are rate limited, escaped and served with the
same headers as every page; the raw attachment stays reachable only through the admin route.

## Data model

**Decision (#1047, #1049): no database.** Like gitquarry, the server's only storage is an object bucket, reached
through `s3db` (`git.yerasuu.com/yerasuu/s3db`, pinned by tag `v0.6.1`): a typed, encrypted, compare-and-swap
database over S3, R2 or a local directory. Everything is in the bucket, attachments included; the bucket is the
owner's own MinIO. The code is in `src/store/`.

**Partitions (#1055).** The bucket holds two independent s3db stores, one per product: `decanter/` and `psport/`
(under the optional common root `storage.prefix`, through the server's prefix layer for `s3` and `r2`), or the
directories `<storage.dir>/decanter` and `<storage.dir>/psport` for the `local` driver. Each has its own encryption
config (`<partition>/.conf/encryption.json`, derived from the one passphrase), its own transaction records, and
everything below: a partition is never written by a transaction of the other, and the upload picks it from the
validated report's `product`. There is no partition per game or per report; psport titles stay a field and an index
inside `psport/`.

- **Ids.** A report id is the product's letter, `d` or `p`, followed by 32 lowercase hex characters (128 random
  bits). Every route that takes an id reads its partition from the letter, without a lookup, so an id cannot be
  ambiguous and asking for it cannot tell anything about the other partition: a malformed id, an id whose letter
  points at the partition that does not hold it, an unknown id and a wrong or missing delete code all answer the
  same 404, after the same dummy hash comparison. A fingerprint names no product (it hashes the failure, not the
  product, although in practice the template ids differ), so group pages and group actions ask both partitions.

The entities below exist once per partition.

- **`Report`** (`reports/<id>`, `#[derive(Entity)]`): `delete_hash`, `fingerprint`, `product`, `title_id`,
  `game_name`, `failure_kind`, `template_id`, `message`, `created_day`, `received_day`, `hidden`, `resolved_at`,
  `attachment` (blob name or none), `clean_log` (blob name of the public clean log, or none), the validated `json`,
  plus `seq` (insertion order), `deleted` and `tracked` (below).
- **`Group`** (`groups/<fingerprint>`): over the group's visible reports (not hidden, not deleted) the report count,
  the resolved count, the hidden count, the first and last `created_day`, the latest `received_day`, the product,
  kind and latest message, and per title its report and resolved counts and its game names with their counts.
- **Singletons** under `meta/`: `totals` (reports and attachments stored in the partition, what `max_reports` and
  `max_attachments` are checked against, so the effective caps of the whole bucket are twice each figure), `sequence` (the next insertion number, which orders reports without storing any time finer
  than a day) and `sweep` (the last sweep's day and sequence, so the sweep only cleans up inserts that cannot still
  be running).
- **Attachments**: `Blob`s (`attachments/<id>-<random>`), at most 5 MiB, named by the server, never by input.
- **Clean logs**: `Blob`s (`cleanlogs/<id>-<random>`), at most 1 MiB, the stored `CleanLog` of each attachment.
- **Indexes**: s3db `MultiIndex`es under `idx/`, whose member names sort the way they are read, because object
  stores list keys in ascending order only. Newest first by inverted day and sequence: all reports (`all`, by received
  day), by fingerprint (`fp`), by product, by title. Oldest first: unresolved reports (`queue/open`), attached ones
  (`queue/attached`), resolved ones by resolution day (`queue/resolved`). Groups newest first by their latest
  received day (`groups/order`), and the groups of each title (`gtitle`). Bookkeeping: removals still being cleaned up
  (`queue/gone`) and inserts not yet seen to land (`queue/pending`).

**Consistency.** Every change to reports runs as one s3db `run_update` transaction over the reports, their groups
and the totals, each written against the etag it was read with: two requests racing on one group cannot both land,
the loser re-reads and retries (up to 64 times, with jittered backoff), so the counters never drift. An upload first
reserves its sequence number with a compare-and-swap on `sequence` (kept out of the transactions, because on S3 a
transaction does not lock against plain writes) and writes its index entries once, so each retry is only a re-read
and a commit; with 12 concurrent uploaders on the local driver none failed (#1048).

s3db transactions cannot delete, so a removal sets `deleted` in the transaction (and creates its `queue/gone` entry
in the same one); the object, its index entries and its blob go after it lands. Index entries are written before the
transaction that makes them true and removed after the one that makes them false: an entry can be stale, a live
report is never missing from a list, and every reader checks an entry against the object it names. A blob is written
before the transaction that names it and removed if that transaction does not land, so no report points at a
missing blob. On S3 a transaction holds lock objects; a crash leaves them, and `recover_stale_updates` (at startup
and every minute, for each partition) rolls such a transaction back or forward. The daily sweep, run on each
partition in turn, finishes removals and failed inserts, removes blobs no report names (older than an hour) and fixes
the group indexes.

**Bounded reads.** `GET /` reads `Group` rows in the order index, never reports (finding H1 of #1038 at its root);
the index cache stays. A group page, the admin list and the evictions walk their index and read only what they show
or take. Eviction at `max_reports` takes the head of the full partition's `queue/open`; at `max_attachments`, of its
`queue/attached`: one product's flood never evicts the other's reports.

**Lists across partitions.** With `?product=` the index and the admin list read that partition only. Without it,
each partition is read up to the end of the requested page (offset plus page size, as one partition alone already
reads past the offset) and the two lists are merged:

- *Index:* by the order the group index itself uses, latest received day first, then fingerprint. A fingerprint held
  by both partitions shows as two rows, one per product, each with its own counters; its `/g/` page lists the
  reports of both.
- *Admin list and group page:* newest received day first. Insertion order is a per-partition sequence, so within one
  received day the decanter reports come before the psport ones instead of interleaving by arrival (nothing finer
  than a day is stored to order them by).
- *Group actions* (`/admin/groups/<fp>/...`) run on each partition that holds the fingerprint, one after the other;
  if the second partition fails after the first changed, the answer is 500 and repeating the call finishes it.

**Capped lists.** A group tracks at most 32 titles and 8 game names per title; reports beyond count in an overflow
counter, and each report records (`tracked`) what it was counted under, so removing it undoes exactly that. A group
flooded with made-up titles therefore cannot grow past s3db's 256 KiB object limit; its "(+N más)" then shows at
least one more title than it tracks.

**What the index shows differs slightly from the SQLite version** (drift accepted with the move to `Group` rows):

- *First and last day* are the visible reports' `created_day` range, and only widen while reports are hidden,
  resolved or deleted (a minimum cannot be undone without reading every report); they reset when nothing visible is
  left. SQLite recomputed them from the remaining rows.
- *Counts per filter*: with `title=` the row counts that title's reports; with `resolved=0` or `resolved=1` the
  unresolved or resolved ones; with both, that title's. The day range, the latest message and, under `resolved=`,
  the game name stay those of the whole visible group, where SQLite computed them over the matching rows only.
- *Order* is by the latest `received_day` the group ever had, so deleting a group's newest reports does not move it
  down; SQLite ordered by the current maximum of the matching rows. Ties sort by fingerprint as before.
- A title page sorts at most 10 000 groups of that title.

Days are UTC `YYYY-MM-DD`; nothing finer is stored, and no address or user agent. Ids (behind their product letter)
and delete codes are 128-bit random values from the operating system; the delete code is shown once and only its
SHA-256 hash is kept.
Encryption at rest is s3db's: every object is sealed (XChaCha20-Poly1305) under a data key wrapped by a key derived
from a passphrase (Argon2id) that the server reads as a credential, like the admin key; the bucket provider sees only
object names and sizes. The bucket credentials and the passphrase are never in `config.toml`; losing the passphrase
makes the bucket unreadable.

## Lifecycle of a report

1. **Upload.** Size cap, JSON content type, per-address and global rate limit, `validate`; on success the report
   is stored in its product's partition with its group and the partition's totals in one transaction, and
   `{id, delete_code, url, fingerprint}` returned, the id starting with the product's letter. A rejection returns the
   path and rule that failed, never the value, and stores nothing.
2. **Visible.** Public at `/r/<id>` and in its group at once, and in the merged index and its product's filtered
   one. The index is cached for a few seconds.
3. **Attachment (optional).** `PUT` with the delete code; refused once resolved. Its clean log is built and stored
   in the same transaction and published at `/r/<id>/log`.
4. **Resolved.** A maintainer marks it (or the whole group). It stays public, marked, loses its attachment (keeps
   its clean log) and is deleted `resolved_retention_days` later (default 90).
5. **Hidden or deleted.** The maintainer hides a report (404 on the public pages, kept for review) or deletes it;
   the player deletes their own with the code.
6. **Expiry.** Unresolved reports are deleted after `retention_days` (default 365). At `max_reports` and
   `max_attachments`, counted per partition, the oldest unresolved ones of that partition are evicted in batches
   rather than refusing uploads.

## Trust and threats

Everything a client sends is hostile, including what looks like decanter's own output.

- **Privacy.** The guarantee that a published report holds no private data rests on the crate's allow-list (shape
  rules, closed vocabulary, denylist at the client), not on the server. The server is the last barrier only in
  the sense that it re-validates and escapes. Residual risks are stated in decanter's note #1033: a one-word user
  name inside a function-name slot, a game name that is really a monitor name, numeric fields as a few-bit data
  channel. The automatic publication is switched on only after a fresh adversarial test of `game.name`,
  `os_version`, GPU and driver text passes.
- **Pages.** Server-rendered HTML with one `escape_html` for every dynamic string, no markdown, no script, no
  external resources, strict headers (`Content-Security-Policy: default-src 'none'`, `nosniff`, `no-referrer`,
  `X-Frame-Options: DENY`).
- **Abuse.** Per-address upload and read limits kept in memory only, a global cap spent only by valid reports, a
  failure limiter on wrong admin keys and wrong delete codes, bounded connections and header timeouts in the
  process, caps that evict instead of refusing, bulk actions by fingerprint for clean-up.
- **Authentication.** One admin key (at least 32 characters, from a credential file or the environment, held as
  a SHA-256 in memory, constant-time comparison). Without it, or a session, every `/admin` route but the login form,
  any wrong method and any unknown path answer the same 404 in the same bytes, and count as an authentication
  failure. Players hold only their delete code.
- **Admin sessions** (#1060, #1062). A browser cannot send the Bearer header, so `POST /admin/login` trades the key
  (form field, same constant-time check, same-site `Origin` required) for a session: a 256-bit random id held in
  memory only as its SHA-256, with an idle timeout and an absolute lifetime, a cap that ends the oldest session,
  and expired entries swept on every login and lookup. A restart ends every session. The cookie is
  `__Host-session` (`Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/`, no `Domain`), so it is never sent on a
  cross-site request, never readable by a script, and returned by a browser only over HTTPS or to `localhost`.
  One `session::gate` middleware in front of every route decides for all of `/admin`: a valid Bearer passes as
  before; a valid session passes reads, and every other method only with the session's CSRF token (256 random
  bits, from the `csrf` form field or `X-CSRF-Token`, constant-time compared) and an `Origin` (else `Referer`)
  equal to `public_url`'s origin, loopback only in development; anything else is the uniform 404. A failed CSRF or
  origin check with a valid session answers 403 and changes nothing. Handlers take an `Admin` proof the gate
  inserts, so no admin handler can run without it.
- **CSRF.** `SameSite=Strict` already keeps the cookie off cross-site requests; the token and the origin check are
  the second and third layer (for a browser without SameSite, or a same-site but foreign origin). The admin pages
  answer `Referrer-Policy: same-origin` because under `no-referrer` a browser posts `Origin: null`, which the check
  refuses. Their CSP adds nothing for forms beyond the site's: `form-action 'self'`, `base-uri 'none'`,
  `frame-ancestors 'none'`, no script.
- **No oracle.** An unknown id, a malformed id, an id of the other partition, a wrong code and a missing code answer
  the same 404 in the same bytes.
- **Client address.** Read from `client_ip_header` only when the connection comes from a `trusted_proxies`
  address, else from the socket. It is used for the in-memory limits and never written or logged.
- **Storage.** The bucket holds only encrypted objects (s3db), the bucket credentials and the passphrase are
  credentials like the admin key (files owner-only or from `LoadCredential=`, never logged), and attachment keys are
  generated, never taken from input. The server keeps no other local state (the `local` driver aside).

The reviews that shaped this are in decanter's notes #1033 (the crate) and #1038 (the server, findings fixed in
#1039 and #1040).

## Deployment topology

`reports.yerasuu.com` is a record in Cloudflare that points at a Cloudflare Tunnel; `cloudflared` on the home
server hands the traffic to Traefik, which routes the host to this server's port. Requirements, detailed in
`docs/deploy.md`: only the server's port is reachable, TLS ends at Cloudflare, the client address reaches the
server in a single-valued header the client cannot control (`CF-Connecting-IP`, trusted only if Traefik accepts
traffic from `cloudflared` alone and trusts that header at its entry point), `trusted_proxies` is the address
Traefik connects from (not `127.0.0.1` inside a container network), and header and idle timeouts and connection
limits are set at the tunnel and at Traefik as a second layer. The service runs under systemd with a dynamic user
and a credential for the admin key, or in a rootless container.

## Open points

- Route and middlewares in Traefik, DNS record and tunnel: the owner's (task #994); `docs/deploy.md` has no
  Traefik section yet because it must follow the documentation of the version in use.
- ~~Moving the storage from SQLite to `s3db`~~: done (#1048), with the licence now AGPL-3.0-or-later (#1049) and the
  backend the owner's MinIO. Not yet exercised against a real MinIO; `s3db-inspect` cannot decrypt a prefixed
  bucket, so `decanter-reports inspect` does it per partition.
- The partitions (#1056) have no migration from the single store that preceded them: nothing was deployed with it.
  A `local` directory of that layout (`<dir>/s3db.redb`) is not read; the partitions start empty beside it.
- Real unit start under systemd with `DynamicUser`, `PrivateUsers` and `LoadCredential` (now four credentials), the
  container image build (its secrets' modes) and the forged-header check through the real tunnel are untested.
- Reports with several failures of one run (today one report per run; a new schema version would be needed).
- ~~Readable template text on the pages~~: done (#1064); a message the crate cannot render still shows as its
  template id and slots.
- A second adversarial pass over the validator before the automatic publication is enabled.
