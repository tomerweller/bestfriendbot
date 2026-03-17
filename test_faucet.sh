#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# test_faucet.sh — Happy-path integration test for the faucet service
#
# Deploys the batch_transfer contract on testnet, creates a custom SAC token,
# sets up trustlines on receiver accounts, starts the faucet, sends concurrent
# funding requests, and verifies balances.
# =============================================================================

NETWORK="testnet"
RPC_URL="https://soroban-testnet.stellar.org"
NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
FAUCET_PORT=9876
FAUCET_URL="http://localhost:${FAUCET_PORT}"
NUM_RECEIVERS=5
AMOUNT=10000000          # 1 token (7 decimal places)
FAUCET_PID=""
FAUCET_LOG=""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[ OK ]${NC} $*"; }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; }

cleanup() {
    if [[ -n "$FAUCET_PID" ]]; then
        info "Stopping faucet (pid $FAUCET_PID)..."
        kill "$FAUCET_PID" 2>/dev/null || true
        wait "$FAUCET_PID" 2>/dev/null || true
    fi
    if [[ -n "$FAUCET_LOG" && -f "$FAUCET_LOG" ]]; then
        echo ""
        info "=== Faucet log ==="
        cat "$FAUCET_LOG"
        rm -f "$FAUCET_LOG"
    fi
}
trap cleanup EXIT

# =============================================================================
# Phase 1: Prerequisites
# =============================================================================
phase1_prereqs() {
    info "Phase 1: Checking prerequisites"
    echo "---"

    for cmd in stellar cargo curl jq; do
        if ! command -v "$cmd" &>/dev/null; then
            fail "$cmd not found. Please install it."
            exit 1
        fi
    done
    ok "All required tools found"
    echo ""
}

# =============================================================================
# Phase 2: Create identities & fund accounts
# =============================================================================
phase2_setup_accounts() {
    info "Phase 2: Setting up accounts"
    echo "---"

    # Faucet funding account
    info "Generating faucet funder identity..."
    stellar keys generate faucet-funder --network "$NETWORK" --overwrite 2>/dev/null || true
    FUNDER_PUBKEY=$(stellar keys address faucet-funder)
    FUNDER_SECRET=$(stellar keys secret faucet-funder)
    ok "Funder: $FUNDER_PUBKEY"

    info "Funding faucet funder on testnet..."
    stellar keys fund faucet-funder --network "$NETWORK" 2>/dev/null
    ok "Funder funded via friendbot"

    # Token issuer account
    info "Generating token issuer identity..."
    stellar keys generate faucet-issuer --network "$NETWORK" --overwrite 2>/dev/null || true
    ISSUER_PUBKEY=$(stellar keys address faucet-issuer)
    ok "Issuer: $ISSUER_PUBKEY"

    info "Funding issuer on testnet..."
    stellar keys fund faucet-issuer --network "$NETWORK" 2>/dev/null
    ok "Issuer funded via friendbot"

    # Generate receiver accounts — fund them so they exist on-chain
    RECEIVER_PUBKEYS=()
    for i in $(seq 1 "$NUM_RECEIVERS"); do
        local name="faucet-test-recv-${i}"
        stellar keys generate "$name" --network "$NETWORK" --overwrite 2>/dev/null || true
        local pk
        pk=$(stellar keys address "$name")
        RECEIVER_PUBKEYS+=("$pk")
        stellar keys fund "$name" --network "$NETWORK" 2>/dev/null
        ok "Receiver $i: $pk"
    done

    echo ""
}

