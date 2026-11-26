#!/usr/bin/env bash
# Runs every AGENTS.md section 3 gate that applies to what exists in the tree and
# prints a short PASS/FAIL table plus the tail of each command's real output.
# Usage: scripts/verify.sh [log-file]   (default: target/verify.log)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="${1:-$ROOT/target/verify.log}"
mkdir -p "$(dirname "$LOG")"
: >"$LOG"
FAILED=0

run() {
    local name="$1" dir="$2"
    shift 2
    local out status
    out="$(mktemp)"
    if (cd "$dir" && "$@") >"$out" 2>&1; then status=PASS; else status=FAIL; FAILED=1; fi
    {
        echo "=== [$status] $name"
        echo "\$ (cd ${dir#"$ROOT"/} && $*)"
        tail -n 12 "$out"
        echo
    } >>"$LOG"
    printf '%-6s %s\n' "$status" "$name"
    rm -f "$out"
}

run "cargo fmt --check" "$ROOT" cargo fmt --check
run "cargo clippy --workspace --all-targets -- -D warnings" "$ROOT" \
    cargo clippy --workspace --all-targets -- -D warnings
run "cargo test --workspace" "$ROOT" cargo test --workspace
run "uv run ruff check ." "$ROOT/harness" uv run ruff check .
run "uv run pytest" "$ROOT/harness" uv run pytest
if [ -d "$ROOT/companion-android" ]; then
    if [ -z "${JAVA_HOME:-}" ] && [ -d /opt/homebrew/opt/openjdk@21 ]; then
        export JAVA_HOME=/opt/homebrew/opt/openjdk@21
    fi
    run "./gradlew test assembleDebug" "$ROOT/companion-android" ./gradlew test assembleDebug
fi
if [ -d "$ROOT/os-image" ]; then
    # = os-image/**/*.sh (macOS ships bash 3.2, which has no globstar).
    scripts=()
    while IFS= read -r f; do scripts+=("$f"); done < <(find "$ROOT/os-image" "$ROOT/scripts" -name '*.sh' | sort)
    run "shellcheck os-image/**/*.sh scripts/*.sh" "$ROOT" shellcheck "${scripts[@]}"
fi
echo "log: $LOG"
exit "$FAILED"
