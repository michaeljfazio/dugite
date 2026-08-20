---
name: issue-1102-1105-keepalive-dataflow-gating
description: "#1102's inbound-accept KeepAlive-client fix was too broad (direction-gated, not DataFlow-gated); #1105 fixes it correctly on min(local,remote) diffusionMode, live-proven both directions with two throwaway dugite-node instances"
type: project
---

## What happened

#1102 (commit `1c47a7d634`) found that `register_inbound_connection`
unconditionally started dugite's own KeepAlive-CLIENT on every ACCEPTED N2N
connection, which broke `cardano-cli ping` (hardcodes `InitiatorOnly` — a
minimal probe implementing only the KeepAlive client role, no responder).
The shipped fix REMOVED the KeepAlive-client start unconditionally — too
broad. A same-day devnet-validate re-gate (`project_1102_duplex_accept_keepalive_gap_2026_08_21.md`,
cardano-node-validator's memory) caught the regression LIVE: dugite-relay
accepting a connection FROM cardano-bp (genuinely Duplex — both declare
`InitiatorAndResponder`) now never sends its own `MsgKeepAlive`, so
cardano-bp's KeepAlive-SERVER times out and tears down the whole mux —
continuous ~90-112s reconnect churn + false peer-failure backoff, plus 2
confirmed OFFDIAG rows in the bidirectional-parity oracle
(`01i-many-inputs`, `18l-v2-script-v3-builtins`).

## The correct mechanism (#1105, grounded in
`.claude/agent-memory/cardano-haskell-oracle/n2n-diffusionmode-dataflow-duplex-gating.md`)

Gate on the connection's NEGOTIATED `DataFlow` (`min(local, remote)`
`diffusionMode` per `Cardano.Network.NodeToNode.Version.acceptableVersion`),
not on inbound-vs-outbound direction. `Duplex` only when NEITHER side
declared `InitiatorOnly`. This is Haskell's "Awake" transition modelled
without a second dial — reuse the SAME accepted socket for our own
Initiator-role protocols (KeepAlive client) rather than relying on a
separate outbound connection to the same peer.

`HandshakeResult` (dugite-network `handshake/mod.rs`) gained
`negotiated_initiator_only: Option<bool>` — `Some(accepted.initiator_only)`
on every N2N `Ok(...)` arm (server accept, server query-reply, client
accept-version, client simultaneous-open), `None` for N2C (no
diffusion-mode concept) and the client's query-mode reply. #1104's fix
(same day, `MsgAcceptVersion` carries the AGREED not raw-local data) meant
the CLIENT side's `their_data` decoded off `MsgAcceptVersion` already IS the
negotiated value — no need to re-run `accept()` there, just read
`their_data.initiator_only` directly.

`PeerConnection` (dugite-node `peer_connection.rs`) gained
`pub(crate) negotiated_duplex: bool` — `true` unconditionally for OUTBOUND
(dialing intrinsically means running our own client role, independent of
DataFlow; DataFlow only gates whether an ACCEPTED connection may ALSO be
used for our client role), derived as
`!handshake_result.negotiated_initiator_only.unwrap_or(true)` (fail-safe:
under-running is silent, over-running sends unsolicited traffic) for INBOUND
in `PeerConnection::accept()`.

`register_inbound_connection` (dugite-node `connection_lifecycle.rs`) now
does `if conn.negotiated_duplex { start_warm_protocols(keepalive_fn) }` —
directly, at accept time, on the SAME connection object. No governor
`promote_to_warm` involvement needed — this is a simpler, more direct
implementation of "Awake" than routing through the outbound-dial promotion
path, and sidesteps the memory's open question #4 (whether
`has_any_to`/`existing_to_peer` governor logic needs a fix) entirely.

## Live-proven both directions (not just unit tests)

Two throwaway dugite-node instances, isolated `/tmp` scratch dirs/ports
(19001/19002), preview genesis (symlinked from `config/preview/`, network
magic 2, no real sync needed — just N2N handshake + KeepAlive):
- Node A (acceptor): topology has ONE decoy unreachable local-root
  (`127.0.0.1:19098`, needed only because dugite-node exits cleanly with
  `warn!("No peers configured in topology"); return Ok(())` when
  `topology.detailed_peers()` is empty — a real constraint worth remembering
  for future minimal repros) — never dials node B.
- Node B (dialer): local-root = node A. Confirms the ASYMMETRIC accept-only
  case (unlike the existing local-devnet 3-node topology, where
  dugite-bp/dugite-relay templates dial EACH OTHER symmetrically and so
  never exercise the accept-only path at all).

Result: `N2N handshake complete (inbound) ... negotiated_duplex=true`,
`started warm protocols (KeepAlive)` on the ACCEPTOR, real reciprocal
ping/pong flowing every ~10s for 240s+ with zero teardown.

`cardano-cli ping --host 127.0.0.1 --port 19001 --magic 2 --count 5 --json`
against the SAME acceptor (now with a real InitiatorOnly peer instead of a
Duplex one): `negotiated_version.initiator: InitiatorOnly`,
`negotiated_duplex=false`, log line `not starting KeepAlive-client`, 5/5
pongs, clean `MsgDone`, responder re-armed (#980's mechanism) — no
connection reset, matching #1102's original RED-proof exactly but now
confirmed to still hold under #1105's narrower gate.

RED-proven at the unit level too: temporarily forcing the gate to `if false`
(mimicking #1102's shipped over-broad fix) makes
`register_inbound_connection_duplex_starts_keepalive_client` fail while
`register_inbound_connection_unidirectional_never_starts_keepalive_client`
still passes — isolates exactly what #1102 got wrong.

## Files touched (uncommitted, left in working tree per task instruction)

- `crates/dugite-network/src/handshake/mod.rs` — `HandshakeResult.negotiated_initiator_only`
- `crates/dugite-node/src/node/peer_connection.rs` — `PeerConnection.negotiated_duplex` field, derivation in `accept()`, `true` in `connect()`, `set_negotiated_duplex_for_test` helper, all 6 struct-literal sites updated
- `crates/dugite-node/src/node/connection_lifecycle.rs` — `register_inbound_connection` gate + 2 tests (renamed Unidirectional test + new Duplex test)

Full workspace `cargo build --all-targets --all-features`, `cargo clippy
--all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `cargo
nextest run --workspace` all clean except the pre-existing, unrelated
`xtask::qa_report_covers_shipped_code` staleness (expected — the QA report
is pinned to an earlier `git_rev` and always goes stale on any new commit
touching `crates/`; a release-lead job, not this fix's concern).

## Reusable pattern worth remembering

An under-scoped Haskell-alignment fix that gates on the wrong CONDITION
(direction) instead of the right one (negotiated protocol STATE) can look
completely correct in isolation — #1102's own RED-proof (cardano-cli ping)
still passes today — while silently breaking the opposite case. The
adversarial-review process this project mandates for divergence fixes is
exactly what caught it, one release cycle later, via a live multi-node gate
run rather than any unit test.
