#!/usr/bin/env sh
# One-shot deploy + fund script run by the `deploy` compose service.
#
#   1. forge script DeployTest.s.sol → mocks + MASP on anvil
#   2. fund FUND_RECIPIENT with native ETH, WETH, mDAI, mWBTC
#   3. write /addresses/addresses.env (sourced by every backend's wrapper)
#
# Required env (set in docker-compose.yml):
#   RPC_URL DEPLOYER_KEY FUND_RECIPIENT FUND_NATIVE FUND_WETH FUND_ERC20

set -eu

cd /contracts

# ── 1a. Deploy MASP + verifiers + mock tokens ───────────────────────────
forge script script/DeployTest.s.sol:DeployTest \
    --rpc-url "$RPC_URL" \
    --private-key "$DEPLOYER_KEY" \
    --broadcast \
    --disable-code-size-limit 2>&1 | tee /tmp/forge.log

# Parse first pass — need MASP + PERMIT2 + TOKEN_1..3 as env for swap step.
grep -oE '[A-Z_0-9]+=0x[0-9a-fA-F]{40}' /tmp/forge.log | sort -u > /tmp/raw.env
get() { awk -F= -v k="$1" '$1==k{print $2}' /tmp/raw.env; }

MASP=$(get MASP)
PERMIT2=$(get PERMIT2)
TOKEN_1=$(get TOKEN_1)
TOKEN_2=$(get TOKEN_2)
TOKEN_3=$(get TOKEN_3)
WETH=$(get WRAPPED_NATIVE)
[ -n "$MASP" ]    || { echo "no MASP in forge output" >&2; exit 1; }
[ -n "$PERMIT2" ] || { echo "no PERMIT2 in forge output" >&2; exit 1; }
[ -n "$TOKEN_1" ] || { echo "no TOKEN_1 in forge output" >&2; exit 1; }

# ── 1b. Deploy swap stack (UniV3Adapter + SwapWrapper + mocks) ──────────
MASP="$MASP" PERMIT2="$PERMIT2" \
TOKEN_1="$TOKEN_1" TOKEN_2="$TOKEN_2" TOKEN_3="$TOKEN_3" \
forge script script/DeployTestSwap.s.sol:DeployTestSwap \
    --rpc-url "$RPC_URL" \
    --private-key "$DEPLOYER_KEY" \
    --broadcast \
    --disable-code-size-limit 2>&1 | tee -a /tmp/forge.log

# Re-parse with swap-stack additions.
grep -oE '[A-Z_0-9]+=0x[0-9a-fA-F]{40}' /tmp/forge.log | sort -u > /tmp/raw.env

SWAP_WRAPPER=$(get SWAP_WRAPPER)
UNIV3_ADAPTER=$(get UNIV3_ADAPTER)
UNIV3_QUOTER=$(get UNIV3_QUOTER)
MOCK_SWAP_ROUTER=$(get MOCK_SWAP_ROUTER)
[ -n "$SWAP_WRAPPER" ] || { echo "no SWAP_WRAPPER in forge output" >&2; exit 1; }

# ── 3. Fund recipient ───────────────────────────────────────────────────
send() {
    cast send --rpc-url "$RPC_URL" --private-key "$DEPLOYER_KEY" "$@" >/dev/null
}

# Native ETH: direct value transfer.
send --value "$FUND_NATIVE" "$FUND_RECIPIENT"
# WETH: deployer wraps, then transfers (MockWETH9 has no public mint).
send --value "$FUND_WETH" "$WETH" "deposit()"
send "$WETH" "transfer(address,uint256)" "$FUND_RECIPIENT" "$FUND_WETH"
# Plain ERC20 mocks expose `mint(address,uint256)`.
for T in "$TOKEN_2" "$TOKEN_3"; do
    send "$T" "mint(address,uint256)" "$FUND_RECIPIENT" "$FUND_ERC20"
done
echo "funded $FUND_RECIPIENT: native=$FUND_NATIVE weth=$FUND_WETH erc20s=[$TOKEN_2,$TOKEN_3]"

# Swap-stack rates are seeded inside DeployTest._deploySwap (see
# `setRate` calls there). MockSwapRouter02 mints `tokenOut` to the
# recipient on demand — no inventory seeding, no `setNextOut` needed
# for the dev flow. e2e tests that need a fixed override still use
# `setNextOut` directly via the swap harness.
echo "swap stack: router=$MOCK_SWAP_ROUTER (rate-seeded via DeployTest)"

# ── 4. Emit env file consumed by backend wrapper entrypoints ────────────
# Metaquoter overlay: the .toml has a placeholder anvil chain (id 31337)
# whose UNIV3_QUOTER + UNIV3_ADAPTER are filled in here from the deploy
# log. Same RPC_URL the deploy script used — anvil is reachable as
# `http://anvil:8545` on the compose `backend` network.
{
    echo "INGESTER_CHAIN_31337_POOL_ADDRESS=$MASP"
    echo "INGESTER_CHAIN_31337_RPC_URL=$RPC_URL"
    echo "RELAYER_CHAIN_31337_POOL_ADDRESS=$MASP"
    echo "RELAYER_CHAIN_31337_RPC_URL=$RPC_URL"
    echo "RELAYER_CHAIN_31337_SWAP_WRAPPER_ADDRESS=$SWAP_WRAPPER"
    echo "METAQUOTER_CHAIN_31337_RPC_URL=$RPC_URL"
    echo "METAQUOTER_CHAIN_31337_UNIV3_QUOTER=$UNIV3_QUOTER"
    echo "METAQUOTER_CHAIN_31337_UNIV3_ADAPTER=$UNIV3_ADAPTER"
    cat /tmp/raw.env
} > /addresses/addresses.env
echo "wrote MASP=$MASP SWAP_WRAPPER=$SWAP_WRAPPER UNIV3_ADAPTER=$UNIV3_ADAPTER"
