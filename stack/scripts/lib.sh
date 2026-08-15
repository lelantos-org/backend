# Shared helpers for the stack scripts. POSIX sh — sourced, never executed.
#
# Sourced by scripts/fetch-circuits.sh (runs on the host, bash) and
# scripts/deploy-contracts.sh (runs inside the foundry container, busybox sh),
# so nothing here may rely on bash builtins.
#
# Env:
#   DEBUG=1     emit debug() detail lines
#   TRACE=1     turn on `set -x` in the sourcing script (very noisy; implies
#               DEBUG since the trace would otherwise hide the detail lines)
#   NO_COLOR=1  disable ANSI colour (also auto-disabled when stderr isn't a TTY)

if [ -t 2 ] && [ -z "${NO_COLOR:-}" ]; then
    _c_dim='\033[2m'; _c_red='\033[1;31m'; _c_yellow='\033[33m'
    _c_green='\033[1;32m'; _c_blue='\033[1;34m'; _c_off='\033[0m'
else
    _c_dim=''; _c_red=''; _c_yellow=''; _c_green=''; _c_blue=''; _c_off=''
fi

# All diagnostics go to stderr so stdout stays usable for real output
# (e.g. `addr MASP` echoing an address into a command substitution).

# Major phase banner.
step() { printf '%b\n== %s %b\n' "$_c_blue" "$*" "$_c_off" >&2; }

# Routine progress.
log() { printf '%b -->%b %s\n' "$_c_green" "$_c_off" "$*" >&2; }

# Detail, only under DEBUG=1.
debug() {
    [ "${DEBUG:-0}" = "1" ] || return 0
    printf '%b     %s%b\n' "$_c_dim" "$*" "$_c_off" >&2
}

warn() { printf '%bwarn:%b %s\n' "$_c_yellow" "$_c_off" "$*" >&2; }

# die <message> [exit-code]
die() {
    printf '%berror:%b %s\n' "$_c_red" "$_c_off" "$1" >&2
    exit "${2:-1}"
}

# require_env VAR...  — abort listing every missing/empty var at once, rather
# than failing on the first one and hiding the rest.
require_env() {
    _missing=''
    for _name in "$@"; do
        eval "_value=\${$_name:-}"
        [ -n "$_value" ] || _missing="$_missing $_name"
    done
    [ -n "$_missing" ] && die "missing required env vars:$_missing"
    return 0
}

# require_cmd CMD...  — same, for executables on PATH.
require_cmd() {
    _missing=''
    for _name in "$@"; do
        command -v "$_name" >/dev/null 2>&1 || _missing="$_missing $_name"
    done
    [ -n "$_missing" ] && die "missing required commands:$_missing"
    return 0
}

# TRACE implies DEBUG, and enables tracing in the sourcing script (`set -x`
# takes effect in the caller because this file is sourced, not executed).
if [ "${TRACE:-0}" = "1" ]; then
    DEBUG=1
    set -x
fi
return 0
