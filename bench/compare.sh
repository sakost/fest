#!/usr/bin/env bash
# End-to-end comparison: fest vs cosmic-ray on a real Python project.
#
# Runs both tools to completion, captures timing and kill/survived counts,
# and prints a side-by-side summary table.
#
# Usage:
#   bash bench/compare.sh [TARGET_DIR]
#
# Arguments:
#   TARGET_DIR   Path to the Python project (default: ../flask)
#
# Environment variables:
#   VENV           Path to venv bin directory (default: TARGET_DIR/.venv313/bin)
#   FEST_TIMEOUT   Per-mutant timeout for fest in seconds (default: 30)
#   CR_TIMEOUT     Per-mutant timeout for cosmic-ray in seconds (default: 30)
#   MAX_TIME       Max wall-clock seconds per tool; 0 = unlimited (default: 0)
#   FEST_BACKEND   fest backend: "plugin" or "subprocess" (default: plugin)
#   FEST_SOURCE    Source glob for fest (default: src/flask/**/*.py)
#   CR_CONFIG      Path to cosmic-ray.toml (default: TARGET_DIR/cosmic-ray.toml)
#
# Prerequisites:
#   - fest binary built: cargo build --release
#   - cosmic-ray installed in VENV
#   - Target project checked out at a working tag (e.g. Flask 3.1.3)
#   - cosmic-ray.toml configured in TARGET_DIR

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DIR="$(cd "${1:-$PROJECT_DIR/../fastapi}" && pwd)"
VENV="${VENV:-$TARGET_DIR/.venv/bin}"
FEST_BIN="$PROJECT_DIR/target/release/fest"
FEST_TIMEOUT="${FEST_TIMEOUT:-30}"
CR_TIMEOUT="${CR_TIMEOUT:-30}"
MAX_TIME="${MAX_TIME:-0}"
FEST_BACKEND="${FEST_BACKEND:-plugin}"
FEST_SOURCE="${FEST_SOURCE:-fastapi/**/*.py}"
FEST_EXCLUDE="${FEST_EXCLUDE:-}"
FEST_COVERAGE_FROM="${FEST_COVERAGE_FROM:-}"
FEST_TEST_ARGS="${FEST_TEST_ARGS:-}"
TEST_EXTRA_ARGS="${TEST_EXTRA_ARGS:-}"
CR_CONFIG="${CR_CONFIG:-$TARGET_DIR/cosmic-ray.toml}"
CR_SESSION="$TARGET_DIR/.cosmic-ray-bench.sqlite"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

info()  { echo -e "${CYAN}[info]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[warn]${RESET} $*"; }
ok()    { echo -e "${GREEN}[ ok ]${RESET} $*"; }
fail()  { echo -e "${RED}[fail]${RESET} $*"; }

# --------------------------------------------------------------------------
# fest
# --------------------------------------------------------------------------

run_fest() {
    info "Building fest (release)..."
    (cd "$PROJECT_DIR" && cargo build --release --quiet 2>&1) || {
        fail "cargo build failed"; return 1
    }

    if [ ! -x "$FEST_BIN" ]; then
        fail "fest binary not found at $FEST_BIN"
        return 1
    fi

    info "Running fest (backend=$FEST_BACKEND)..."
    cd "$TARGET_DIR"

    local tmpout
    tmpout=$(mktemp)

    local start elapsed exit_code=0
    start=$(date +%s%N)

    local timeout_args=()
    if [ "$MAX_TIME" -gt 0 ]; then
        timeout_args=(timeout "$MAX_TIME")
    fi

    local fest_args=(
        --source "$FEST_SOURCE"
        --backend "$FEST_BACKEND"
        --timeout "$FEST_TIMEOUT"
        --progress plain
        --reset
    )
    if [ -n "$FEST_EXCLUDE" ]; then
        fest_args+=(--exclude "$FEST_EXCLUDE")
    fi
    if [ -n "$FEST_COVERAGE_FROM" ]; then
        fest_args+=(--coverage-from "$FEST_COVERAGE_FROM")
    fi

    "${timeout_args[@]}" "$FEST_BIN" run "${fest_args[@]}" \
        > "$tmpout" 2>&1 || exit_code=$?

    local end
    end=$(date +%s%N)
    elapsed=$(( (end - start) / 1000000 ))

    if [ "$exit_code" -eq 124 ]; then
        warn "fest timed out after ${MAX_TIME}s"
        FEST_TIME="TIMEOUT"
    else
        FEST_TIME="$elapsed"
    fi

    # Parse fest output for stats
    FEST_OUTPUT=$(cat "$tmpout")
    FEST_TOTAL=$(echo "$FEST_OUTPUT" | grep -oP 'Mutants generated:\s*\K[0-9]+' || echo "?")
    FEST_KILLED=$(echo "$FEST_OUTPUT" | grep -oP 'Killed:\s*\K[0-9]+' | head -1 || echo "?")
    FEST_SURVIVED=$(echo "$FEST_OUTPUT" | grep -oP 'Survived:\s*\K[0-9]+' | head -1 || echo "?")
    FEST_ERRORS=$(echo "$FEST_OUTPUT" | grep -oP 'Errors:\s*\K[0-9]+' | head -1 || echo "?")
    FEST_SCORE=$(echo "$FEST_OUTPUT" | grep -oP 'Mutation Score:\s*\K[0-9.]+%' || echo "?")

    rm -f "$tmpout"

    if [ "$FEST_TIME" != "TIMEOUT" ]; then
        ok "fest completed in $(format_time "$FEST_TIME")"
    fi
}

