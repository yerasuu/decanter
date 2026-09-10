#!/usr/bin/env bash
# Puts the proxy Galaxy64.dll into a game, or takes it back out.
#
#   tools/implant.sh install <directorio del juego>
#   tools/implant.sh status  <directorio del juego>
#   tools/implant.sh remove  <directorio del juego>
#
# The proxy has to *be* `Galaxy64.dll` — the game resolves the import by
# filename — so installing means renaming the real one to
# `Galaxy64.orig.dll` and dropping ours in its place. It must stay in the
# same directory: Galaxy loads further pieces relative to its own location.
#
# Every copy in the tree is patched, not just one. A Unity game ships the
# same DLL beside the executable and under `<Game>_Data/Plugins`, and which
# one Mono resolves is not something to guess at from out here.
set -euo pipefail

DLL_NAME="Galaxy64.dll"
REAL_NAME="Galaxy64.orig.dll"
LOG_NAME="decanter-achievements.jsonl"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILT="$HERE/crates/hook/target/x86_64-pc-windows-gnu/release/$DLL_NAME"

usage() {
    sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
    exit 2
}

# Every Galaxy64.dll in the tree, real or already proxied.
find_targets() {
    find "$1" -name "$DLL_NAME" -o -name "$REAL_NAME" | sed "s/\.orig\.dll$/.dll/" | sort -u
}

# Ours or the game's? The real one is ~14 MB of Galaxy SDK; the proxy is
# under a megabyte and exports the same names. Size is a cruder test than
# reading the export table, but it cannot be fooled into overwriting the
# real DLL with itself and losing it — which is the one mistake here that
# is not recoverable without reinstalling.
is_proxy() {
    [ -f "$1" ] && [ "$(stat -c %s "$1")" -lt 4000000 ]
}

cmd_install() {
    local game="$1" patched=0
    [ -f "$BUILT" ] || {
        echo "no está construido: $BUILT" >&2
        echo "  cd crates/hook && cargo build --release" >&2
        exit 1
    }

    while read -r dll; do
        [ -n "$dll" ] || continue
        local dir real
        dir="$(dirname "$dll")"
        real="$dir/$REAL_NAME"

        if [ ! -f "$real" ]; then
            if is_proxy "$dll"; then
                echo "  ! $dir: hay un proxy sin original al lado, lo dejo" >&2
                continue
            fi
            mv "$dll" "$real"
        fi
        cp "$BUILT" "$dll"
        echo "  implantado en $dir"
        patched=$((patched + 1))
    done < <(find_targets "$game")

    [ "$patched" -gt 0 ] || {
        echo "no hay ningún $DLL_NAME en $game" >&2
        exit 1
    }
    echo
    echo "listo. el registro sale en <juego>/$LOG_NAME"
    echo "para mandarlo a otro sitio: DECANTER_ACHIEVEMENT_LOG=/ruta/al.jsonl"
}

cmd_remove() {
    while read -r dll; do
        [ -n "$dll" ] || continue
        local dir real
        dir="$(dirname "$dll")"
        real="$dir/$REAL_NAME"
        if [ -f "$real" ]; then
            mv -f "$real" "$dll"
            echo "  restaurado $dir"
        fi
        rm -f "$dir/$LOG_NAME"
    done < <(find_targets "$1")
}

cmd_status() {
    while read -r dll; do
        [ -n "$dll" ] || continue
        local dir state
        dir="$(dirname "$dll")"
        if [ -f "$dir/$REAL_NAME" ] && is_proxy "$dll"; then
            state="implantado"
        elif [ -f "$dir/$REAL_NAME" ]; then
            state="a medias (hay original pero el DLL no es el proxy)"
        else
            state="sin tocar"
        fi
        echo "  $state — $dir"
    done < <(find_targets "$1")

    local log
    log="$(find "$1" -name "$LOG_NAME" | head -1)"
    if [ -n "$log" ]; then
        echo
        echo "registro: $log ($(wc -l < "$log") líneas)"
        tail -5 "$log"
    fi
}

[ $# -eq 2 ] || usage
[ -d "$2" ] || { echo "no es un directorio: $2" >&2; exit 1; }

case "$1" in
    install) cmd_install "$2" ;;
    remove)  cmd_remove "$2" ;;
    status)  cmd_status "$2" ;;
    *)       usage ;;
esac
