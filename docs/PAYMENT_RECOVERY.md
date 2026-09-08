# Payment approvals and recovery

A preliminary media estimate is informational. The executable final quote is
prepared from the actual segments, optional original, public DataMaps, manifest,
and both catalog snapshots. Approval binds their hashes and sizes, payment
modes, quote ID, network identity (including chain genesis and payer), expiry,
and separate aggregate storage/gas caps. Malformed amounts are rejected.

Before signing, the gateway atomically reserves storage and worst-case gas in
its SQLite journal. Each transaction has a gas limit and fee ceiling. Token
allowance is bounded to the approved operation. Signed bytes, hash, nonce,
receipts, and completed uploads are journaled; identical retries return the
original result without another payment. A wallet/network has one ordered
signing stream, and uncertain transaction state blocks subsequent signing.

Workers renew leases and send the job identity and ownership generation with
payment requests. Losing a lease cancels work; stale workers cannot complete
jobs or publish catalog results. Catalog changes during a quote/upload invalidate
the snapshot and require approval of a new quote.

## Operator-visible states

| State | Action |
| --- | --- |
| `awaiting_approval` / catalog `draft` | Review storage and gas caps, then approve. |
| `approval_required` | Content, prices, or expiry invalidated the plan. Prepare and review a new quote. |
| `payment_recovery_required` | Preserve the original files, approval, job, and gateway journal. Resume the existing operation or reconcile uncertain payment state. |
| `ready` / catalog `complete` | Every required object was stored and configured verification succeeded. |

“Resume approved upload” and “Resume approved catalog publication” requeue the
same failed job and approval. The gateway replays completed objects and uses
upstream resumable finalization for partial storage. It never creates a new
payment to replace an uncertain one. Fresh catalog quotes are blocked while an
earlier catalog payment is active or uncertain.

Upstream prepared/resumable handles are opaque in-memory values, retained for
up to one hour (at most four). They are not serialized as a durable format. If
a restart loses material required to finish paid storage, recovery remains
paused. Repeatedly pressing Resume cannot recover lost cryptographic material.
Do not delete the journal, manually reset a nonce, or create a replacement
approval as a payment-recovery shortcut.

## Backup and restore

Back up the admin database and gateway payment journal together, along with
catalog state and pending processing files. Drain active paid work before a
migration/restore snapshot. Online scheduled backups protect database integrity
but are not a cross-database transaction; a restored snapshot always needs
payment reconciliation.

`restore-production.sh` requires both databases and validates them before
replacing either destination. It pauses all restored approvals and sets the
journal's `restore_reconciliation_required` signing guard. Health then reports
write readiness as false. New approvals cannot bypass this guard.

With services stopped, an operator must compare journal transactions (network,
nonce, hash, signed bytes, receipts) to the matching chain and account, including
transactions after the backup time. Preserve this evidence and reconcile
uncertain/pending records before lifting the guard. There is deliberately no
blind “clear recovery” HTTP endpoint. Resuming paid storage still requires its
retained upstream material. If that material is lost, seek a reviewed recovery
procedure; do not automatically pay again.

Migration 0003 regenerates legacy pending approvals. Existing published
addresses remain readable and are not rewritten or deleted on the network.