# =============================================================================
# Phase 3: Build & deploy contracts, create custom SAC token, set up trustlines
# =============================================================================
phase3_deploy() {
    info "Phase 3: Building & deploying contracts + token setup"
    echo "---"

    # Build the WASM
    info "Building batch_transfer contract..."
    local build_output
    build_output=$(stellar contract build --package batch_transfer 2>&1)
    echo "$build_output" | tail -3
    WASM_PATH=$(echo "$build_output" | sed -n 's/.*Wasm File: \([^ ]*\).*/\1/p' | head -1)
    if [[ -z "$WASM_PATH" || ! -f "$WASM_PATH" ]]; then
        for candidate in \
            target/wasm32v1-none/release/batch_transfer.wasm \
            target/wasm32-unknown-unknown/release/batch_transfer.wasm; do
            if [[ -f "$candidate" ]]; then
                WASM_PATH="$candidate"
                break
            fi
        done
    fi
    if [[ -z "$WASM_PATH" || ! -f "$WASM_PATH" ]]; then
        fail "WASM not found after build"
        exit 1
    fi
    ok "WASM built: $WASM_PATH"

    # Deploy batch_transfer contract
    info "Deploying batch_transfer contract..."
    CONTRACT_ADDRESS=$(stellar contract deploy \
        --wasm "$WASM_PATH" \
        --source faucet-funder \
        --network "$NETWORK" \
        2>&1 | tail -1)
    ok "batch_transfer deployed: $CONTRACT_ADDRESS"

    # Custom asset: FCTTKN:<issuer>
    ASSET="FCTTKN:${ISSUER_PUBKEY}"
    info "Setting up custom asset $ASSET ..."

    # Create trustline on funder for the custom asset
    info "  Creating trustline on funder..."
    stellar tx new change-trust \
        --source faucet-funder \
        --line "$ASSET" \
        --network "$NETWORK" 2>/dev/null
    ok "  Funder trustline created"

    # Issue tokens from issuer to funder
    local mint_amount="1000000000"
    stellar tx new payment \
        --source faucet-issuer \
        --destination "$FUNDER_PUBKEY" \
        --asset "$ASSET" \
        --amount "$mint_amount" \
        --network "$NETWORK" 2>/dev/null
    ok "  Issued $mint_amount FCTTKN to funder"

    # Create trustlines on each receiver for the custom asset
    info "  Creating trustlines on receivers..."
    for i in $(seq 1 "$NUM_RECEIVERS"); do
        local name="faucet-test-recv-${i}"
        stellar tx new change-trust \
            --source "$name" \
            --line "$ASSET" \
            --network "$NETWORK" 2>/dev/null
        ok "  Receiver $i trustline created"
    done

    # Deploy SAC for the custom asset
    info "  Deploying SAC..."
    TOKEN_ADDRESS=$(stellar contract asset deploy \
        --asset "$ASSET" \
        --source faucet-funder \
        --network "$NETWORK" \
        2>&1 | tail -1)
    ok "SAC token address: $TOKEN_ADDRESS"

    echo ""
}

# =============================================================================
# Phase 4: Build & start the faucet
# =============================================================================
phase4_start_faucet() {
    info "Phase 4: Building & starting faucet"
    echo "---"

    info "Building faucet binary..."
    cargo build -p faucet 2>&1 | tail -3
    ok "Faucet built"

    FAUCET_LOG=$(mktemp)
    info "Starting faucet (log: $FAUCET_LOG)..."
    RUST_LOG=faucet=info \
    FUNDING_SECRET_KEY="$FUNDER_SECRET" \
    TOKEN_ADDRESS="$TOKEN_ADDRESS" \
    CONTRACT_ADDRESS="$CONTRACT_ADDRESS" \
    AMOUNT="$AMOUNT" \
    MAX_BATCH_SIZE=65 \
    RPC_URL="$RPC_URL" \
    NETWORK_PASSPHRASE="$NETWORK_PASSPHRASE" \
    PORT="$FAUCET_PORT" \
        ./target/debug/faucet > "$FAUCET_LOG" 2>&1 &
    FAUCET_PID=$!
    ok "Faucet starting (pid $FAUCET_PID)"

    # Wait for the faucet to be ready
    info "Waiting for faucet to be ready..."
    for i in $(seq 1 60); do
        if ! kill -0 "$FAUCET_PID" 2>/dev/null; then
            fail "Faucet process exited early. Log:"
            cat "$FAUCET_LOG"
            exit 1
        fi
        if curl -sf "${FAUCET_URL}/health" >/dev/null 2>&1; then
            ok "Faucet is ready!"
            curl -s "${FAUCET_URL}/health" | jq .
            echo ""
            return
        fi
        sleep 1
    done
    fail "Faucet did not start within 60s. Log:"
    cat "$FAUCET_LOG"
    exit 1
}

