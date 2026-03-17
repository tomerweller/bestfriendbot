#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# submit_every_ledger.sh
#
# Determines the maximum safe interval between transaction submissions to
# guarantee an account has a transaction in every consecutive ledger on
# Stellar testnet.
# =============================================================================

HORIZON="https://horizon-testnet.stellar.org"
TXNS_PER_TRIAL=10
BINARY_SEARCH_PRECISION="0.25"  # seconds — stop binary search when range < this

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[OK]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; }

# =============================================================================
# Phase 1: Setup
# =============================================================================

phase1_setup() {
    info "Phase 1: Setup"
    echo "---"

    # Check stellar CLI
    if ! command -v stellar &>/dev/null; then
        fail "stellar CLI not found. Install it first."
        exit 1
    fi
    ok "stellar CLI found: $(stellar --version 2>&1 | head -1)"

    # Generate identities (--overwrite in case they exist from a previous run)
    info "Generating sender identity..."
    stellar keys generate sender --network testnet --overwrite 2>/dev/null || true
    SENDER_PUBKEY=$(stellar keys address sender)
    ok "Sender: $SENDER_PUBKEY"

    info "Generating receiver identity..."
    stellar keys generate receiver --network testnet --overwrite 2>/dev/null || true
    RECEIVER_PUBKEY=$(stellar keys address receiver)
    ok "Receiver: $RECEIVER_PUBKEY"

    # Fund both accounts via friendbot
    info "Funding sender..."
    stellar keys fund sender --network testnet 2>/dev/null
    ok "Sender funded"

    info "Funding receiver..."
    stellar keys fund receiver --network testnet 2>/dev/null
    ok "Receiver funded"

    echo ""
}

# =============================================================================
# Phase 2: Measure Ledger Timing
# =============================================================================

