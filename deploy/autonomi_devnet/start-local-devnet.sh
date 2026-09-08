#!/usr/bin/env bash
set -euo pipefail
umask 077

LOG_DIR="${LOG_DIR:-/data/logs}"
MANIFEST="${ANT_DEVNET_MANIFEST:-/data/ant-devnet-manifest.json}"
DATA_DIR="${ANT_DEVNET_DATA_DIR:-/data/nodes}"
PRESET="${ANT_DEVNET_PRESET:-default}"
QUOTE_TIMEOUT_SECS="${ANTD_QUOTE_TIMEOUT_SECS:-60}"
STORE_TIMEOUT_SECS="${ANTD_STORE_TIMEOUT_SECS:-120}"
RESET_ON_START="${ANT_DEVNET_RESET_ON_START:-false}"
REST_ADDR="${ANTD_REST_ADDR:-127.0.0.1:8082}"
GATEWAY_BIN="${AUTVID_GATEWAY_BIN:-autvid-antd-gateway}"
HEALTH_PORT="${REST_ADDR##*:}"

mkdir -p "$LOG_DIR" "$DATA_DIR"
rm -f "$MANIFEST"

cleanup() {
  for pid in $(jobs -pr); do kill "$pid" 2>/dev/null || true; done
}
trap cleanup EXIT INT TERM

case "$RESET_ON_START" in
  1|true|TRUE|yes|YES)
  echo "[autonomi-devnet] resetting active node data dir ${DATA_DIR}"
  rm -rf "${DATA_DIR:?}/"* ;;
esac

echo "[autonomi-devnet] starting ant-devnet preset=${PRESET}"
ant-devnet \
  --preset "$PRESET" \
  --enable-evm \
  --no-cleanup \
  --enable-logging \
  --log-level "${ANT_DEVNET_LOG_LEVEL:-info}" \
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

echo "[autonomi-devnet] starting autvid antd gateway"
ANTD_PEERS="$PEERS" \
AUTONOMI_WALLET_KEY="$WALLET_KEY" \
EVM_RPC_URL="$EVM_RPC" \
EVM_PAYMENT_TOKEN_ADDRESS="$EVM_TOKEN" \
EVM_PAYMENT_VAULT_ADDRESS="$EVM_VAULT" \
ANTD_NETWORK=local \
ANTD_REST_ADDR="$REST_ADDR" \
ANTD_QUOTE_TIMEOUT_SECS="$QUOTE_TIMEOUT_SECS" \
ANTD_STORE_TIMEOUT_SECS="$STORE_TIMEOUT_SECS" \
"$GATEWAY_BIN" \
  > "$LOG_DIR/antd.log" 2>&1 &
ANTD_PID=$!

echo "[autonomi-devnet] waiting for antd gateway health"
for _ in $(seq 1 60); do
  if curl -sf --max-time 2 "http://127.0.0.1:${HEALTH_PORT}/health" >/dev/null 2>&1; then
    echo "[autonomi-devnet] ready"
    echo "[autonomi-devnet] manifest: $MANIFEST"
    echo "[autonomi-devnet] logs: $LOG_DIR"
    while kill -0 "$DEVNET_PID" 2>/dev/null && kill -0 "$ANTD_PID" 2>/dev/null; do sleep 1; done
    if ! kill -0 "$DEVNET_PID" 2>/dev/null; then wait "$DEVNET_PID"; else wait "$ANTD_PID"; fi
    exit $?
  fi
  sleep 2
done

echo "[autonomi-devnet] ERROR: antd did not become healthy" >&2
tail -100 "$LOG_DIR/antd.log" >&2 || true
exit 1
