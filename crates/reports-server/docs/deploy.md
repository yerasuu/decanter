# Deploying decanter-reports

Nothing here is deployed by the repository; these are the pieces the owner assembles. The files are in `deploy/`.

## Requirements on the host and the tunnel

The server speaks plain HTTP and trusts what is in front of it for three things. Whatever tunnel is chosen must:

1. **Expose only the server's port.** Nothing else on the host or the local network is reachable through it.
2. **Terminate TLS.** The server has no TLS; public traffic must be HTTPS, and the process listens on `127.0.0.1`
   (or on a container-private address).
3. **Pass the client address in one single-valued header that the client cannot choose**, and put the address the
   tunnel connects from in `trusted_proxies` (single addresses, no ranges) and the header name in
   `client_ip_header`. Without both, every request is counted against the tunnel's own address and the per-IP
   limits become one global limit.

   Prefer a header the tunnel *overwrites* with the address it saw, such as `CF-Connecting-IP` (Cloudflare
   Tunnel). `X-Forwarded-For` is a list that proxies append to: it is safe only when the tunnel replaces what the
   client sent, and the server then reads the rightmost entry that is not a trusted proxy. With a tunnel that
   forwards the client's own copy unchanged, the attacker chooses the address the limits count and can spend
   someone else's allowance. `--check-config` warns when `client_ip_header` is set and `listen` is not loopback,
   when `trusted_proxies` holds `0.0.0.0` or `::` (a range cannot be written, and these match no connection), and
   when the header is `x-forwarded-for`, `forwarded` or `x-real-ip`.

   **Verify it through the real tunnel** (once, and after any change to the tunnel). From a machine that is not
   the tunnel host, with `limits.per_ip_hour` left at its default of 10:

   ```sh
   for i in $(seq 1 12); do
     curl -s -o /dev/null -w '%{http_code} ' -X POST -H 'Content-Type: application/json' \
       -H 'CF-Connecting-IP: 203.0.113.50' --data-binary '{}' https://reports.example.org/v1/reports
   done; echo
   ```

   The forged value must make no difference: the answers are 422 for the first ten and 429 from the eleventh on
   (every request counts as your own address), and a second run with another forged value is still 429. If a
   different forged value starts a fresh budget of ten, the tunnel forwards the client's header: fix the tunnel
   (or switch `client_ip_header`) before exposing the server. Repeat with `X-Forwarded-For` if that is the header
   in use.

Also worth having at the tunnel: its own connection and header-read timeout, and a request-rate cap above the
application's. The process itself closes a connection that does not finish its request head in
`header_timeout_secs` (also the idle keep-alive limit) and refuses more than `max_connections` at once; request
bodies are capped in size and read time.

## The bucket (MinIO)

