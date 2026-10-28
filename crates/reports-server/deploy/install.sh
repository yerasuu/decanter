#!/bin/sh
# Installs or upgrades decanter-reports behind Traefik from the published image.
#   ./install.sh <version>      e.g. ./install.sh 0.1.0 for the tag reports-v0.1.0
# Run it from a deployment folder outside any checkout; it keeps compose.yaml and .env there. Read it before running
# it (docs/deploy.md, "Docker and Traefik"): it handles the bucket keys and the passphrase.
set -eu

IMAGE_REPO=ghcr.io/yerasuu/decanter-reports
ATTESTATION_OWNER=yerasuu
NETWORK=expose-public

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# Prompts go to stderr so $(ask ...) captures only the answer.
ask() {
    printf '%s [%s]: ' "$1" "$2" >&2
    IFS= read -r answer || die "no input"
    printf '%s\n' "${answer:-$2}"
}

ask_required() {
    printf '%s: ' "$1" >&2
    IFS= read -r answer || die "no input"
    [ -n "$answer" ] || die "$1 is required"
    printf '%s\n' "$answer"
}

ask_secret() {
    printf '%s: ' "$1" >&2
    # Restores the terminal even if interrupted while echo is off.
    trap 'stty echo 2>/dev/null || true' EXIT INT TERM
    stty -echo 2>/dev/null || true
    IFS= read -r answer || die "no input"
    stty echo 2>/dev/null || true
    trap - EXIT INT TERM
    printf '\n' >&2
    [ -n "$answer" ] || die "$1 is required"
    printf '%s\n' "$answer"
}

# A value that goes into .env between single quotes: Compose reads it literally, so only a quote or a line break
# could break out of it.
check_quotable() {
    case $2 in
    *"'"* | *'
'*) die "$1 must not contain a single quote or a line break" ;;
    esac
}

[ "$#" -eq 1 ] || die "usage: $0 <version>   (for the tag reports-vX.Y.Z pass X.Y.Z)"
version=$1
case $version in
latest | '' | *[!0-9A-Za-z._-]*) die "version must be a release number such as 0.1.0, not '$version'" ;;
esac
case $version in
[0-9]*.[0-9]*.[0-9]*) ;;
*) die "version must look like X.Y.Z, not '$version'" ;;
esac

src_dir=$(cd "$(dirname "$0")" && pwd)
[ -f "$src_dir/compose.yaml" ] || die "compose.yaml not found next to $0"

# 1. Tools and network.
command -v docker >/dev/null 2>&1 || die "docker is not installed"
docker compose version >/dev/null 2>&1 || die "docker compose v2 is required ('docker compose version' failed)"
docker network inspect "$NETWORK" >/dev/null 2>&1 || die "the external network '$NETWORK' does not exist"

if command -v git >/dev/null 2>&1 && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    die "run this from a deployment folder outside any git checkout, so .env (it holds the secrets) is never committed"
fi

if [ "$src_dir" != "$(pwd)" ]; then
    cp -- "$src_dir/compose.yaml" compose.yaml
    printf 'compose.yaml copied from %s\n' "$src_dir"
fi

# 5. The image: the tag is resolved to a digest once and the digest is what gets deployed.
tag_image="$IMAGE_REPO:$version"
printf 'pulling %s\n' "$tag_image"
docker pull "$tag_image" >/dev/null
digest_image=$(docker image inspect --format '{{index .RepoDigests 0}}' "$tag_image")
case $digest_image in
"$IMAGE_REPO"@sha256:*) ;;
*) die "could not resolve $tag_image to a digest (got '$digest_image')" ;;
esac
if command -v gh >/dev/null 2>&1; then
    gh attestation verify "oci://$digest_image" --owner "$ATTESTATION_OWNER" || die "provenance attestation did not verify"
else
    printf 'warning: gh is not installed, the provenance attestation of %s was NOT verified\n' "$digest_image" >&2
fi

if [ -e .env ]; then
    # 2. An existing .env is never touched: an upgrade is editing REPORTS_IMAGE in it.
    printf '.env exists and is kept as it is. To upgrade, set REPORTS_IMAGE=%s in it.\n' "$digest_image"
    printf 'Then: docker compose up -d\n'
    exit 0
fi

# 3. Questions.
public_host=$(ask_required "Public host name (e.g. reports.example.org)")
s3_endpoint=$(ask_required "S3 endpoint (https://...)")
bucket=$(ask "Bucket" "decanter-reports")
region=$(ask "Region" "us-east-1")
access_key_id=$(ask_required "Bucket access key id")
secret_access_key=$(ask_secret "Bucket secret access key (not shown)")
for pair in "host:$public_host" "endpoint:$s3_endpoint" "bucket:$bucket" "region:$region" \
    "access key id:$access_key_id" "secret access key:$secret_access_key"; do
    check_quotable "${pair%%:*}" "${pair#*:}"
done
case $public_host in
*[!0-9A-Za-z.-]*) die "the host name has characters a host cannot have" ;;
esac
case $s3_endpoint in
https://*) ;;
*) die "the S3 endpoint must start with https://" ;;
esac

