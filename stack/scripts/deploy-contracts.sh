#!/usr/bin/env sh
# One-shot contract deploy + fund, run by the `deploy` compose service.
#
#   1. forge script DeployTest.s.sol      → verifiers, MASP, mock tokens
#   2. forge script DeployTestSwap.s.sol  → UniV3Adapter, SwapWrapper, mocks
#   3. forge script DeployTestYield.s.sol → MockERC4626 vaults, ERC4626Venues
#   4. fund FUND_RECIPIENT with native ETH, WETH and two mock ERC20s
#   5. write addresses.env, sourced by every backend's entrypoint wrapper
#
# Required env (set in docker-compose.yml):
#   RPC_URL DEPLOYER_KEY FUND_RECIPIENT FUND_NATIVE FUND_WETH FUND_ERC20
#
# Optional env (defaults suit the container; override to run on the host):
#   CONTRACTS_DIR  forge project root         (default /contracts)
#   OUT_FILE       env file to write          (default /addresses/addresses.env)
#   WORK_DIR       scratch for logs/parsing   (default /tmp)
#   CHAIN_ID       chain id used in var names (default 31337)
#   DEBUG=1        verbose progress detail
#   TRACE=1        shell tracing (`set -x`); implies DEBUG
#
# Debugging: the full forge output is kept at $WORK_DIR/forge.log and the
# parsed address table at $WORK_DIR/addresses.parsed, both readable after a
# failure with `docker compose run --rm --entrypoint sh deploy`.

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
. "${SCRIPT_DIR}/lib.sh"

CONTRACTS_DIR="${CONTRACTS_DIR:-/contracts}"
OUT_FILE="${OUT_FILE:-/addresses/addresses.env}"
WORK_DIR="${WORK_DIR:-/tmp}"
CHAIN_ID="${CHAIN_ID:-31337}"

FORGE_LOG="${WORK_DIR}/forge.log"
ADDR_FILE="${WORK_DIR}/addresses.parsed"
RC_FILE="${WORK_DIR}/forge.rc"

# ── forge / cast wrappers ───────────────────────────────────────────────

# forge_script <Script.s.sol:Contract>
#
# `sh` has no `pipefail`, so a plain `forge … | tee` would report tee's exit
# status and let a failed deploy sail through. Run forge inside a subshell
# that stamps its own status into $RC_FILE, then check that. `set +e` is
# required inside the subshell or errexit tears it down before the stamp.
forge_script() {
    _target="$1"
    log "forge script ${_target}"
    rm -f "$RC_FILE"
    (
        set +e
        forge script "script/${_target}" \
            --rpc-url "$RPC_URL" \
            --private-key "$DEPLOYER_KEY" \
            --broadcast \
            --disable-code-size-limit 2>&1
        echo "$?" > "$RC_FILE"
    ) | tee -a "$FORGE_LOG"

    _rc="$(cat "$RC_FILE" 2>/dev/null || echo 1)"
    [ "$_rc" = "0" ] || die "forge script ${_target} failed (exit ${_rc}); see ${FORGE_LOG}" "$_rc"
}

# cast_send <cast-send-args...>
cast_send() {
    debug "cast send $*"
    cast send --rpc-url "$RPC_URL" --private-key "$DEPLOYER_KEY" "$@" >/dev/null
}

# ── address table ───────────────────────────────────────────────────────

# Rebuild $ADDR_FILE from everything logged so far. The forge scripts print
# `KEY=0x…` lines (see BaseDeploy._logCoreKv); we keep the LAST value seen for
# each key so a later pass overrides an earlier one. Plain `sort -u` would
# instead keep both rows when a key is re-logged with a different address,
# and `addr` would then echo two lines into one variable.
reload_addresses() {
    grep -oE '[A-Z_0-9]+=0x[0-9a-fA-F]{40}' "$FORGE_LOG" \
        | awk -F= '{ seen[$1] = $2 } END { for (k in seen) print k "=" seen[k] }' \
        | sort > "$ADDR_FILE"
    debug "parsed $(wc -l < "$ADDR_FILE" | tr -d ' ') addresses into ${ADDR_FILE}"
}

# addr <KEY> — echo the address, empty if absent.
addr() { awk -F= -v key="$1" '$1 == key { print $2 }' "$ADDR_FILE"; }

# addr_req <KEY> — echo the address, abort the script if absent. Runs in a
# command substitution, so the non-zero exit propagates via `set -e`.
addr_req() {
    _value="$(addr "$1")"
    [ -n "$_value" ] || die "$1 missing from forge output; see ${FORGE_LOG}"
    echo "$_value"
}

# ── phases ──────────────────────────────────────────────────────────────

