#!/bin/sh
# Print a handle's record from the name registrar.
#
# Runs inside the `deploy` container (foundry image, /addresses mounted, on the
# backend network), as earn.sh does. Invoked by `just name`.
#
# Usage: name.sh <label>
#
# Env (supplied by the compose service):
#   RPC_URL

set -eu
. /scripts/lib.sh

ADDR_FILE=/addresses/addresses.env

LABEL="${1:-}"
[ -n "$LABEL" ] || die "usage: name.sh <label>"
require_env RPC_URL
require_cmd cast
[ -f "$ADDR_FILE" ] || die "$ADDR_FILE not found — has the deploy one-shot run?"

# shellcheck disable=SC1090
. "$ADDR_FILE"

[ -n "${NAME_REGISTRAR:-}" ] || die "NAME_REGISTRAR not in $ADDR_FILE — has the deploy one-shot run?"

# One member per line: value, controller, nonce. The controller is zero when the
# label is not registered.
cast_call "$NAME_REGISTRAR" 'recordOf(string)(string,address,uint64)' "$LABEL" \
    || die "recordOf($LABEL) failed on $NAME_REGISTRAR — is the node up?"