# --------------------------------------------------------------------------
# cosmic-ray
# --------------------------------------------------------------------------

run_cosmic_ray() {
    if ! "$VENV/cosmic-ray" --version > /dev/null 2>&1; then
        fail "cosmic-ray not found in $VENV"
        return 1
    fi

    if [ ! -f "$CR_CONFIG" ]; then
        fail "cosmic-ray config not found at $CR_CONFIG"
        return 1
    fi

    info "Running cosmic-ray (init + exec)..."
    cd "$TARGET_DIR"

    rm -f "$CR_SESSION" 2>/dev/null || true

    local start elapsed end exit_code=0
    start=$(date +%s%N)

    # Init phase
    "$VENV/cosmic-ray" init "$CR_CONFIG" "$CR_SESSION" 2>&1 || {
        fail "cosmic-ray init failed"; return 1
    }

    local timeout_args=()
    if [ "$MAX_TIME" -gt 0 ]; then
        # Subtract time already spent on init
        end=$(date +%s%N)
        local init_secs=$(( (end - start) / 1000000000 ))
        local remaining=$(( MAX_TIME - init_secs ))
        if [ "$remaining" -le 0 ]; then
            warn "cosmic-ray timed out during init"
            CR_TIME="TIMEOUT"
            return 0
        fi
        timeout_args=(timeout "$remaining")
    fi

    # Exec phase
    "${timeout_args[@]}" "$VENV/cosmic-ray" exec "$CR_CONFIG" "$CR_SESSION" 2>&1 || exit_code=$?

    end=$(date +%s%N)
    elapsed=$(( (end - start) / 1000000 ))

    if [ "$exit_code" -eq 124 ]; then
        warn "cosmic-ray timed out after ${MAX_TIME}s"
        CR_TIME="TIMEOUT"
    else
        CR_TIME="$elapsed"
    fi

    # Extract results from SQLite session
    CR_TOTAL=$("$VENV/python" -c "
import sqlite3
conn = sqlite3.connect('$CR_SESSION')
cur = conn.cursor()
cur.execute('SELECT COUNT(*) FROM work_items')
print(cur.fetchone()[0])
conn.close()
" 2>/dev/null || echo "?")

    CR_KILLED=$("$VENV/python" -c "
import sqlite3
conn = sqlite3.connect('$CR_SESSION')
cur = conn.cursor()
cur.execute(\"SELECT COUNT(*) FROM work_results WHERE worker_outcome = 'NORMAL' AND test_outcome = 'KILLED'\")
print(cur.fetchone()[0])
conn.close()
" 2>/dev/null || echo "?")

    CR_SURVIVED=$("$VENV/python" -c "
import sqlite3
conn = sqlite3.connect('$CR_SESSION')
cur = conn.cursor()
cur.execute(\"SELECT COUNT(*) FROM work_results WHERE worker_outcome = 'NORMAL' AND test_outcome = 'SURVIVED'\")
print(cur.fetchone()[0])
conn.close()
" 2>/dev/null || echo "?")

    CR_ERRORS=$("$VENV/python" -c "
import sqlite3
conn = sqlite3.connect('$CR_SESSION')
cur = conn.cursor()
cur.execute(\"SELECT COUNT(*) FROM work_results WHERE worker_outcome != 'NORMAL'\")
print(cur.fetchone()[0])
conn.close()
" 2>/dev/null || echo "?")

    if [ "$CR_KILLED" != "?" ] && [ "$CR_TOTAL" != "?" ] && [ "$CR_TOTAL" -gt 0 ]; then
        CR_SCORE=$("$VENV/python" -c "
killed=$CR_KILLED; total=$CR_TOTAL
if total > 0: print(f'{killed/total*100:.1f}%')
else: print('N/A')
" 2>/dev/null || echo "?")
    else
        CR_SCORE="?"
    fi

    rm -f "$CR_SESSION" 2>/dev/null || true

    if [ "$CR_TIME" != "TIMEOUT" ]; then
        ok "cosmic-ray completed in $(format_time "$CR_TIME")"
    fi
}

# --------------------------------------------------------------------------
# Formatting helpers
# --------------------------------------------------------------------------

format_time() {
    local ms="$1"
    if [ "$ms" = "TIMEOUT" ]; then
        echo "TIMEOUT"
        return
    fi
    if [ "$ms" -ge 60000 ]; then
        local mins=$(( ms / 60000 ))
        local secs=$(( (ms % 60000) / 1000 ))
        printf "%dm %ds" "$mins" "$secs"
    elif [ "$ms" -ge 1000 ]; then
        local secs=$(( ms / 1000 ))
        local frac=$(( (ms % 1000) / 100 ))
        printf "%d.%ds" "$secs" "$frac"
    else
        printf "%dms" "$ms"
    fi
}

speedup() {
    local slow="$1"
    local fast="$2"
    if [ "$slow" = "TIMEOUT" ] || [ "$fast" = "TIMEOUT" ]; then
        echo "N/A"
        return
    fi
    if [ "$fast" -eq 0 ]; then
        echo "inf"
        return
    fi
    "$VENV/python" -c "print(f'{$slow / $fast:.1f}x')" 2>/dev/null || echo "?"
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------

echo ""
echo -e "${BOLD}=== Mutation Testing Benchmark ===${RESET}"
echo -e "${DIM}fest vs cosmic-ray${RESET}"
echo ""
echo -e "  Target:       $TARGET_DIR"
echo -e "  Venv:         $VENV"
echo -e "  fest backend: $FEST_BACKEND"
echo -e "  fest source:  $FEST_SOURCE"
[ -n "$FEST_EXCLUDE" ] && echo -e "  fest exclude: $FEST_EXCLUDE"
[ -n "$FEST_COVERAGE_FROM" ] && echo -e "  fest cov:     $FEST_COVERAGE_FROM"
echo -e "  CR config:    $CR_CONFIG"
echo -e "  Per-mutant timeout: fest=${FEST_TIMEOUT}s, CR=${CR_TIMEOUT}s"
if [ "$MAX_TIME" -gt 0 ]; then
    echo -e "  Max wall time: ${MAX_TIME}s per tool"
else
    echo -e "  Max wall time: unlimited"
fi
echo ""

# Verify tests pass first
info "Verifying tests pass..."
cd "$TARGET_DIR"
if "$VENV/python" -m pytest tests/ -x -q --no-header $TEST_EXTRA_ARGS > /dev/null 2>&1; then
    ok "Tests pass"
else
    fail "Tests fail — fix the target project first"
    exit 1
fi
echo ""

# Initialize result variables
FEST_TIME="" FEST_TOTAL="" FEST_KILLED="" FEST_SURVIVED="" FEST_ERRORS="" FEST_SCORE=""
CR_TIME="" CR_TOTAL="" CR_KILLED="" CR_SURVIVED="" CR_ERRORS="" CR_SCORE=""

# Run tools
run_fest
echo ""
run_cosmic_ray
echo ""

# --------------------------------------------------------------------------
# Summary table
# --------------------------------------------------------------------------

echo -e "${BOLD}=== Results ===${RESET}"
echo ""
printf "  ${BOLD}%-20s %12s %12s${RESET}\n" "Metric" "fest" "cosmic-ray"
printf "  %-20s %12s %12s\n"               "------" "----" "----------"
printf "  %-20s %12s %12s\n" "Total mutants"  "$FEST_TOTAL"    "$CR_TOTAL"
printf "  %-20s %12s %12s\n" "Killed"         "$FEST_KILLED"   "$CR_KILLED"
printf "  %-20s %12s %12s\n" "Survived"       "$FEST_SURVIVED" "$CR_SURVIVED"
printf "  %-20s %12s %12s\n" "Errors"         "$FEST_ERRORS"   "$CR_ERRORS"
printf "  %-20s %12s %12s\n" "Kill rate"      "$FEST_SCORE"    "$CR_SCORE"

fest_display=$(format_time "$FEST_TIME")
cr_display=$(format_time "$CR_TIME")
printf "  %-20s %12s %12s\n" "Wall time" "$fest_display" "$cr_display"

if [ "$FEST_TIME" != "TIMEOUT" ] && [ "$CR_TIME" != "TIMEOUT" ] && \
   [ -n "$FEST_TIME" ] && [ -n "$CR_TIME" ]; then
    ratio=$(speedup "$CR_TIME" "$FEST_TIME")
    echo ""
    echo -e "  ${GREEN}fest is ${BOLD}${ratio}${RESET}${GREEN} faster than cosmic-ray${RESET}"
fi

echo ""
echo -e "${BOLD}Done.${RESET}"
