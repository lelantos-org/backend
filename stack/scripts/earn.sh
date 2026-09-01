#!/bin/sh
# Credit interest on a yield asset's mock vault, so its index rises.
#
# Runs inside the `deploy` container (foundry image, /addresses mounted, on the
# backend network), which is the only place holding both `cast` and the address
# table. Invoked by `just earn`.
#
# What this models: a venue that earned. `MockERC4626.earn` raises the vault's
# `totalAssetsHeld` without minting shares, so the pool's existing shares are
# worth more and `MASP.yieldState(id).index` climbs above RAY. The mock makes
# the caller fund it (`transferFrom`), which is what keeps the vault's token
# balance and its accounting consistent — so this acquires the underlying first.
#
# Usage: earn.sh <yield-asset-id> <amount-in-base-units>
#
# Env (supplied by the compose service):
#   RPC_URL DEPLOYER_KEY

set -eu
. /scripts/lib.sh

ADDR_FILE=/addresses/addresses.env

ID="${1:-}"
AMT="${2:-}"
[ -n "$ID" ] && [ -n "$AMT" ] || die "usage: earn.sh <yield-asset-id> <amount-in-base-units>"
require_env RPC_URL DEPLOYER_KEY
require_cmd cast
[ -f "$ADDR_FILE" ] || die "$ADDR_FILE not found — has the deploy one-shot run?"

# shellcheck disable=SC1090
. "$ADDR_FILE"

[ -n "${MASP:-}" ] || die "MASP not in $ADDR_FILE — has the deploy one-shot run?"

step "earn $AMT on asset $ID"

# The venue triple comes from the chain, not from `YIELD_VAULT_$ID` /
# `YIELD_TOKEN_$ID` in the address table. The pool names the venue and the
# venue's immutables name the vault and its underlying, so this works for any
# bound asset — including one bound after the last deploy, and the ids whose
# `YIELD_TOKEN_*` key deploy-contracts.sh never asserts.
#
# Every read is captured before it is parsed. `x=$(cast ... | sed | cut)` exits
# with the status of `cut`, so `set -e` never sees cast fail: an unreachable node
# left `$supply` empty, `[ "" != "0" ]` is true, and the "vault holds no shares"
# guard below was bypassed on exactly the failure it exists to catch.

# `yield_field <n>` — member n of the `yieldState` tuple, 1-based. `cast` prints
# one member per line. One definition so the signature cannot drift between its
# two readers, which sit 20 lines apart and read different fields.
yield_field() {
    _raw=$(cast_call "$MASP" \
        'yieldState(uint64)(address,uint16,uint16,bool,uint256,uint256,uint256,uint256,uint256)' \
        "$ID") || die "yieldState($ID) failed on $MASP — is the node up?"
    printf '%s\n' "$_raw" | sed -n "$1p" | cut -d' ' -f1
}

VENUE=$(yield_field 1)
case "$VENUE" in
    0x0000000000000000000000000000000000000000 | "")
        die "asset $ID is not a yield asset (no venue bound on $MASP)" ;;
esac
VAULT=$(cast_call "$VENUE" 'VAULT()(address)' | cut -d' ' -f1) \
    || die "$VENUE.VAULT() failed"
TOKEN=$(cast_call "$VENUE" 'UNDERLYING()(address)' | cut -d' ' -f1) \
    || die "$VENUE.UNDERLYING() failed"
[ -n "$VAULT" ] && [ -n "$TOKEN" ] || die "venue $VENUE returned an empty vault or token"
debug "venue $VENUE vault $VAULT token $TOKEN"

# The mock mints shares pro rata against `totalAssetsHeld`, so a donation into a
# vault with no shares outstanding raises the price of nothing and is simply
# lost. The pool takes its shares when a deposit settles (`_fundVenue`), so an
# untouched asset has to be deposited into before it can earn.
supply_raw=$(cast_call "$VAULT" 'totalSupply()(uint256)') || die "$VAULT.totalSupply() failed"
supply=$(printf '%s\n' "$supply_raw" | cut -d' ' -f1)
[ -n "$supply" ] || die "$VAULT.totalSupply() returned nothing"
[ "$supply" != "0" ] || die "vault for asset $ID holds no shares — deposit into asset $ID first, or the credit goes nowhere"

# `index` is the 9th member of the `yieldState` tuple.
read_index() { yield_field 9; }
index_before=$(read_index)

# MockWETH9 has no public mint, so the deployer wraps; the plain ERC20 mocks do
# expose `mint(address,uint256)`. Same split as the funding step in
# deploy-contracts.sh.
if [ "$TOKEN" = "${WRAPPED_NATIVE:-}" ]; then
    log "wrapping $AMT native"
    cast_send --value "$AMT" "$TOKEN" "deposit()"
else
    log "minting $AMT of $TOKEN"
    deployer=$(cast wallet address --private-key "$DEPLOYER_KEY")
    cast_send "$TOKEN" "mint(address,uint256)" "$deployer" "$AMT"
fi

log "approving vault $VAULT"
cast_send "$TOKEN" "approve(address,uint256)" "$VAULT" "$AMT"

log "crediting the vault"
cast_send "$VAULT" "earn(uint256)" "$AMT"

# The index is what every client converts through, so it is the figure worth
# echoing — and both sides of the move, because what a credit is worth depends
# on a supply this script never saw. RAY (1e27) means nothing has been earned.
index_after=$(read_index)
log "asset $ID index $index_before -> $index_after (RAY = 1000000000000000000000000000)"
