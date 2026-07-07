---
name: project_dugite_genesisdeleg_mir_gaps_2026_07_06
description: Two concrete dugite-ledger divergences found 2026-07-06 while researching GenesisDelegCert maturation and MIR quorum witnessing for a user fix — not yet filed as issues as of this writing
metadata:
  type: project
---

Found while answering a user question about Haskell semantics for a planned dugite fix (2026-07-06).
See [[shelley-genesisdeleg-and-mir-witness-quorum]] for the verified Haskell source facts these are
compared against.

## Gap 1: `Certificate::GenesisKeyDelegation` applies immediately, no two-phase queue

`crates/dugite-ledger/src/state/certificates.rs` ~L617-650: on `GenesisKeyDelegation`, dugite does
`self.genesis_delegates.insert(gkey, (dkey, *vrf_keyhash))` immediately. The existing code comment
says Haskell uses "futureGenDelegs → genDelegs after 2 * stability_window" and claims immediate
application is "observationally equivalent ... differs only during a Byron-genesis replay."

**Both parts of that comment are inaccurate**: the real Haskell maturation delay is `1 *
stabilityWindow` (`3k/f`), not `2 *` (the 2x figure belongs to a different mechanism, the PPUP
submission deadline / HFC point-of-no-return — see the linked memory for the exact code). And the
delay is a consensus-relevant anti-adaptive-corruption mechanism in the original Shelley spec, not
a Byron-replay-only curiosity — collapsing it to an immediate insert is a real behavioral
divergence any time a `GenesisDelegTxCert` lands within `stabilityWindow` slots of other genesis-
delegation-dependent activity (e.g. a subsequent MIR cert's quorum check reading `dsGenDelegs`
before vs. after the real maturation point, or a second `GenesisDelegTxCert` for the same genesis
key racing the queue). No duplicate-delegate/duplicate-VRF checks
(`DuplicateGenesisDelegateDELEG` / `DuplicateGenesisVRFDELEG`) were found near this call site
either — Haskell checks the new cold/VRF key isn't already in use by *any other* genesis key,
across both current and future maps, before queuing.

## Gap 2: MIR certs require zero witnesses — `validateMIRInsufficientGenesisSigs` has no dugite equivalent

`crates/dugite-ledger/src/validation/phase1.rs` `cert_required_witnesses` (~L106-107, L154-156):
```rust
// Legacy certificates — no witness checks.
Certificate::GenesisKeyDelegation { .. } | Certificate::MoveInstantaneousRewards { .. } => {
    vec![]
}
```
A grep across `crates/dugite-ledger/src` for `quorum`/`MIRInsufficientGenesisSigs`/`genesis_sig`
found the `update_quorum` field wired only into the pre-Conway PPUP tally path
(`validation/ppup.rs`, `eras/shelley.rs`, `eras/conway.rs`, `state/epoch.rs`, `state/mod.rs` —
the #784 PPUP fix, see MEMORY.md); there is **no code path anywhere that intersects
`dsGenDelegs`'s delegate-key-hashes against the tx's VKey witnesses for an MIR cert, and no
`MIRInsufficientGenesisSigsUTXOW`-equivalent error variant.** As it stands, a transaction
carrying an `MoveInstantaneousRewards` certificate with **no genesis-delegate witnesses at all**
would currently pass dugite's phase-1 witness check, where Haskell would reject it (Shelley
through Babbage) unless at least `update_quorum` (mainnet 5) of the current genesis delegates'
hot-key hashes are present as VKey witnesses.

**Why:** genuine correctness/security gap (dugite is meant to be adversarial-hardened per
[[feedback_dugite_node_hostile_environment]] and Haskell-byte-exact per
[[feedback_haskell_byte_exact_only]]) — MIR certs are AtMostEra Babbage only (Conway correctly has
no MIR path at all, matching Haskell's removal), so the fix scope is Shelley/Allegra/Mary/Alonzo/
Babbage.

**How to apply:** when implementing the fix, wire the quorum check as its own validation step
(mirroring Babbage's dedicated `babbageUtxowMirTransition`, i.e. check `Set::intersection` of
`{genesis_delegates.values().map(|(dkey, _)| dkey)}` against the tx's witness key-hash set, sized
against `update_quorum`, gated on `!mir_certs.is_empty()`) rather than folding it into
`cert_required_witnesses` (which is an ALL-of-N-required-signers model; this is an M-of-N quorum
model over a *different* key set than the cert's own credential, so it doesn't fit that helper's
shape). Neither gap had an open GitHub issue as of 2026-07-06; file one before or alongside the fix
per repo convention.