# 4. Suggestions from what is attached to the network; the owner confirms them.
printf 'Containers on %s:\n' "$NETWORK"
docker network inspect "$NETWORK" --format '{{range .Containers}}  {{.Name}} {{.IPv4Address}}{{"\n"}}{{end}}'
suggest() {
    docker network inspect "$NETWORK" \
        --format '{{range .Containers}}{{.Name}} {{.IPv4Address}}{{"\n"}}{{end}}' |
        awk -v pat="$1" '$1 ~ pat { split($2, a, "/"); print a[1]; exit }'
}
traefik_ip=$(ask "Traefik's address on $NETWORK (single address)" "$(suggest traefik)")
tunnel_ip=$(ask "Address the tunnel connects from (address or CIDR)" "$(suggest cloudflared)")
[ -n "$traefik_ip" ] || die "Traefik's address is required"
[ -n "$tunnel_ip" ] || die "the tunnel's address is required"
case $tunnel_ip in
*/*) ;;
*) tunnel_ip="$tunnel_ip/32" ;;
esac
check_quotable "Traefik address" "$traefik_ip"
check_quotable "tunnel address" "$tunnel_ip"

# 6. The passphrase is made by the binary, never by the serving process.
passphrase=$(docker run --rm --pull=never "$digest_image" secret)
case $passphrase in
'' | *[!0-9A-Za-z._~+/=-]*) die "'decanter-reports secret' printed something that is not a single token" ;;
esac

# 7. .env: owner-only from the first byte.
umask 077
tmp_env=$(mktemp ./.env.XXXXXX)
trap 'rm -f -- "$tmp_env"' EXIT INT TERM
{
    printf '# Written by install.sh for reports-v%s. Holds secrets: mode 0600, never in git.\n' "$version"
    printf "REPORTS_IMAGE=%s\n" "$digest_image"
    printf "REPORTS_HOST=%s\n" "$public_host"
    printf "TUNNEL_SOURCE=%s\n" "$tunnel_ip"
    printf "DECANTER_REPORTS_PUBLIC_URL=https://%s\n" "$public_host"
    printf "DECANTER_REPORTS_TRUSTED_PROXIES=%s\n" "$traefik_ip"
    printf "DECANTER_REPORTS_CLIENT_IP_HEADER=cf-connecting-ip\n"
    printf "DECANTER_REPORTS_STORAGE_DRIVER=s3\n"
    printf "DECANTER_REPORTS_STORAGE_ENDPOINT=%s\n" "$s3_endpoint"
    printf "DECANTER_REPORTS_STORAGE_BUCKET=%s\n" "$bucket"
    printf "DECANTER_REPORTS_STORAGE_REGION=%s\n" "$region"
    printf "DECANTER_REPORTS_STORAGE_PATH_STYLE=true\n"
    printf "DECANTER_REPORTS_S3_ACCESS_KEY_ID='%s'\n" "$access_key_id"
    printf "DECANTER_REPORTS_S3_SECRET_ACCESS_KEY='%s'\n" "$secret_access_key"
    printf "DECANTER_REPORTS_PASSPHRASE='%s'\n" "$passphrase"
} >"$tmp_env"
chmod 0600 "$tmp_env"
# link fails instead of replacing a .env that appeared meanwhile.
ln -- "$tmp_env" .env || die ".env appeared while the script ran; nothing was overwritten"
rm -f -- "$tmp_env"
trap - EXIT INT TERM
printf '.env written (mode 0600)\n'

# 8. Stops here if the passphrase does not open what the bucket already holds.
check_out=$(docker compose run --rm --no-deps decanter-reports --check-config 2>&1) || {
    printf '%s\n' "$check_out" >&2
    die "--check-config failed; .env was kept, fix it and run 'docker compose run --rm --no-deps decanter-reports --check-config'"
}
printf '%s\n' "$check_out"
case $check_out in
*"does not open it"*) die "the passphrase does not open the existing bucket; not starting" ;;
esac

# 9. Shown once. Losing it makes the bucket unreadable.
printf '\n================ STORAGE PASSPHRASE (shown once) ================\n%s\n' "$passphrase"
printf '=================================================================\n'
printf 'Copy it offline now (password manager, paper in a safe), apart from the bucket backups.\n'
while :; do
    printf 'Type its last 6 characters to continue: ' >&2
    IFS= read -r typed || die "no input"
    [ "$typed" = "$(printf '%s' "$passphrase" | tail -c 6)" ] && break
    printf 'does not match, try again\n' >&2
done
passphrase=

# 10.
docker compose up -d

# 11. The first admin; the server prints its key once.
admin_name=$(ask "Name of the first admin" "admin")
check_quotable "admin name" "$admin_name"
docker compose run --rm --no-deps decanter-reports user add "$admin_name" --role admin

cat <<DONE

Installed reports-v$version ($digest_image). Still yours to check (docs/deploy.md, "Before exposing it"):
  - the GHCR package is public and the tunnel reaches https://$public_host
  - the forged-header test through the real tunnel
  - a login at https://$public_host/admin/login with the admin key above
  - the offline copy of the passphrase and the bucket quota
DONE
