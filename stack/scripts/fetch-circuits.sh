#!/usr/bin/env bash
# Fetch the tree_update_batch circuit artifacts (wasm + r1cs + zkey) from a
# @lelantos-org/circuits GitHub release into ./circuits/, which docker-compose
# mounts into the relayer container at /circuits:ro.
#
# Downloads are skipped when ./circuits/.version already records the requested
# tag; `just refetch-circuits` drops that sentinel to force a re-fetch.
#
# Required env:
#   CIRCUITS_VERSION   release tag, e.g. v0.6.4
#
# Optional env:
#   GH_TOKEN           auth for `gh`; falls back to the `gh auth` session
#   DEST               output directory (default <stack>/circuits)
#   DEBUG=1            verbose progress detail
#   TRACE=1            shell tracing (`set -x`); implies DEBUG

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
. "${SCRIPT_DIR}/lib.sh"

STACK_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEST="${DEST:-${STACK_DIR}/circuits}"
REPO="lelantos-org/circuits"
VERSION_FILE="${DEST}/.version"

ASSETS="tree_update_batch.wasm tree_update_batch.r1cs tree_update_batch_final.zkey 3x3_verification_key.json"

# Nothing to do when the cached artifacts already match the requested tag.
is_cached() {
    [ -f "$VERSION_FILE" ] || return 1
    [ "$(cat "$VERSION_FILE")" = "$CIRCUITS_VERSION" ] || return 1
    # A matching sentinel with a missing artifact means a half-applied fetch.
    for _asset in $ASSETS; do
        [ -f "${DEST}/${_asset}" ] || {
            warn "sentinel says ${CIRCUITS_VERSION} but ${_asset} is missing; re-fetching"
            return 1
        }
    done
    return 0
}

download() {
    _tmp="$1"
    # Build `--pattern <asset>` pairs in the positional params ($1 is already
    # saved above, so clobbering them here is safe).
    set --
    for _asset in $ASSETS; do
        set -- "$@" --pattern "$_asset"
    done

    log "downloading ${CIRCUITS_VERSION} from ${REPO}"
    gh release download "$CIRCUITS_VERSION" \
        --repo "$REPO" \
        "$@" \
        --dir "$_tmp" \
        --clobber
}

# Verify the whole set before moving any of it, so a partial release can't
# leave DEST holding a mix of old and new artifacts.
install_assets() {
    _tmp="$1"
    for _asset in $ASSETS; do
        [ -f "${_tmp}/${_asset}" ] \
            || die "release ${CIRCUITS_VERSION} is missing asset ${_asset}"
    done
    for _asset in $ASSETS; do
        debug "install ${_asset}"
        mv "${_tmp}/${_asset}" "${DEST}/${_asset}"
    done
    echo "$CIRCUITS_VERSION" > "$VERSION_FILE"
}

main() {
    require_env CIRCUITS_VERSION
    require_cmd gh

    if is_cached; then
        log "circuits ${CIRCUITS_VERSION} already present in ${DEST}"
        return 0
    fi

    mkdir -p "$DEST"
    _tmp="$(mktemp -d)"
    # shellcheck disable=SC2064  # expand $_tmp now, not at trap time
    trap "rm -rf '${_tmp}'" EXIT

    download "$_tmp"
    install_assets "$_tmp"

    log "circuits ${CIRCUITS_VERSION} ready in ${DEST}"
    [ "${DEBUG:-0}" = "1" ] && ls -lh "$DEST" >&2
    return 0
}

main "$@"