preflight() {
    step "preflight"
    require_cmd forge cast awk grep
    require_env RPC_URL DEPLOYER_KEY FUND_RECIPIENT FUND_NATIVE FUND_WETH FUND_ERC20
    [ -d "$CONTRACTS_DIR" ] || die "CONTRACTS_DIR ${CONTRACTS_DIR} does not exist"

    mkdir -p "$WORK_DIR" "$(dirname "$OUT_FILE")"
    : > "$FORGE_LOG"
    cd "$CONTRACTS_DIR"

    log "contracts=${CONTRACTS_DIR} rpc=${RPC_URL} chain=${CHAIN_ID}"
    debug "out=${OUT_FILE} work=${WORK_DIR}"
}

# Verifiers + MASP + mock tokens + NativeAdapter.
deploy_core() {
    step "deploy core (MASP + verifiers + mock tokens)"
    forge_script "DeployTest.s.sol:DeployTest"
    reload_addresses

    MASP=$(addr_req MASP)
    PERMIT2=$(addr_req PERMIT2)
    TOKEN_1=$(addr_req TOKEN_1)
    TOKEN_2=$(addr_req TOKEN_2)
    TOKEN_3=$(addr_req TOKEN_3)
    WETH=$(addr_req WRAPPED_NATIVE)
    NATIVE_ADAPTER=$(addr_req NATIVE_ADAPTER)

    log "MASP=${MASP}"
}

# UniV3Adapter + SwapWrapper + swap mocks. DeployTestSwap reads the core
# addresses from the environment.
deploy_swap() {
    step "deploy swap stack (UniV3Adapter + UniV4Adapter + SwapWrapper)"
    # Exported rather than prefixed onto the call: a `VAR=x func` prefix on a
    # shell *function* has implementation-defined persistence in POSIX sh.
    export MASP PERMIT2 TOKEN_1 TOKEN_2 TOKEN_3
    forge_script "DeployTestSwap.s.sol:DeployTestSwap"
    reload_addresses

    SWAP_WRAPPER=$(addr_req SWAP_WRAPPER)
    UNIV3_ADAPTER=$(addr_req UNIV3_ADAPTER)
    UNIV3_QUOTER=$(addr_req UNIV3_QUOTER)
    MOCK_SWAP_ROUTER=$(addr_req MOCK_SWAP_ROUTER)
    UNIV4_ADAPTER=$(addr_req UNIV4_ADAPTER)
    UNIV4_QUOTER=$(addr_req UNIV4_QUOTER)
    MOCK_UNIVERSAL_ROUTER=$(addr_req MOCK_UNIVERSAL_ROUTER)

    log "SWAP_WRAPPER=${SWAP_WRAPPER}"
    # Swap rates are seeded inside DeployTest._deploySwap (its `setRate`
    # calls). MockSwapRouter02 mints `tokenOut` on demand, so there is no
    # inventory to seed and no `setNextOut` needed for the dev flow; e2e
    # tests that want a fixed output still call `setNextOut` themselves.
    debug "MOCK_SWAP_ROUTER=${MOCK_SWAP_ROUTER} (rates seeded by DeployTest)"
    debug "MOCK_UNIVERSAL_ROUTER=${MOCK_UNIVERSAL_ROUTER} (rates seeded by DeployTest)"
}

# One MockERC4626 + ERC4626Venue per fixture asset, each registered as a new
# yield asset id. DeployTestYield reads MASP and the token addresses from the
# environment, and derives the yield ids from the same committed fixture as the
# plain ones (1,2,3 → 4,5,6).
deploy_yield() {
    step "deploy yield stack (MockERC4626 vaults + ERC4626Venues)"
    export MASP TOKEN_1 TOKEN_2 TOKEN_3
    forge_script "DeployTestYield.s.sol:DeployTestYield"
    reload_addresses

    # Keyed by *yield* id, which is the plain id shifted by the asset count.
    # Read back rather than assumed: addr_req fails loudly if the script's id
    # derivation and this offset ever disagree.
    YIELD_VENUE_4=$(addr_req YIELD_VENUE_4)
    YIELD_VENUE_5=$(addr_req YIELD_VENUE_5)
    YIELD_VENUE_6=$(addr_req YIELD_VENUE_6)
    YIELD_VAULT_4=$(addr_req YIELD_VAULT_4)

    log "YIELD_VENUE_4=${YIELD_VENUE_4}"
    debug "YIELD_VENUE_5=${YIELD_VENUE_5} YIELD_VENUE_6=${YIELD_VENUE_6}"
    # The vaults start empty and at a 1:1 share price. `MockERC4626.earn` is
    # how a dev flow moves the index off RAY; nothing here needs it, and a
    # seeded vault would make the first deposit's normalized units depend on
    # deploy-time state.
    debug "YIELD_VAULT_4=${YIELD_VAULT_4} (empty; index starts at RAY)"
}