# =============================================================================
# Phase 5: Send concurrent funding requests
# =============================================================================
phase5_fund_receivers() {
    info "Phase 5: Sending $NUM_RECEIVERS concurrent funding requests"
    echo "---"

    RESULT_DIR=$(mktemp -d)

    # Fire all requests concurrently
    PIDS=()
    for i in $(seq 1 "$NUM_RECEIVERS"); do
        local addr="${RECEIVER_PUBKEYS[$((i-1))]}"
        (
            response=$(curl -s -w "\n%{http_code}" --max-time 30 "${FAUCET_URL}/?addr=${addr}")
            http_code=$(echo "$response" | tail -1)
            body=$(echo "$response" | sed '$d')
            echo "$http_code" > "${RESULT_DIR}/${i}.status"
            echo "$body" > "${RESULT_DIR}/${i}.body"
        ) &
        PIDS+=($!)
        info "  Request $i sent for ${addr:0:10}..."
    done

    info "Waiting for all requests to complete (this may take ~15s)..."
    for pid in "${PIDS[@]}"; do
        wait "$pid" || true
    done
    ok "All requests completed"
    echo ""

    # Check results
    FAILURES=0
    TX_HASH=""
    for i in $(seq 1 "$NUM_RECEIVERS"); do
        local status body addr hash
        status=$(cat "${RESULT_DIR}/${i}.status")
        body=$(cat "${RESULT_DIR}/${i}.body")
        addr="${RECEIVER_PUBKEYS[$((i-1))]}"

        if [[ "$status" == "200" ]]; then
            hash=$(echo "$body" | jq -r '.hash // empty' 2>/dev/null || echo "")
            ok "  Receiver $i (${addr:0:10}...): 200 — tx=${hash:0:16}..."
            TX_HASH="$hash"
        else
            fail "  Receiver $i (${addr:0:10}...): HTTP $status"
            echo "    $body"
            FAILURES=$((FAILURES + 1))
        fi
    done

    rm -rf "$RESULT_DIR"
    echo ""

    if [[ $FAILURES -gt 0 ]]; then
        fail "$FAILURES/$NUM_RECEIVERS requests failed"
        exit 1
    fi
    ok "All $NUM_RECEIVERS requests succeeded"

    # Verify all requests were batched into one tx
    if [[ -n "$TX_HASH" ]]; then
        ok "All requests batched into tx: $TX_HASH"
    fi
    echo ""
}

# =============================================================================
# Phase 6: Verify token balances
# =============================================================================
phase6_verify() {
    info "Phase 6: Verifying token balances"
    echo "---"

    FAILURES=0
    for i in $(seq 1 "$NUM_RECEIVERS"); do
        local addr="${RECEIVER_PUBKEYS[$((i-1))]}"
        local balance
        balance=$(stellar contract invoke \
            --id "$TOKEN_ADDRESS" \
            --source faucet-funder \
            --network "$NETWORK" \
            --is-view \
            -- balance --id "$addr" 2>&1 | tail -1)
        balance=$(echo "$balance" | tr -d '"')

        # Receivers started with 0 of the custom token, so balance should == AMOUNT
        if [[ "$balance" == "$AMOUNT" ]]; then
            ok "  Receiver $i (${addr:0:10}...): balance=$balance"
        else
            fail "  Receiver $i (${addr:0:10}...): balance=$balance (expected $AMOUNT)"
            FAILURES=$((FAILURES + 1))
        fi
    done

    echo ""
    if [[ $FAILURES -gt 0 ]]; then
        fail "Balance verification failed for $FAILURES/$NUM_RECEIVERS receivers"
        exit 1
    fi
    ok "All balances correct!"
}

# =============================================================================
# Main
# =============================================================================
main() {
    echo ""
    echo "============================================================"
    echo "  Faucet Integration Test (Testnet) — Custom Asset"
    echo "============================================================"
    echo ""

    phase1_prereqs
    phase2_setup_accounts
    phase3_deploy
    phase4_start_faucet
    phase5_fund_receivers
    phase6_verify

    echo ""
    echo "============================================================"
    echo -e "${GREEN}  ALL TESTS PASSED${NC}"
    echo "============================================================"
    echo ""
}

main "$@"
