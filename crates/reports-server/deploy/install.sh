#!/bin/sh
# Installs or upgrades decanter-reports behind Traefik from the published image.
#   ./install.sh <version>      e.g. ./install.sh 0.1.0 for the tag reports-v0.1.0
# Run it from a deployment folder outside any checkout; it keeps compose.yaml and .env there. Read it before running
# it (docs/deploy.md, "Docker and Traefik"): it handles the bucket keys and the passphrase.
set -eu

IMAGE_REPO=ghcr.io/yerasuu/decanter-reports
ATTESTATION_REPO=yerasuu/decanter
ATTESTATION_SIGNER_WORKFLOW=$ATTESTATION_REPO/.github/workflows/reports-image.yml
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

# Every value goes into .env between single quotes, which Compose reads literally: only a quote or a line break could
# break out, and a quote cannot be escaped inside them (unverified for the pinned Compose), so both are refused.
check_quotable() {
    case $2 in
    *"'"* | *'
'*) die "$1 must not contain a single quote or a line break" ;;
    esac
}

# One IPv4 literal: four decimal fields of 0-255 without leading zeros, and not 0.0.0.0.
is_ipv4() {
    case $1 in
    '' | *[!0-9.]* | .* | *. | *..*) return 1 ;;
    esac
    [ "$1" != 0.0.0.0 ] || return 1
    (
        IFS=.
        set -f
        # shellcheck disable=SC2086
        set -- $1
        [ "$#" -eq 4 ] || exit 1
        for field in "$@"; do
            case $field in
            0 | [1-9] | [1-9][0-9] | [1-9][0-9][0-9]) [ "$field" -le 255 ] || exit 1 ;;
            *) exit 1 ;;
            esac
        done
    )
}

# One IPv6 literal in hex and colons (no embedded IPv4, no zone): groups of 1-4 digits, at most one "::", and not "::".
is_ipv6() {
    case $1 in
    '' | *[!0-9A-Fa-f:]* | *:::* | :[!:]* | *[!:]:) return 1 ;;
    *:*) ;;
    *) return 1 ;;
    esac
    [ "$1" != "::" ] || return 1
    compressed=no
    case $1 in
    *::*)
        compressed=yes
        case ${1#*::} in *::*) return 1 ;; esac
        ;;
    esac
    (
        IFS=:
        set -f
        # shellcheck disable=SC2086
        set -- $1
        groups=0
        for group in "$@"; do
            [ -n "$group" ] || continue
            case $group in
            ????? | *[!0-9A-Fa-f]*) exit 1 ;;
            esac
            groups=$((groups + 1))
        done
        if [ "$compressed" = yes ]; then [ "$groups" -le 7 ]; else [ "$groups" -eq 8 ]; fi
    )
}

is_address() {
    is_ipv4 "$1" || is_ipv6 "$1"
}

# An address, or an address with a prefix no shorter than /8 (IPv4) or /32 (IPv6): a wider range would let more than
# the tunnel through the allow list.
is_address_or_cidr() {
    case $1 in
    */*)
        addr=${1%%/*}
        prefix=${1#*/}
        case $prefix in
        '' | 0* | *[!0-9]*) return 1 ;;
        esac
        [ "${#prefix}" -le 3 ] || return 1
        if is_ipv4 "$addr"; then
            [ "$prefix" -ge 8 ] && [ "$prefix" -le 32 ]
        elif is_ipv6 "$addr"; then
            [ "$prefix" -ge 32 ] && [ "$prefix" -le 128 ]
        else
            return 1
        fi
        ;;
    *) is_address "$1" ;;
    esac
}