phase2_measure_ledgers() {
    info "Phase 2: Measure Ledger Timing"
    echo "---"

    info "Fetching last 200 ledgers from Horizon..."
    local ledgers_json
    ledgers_json=$(curl -s "${HORIZON}/ledgers?order=desc&limit=200")

    # Extract close times and compute gaps
    # Each ledger has closed_at (ISO 8601). We compute successive differences.
    local close_times
    close_times=$(echo "$ledgers_json" | python3 -c "
import json, sys
from datetime import datetime

data = json.load(sys.stdin)
records = data['_embedded']['records']
times = []
for r in records:
    t = datetime.fromisoformat(r['closed_at'].replace('Z', '+00:00'))
    times.append(t)

# times are desc order, reverse for chronological
times.reverse()

gaps = []
for i in range(1, len(times)):
    gap = (times[i] - times[i-1]).total_seconds()
    gaps.append(gap)

if not gaps:
    print('NO_DATA')
    sys.exit(0)

min_gap = min(gaps)
max_gap = max(gaps)
avg_gap = sum(gaps) / len(gaps)

# Distribution
from collections import Counter
dist = Counter(gaps)

print(f'{min_gap}|{max_gap}|{avg_gap}|{len(gaps)}')
for g in sorted(dist.keys()):
    print(f'DIST|{g}|{dist[g]}')
")

    if [[ "$close_times" == "NO_DATA" ]]; then
        fail "Could not fetch ledger data"
        exit 1
    fi

    local summary
    summary=$(echo "$close_times" | head -1)
    MIN_LEDGER_GAP=$(echo "$summary" | cut -d'|' -f1)
    MAX_LEDGER_GAP=$(echo "$summary" | cut -d'|' -f2)
    AVG_LEDGER_GAP=$(echo "$summary" | cut -d'|' -f3)
    local count
    count=$(echo "$summary" | cut -d'|' -f4)

    ok "Sampled $count ledger gaps"
    echo "   Min gap:  ${MIN_LEDGER_GAP}s"
    echo "   Max gap:  ${MAX_LEDGER_GAP}s"
    echo "   Avg gap:  ${AVG_LEDGER_GAP}s"

    echo "   Distribution:"
    echo "$close_times" | grep "^DIST|" | while IFS='|' read -r _ gap cnt; do
        echo "     ${gap}s: ${cnt} occurrences"
    done

    echo ""
}

# =============================================================================
# Phase 3: Find Maximum Safe Submission Interval
# =============================================================================

# Submit N transactions at a fixed interval, return space-separated ledger seqs
run_trial() {
    local interval="$1"
    local n="$2"
    local label="$3"

    info "Trial '$label': submitting $n txns at ${interval}s intervals..." >&2

    local tx_hashes=()
    local start_time end_time elapsed

    for ((i = 1; i <= n; i++)); do
        start_time=$(python3 -c "import time; print(time.time())")

        # Submit a 1-stroop payment
        local result
        result=$(stellar tx new payment \
            --source sender \
            --destination "$RECEIVER_PUBKEY" \
            --asset native \
            --amount 1 \
            --network testnet \
            2>&1) || {
            warn "  tx $i failed: $result" >&2
            tx_hashes+=("FAILED")
            continue
        }

        # Extract the tx hash from output
        local tx_hash
        tx_hash=$(echo "$result" | grep -oE '[a-f0-9]{64}' | head -1 || echo "")
        if [[ -z "$tx_hash" ]]; then
            tx_hash=$(echo "$result" | python3 -c "import json,sys; print(json.load(sys.stdin).get('hash',''))" 2>/dev/null || echo "")
        fi

        end_time=$(python3 -c "import time; print(time.time())")
        elapsed=$(python3 -c "print(round($end_time - $start_time, 3))")

        if [[ -n "$tx_hash" && "$tx_hash" != "" ]]; then
            tx_hashes+=("$tx_hash")
            echo "  tx $i/$n: hash=${tx_hash:0:12}... (${elapsed}s)" >&2
        else
            warn "  tx $i/$n: submitted but couldn't extract hash (${elapsed}s)" >&2
            warn "  output: ${result:0:200}" >&2
            tx_hashes+=("UNKNOWN")
        fi

        # Sleep for the remaining interval (subtract elapsed time)
        if ((i < n)); then
            local sleep_time
            sleep_time=$(python3 -c "print(max(0, $interval - $elapsed))")
            sleep "$sleep_time"
        fi
    done

    # Wait a moment for the last tx to finalize
    sleep 3

    # Query account transactions to find ledger sequences
    info "  Checking ledger sequences..." >&2
    local txns_json
    txns_json=$(curl -s "${HORIZON}/accounts/${SENDER_PUBKEY}/transactions?order=desc&limit=100")

    # Map tx hashes to ledger sequences
    echo "$txns_json" | python3 -c "
import json, sys

data = json.load(sys.stdin)
records = data.get('_embedded', {}).get('records', [])

# Build hash -> ledger map
h2l = {}
for r in records:
    h2l[r['hash']] = r['ledger']

# Print ledger for each requested hash
hashes = sys.argv[1:]
for h in hashes:
    if h in ('FAILED', 'UNKNOWN'):
        print(h)
    elif h in h2l:
        print(h2l[h])
    else:
        print('NOT_FOUND')
" "${tx_hashes[@]}"
}

# Check if ledger sequences are all consecutive (no gaps)
check_consecutive() {
    local seqs=("$@")
    local valid_seqs=()

    for s in "${seqs[@]}"; do
        if [[ "$s" =~ ^[0-9]+$ ]]; then
            valid_seqs+=("$s")
        fi
    done

    if [[ ${#valid_seqs[@]} -lt 2 ]]; then
        echo "INSUFFICIENT_DATA"
        return
    fi

    # Sort numerically
    local sorted
    sorted=($(printf '%s\n' "${valid_seqs[@]}" | sort -n))

    local gaps=0
    local max_gap=0
    local details=""
    for ((i = 1; i < ${#sorted[@]}; i++)); do
        local diff=$(( sorted[i] - sorted[i-1] ))
        if ((diff > 1)); then
            gaps=$((gaps + diff - 1))
            if ((diff - 1 > max_gap)); then
                max_gap=$((diff - 1))
            fi
            details+=" gap(${sorted[i-1]}->${sorted[i]},missed=$((diff-1)))"
        fi
    done

    if ((gaps == 0)); then
        local last_idx=$(( ${#sorted[@]} - 1 ))
        echo "CONSECUTIVE|${sorted[0]}|${sorted[$last_idx]}|${#sorted[@]}"
    else
        echo "GAPS|${gaps}|${max_gap}|${details}"
    fi
}

phase3_find_interval() {
    info "Phase 3: Find Maximum Safe Submission Interval"
    echo "---"

    # First, measure CLI overhead for a single transaction
    info "Measuring CLI overhead (build+sign+submit latency)..."
    local t0 t1 cli_overhead
    t0=$(python3 -c "import time; print(time.time())")
    stellar tx new payment \
        --source sender \
        --destination "$RECEIVER_PUBKEY" \
        --asset native \
        --amount 1 \
        --network testnet \
        2>/dev/null || true
    t1=$(python3 -c "import time; print(time.time())")
    cli_overhead=$(python3 -c "print(round($t1 - $t0, 3))")
    ok "CLI overhead: ${cli_overhead}s"

    # Theoretical max = min_ledger_gap - cli_overhead
    local theoretical_max
    theoretical_max=$(python3 -c "print(round($MIN_LEDGER_GAP - $cli_overhead, 3))")
    info "Theoretical max interval: ${MIN_LEDGER_GAP}s (min ledger gap) - ${cli_overhead}s (CLI overhead) = ${theoretical_max}s"
    echo ""

    # Binary search for the maximum safe interval
    local lo="0.5"
    local hi="$MIN_LEDGER_GAP"
    local best_passing="0"
    local trial_num=0

    TRIAL_LOG=$(mktemp)
    trap "rm -f '$TRIAL_LOG'" EXIT

    run_and_check_trial() {
        local intv="$1"
        local lbl="$2"
        local trial_output
        trial_output=$(run_trial "$intv" "$TXNS_PER_TRIAL" "$lbl")
        local trial_seqs=()
        while IFS= read -r line; do
            trial_seqs+=("$line")
        done <<< "$trial_output"

        local trial_check
        trial_check=$(check_consecutive "${trial_seqs[@]}")
        local trial_status
        trial_status=$(echo "$trial_check" | cut -d'|' -f1)

        echo "${intv}|${trial_status}|${trial_check}" >> "$TRIAL_LOG"

        if [[ "$trial_status" == "CONSECUTIVE" ]]; then
            ok "  $lbl: PASS — all consecutive"
            return 0
        else
            warn "  $lbl: FAIL — $trial_check"
            return 1
        fi
    }

    # First run a conservative baseline to make sure things work
    info "Running baseline trial at 1.0s interval..."
    if run_and_check_trial "1.0" "baseline-1.0s"; then
        best_passing="1.0"
    fi
    echo ""

    # Now binary search
    while true; do
        local range
        range=$(python3 -c "print($hi - $lo)")
        local done_searching
        done_searching=$(python3 -c "print('yes' if $range < $BINARY_SEARCH_PRECISION else 'no')")
        if [[ "$done_searching" == "yes" ]]; then
            break
        fi

        local mid
        mid=$(python3 -c "print(round(($lo + $hi) / 2, 2))")
        trial_num=$((trial_num + 1))

        info "Binary search: trying ${mid}s (range: ${lo}s - ${hi}s)"
        if run_and_check_trial "$mid" "trial-${trial_num}-${mid}s"; then
            best_passing="$mid"
            lo="$mid"
        else
            hi="$mid"
        fi
        echo ""
    done

    MAX_SAFE_INTERVAL="$best_passing"
}

# =============================================================================
# Phase 4: Report
# =============================================================================

phase4_report() {
    echo ""
    echo "============================================================"
    echo -e "${CYAN}RESULTS SUMMARY${NC}"
    echo "============================================================"
    echo ""
    echo "Ledger Timing (sampled from last 200 ledgers):"
    echo "  Min gap:  ${MIN_LEDGER_GAP}s"
    echo "  Max gap:  ${MAX_LEDGER_GAP}s"
    echo "  Avg gap:  ${AVG_LEDGER_GAP}s"
    echo ""
    echo "Trial Results:"
    sort -t'|' -k1 -n "$TRIAL_LOG" | while IFS='|' read -r interval status details; do
        if [[ "$status" == "CONSECUTIVE" ]]; then
            echo -e "  ${interval}s: ${GREEN}PASS${NC} (all txns in consecutive ledgers)"
        else
            echo -e "  ${interval}s: ${RED}FAIL${NC} — ${details}"
        fi
    done
    echo ""
    echo "============================================================"
    echo -e "${GREEN}Maximum safe submission interval: ${MAX_SAFE_INTERVAL}s${NC}"
    echo "============================================================"
    echo ""
    echo "To guarantee a transaction in every consecutive ledger,"
    echo "submit transactions no more than ${MAX_SAFE_INTERVAL}s apart."
    echo ""
}

# =============================================================================
# Main
# =============================================================================

main() {
    echo ""
    echo "============================================================"
    echo "  Stellar Testnet: Every-Ledger Transaction Interval Finder"
    echo "============================================================"
    echo ""

    phase1_setup
    phase2_measure_ledgers
    phase3_find_interval
    phase4_report
}

main "$@"
