#!/usr/bin/env bash
# start_autonomi.sh — Start upstream SDK tooling (port 8182; application gateway uses 8082) at devcontainer startup.
#
# ANTD_NETWORK=local   (default) — spins up ant-devnet local testnet + antd.
#                                   Free writes, no real tokens needed.
# ANTD_NETWORK=default           — connects antd to the Autonomi mainnet.
#                                   Write operations require AUTONOMI_WALLET_KEY.
#
# Runs every time the container starts (via postStartCommand).

set -uo pipefail

NETWORK="${ANTD_NETWORK:-local}"
LOG_DIR="/tmp/ant-logs"
MANIFEST="/tmp/ant-devnet-manifest.json"

mkdir -p "$LOG_DIR"

# ── Fix Docker socket permissions (OrbStack mounts it as root:root 0660) ─────
# Reassign the socket to the docker group so the vscode user can reach it.
sudo chgrp docker /var/run/docker.sock 2>/dev/null || true

# ── Kill any stale processes from a previous container run ────────────────────
pkill -f "^antd-upstream"      2>/dev/null || true
pkill -x ant-devnet 2>/dev/null || true
sleep 1

# ── Helper: wait for antd REST health endpoint ────────────────────────────────
wait_for_antd() {
    local tries=40
    for i in $(seq 1 $tries); do
        if curl -sf --max-time 2 http://localhost:8182/health > /dev/null 2>&1; then
            return 0
        fi
        sleep 2
    done
    return 1
}

# ─────────────────────────────────────────────────────────────────────────────
if [ "$NETWORK" = "local" ]; then
    echo "[autonomi] Starting local devnet (ant-devnet + antd)..."

    # Start ant-devnet — it spins up local Autonomi nodes + an Anvil EVM chain,
    # then writes a JSON manifest with bootstrap peers and EVM contract addresses.
    rm -f "$MANIFEST"
    setsid ant-devnet \
        --preset default \
        --enable-evm \
        --manifest "$MANIFEST" \
        >> "$LOG_DIR/devnet.log" 2>&1 < /dev/null &
    DEVNET_PID=$!
    echo "[autonomi]   ant-devnet PID $DEVNET_PID"

    # Wait for the manifest (up to ~3 minutes; first startup can be slow)
    echo "[autonomi]   Waiting for devnet nodes to initialise..."
    READY=0
    for i in $(seq 1 90); do
        if [ -f "$MANIFEST" ]; then
            BOOTSTRAP=$(python3 -c \
                "import json,sys; m=json.load(open('$MANIFEST')); b=m.get('bootstrap',[]); print(b[0] if b else '')" \
                2>/dev/null || true)
            if [ -n "$BOOTSTRAP" ]; then
                READY=1
                break
            fi
        fi
        sleep 2
    done

    if [ "$READY" -ne 1 ]; then
        echo "[autonomi] ERROR: devnet did not initialise within timeout."
        echo "[autonomi]        Check $LOG_DIR/devnet.log for details."
        exit 1
    fi

    # Parse manifest for antd configuration
    PEERS=$(python3 -c \
        "import json; m=json.load(open('$MANIFEST')); print(','.join(m.get('bootstrap',[])))" \
        2>/dev/null || true)
    WALLET_KEY=$(python3 -c \
        "import json; m=json.load(open('$MANIFEST')); k=m.get('evm',{}).get('wallet_private_key',''); print(k.lstrip('0x'))" \
        2>/dev/null || true)
    EVM_RPC=$(python3 -c \
        "import json; m=json.load(open('$MANIFEST')); print(m.get('evm',{}).get('rpc_url',''))" \
        2>/dev/null || true)
    EVM_TOKEN=$(python3 -c \
        "import json; m=json.load(open('$MANIFEST')); print(m.get('evm',{}).get('payment_token_address',''))" \
        2>/dev/null || true)
    EVM_VAULT=$(python3 -c \
        "import json; evm=json.load(open('$MANIFEST')).get('evm',{}); print(evm.get('payment_vault_address') or evm.get('data_payments_address',''))" \
        2>/dev/null || true)

    echo "[autonomi]   Devnet ready — starting antd (network=local)..."

    ANTD_PEERS="$PEERS" \
    AUTONOMI_WALLET_KEY="$WALLET_KEY" \
    EVM_RPC_URL="$EVM_RPC" \
    EVM_PAYMENT_TOKEN_ADDRESS="$EVM_TOKEN" \
    EVM_PAYMENT_VAULT_ADDRESS="$EVM_VAULT" \
    setsid antd-upstream --rest-addr 127.0.0.1:8182 --network local --cors --log-level warn \
        --quote-timeout-secs 60 \
        --store-timeout-secs 120 \
        >> "$LOG_DIR/antd.log" 2>&1 < /dev/null &

else
    echo "[autonomi] Starting antd on default (mainnet) network..."
    echo "[autonomi]   Note: write operations require AUTONOMI_WALLET_KEY to be set."
    setsid antd-upstream --rest-addr 127.0.0.1:8182 --network default --cors --log-level warn \
        --quote-timeout-secs 60 \
        --store-timeout-secs 120 \
        >> "$LOG_DIR/antd.log" 2>&1 < /dev/null &
fi

# ── Wait for antd to be ready ─────────────────────────────────────────────────
echo "[autonomi] Waiting for antd REST API..."
if wait_for_antd; then
    STATUS=$(curl -s http://localhost:8182/health 2>/dev/null || echo "{}")
    echo "[autonomi] antd is ready — $STATUS"
    echo "[autonomi] Logs: $LOG_DIR/"
else
    echo "[autonomi] WARNING: antd did not respond within timeout."
    echo "[autonomi]          Check $LOG_DIR/antd.log"
fi