env_line() {
    printf "%s='%s'\n" "$1" "$2"
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
    gh attestation verify "oci://$digest_image" --repo "$ATTESTATION_REPO" \
        --signer-workflow "$ATTESTATION_SIGNER_WORKFLOW" || die "provenance attestation did not verify"
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
https://?*) ;;
*) die "the S3 endpoint must be an https:// URL" ;;
esac
case $s3_endpoint in
*[!A-Za-z0-9._~:/%-]*) die "the S3 endpoint has characters a plain https:// URL does not need (spaces, quotes, @, ?, #)" ;;
esac
case $bucket in
'' | *[!a-z0-9.-]*) die "the bucket name must be lowercase letters, digits, dots and hyphens" ;;
esac
case $region in
'' | *[!a-z0-9.-]*) die "the region must be lowercase letters, digits, dots and hyphens" ;;
esac

# 4. Suggestions from what is attached to the network; the owner confirms them.
printf 'Containers on %s:\n' "$NETWORK"
docker network inspect "$NETWORK" --format '{{range .Containers}}  {{.Name}} {{.IPv4Address}}{{"\n"}}{{end}}'
# Offers an address only when exactly one container matches, and says which, so the owner confirms it with Enter.
suggest() {
    matches=$(docker network inspect "$NETWORK" \
        --format '{{range .Containers}}{{.Name}} {{.IPv4Address}}{{"\n"}}{{end}}' |
        awk -v pat="$1" 'NF >= 2 && $1 ~ pat')
    count=$(printf '%s\n' "$matches" | awk 'NF { n++ } END { print n + 0 }')
    case $count in
    0) printf 'no container on %s matches "%s"; type the address\n' "$NETWORK" "$1" >&2 ;;
    1)
        found_addr=${matches##* }
        found_addr=${found_addr%%/*}
        printf 'one container matches "%s": %s at %s (Enter accepts it)\n' "$1" "${matches%% *}" "$found_addr" >&2
        printf '%s\n' "$found_addr"
        ;;
    *) printf 'several containers match "%s", none is chosen; type the address:\n%s\n' "$1" "$matches" >&2 ;;
    esac
}
traefik_ip=$(ask "Traefik's address on $NETWORK (single address)" "$(suggest traefik)")
tunnel_ip=$(ask "Address the tunnel connects from (address or CIDR)" "$(suggest cloudflared)")
[ -n "$traefik_ip" ] || die "Traefik's address is required"
[ -n "$tunnel_ip" ] || die "the tunnel's address is required"
is_address "$traefik_ip" || die "Traefik's address must be one IPv4 or IPv6 address, not '$traefik_ip'"
is_address_or_cidr "$tunnel_ip" ||
    die "the tunnel's address must be one address or one CIDR (at least /8 for IPv4, /32 for IPv6), not '$tunnel_ip'"
case $tunnel_ip in
*/*) ;;
*:*) tunnel_ip="$tunnel_ip/128" ;;
*) tunnel_ip="$tunnel_ip/32" ;;
esac

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
    env_line REPORTS_IMAGE "$digest_image"
    env_line REPORTS_HOST "$public_host"
    env_line TUNNEL_SOURCE "$tunnel_ip"
    env_line DECANTER_REPORTS_PUBLIC_URL "https://$public_host"
    env_line DECANTER_REPORTS_TRUSTED_PROXIES "$traefik_ip"
    env_line DECANTER_REPORTS_CLIENT_IP_HEADER cf-connecting-ip
    env_line DECANTER_REPORTS_STORAGE_DRIVER s3
    env_line DECANTER_REPORTS_STORAGE_ENDPOINT "$s3_endpoint"
    env_line DECANTER_REPORTS_STORAGE_BUCKET "$bucket"
    env_line DECANTER_REPORTS_STORAGE_REGION "$region"
    env_line DECANTER_REPORTS_STORAGE_PATH_STYLE true
    env_line DECANTER_REPORTS_S3_ACCESS_KEY_ID "$access_key_id"
    env_line DECANTER_REPORTS_S3_SECRET_ACCESS_KEY "$secret_access_key"
    env_line DECANTER_REPORTS_PASSPHRASE "$passphrase"
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
*"the passphrase does not open this bucket"*) die "the passphrase does not open the existing bucket; not starting" ;;
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
