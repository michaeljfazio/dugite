# UTxO RPC (gRPC) Server

Dugite-node ships a native [UTxO RPC](https://utxorpc.org) gRPC server —
the emerging standard programmable interface for UTxO chains adopted by
Dolos, Demeter, and most Cardano indexers. The server is **disabled by
default**; opt in via the `Rpc` config block or CLI flags below.

## Quick start

```bash
# Enable on the default port (50051), bind loopback only.
dugite-node run \
    --config config/mainnet/config.json \
    --topology config/mainnet/topology.json \
    --database-path ./db-mainnet \
    --socket-path ./node.sock \
    --host-addr 0.0.0.0 --port 3001 \
    --rpc-port 50051
```

```bash
# Verify it's up and list registered services.
grpcurl -plaintext localhost:50051 list
# Expect: utxorpc.v1alpha.{sync,query,submit,watch}.{Sync,Query,Submit,Watch}Service
#         utxorpc.v1beta.{sync,query,submit,watch}.{Sync,Query,Submit,Watch}Service
#         grpc.reflection.v1.ServerReflection

grpcurl -plaintext localhost:50051 utxorpc.v1beta.sync.SyncService/ReadTip
# {
#   "tip": {
#     "slot": "...",
#     "hash": "...",
#     "height": "..."
#   }
# }
```

## Services exposed

Every service ships in both **`v1alpha`** (for backwards compatibility
with older clients) and **`v1beta`** (current), with one exception:
`QueryService.ReadState` exists only in `v1beta` — upstream added it
after `v1alpha` was frozen. The spec is pinned in-tree at
`crates/dugite-rpc/proto/VERSION` (currently `v0.19.2`).

| Service | Method | Status |
|---|---|---|
| `SyncService` | `ReadTip` | ✅ implemented |
| `SyncService` | `FetchBlock` | ✅ implemented |
| `SyncService` | `DumpHistory` | ✅ implemented |
| `SyncService` | `FollowTip` (stream) | ✅ implemented |
| `QueryService` | `ReadParams` | ✅ implemented |
| `QueryService` | `ReadUtxos` | ✅ implemented |
| `QueryService` | `ReadGenesis` | ✅ implemented (minimum-viable envelope) |
| `QueryService` | `ReadEraSummary` | ✅ implemented |
| `QueryService` | `SearchUtxos` | ✅ implemented (`exact_address` / `payment_part` / `delegation_part` / `asset` plus `not` / `all_of` / `any_of` composites) |
| `QueryService` | `ReadData` | ✅ implemented (bounded scan: live inline datums + mempool tx witness sets) |
| `QueryService` | `ReadTx` | ✅ implemented (bounded scan: mempool + last ~43 200 slots of VolatileDB) |
| `QueryService` | `ReadState` | ✅ implemented, **`v1beta` only** (minimum-viable envelope: epoch + tip slot) |
| `SubmitService` | `SubmitTx` | ✅ implemented |
| `SubmitService` | `ReadMempool` | ✅ implemented |
| `SubmitService` | `WaitForTx` (stream) | ✅ implemented |
| `SubmitService` | `WatchMempool` (stream) | ✅ implemented (full `TxPredicate` filtering, same matcher as `WatchTx`) |
| `SubmitService` | `EvalTx` | ✅ implemented (per-redeemer `ex_units` + Plutus traces) |
| `WatchService` | `WatchTx` (stream) | ✅ implemented (full `TxPredicate` filtering: address / asset / mint / `not` / `all_of` / `any_of`) — mempool-sourced, see [Limitations](#limitations) |

Every method above honours a request's `google.protobuf.FieldMask` (issue
#1004): unselected fields are pruned from the response, recursively,
including into repeated fields like `FetchBlockResponse.block` — see
`crates/dugite-rpc/src/masking.rs` for the exact semantics (a mask that's
absent or empty returns everything, matching the canonical FieldMask doc).

## Configuration

### JSON config block

Add an `Rpc` block to `config/<network>/config.json`:

```json
{
  "Rpc": {
    "Enabled": true,
    "ListenAddr": "127.0.0.1",
    "Port": 50051,
    "MaxConcurrentStreams": 64,
    "StreamBufferSize": 256,
    "ReflectionEnabled": true,
    "WebEnabled": false,
    "AlphaEnabled": true,
    "Tls": {
      "CertPath": "/etc/dugite/tls/rpc.crt",
      "KeyPath": "/etc/dugite/tls/rpc.key"
    }
  }
}
```

All fields are optional. Defaults:

| Field | Default | Notes |
|---|---|---|
| `Enabled` | `false` | Server stays disabled unless this is `true` or a `--rpc-*` CLI flag is passed. |
| `ListenAddr` | `127.0.0.1` | Loopback only — protects an unauthenticated TCP gRPC endpoint from the network. Set to `0.0.0.0` only if you've fronted it with TLS or a reverse proxy. |
| `Port` | `50051` | The de-facto UTxO RPC port used by Dolos, Demeter, and others. |
| `MaxConcurrentStreams` | `64` | HTTP/2 streams per connection. |
| `StreamBufferSize` | `256` | Per-stream event buffer. Slow consumers exceeding this drop with `RESOURCE_EXHAUSTED`. |
| `ReflectionEnabled` | `true` | Exposes `grpc.reflection.v1.ServerReflection` so `grpcurl -plaintext :50051 list` works without a schema bundle. |
| `WebEnabled` | `false` | Accept gRPC-Web (HTTP/1.1) for browser dApps. Costs a small per-connection bookkeeping when enabled. |
| `AlphaEnabled` | `true` | Expose `v1alpha` services alongside `v1beta`. Operators can pre-disable to test that their clients have migrated. |
| `Tls` | absent | Optional TLS termination. PEM-encoded cert/key on disk; no hot-reload (config changes require a restart). |

### CLI flags

CLI flags override the JSON config block:

| Flag | Behaviour |
|---|---|
| `--rpc-port <PORT>` | Force-enable RPC on this port (overrides `Rpc.Port`). |
| `--rpc-host <IP>` | Force-enable RPC on this address (overrides `Rpc.ListenAddr`). |
| `--no-rpc` | Force-disable RPC, regardless of the config block. |

Precedence (highest first):

1. `--no-rpc` → server disabled.
2. `--rpc-host` / `--rpc-port` → server enabled with CLI values overriding the config block.
3. `Rpc.Enabled = true` in JSON → server enabled with config values.
4. Otherwise → server disabled.

### Configuration editor (`dugite-config`)

The `Rpc` section is exposed read-only as a JSON `Object` in
[`dugite-config`](./config-editor.md) so operators can see what's
configured at a glance. Edit sub-fields directly in the config JSON.

## TLS

For non-loopback deployments, set `Tls.CertPath` + `Tls.KeyPath` to
PEM-encoded files. Both files are read at startup; missing or
unreadable files fail-fast with an `io::Error` rather than letting
the server come up unsecured.

For mTLS, mutual auth, or rotating certificates, terminate TLS at a
reverse proxy (Envoy, nginx) and leave Dugite's `Tls` block absent.

## Operations

### Metrics

**The RPC server currently emits no Prometheus metrics.** `dugite-rpc`
defines an `RpcMetricsSink` trait (`request_started`,
`request_completed`, `stream_started`, `stream_ended`) so a host can plug
in Prometheus or OpenTelemetry, but `dugite-node` wires
`dugite_rpc::noop_metrics()` — every callback is a no-op. No
`dugite_rpc_*` series appear on the node's `/metrics` endpoint.

Until a real sink is wired, observe the RPC server through logs (below)
and through the node-level metrics it drives indirectly
(`dugite_mempool_tx_count`, `dugite_n2c_txs_*`, and friends).

### Logging

Service-level events log at `INFO` or `DEBUG` under the
`dugite_rpc::server` target. Streaming RPCs log slow-consumer drops at
`WARN` with `service` / `method` labels.

### Spec-bump workflow

The UTxO RPC `.proto` files are vendored at
`crates/dugite-rpc/proto/utxorpc/` and pinned via
`crates/dugite-rpc/proto/VERSION`. To refresh:

```bash
just bump-utxorpc-spec v0.20.0    # replace with the desired tag
```

The script:

1. Clones the tag from `https://github.com/utxorpc/spec` into a tempdir.
2. Re-copies the Cardano-only subset (`cardano + sync + query + submit + watch`
   for both `v1alpha` and `v1beta`; `bitcoin` and `handshake` intentionally
   omitted).
3. Rewrites `VERSION` with the new tag + resolved commit + today's date.
4. Builds and tests `dugite-rpc` so codegen breakage / golden-test
   drift surfaces before the resulting commit is pushed.

Bumps land as their own PRs alongside any code changes needed to track
upstream protobuf shape changes. The single-source-of-truth lives in
`VERSION` — out-of-sync bumps (e.g. files refreshed without `VERSION`
updated, or vice versa) are caught by code review against the diff.

## Limitations

* `SearchUtxos` with a fully-wildcard predicate (no `match` /
  combinators) is rejected with `UNIMPLEMENTED`: dugite refuses to
  materialise the entire UTxO set in a single response. Supply at
  least one selector (address / payment_part / delegation_part /
  asset / composite) so the result set is bounded.
* `ReadTx` walks at most the last ~43 200 slots of `VolatileDB`. A
  chain-wide tx index would extend the lookup window to immutable
  history; not built today.
* `ReadData` scans the live UTxO set's inline datums and the
  mempool's witness-set datums. Witness-set datums from immutable
  blocks are not retained — clients that need them should consult
  the originating tx via `ReadTx`.
* `ReadState`'s `AnyChainStateData.cardano` is currently empty: the
  endpoint returns the ledger tip + epoch only. Per-query state
  projections (stake-pool distribution, DRep info) land on top of
  this stub.
* `EvalTx`'s per-redeemer `ex_units` are CEK-machine consumed
  values, not declared. Cost-model overrides are not yet read from
  protocol params — the CEK falls back to per-step defaults, which
  is *conservative* (over-approximates) and therefore safe for
  fee-estimation use cases but may diverge slightly from cardano-node
  on the high end.
* `WatchTx` / `WatchMempool` filter on tx output fields (`produces` /
  `has_address` / `moves_asset`) and minting (`mints_asset`). Two
  `TxPattern` leaves are not implemented: `consumes` (needs resolved-input
  UTxO data unavailable on the mempool-only watch path) and
  `has_certificate` (needs a certificate-type matcher not yet built). A
  request naming either — anywhere, including nested under `not` /
  `all_of` / `any_of` — is **rejected** with `UNIMPLEMENTED` before it
  ever subscribes, rather than silently accepted and under-filtered.
* `WatchTx` is **mempool-sourced**, not chain-sourced: it streams
  `MempoolEvent::Added` (pre-confirmation), matching
  `SubmitService.WatchMempool`'s semantics rather than the proto's own
  "stream transactions from the chain" comment. Two consequences:
  `AnyChainTx.block` is always unset (a mempool tx has no confirming
  block yet), and the `undo` / `idle` `WatchTxResponse` variants are
  never emitted (both are block-scoped concepts). Tracked as
  [#1007](https://github.com/michaeljfazio/dugite/issues/1007) — the
  fix needs `TipRollback` to carry the rolled-back block's transactions,
  which today it does not.
* `FollowTip` apply events carry `AnyChainBlock.native_bytes` (the
  raw block CBOR); clients that only need tip metadata can ignore
  the payload.

## See also

* [UTxO RPC spec](https://utxorpc.org)
* `crates/dugite-rpc/proto/VERSION` — the pinned spec tag.
* `crates/dugite-rpc/tests/` — golden + integration tests covering each service.
