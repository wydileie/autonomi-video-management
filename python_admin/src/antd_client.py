import base64
from dataclasses import dataclass
from typing import Any

import httpx


class AntdError(RuntimeError):
    """Raised when the antd REST daemon returns an error or unexpected shape."""


@dataclass(slots=True)
class HealthStatus:
    ok: bool
    network: str


@dataclass(slots=True)
class WalletAddress:
    address: str


@dataclass(slots=True)
class WalletBalance:
    balance: str
    gas_balance: str


@dataclass(slots=True)
class DataPutResult:
    address: str
    chunks_stored: int | None = None
    payment_mode_used: str | None = None
    cost: str | None = None


@dataclass(slots=True)
class DataCostEstimate:
    cost: str
    file_size: int
    chunk_count: int
    estimated_gas_cost_wei: str
    payment_mode: str


class AsyncAntdClient:
    """Tiny async client for the antd 2.x REST API used by this service."""

    def __init__(self, base_url: str, timeout: float = 60):
        self._client = httpx.AsyncClient(
            base_url=base_url.rstrip("/"),
            timeout=httpx.Timeout(timeout),
        )

    async def close(self):
        await self._client.aclose()

    async def health(self) -> HealthStatus:
        payload = await self._request_json("GET", "/health")
        status = str(payload.get("status", "")).lower()
        return HealthStatus(
            ok=status == "ok",
            network=str(payload.get("network", "")),
        )

    async def wallet_address(self) -> WalletAddress:
        payload = await self._request_json("GET", "/v1/wallet/address")
        return WalletAddress(address=str(payload["address"]))

    async def wallet_balance(self) -> WalletBalance:
        payload = await self._request_json("GET", "/v1/wallet/balance")
        return WalletBalance(
            balance=str(payload["balance"]),
            gas_balance=str(payload["gas_balance"]),
        )

    async def wallet_approve(self) -> bool:
        payload = await self._request_json("POST", "/v1/wallet/approve")
        return bool(payload.get("approved", False))

    async def data_put_public(self, data: bytes, payment_mode: str = "auto") -> DataPutResult:
        payload = await self._request_json(
            "POST",
            "/v1/data/public",
            json={
                "data": base64.b64encode(data).decode("ascii"),
                "payment_mode": payment_mode,
            },
        )
        return DataPutResult(
            address=str(payload["address"]),
            chunks_stored=payload.get("chunks_stored"),
            payment_mode_used=payload.get("payment_mode_used"),
            cost=payload.get("cost"),
        )

    async def data_get_public(self, address: str) -> bytes:
        payload = await self._request_json("GET", f"/v1/data/public/{address}")
        try:
            return base64.b64decode(payload["data"])
        except (KeyError, ValueError) as exc:
            raise AntdError("antd returned invalid public data payload") from exc

    async def data_cost(self, data: bytes) -> DataCostEstimate:
        payload = await self._request_json(
            "POST",
            "/v1/data/cost",
            json={"data": base64.b64encode(data).decode("ascii")},
        )
        return DataCostEstimate(
            cost=str(payload.get("cost", "0")),
            file_size=int(payload.get("file_size", 0) or 0),
            chunk_count=int(payload.get("chunk_count", 0) or 0),
            estimated_gas_cost_wei=str(payload.get("estimated_gas_cost_wei", "0")),
            payment_mode=str(payload.get("payment_mode", "")),
        )

    async def _request_json(self, method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        try:
            response = await self._client.request(method, path, **kwargs)
            response.raise_for_status()
            payload = response.json()
        except httpx.HTTPStatusError as exc:
            detail = exc.response.text
            raise AntdError(f"{method} {path} failed: {exc.response.status_code} {detail}") from exc
        except (httpx.HTTPError, ValueError) as exc:
            raise AntdError(f"{method} {path} failed: {exc}") from exc

        if not isinstance(payload, dict):
            raise AntdError(f"{method} {path} returned non-object JSON")
        return payload
