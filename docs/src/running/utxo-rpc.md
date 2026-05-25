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
with older clients) and **`v1beta`** (current). The spec is pinned
in-tree at `crates/dugite-rpc/proto/VERSION`.

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
| `QueryService` | `SearchUtxos` / `ReadData` / `ReadTx` / `ReadState` | ⏳ M2.B follow-up |
| `SubmitService` | `SubmitTx` | ✅ implemented |
| `SubmitService` | `ReadMempool` | ✅ implemented |
| `SubmitService` | `WaitForTx` (stream) | ✅ implemented |
| `SubmitService` | `WatchMempool` (stream) | ✅ implemented |
| `SubmitService` | `EvalTx` | ⏳ follow-up — needs non-committing UPLC helper |
| `WatchService` | `WatchTx` (stream) | ✅ implemented (match-all in v1; pattern filtering follow-up) |

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

The RPC server emits standard Prometheus counters / histograms via the
node's existing metrics endpoint:

* `dugite_rpc_requests_total{service, method, status}` (counter)
* `dugite_rpc_request_duration_seconds{service, method}` (histogram)
* `dugite_rpc_active_streams{service, method}` (gauge)

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

* `EvalTx` (Plutus dry-run) returns `UNIMPLEMENTED` until a non-
  committing UPLC evaluation helper lands.
* Cert / governance / script / Plutus-witness / metadatum fields on
  `Tx` are populated minimally in M1.B; full mapping is the M2.B
  follow-up.
* `SearchUtxos` returns `UNIMPLEMENTED` pending the in-ledger
  payment-credential and asset indexes.
* `FollowTip` apply events carry tip metadata only; the full
  `AnyChainBlock.native_bytes` requires an async block fetch per
  event — clients should `FetchBlock` with `tip.hash` for the bytes.

## See also

* [UTxO RPC spec](https://utxorpc.org)
* `crates/dugite-rpc/proto/VERSION` — the pinned spec tag.
* `crates/dugite-rpc/tests/` — golden + integration tests covering each service.
