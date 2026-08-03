---
name: ledgerseq-genesis-anchor-overlay-wedge
description: v2.5.0 preview BP wedge — LedgerSeq is never re-anchored after the startup chunk replay, so a quarantined-snapshot boot leaves a GENESIS anchor; the first at-tip fork switch installs genesis pparams (PV6/d=1) via rollback_via_seq and the TPraos overlay check falsely rejects a canonical Conway block, permanently poisoning invalid_cache
metadata:
  type: project
---

# LedgerSeq genesis-anchor → overlay false-reject → permanent wedge (found 2026-08-03, preview BP v2.5.0)

**Filed as issue #985** (bug, critical, correctness, priority:p0).

**Symptom**: `Not an active overlay slot: slot 119084816 ...` during "Fork replay: Praos
header validation FAILED" on preview (Conway PV11), then `chain_sel: candidate fork
contains a known-invalid block — refusing to adopt (StoreButDontChange)` forever.
Both blocks were CANONICAL on preview (Koios: 62/26 confirmations). Network was
PREVIEW even though reported as "preprod" — always verify via wall-clock-slot
arithmetic + Koios `block_info` on the logged hashes.

**Defect chain** (all v2.5.0 = main @ 457b317f82):
1. `Node::new` anchors LedgerSeq at the PRE-replay startup ledger
   (`crates/dugite-node/src/node/mod.rs:1977-1982`). On any snapshot-load failure —
   guaranteed on the first boot after a SNAPSHOT_VERSION bump (31→32 quarantine,
   `snapshot.rs:672-684`) — that ledger is `init_fresh_ledger`'s genesis state:
   preview Shelley genesis = **PV6, d=1/1, 7 genDelegs, tip=Origin**.
2. `replay_ledger_from_storage` (`node/sync.rs:2345`) advances ONLY the live
   `ledger_state`; it never pushes seq deltas and never re-anchors.
   `startup::recover_ledger_seq` (which DOES replay the gap into the anchor) is
   DEAD CODE in the run path; `LedgerSeq::reset_anchor` has zero callers.
3. Live at-tip applies push deltas onto the genesis anchor. First fork switch →
   `rollback_via_seq` (`state/mod.rs:2202`) wholesale-installs
   `seq.tip_state().epochs/certs/gov/consensus` = genesis-anchor + tip-deltas
   CHIMERA into the live ls (`state/mod.rs:2260-2274`) — tip point is correct
   (delta-tracked) but pparams/nonces/certs are genesis-era.
4. `validate_peer_header_full`'s overlay gate (`node/mod.rs:6239`) keys on ls
   pparams: PV6<7 ∧ d=1 ∧ delegs≠∅ → builds OverlayContext for a CONWAY block.
   d=1 ⇒ every slot overlay; f=1/20 ⇒ asc_inv=20; epoch_slot 25616 % 20 = 16 ⇒
   NonActiveSlot ⇒ `NotActiveOverlaySlot` (praos.rs:742, strict mode).
5. `abandon_failed_fork` inserts the canonical block into `invalid_cache`
   (in-memory, `chain_sel_queue.rs:770`) → every honest descendant refused →
   permanent wedge until process restart. Forge loop keeps running on the
   corrupted ledger.

**Haskell truth (oracle-verified, refs pinned in cardano-haskell-oracle memory
`tpraos-overlay-vs-praos-no-overlay.md`)**: OVERLAY is TPraos-only
(`Cardano.Protocol.TPraos.Rules.Overlay/Prtcl`); Praos `updateChainDepState` =
KES+VRF+OCert only, `PraosValidationErr` has NO overlay constructor; Praos
LedgerView has no `lvD`/GenDelegs; Babbage/Conway = `ShelleyBlock (Praos c)`.
A Conway header can STRUCTURALLY never be overlay-rejected in Haskell.

**Preview trap**: dugite's retained `.d` is 1/1 forever on preview (chain never
zeroed d on-chain; Babbage translation doesn't clear it) and genesis_delegates
are re-seeded every startup — so `PV<7` is the ONLY guard on the overlay path.
A genesis-pparams chimera fires it on mainnet/preprod too (genesis d=1 there).

**Milder always-on variant**: EVERY normal boot anchors at snapshot-slot S,
replays gap S→immutable-tip into ls only ⇒ seq has an un-delta'd gap. A fork
switch reconstructs non-UTxO state from state-at-S; if an epoch boundary lies
in S..T the reconstructed epochs/nonces/snapshots are one epoch stale.

**Fixes recommended** (not yet implemented):
- Re-anchor after replay: `ledger_seq.reset_anchor(ls.clone_without_utxos())` at
  end of `Node::start` replay (or use `recover_ledger_seq`); same after the
  rollback slow path (`*ls = snapshot_state` + replay) and gap-bridge.
  Invariant: any bulk ls advance outside `apply_block_with_delta`+`seq.push`
  MUST re-anchor.
- Era-gate overlay on the BLOCK's era (`block.era < Babbage`), not ledger
  pparams — matches Haskell protocol-per-era wiring; makes the false reject
  structurally impossible for Conway blocks.
- Coherence guard in `rollback_via_seq`/`tip_state`: verify deltas[0] links to
  anchor_point; bail to snapshot slow path on mismatch.

**Recovery**: restart clears invalid_cache; the post-replay single save
(sync.rs:2487) already wrote a healthy v32 snapshot, so the restarted boot
anchors coherently. Snapshot worker fires from the APPLY path only, so a wedged
node does not persist the chimera. After ANY boot logging "Failed to load
ledger snapshot, starting fresh" — restart once after replay completes to close
the wedge-armed window.