All state lives in one S3-compatible bucket through [s3db](https://git.yerasuu.com/yerasuu/s3db): reports, groups,
counters and the private attachments, every object encrypted under a key derived from a passphrase. The bucket is
split by product (#1055): every key the server writes is under `decanter/` or `psport/` (each an s3db store of its
own), behind the optional root `prefix`. The owner's bucket is a MinIO (or similar) on the home server. Create a
bucket of its own and a user that can reach only that bucket, with the MinIO client (`mc`; check the commands against
the version installed):

```sh
mc alias set home https://minio.example.lan:9000 <admin user> <admin password>
mc mb home/decanter-reports
cat > decanter-reports-policy.json <<'JSON'
{
  "Version": "2012-10-17",
  "Statement": [
    { "Effect": "Allow", "Action": ["s3:ListBucket"], "Resource": ["arn:aws:s3:::decanter-reports"] },
    { "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
      "Resource": ["arn:aws:s3:::decanter-reports/*"] }
  ]
}
JSON
mc admin policy create home decanter-reports decanter-reports-policy.json
mc admin user add home decanter-reports "$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
mc admin policy attach home decanter-reports --user decanter-reports
```

The user name is the access key id and the generated password the secret access key; write both into the credential
files below and nowhere else. The server needs `ListBucket` (every list is a prefix listing) and get, put and delete
on the objects; nothing else. `decanter-reports/*` covers both partitions, `decanter-reports/decanter/*` and
`decanter-reports/psport/*`. s3db writes with conditional requests (`If-Match`, `If-None-Match`), which MinIO
supports.

- **Addressing.** `path_style = true` (`https://minio:9000/bucket/key`) unless MinIO answers on wildcard DNS
  (`bucket.minio...`).
- **TLS.** Give MinIO a certificate and use an `https://` endpoint: the access keys and every object (encrypted, but
  with its name in the clear) cross the network. The client trusts the system's certificate store
  (`rustls-native-certs`), so a private CA must be installed there. A plain `http://` endpoint is refused unless
  `allow_insecure_http = true`, which is only for MinIO on the same host.
- **Region.** MinIO accepts any; `us-east-1` is the usual value.
- **Partitions.** Without a root prefix the keys are `decanter/...` and `psport/...`. Each partition holds its own
  s3db bookkeeping: `decanter/.conf/encryption.json` and `psport/.conf/encryption.json` (a salt and the data key
  wrapped by the passphrase; both written by the first start, from the same passphrase) and each one's transaction
  records under `<partition>/.conf/txn-update`. Nothing is written at the bucket's top level.
- **Root prefix.** On a bucket shared with something else, `prefix = "decanter-reports"` puts both partitions under
  it (`decanter-reports/decanter/`, `decanter-reports/psport/`); then restrict the policy to the two of them:

  ```json
  { "Effect": "Allow", "Action": ["s3:ListBucket"], "Resource": ["arn:aws:s3:::<bucket>"],
    "Condition": {"StringLike": {"s3:prefix": ["decanter-reports/decanter/*", "decanter-reports/psport/*"]}} },
  { "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
    "Resource": ["arn:aws:s3:::<bucket>/decanter-reports/decanter/*", "arn:aws:s3:::<bucket>/decanter-reports/psport/*"] }
  ```

  Without a root prefix on a shared bucket, the same policy names `decanter/*` and `psport/*`. Neither layout can be
  decrypted by `s3db-inspect` (it knows no prefix); `decanter-reports inspect` can (`docs/runbook.md`).
- **Quota.** A bucket quota (`mc quota set home/decanter-reports --size 20GiB`) bounds what a flood can take. The
  caps apply per partition, so size it for twice `max_reports` reports (64 KiB each at most, plus small index
  objects) and twice `max_attachments` attachments (5 MiB each at most).

### The passphrase

```sh
umask 077
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > /etc/decanter-reports/storage.passphrase   # 64 hex characters
```

The first start writes each partition's encryption config (`decanter/.conf/encryption.json` and
`psport/.conf/encryption.json`: a salt and the data key wrapped by a key derived from the passphrase with Argon2id),
both from this one passphrase. **Losing the passphrase makes the whole bucket unreadable**: there
is no recovery and no back door. Keep a copy offline (a password manager, paper in a safe), apart from the bucket's
backups. Changing it later is s3db's `bootstrap::change_passphrase` on each partition, which rewraps the data key
without re-encrypting the objects; this server has no command for it yet.

## Configuration

Copy `deploy/config.example.toml` to `/etc/decanter-reports/config.toml`, set `public_url` and the `[storage]`
section. The directory and the config are world-readable (the service runs as a transient user that is not root)
and hold no secret; the admin key, the passphrase and the two bucket keys are separate root-only files that systemd
hands to the service as credentials.

```sh
install -d -m 0755 /etc/decanter-reports
install -m 0644 deploy/config.example.toml /etc/decanter-reports/config.toml
umask 077
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > /etc/decanter-reports/admin.key   # 64 hex characters
printf '%s' 'decanter-reports' > /etc/decanter-reports/s3.access_key_id
printf '%s' '<the secret access key>' > /etc/decanter-reports/s3.secret_access_key
chmod 0600 /etc/decanter-reports/admin.key /etc/decanter-reports/storage.passphrase /etc/decanter-reports/s3.*
DECANTER_REPORTS_ADMIN_KEY_FILE=/etc/decanter-reports/admin.key \
DECANTER_REPORTS_PASSPHRASE_FILE=/etc/decanter-reports/storage.passphrase \
DECANTER_REPORTS_ACCESS_KEY_ID_FILE=/etc/decanter-reports/s3.access_key_id \
DECANTER_REPORTS_SECRET_ACCESS_KEY_FILE=/etc/decanter-reports/s3.secret_access_key \
  decanter-reports --check-config /etc/decanter-reports/config.toml
```