fund_recipient() {
    step "fund ${FUND_RECIPIENT}"

    log "native ${FUND_NATIVE}"
    cast_send --value "$FUND_NATIVE" "$FUND_RECIPIENT"

    # MockWETH9 has no public mint, so the deployer wraps then transfers.
    log "WETH ${FUND_WETH}"
    cast_send --value "$FUND_WETH" "$WETH" "deposit()"
    cast_send "$WETH" "transfer(address,uint256)" "$FUND_RECIPIENT" "$FUND_WETH"

    # The plain ERC20 mocks do expose `mint(address,uint256)`.
    for _token in "$TOKEN_2" "$TOKEN_3"; do
        log "ERC20 ${FUND_ERC20} from ${_token}"
        cast_send "$_token" "mint(address,uint256)" "$FUND_RECIPIENT" "$FUND_ERC20"
    done
}

# Write the env file that every backend's entrypoint sources. Each service
# overlays these onto its TOML per-chain block (see `apply_env_overlay`) —
# which only works because config/dev/*.toml declare a matching chain id.
# The relayer's `accepted_fee_tokens`, as the JSON its env overlay parses.
#
# Every registered asset is listed: a payer can only pay the fee in the asset
# they are already moving, so one left out is one nothing can transact in.
# `decimals` is the ERC-20's, not the MASP scale — the relayer converts a gas
# quote into token base units and divides by the scale itself.
#
# Every asset quotes in USD because the dev oracle stub serves exactly that
# pair; adding another `quote_symbol` means adding a file under config/oracle.
fee_tokens_json() {
    _token() {
        printf '{"symbol":"%s","address":"%s","decimals":%s,"quote_symbol":"USD"}' \
            "$1" "$2" "$3"
    }
    printf '[%s,%s,%s]' \
        "$(_token WETH "$TOKEN_1" 18)" \
        "$(_token mDAI "$TOKEN_2" 18)" \
        "$(_token mWBTC "$TOKEN_3" 8)"
}

write_env_file() {
    step "write ${OUT_FILE}"
    # Values are single-quoted because this file is *sourced*
    # (`set -a; . addresses.env`): unquoted, the shell interprets whatever is
    # in them, which silently strips the inner quotes of the JSON emitted
    # below. Addresses and URLs contain no single quote, so nothing here can
    # terminate the quoting early.
    _emit() { printf "%s_CHAIN_%s_%s='%s'\n" "$1" "$CHAIN_ID" "$2" "$3"; }

    {
        _emit INGESTER POOL_ADDRESS "$MASP"
        _emit INGESTER RPC_URL "$RPC_URL"

        _emit RELAYER POOL_ADDRESS "$MASP"
        _emit RELAYER RPC_URL "$RPC_URL"
        _emit RELAYER SWAP_WRAPPER_ADDRESS "$SWAP_WRAPPER"
        # Enables `withdrawNative`; without it the relayer leaves
        # native_adapter_address unset and rejects native withdrawals.
        _emit RELAYER NATIVE_ADAPTER_ADDRESS "$NATIVE_ADAPTER"
        # `accepted_fee_tokens` is the one piece of per-chain relayer config
        # carrying ERC-20 addresses, which only exist once this script has run.
        _emit RELAYER ACCEPTED_FEE_TOKENS "$(fee_tokens_json)"

        _emit METAQUOTER RPC_URL "$RPC_URL"
        _emit METAQUOTER UNIV3_QUOTER "$UNIV3_QUOTER"
        _emit METAQUOTER UNIV3_ADAPTER "$UNIV3_ADAPTER"
        # Only take effect because config/dev/metaquoter.toml declares these
        # keys on the 31337 chain; the overlay cannot introduce a chain or a key.
        _emit METAQUOTER UNIV4_QUOTER "$UNIV4_QUOTER"
        _emit METAQUOTER UNIV4_ADAPTER "$UNIV4_ADAPTER"

        # Every raw KEY=0x… pair from the deploy logs, for tests and manual
        # poking (TOKEN_1, VERIFIER, MOCK_SWAP_ROUTER, …).
        cat "$ADDR_FILE"
    } > "$OUT_FILE"

    log "$(wc -l < "$OUT_FILE" | tr -d ' ') vars written"
}

print_summary() {
    step "summary"
    printf '  %-18s %s\n' \
        MASP           "$MASP" \
        SWAP_WRAPPER   "$SWAP_WRAPPER" \
        UNIV3_ADAPTER  "$UNIV3_ADAPTER" \
        UNIV3_QUOTER   "$UNIV3_QUOTER" \
        UNIV4_ADAPTER  "$UNIV4_ADAPTER" \
        UNIV4_QUOTER   "$UNIV4_QUOTER" \
        NATIVE_ADAPTER "$NATIVE_ADAPTER" \
        WRAPPED_NATIVE "$WETH" \
        YIELD_VENUE_4  "$YIELD_VENUE_4" \
        YIELD_VENUE_5  "$YIELD_VENUE_5" \
        YIELD_VENUE_6  "$YIELD_VENUE_6" \
        funded         "$FUND_RECIPIENT" >&2
}

main() {
    preflight
    deploy_core
    deploy_swap
    deploy_yield
    fund_recipient
    write_env_file
    print_summary
}

main "$@"
