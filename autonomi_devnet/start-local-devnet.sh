#!/usr/bin/env bash
set -euo pipefail

LOG_DIR="${LOG_DIR:-/data/logs}"
MANIFEST="${ANT_DEVNET_MANIFEST:-/data/ant-devnet-manifest.json}"
DATA_DIR="${ANT_DEVNET_DATA_DIR:-/data/nodes}"
PRESET="${ANT_DEVNET_PRESET:-default}"
QUOTE_TIMEOUT_SECS="${ANTD_QUOTE_TIMEOUT_SECS:-60}"
STORE_TIMEOUT_SECS="${ANTD_STORE_TIMEOUT_SECS:-120}"
RESET_ON_START="${ANT_DEVNET_RESET_ON_START:-true}"

mkdir -p "$LOG_DIR" "$DATA_DIR"
rm -f "$MANIFEST"

cleanup() {
  jobs -pr | xargs -r kill 2>/dev/null || true
}
trap cleanup EXIT INT TERM

if [[ "$RESET_ON_START" != "0" && "${RESET_ON_START,,}" != "false" && "${RESET_ON_START,,}" != "no" ]]; then
  echo "[autonomi-devnet] resetting active node data dir ${DATA_DIR}"
  rm -rf "${DATA_DIR:?}/"*
fi

echo "[autonomi-devnet] starting ant-devnet preset=${PRESET}"
ant-devnet \
  --preset "$PRESET" \
  --enable-evm \
  --data-dir "$DATA_DIR" \
  --manifest "$MANIFEST" \
  > "$LOG_DIR/ant-devnet.log" 2>&1 &
DEVNET_PID=$!

echo "[autonomi-devnet] waiting for manifest ${MANIFEST}"
for _ in $(seq 1 120); do
  if [ -f "$MANIFEST" ] && jq -e '.bootstrap[0] and .evm.rpc_url' "$MANIFEST" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done

if ! [ -f "$MANIFEST" ] || ! jq -e '.bootstrap[0] and .evm.rpc_url' "$MANIFEST" >/dev/null 2>&1; then
  echo "[autonomi-devnet] ERROR: devnet manifest was not created in time" >&2
  tail -100 "$LOG_DIR/ant-devnet.log" >&2 || true
  exit 1
fi

PEERS="$(jq -r '.bootstrap | join(",")' "$MANIFEST")"
WALLET_KEY="$(jq -r '.evm.wallet_private_key' "$MANIFEST")"
EVM_RPC="$(jq -r '.evm.rpc_url' "$MANIFEST")"
EVM_TOKEN="$(jq -r '.evm.payment_token_address' "$MANIFEST")"
EVM_VAULT="$(jq -r '.evm.payment_vault_address // .evm.data_payments_address' "$MANIFEST")"

echo "[autonomi-devnet] starting antd"
ANTD_PEERS="$PEERS" \
AUTONOMI_WALLET_KEY="$WALLET_KEY" \
EVM_RPC_URL="$EVM_RPC" \
EVM_PAYMENT_TOKEN_ADDRESS="$EVM_TOKEN" \
EVM_PAYMENT_VAULT_ADDRESS="$EVM_VAULT" \
antd \
  --network local \
  --cors \
  --log-level "${ANTD_LOG_LEVEL:-warn}" \
  --quote-timeout-secs "$QUOTE_TIMEOUT_SECS" \
  --store-timeout-secs "$STORE_TIMEOUT_SECS" \
  > "$LOG_DIR/antd.log" 2>&1 &
ANTD_PID=$!

echo "[autonomi-devnet] waiting for antd health"
for _ in $(seq 1 60); do
  if curl -sf --max-time 2 http://localhost:8082/health >/dev/null 2>&1; then
    echo "[autonomi-devnet] ready"
    echo "[autonomi-devnet] manifest: $MANIFEST"
    echo "[autonomi-devnet] logs: $LOG_DIR"
    wait -n "$DEVNET_PID" "$ANTD_PID"
    exit $?
  fi
  sleep 2
done

echo "[autonomi-devnet] ERROR: antd did not become healthy" >&2
tail -100 "$LOG_DIR/antd.log" >&2 || true
exit 1
