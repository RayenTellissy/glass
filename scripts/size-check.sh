#!/usr/bin/env bash
# Compares installers against the budgets in size-budget.toml and fails when one is
# over. The kind comes from the file extension: .dmg, .msi, .msix, .AppImage.
#
# Usage: scripts/size-check.sh [--budget FILE] INSTALLER...
#   --budget FILE  budget file (default: size-budget.toml at the repo root)
#
# Exit status: 0 when every installer fits, 1 when any is over budget, 2 on usage
# errors (missing file, unknown kind, kind without a budget).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUDGET_FILE="$REPO_ROOT/size-budget.toml"
FILES=()

usage() {
    sed -n "2,9p" "$0" | sed "s/^# \{0,1\}//"
}

usage_error() {
    echo "size-check: $*" >&2
    usage >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --budget)
            [ $# -ge 2 ] && [ -n "$2" ] && [ "${2#-}" = "$2" ] || usage_error "--budget needs a file"
            BUDGET_FILE="$2"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*) usage_error "unknown option: $1" ;;
        *) FILES+=("$1") ;;
    esac
    shift
done

if [ ${#FILES[@]} -eq 0 ]; then
    echo "size-check: no installers given (see --help)" >&2
    exit 2
fi
[ -f "$BUDGET_FILE" ] || { echo "size-check: budget file not found: $BUDGET_FILE" >&2; exit 2; }

kind_of() {
    case "$1" in
        *.dmg) echo dmg ;;
        *.msix) echo msix ;;
        *.msi) echo msi ;;
        *.AppImage | *.appimage) echo appimage ;;
        *) echo "" ;;
    esac
}

# Reads `kind = bytes` from the [budget] table; comments and underscores are allowed.
budget_of() {
    awk -v key="$1" '
        /^[[:space:]]*\[/ { in_budget = ($0 ~ /^[[:space:]]*\[budget\][[:space:]]*(#.*)?$/); next }
        in_budget {
            line = $0
            sub(/#.*/, "", line)
            if (split(line, kv, "=") != 2) next
            k = kv[1]; v = kv[2]
            gsub(/[[:space:]"]/, "", k); gsub(/[[:space:]_]/, "", v)
            if (k == key && v ~ /^[0-9]+$/) { print v; exit }
        }
    ' "$BUDGET_FILE"
}

mib() {
    awk -v b="$1" 'BEGIN { printf "%.2f", b / 1048576 }'
}

status=0
printf "%-10s %10s %12s %16s  %-5s %s\n" "kind" "size MiB" "budget MiB" "headroom bytes" "ok" "installer"
for file in "${FILES[@]}"; do
    [ -f "$file" ] || { echo "size-check: not a file: $file" >&2; exit 2; }
    kind="$(kind_of "$file")"
    [ -n "$kind" ] || { echo "size-check: unknown installer kind: $file" >&2; exit 2; }
    budget="$(budget_of "$kind")"
    [ -n "$budget" ] || { echo "size-check: no budget for '$kind' in $BUDGET_FILE" >&2; exit 2; }
    size=$(wc -c < "$file" | tr -d "[:space:]")
    headroom=$((budget - size))
    if [ "$size" -gt "$budget" ]; then
        ok="OVER"
        status=1
    else
        ok="yes"
    fi
    printf "%-10s %10s %12s %16s  %-5s %s\n" "$kind" "$(mib "$size")" "$(mib "$budget")" "$headroom" "$ok" "$file"
done

if [ "$status" -ne 0 ]; then
    cat >&2 <<MSG

size-check: at least one installer is over its budget in ${BUDGET_FILE#"$REPO_ROOT"/}.
Shrink it, or raise the budget in that file together with a Decision Log line in
PLAN.md ("- YYYY-MM-DD — <decision>. Reason: <reason>.") and the same entry in
docs/DECISIONS.md, saying what grew and why.
MSG
fi
exit "$status"