`config.toml` names each secret by its credential path (`admin_key_file = "$CREDENTIALS_DIRECTORY/admin_key"`,
`storage.passphrase_file = "$CREDENTIALS_DIRECTORY/storage_passphrase"` and so on), which the unit's
`LoadCredential=` lines provide. Any other secret file is refused when group or others can read it (mode `& 0o077`);
a secret from `$CREDENTIALS_DIRECTORY` or from an environment variable is not mode-checked. Run `--check-config` as
root for the check above (the overrides replace the credential paths). It prints no secret, and one line per
partition: it reads that partition's `.conf/encryption.json` once and says whether it is reachable, whether it is
initialized, and whether the passphrase opens it. It writes nothing; a failing partition makes it exit non-zero.

```text
config ok: listening on 127.0.0.1:8787
partition decanter: bucket decanter-reports at https://minio.example.lan:9000, prefix decanter/: reachable, the passphrase opens it
partition psport: bucket decanter-reports at https://minio.example.lan:9000, prefix psport/: reachable, not initialized yet (the first start creates its encryption)
```

With `driver = "local"` (development, or a single host without MinIO) each partition is one database file, in
`storage.dir/decanter` and `storage.dir/psport`, created `0700` (a looser existing directory is narrowed with a
warning in the log); the process sets umask `077`, so the files are `0600`. The passphrase is still required: the
local files are encrypted the same way. `--check-config` does not open a local partition the server has not created
yet, and needs the server stopped (the database file is locked while it runs).

## systemd

`deploy/decanter-reports.service` runs the binary as a transient unprivileged user (`DynamicUser=yes`) with no
state of its own (everything is in the bucket) and a hardened sandbox. With the local driver, uncomment
`StateDirectory=` (mode 0700, writable under `ProtectSystem=strict` without `ReadWritePaths=`) and set
`storage.dir = "/var/lib/decanter-reports"`. `systemd-analyze security --offline=true` scores it 0.9 SAFE (the
previous unit 2.2 OK); the remaining findings are the network access it needs.

```sh
install -m 0755 target/release/decanter-reports /usr/local/bin/
install -m 0644 deploy/decanter-reports.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now decanter-reports
journalctl -u decanter-reports
```

The unit denies every address but loopback: put MinIO's address on the `IPAddressAllow=<MinIO address>` line (not
needed for MinIO on this host), and the tunnel's on the other one if it runs on another host. A hostname endpoint
also needs the resolver's address allowed (`127.0.0.53` is loopback already). `MemoryMax=512M`, `TasksMax=256`,
`LimitNOFILE=4096` and `TimeoutStopSec=10` bound what a flood can take; `max_connections` (512) stays below the file
limit. Backups are the bucket's, see `docs/runbook.md`.

## Container

`deploy/Containerfile` builds a release image on distroless. The build context is the root of decanter's repository,
because `decanter-report` is a path dependency in the same repository, and the build stage needs network to fetch
`s3db` (a git dependency on `git.yerasuu.com`) and the crates; `deploy/Containerfile.containerignore` lets through
only the workspace manifests, `decanter-report` and this crate, and keeps `.git`, `.brain`, `target` and keys out of
the build (podman reads it next to the Containerfile; copy it to the context root as `.containerignore` with an
older podman).

The base images are named by tag only: this file was written offline and could not fetch digests. Before a real
build, pin them:

```sh
podman pull docker.io/library/rust:1.98-bookworm gcr.io/distroless/cc-debian12:nonroot
podman inspect --format '{{index .RepoDigests 0}}' docker.io/library/rust:1.98-bookworm
podman inspect --format '{{index .RepoDigests 0}}' gcr.io/distroless/cc-debian12:nonroot
# then: podman build --build-arg RUST_IMAGE=<name@sha256:...> --build-arg RUNTIME_IMAGE=<name@sha256:...> ...
```

```sh
podman build -f crates/reports-server/deploy/Containerfile -t decanter-reports .
for name in admin.key storage.passphrase s3.access_key_id s3.secret_access_key; do
  podman secret create "decanter-reports-$name" "/etc/decanter-reports/$name"
done
podman run -d --name decanter-reports --restart=always \
  --read-only --cap-drop=all --security-opt no-new-privileges \
  -p 127.0.0.1:8787:8787 \
  --secret decanter-reports-admin.key --secret decanter-reports-storage.passphrase \
  --secret decanter-reports-s3.access_key_id --secret decanter-reports-s3.secret_access_key \
  -e DECANTER_REPORTS_ADMIN_KEY_FILE=/run/secrets/decanter-reports-admin.key \
  -e DECANTER_REPORTS_PASSPHRASE_FILE=/run/secrets/decanter-reports-storage.passphrase \
  -e DECANTER_REPORTS_ACCESS_KEY_ID_FILE=/run/secrets/decanter-reports-s3.access_key_id \
  -e DECANTER_REPORTS_SECRET_ACCESS_KEY_FILE=/run/secrets/decanter-reports-s3.secret_access_key \
  -e DECANTER_REPORTS_STORAGE_DRIVER=s3 -e DECANTER_REPORTS_STORAGE_BUCKET=decanter-reports \
  -e DECANTER_REPORTS_STORAGE_ENDPOINT=https://minio.example.lan:9000 -e DECANTER_REPORTS_STORAGE_REGION=us-east-1 \
  -e DECANTER_REPORTS_STORAGE_PATH_STYLE=true \
  -e DECANTER_REPORTS_PUBLIC_URL=https://reports.example.org \
  -e DECANTER_REPORTS_TRUSTED_PROXIES=<tunnel address as the container sees it> \
  -e DECANTER_REPORTS_CLIENT_IP_HEADER=cf-connecting-ip \
  decanter-reports
```

