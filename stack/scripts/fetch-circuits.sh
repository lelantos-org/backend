#!/usr/bin/env bash
# Fetch tree_update_batch circuit artifacts (wasm + r1cs + zkey) from the
# @lelantos-org/circuits GitHub release. Output → ./circuits/, mounted into
# the relayer container at /circuits:ro via docker-compose.
#
# Env:
#   CIRCUITS_VERSION   release tag (e.g. v0.5.0). Required.
#   GH_TOKEN           optional; falls back to `gh auth` if unset.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STACK_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEST="${STACK_DIR}/circuits"
REPO="lelantos-org/circuits"

VERSION="${CIRCUITS_VERSION:-}"
if [ -z "$VERSION" ]; then
    echo "ERROR: CIRCUITS_VERSION not set (e.g. v0.5.0)" >&2
    exit 1
fi

# Skip if cached version matches.
if [ -f "${DEST}/.version" ] && [ "$(cat "${DEST}/.version")" = "$VERSION" ]; then
    echo "circuits ${VERSION} already present in ${DEST}"
    exit 0
fi

command -v gh >/dev/null || { echo "ERROR: gh CLI required" >&2; exit 1; }

mkdir -p "$DEST"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "==> Downloading circuits ${VERSION} from ${REPO}"
gh release download "$VERSION" \
    --repo "$REPO" \
    --pattern 'tree_update_batch.wasm' \
    --pattern 'tree_update_batch.r1cs' \
    --pattern 'tree_update_batch_final.zkey' \
    --dir "$TMP" \
    --clobber

for f in tree_update_batch.wasm tree_update_batch.r1cs tree_update_batch_final.zkey; do
    [ -f "${TMP}/${f}" ] || { echo "ERROR: missing asset ${f} in release ${VERSION}" >&2; exit 1; }
    mv "${TMP}/${f}" "${DEST}/${f}"
done

echo "$VERSION" > "${DEST}/.version"
echo "==> Done. Artifacts in ${DEST}"
ls -lh "${DEST}"