Podman mounts each secret at `/run/secrets/<name>`. The server refuses a secret file that group or others can read,
so if it stops with "group and others must have no access", add `,mode=0400` to each `--secret` (with
`uid=65532` if the file is not owned by the container's user). This was not run when this file was written. With the
local driver add `-v decanter-reports-data:/data` and drop the storage variables: the process runs as uid 65532 and
the image ships `/data` owned by it, so a new named volume is writable. A bind mount keeps the host's ownership: map your user to 65532 (`--userns=keep-id:uid=65532,gid=65532` for rootless
podman) or chown the directory first (`podman unshare chown 65532:65532 ./data`, or `chown 65532:65532` as root
for rootful podman). `--check-config` warns about the container's `0.0.0.0` listener; that is expected as long as
the published port is bound to loopback.

**`trusted_proxies` in a container.** `10.88.0.1` is the gateway of rootful podman's default bridge, not what a
rootless container sees: depending on the podman version and network backend (pasta, slirp4netns, the bridge)
the tunnel's connections arrive from the host's own address, from the gateway or from the container's address.
Find the real one instead of guessing: hold a connection open and list it from inside the container's network
namespace (the server closes it after `header_timeout_secs`, so run the second command within that time):

```sh
nc 127.0.0.1 8787 &                      # from the host, as the tunnel would connect
podman unshare nsenter -t "$(podman inspect --format '{{.State.Pid}}' decanter-reports)" -n ss -tn 'sport = :8787'
```

The `Peer Address` column is the value for `trusted_proxies`. This procedure was not run when this file was
written (no container was built offline); check it on the host, and confirm the result with the forged-header test
above.

## Operating it

- **Flooded group.** Find it on the index, look at it with `GET /admin/reports?fingerprint=<fp>&day=<day>`, then
  hide it in one call (`POST /admin/groups/<fp>/hide`, reversible with `/restore`), resolve it, or delete it with its
  attachments (`DELETE /admin/groups/<fp>`). Hiding and deleting clear the cached index at once.
- **Full store.** At `max_reports` (per partition) uploads evict the oldest unresolved reports of that partition in
  batches of 1% and the log says `report store full: oldest unresolved reports evicted` with the product and a count. Repeated lines mean a flood or too small a
  cap; resolved reports are never evicted, so a 503 `storage_full` means the store holds only resolved reports
  (lower `resolved_retention_days` or delete some).
- **Reads.** `get_per_ip_minute` behind a tunnel only works if the client address header is right (see the
  requirements above); otherwise every visitor shares the tunnel's address and its budget.

## Before exposing it

- `--check-config` passes without warnings and the admin key has been tried: `curl -H "Authorization: Bearer $KEY" .../admin/reports`
  answers 200 and the same call without the key answers 404.
- `public_url` is the exact `https://` address the browser shows (the admin session's origin check compares it), and
  a login through the tunnel at `<public_url>/admin/login` opens `/admin`; after a restart of the service the same
  browser gets the plain 404 on `/admin` (sessions live in memory only).
- A valid report is accepted and one with a private path is refused with 422 (see README, "Try it").
- The forged-header test in the requirements above passes through the real tunnel.
- A burst of `GET /` from one address ends in 429 with `Retry-After` while `GET /healthz` still answers 200.
- `--check-config` says both partitions are reachable and the passphrase opens them; the passphrase has an offline
  copy.
- The bucket quota leaves room for twice `max_reports` (64 KiB each at most) plus twice `max_attachments` (5 MiB each
  at most): the caps are per partition.
