# Credential-Ord inversion analysis (backlog #26)

Decision artifact synthesizing three research reports + code verification at HEAD.

## TL;DR

- **dugite's `dugite_primitives::Credential` derives `Ord` as `VerificationKey(0) < Script(1)` ⇒ Key < Script**
  (`crates/dugite-primitives/src/credentials.rs:5-11`). This matches the *Plutus* `Credential`
  (`PubKeyCredential(0) < ScriptCredential(1)`) and the CBOR key-byte order, but is **INVERTED**
  relative to the *ledger* `Credential` (`ScriptHashObj(0) < KeyHashObj(1)` ⇒ Script < Key).
- **The reward/stake/deposit/delegation pipeline NEVER consults `Credential` Ord.** Every credential
  is projected to a type-tagged `Hash32` via `to_typed_hash32()` at ingest and the enum is discarded.
  Every Koios-validated total is an order-independent integer fold over `HashMap<Hash32,_>`. A
  Credential-Ord flip is therefore **invisible to reward/stake** — zero regression risk to the
  validated conservation outputs.
- **BUT** the *same* `Credential` enum's `Ord` feeds the phase-2 ScriptContext / redeemer-index
  paths, where the order **is** semantically observed and must match the *ledger* Script < Key.
  A blind global flip of the enum's derived `Ord` to Script < Key would *fix* the phase-2 sites but
  would also re-order the live `BTreeMap<Credential,u64>` for UpdateCommittee `members_to_add`
  (CBOR re-encode at `encode/governance.rs:182-186`) and any future `BTreeMap<Credential>`/`BTreeMap<Voter>`
  consumer — so it is **not** safe-and-sufficient as a one-liner. The right fix is per-consumer.
- **Practical observability:** in EVERY site the 28-byte hash dominates; the script-vs-key bit is only
  a tie-breaker for two entries sharing an identical 28-byte hash, which cannot occur in practice (a
  given 28-byte blake2b-224 value is a key-hash XOR a script-hash, never both in one collection). So
  there is **no known live byte-exact divergence today** — this is a latent / adversarial correctness
  fix, not an active regression.

---

## (a) Haskell-vs-dugite ordering table, per site

Legend:
- "Haskell Ord" = the `Ord` that governs the observable Haskell collection.
- "dugite ordering" = the basis dugite actually uses at HEAD.
- "Inverted?" = does dugite's type tie-break disagree with Haskell's? (Y when dugite=Key<Script and Haskell=Script<Key.)
- "Byte-exact-observable?" = could the inversion ever change observable bytes? Only if a single collection
  holds the same 28-byte hash as BOTH a key-cred and a script-cred (practically impossible) — so "Y(adv.)"
  means observable only under an adversarial same-hash-both-types collection; the hash dominates otherwise.

### Phase-2 ScriptContext / TxInfo (script result, ExUnits→fees, `serialiseData` byte-exact)

| Site (file:line) | Haskell Ord | dugite ordering | Inverted? | Byte-exact-observable? |
|---|---|---|---|---|
| `txInfoVotes` `Map Voter (Map GovActionId Vote)` — `populate_gov.rs:119` (voting_procedures_to_plutus), `script_context.rs` votes field | `Voter` derived: CC(0)<DRep(1)<SPO(2), inner ledger `Credential` **Script<Key** | iterates `BTreeMap<Voter,_>`; `Voter` derived CC<DRep<SPO, inner dugite `Credential` **Key<Script** (`transaction.rs:501-506`) | **Y** (inner cred) | Y(adv.) — same hash as both CC/DRep key+script voter in one body |
| Vote redeemer-pointer index — `redeemer_resolve.rs:318` (`voting_procedures.iter().nth(idx)`) | `Set.elemAt idx` over `VotingProcedures` `Map Voter` keyset = ledger `Voter` Ord (**Script<Key** inner) | `nth(idx)` over dugite `BTreeMap<Voter,_>` (**Key<Script** inner) | **Y** | Y(adv.) — resolves wrong voter→wrong script under same-hash collision |
| `txInfoWdrl` V3 `Map Credential Lovelace` — `populate_v3.rs:107-124` | `Map.toList` over `Map RewardAccount Coin` = (Network, ledger `Credential` **Script<Key**) | iterates `withdrawals: BTreeMap<Vec<u8>,Lovelace>` keyed by 29-byte blob; header `0xE_`(key)<`0xF_`(script) ⇒ **Key<Script** | **Y** | Y(adv.) — mixed key+script reward acct on same network |
| `txInfoWdrl` V1/V2 `[(StakingCredential,Integer)]` — `tx_info_populate.rs:569-583` | same as V3 (Script<Key) | same blob order (Key<Script) | **Y** | Y(adv.) |
| Reward/withdrawal redeemer-pointer index — `redeemer_resolve.rs:256` (`withdrawals.iter().nth(idx)`) | `Set.elemAt idx` over `Map RewardAccount` (Script<Key) | `nth(idx)` over blob `BTreeMap` (Key<Script) | **Y** | Y(adv.) |
| `TreasuryWithdrawals` gov-action `Map Credential Lovelace` (inside `txInfoProposalProcedures`) — `populate_gov.rs:243` | `transGovAction` map keyed by ledger stake `Credential` **Script<Key** | iterates blob `BTreeMap` ⇒ **Key<Script** | **Y** | Y(adv.) |
| `UpdateCommittee` gov-action `Map ColdCredential Epoch` — `populate_gov.rs` (members_to_add) | ledger `Map (Credential ColdCommitteeRole) EpochNo` **Script<Key** | iterates `members_to_add: BTreeMap<Credential,u64>` (`transaction.rs:467`) ⇒ **Key<Script** | **Y** | Y(adv.) |
| `txInfoSignatories` `[PubKeyHash]` — `tx_info_populate.rs:481-485`, `populate_v3.rs:92` | sorted `Set (KeyHash Witness)` raw-byte ascending | `required_signers.iter()` in WIRE order, never re-sorted | **N (not inversion)** — a *missing sort* | Y — diverges whenever wire array isn't already sorted; keys-only, no credential type |
| `txInfoRedeemers` `Map ScriptPurpose Redeemer` — `populate_v3.rs:134-142` | `ConwayPlutusPurpose` ctor order (Spend0..Propose5) then Word32 index | sort by `(purpose_rank, index)` | n/a | OK — index is Word32, resolved separately; no credential Ord |
| `txInfoData`, `txInfoMint`, Spend/Mint/Cert/Propose redeemer indices | by 32-byte hash / PolicyId / OSet position | same | n/a | OK — no credential Ord |

### Ledger-state CBOR (epoch-state byte-exactness) + tx CBOR re-encode (script_data_hash / tx hash / consensus)

| Site (file:line) | Haskell Ord | dugite ordering | Inverted? | Byte-exact-observable? |
|---|---|---|---|---|
| Reward/stake/deposit/delegation maps — `state/mod.rs` rewards`:169` stake_map`:468` delegations`:526` vote_delegations`:282`; `snapshot_format.rs:91,102` | `Map (Credential Staking)` / `Map DRep` etc. (in-mem Ord) | **`HashMap`/`ImblHashMap<Hash32,_>`** keyed by `to_typed_hash32()` (`mod.rs:2187`) — UNORDERED | **N/A** | **N** — never feeds an Ord-sensitive byte-exact emission; totals are folds; debug dump re-sorts by hex-id+amount (`epoch_state_debug.rs`) |
| `UpdateCommittee.members_to_add` tx CBOR re-encode — `encode/governance.rs:182-186` via `encode_credential` (`encode/certificate.rs:7`, tags `0`=key/`1`=script) | cardano-ledger emits its `Map (Credential ColdCommitteeRole) EpochNo` in in-mem `Map.toList` = **Script<Key** (non-canonical CBOR for this case) | iterates `BTreeMap<Credential,u64>` ⇒ **Key<Script** (= canonical CBOR key-byte order `[0,h]<[1,h]`) | **Y** | Y(adv.) — affects script_data_hash/tx-hash/block-body-hash only for a tx with two same-hash committee creds |

**Note on the test comment:** `crates/dugite-primitives/src/credentials.rs:142-148`
(`test_credential_ord_key_before_script`) asserts Key < Script and comments only
*"Derived Ord: enum variant order (VerificationKey=0 < Script=1)"*. It does **not** claim parity
with the ledger `Credential` (the haskell-ord-and-sites report overstated this). The assertion itself
is correct for the *enum*; the comment should be augmented to flag that this is **opposite** the ledger
`Credential` Ord (Script<Key) and matches Plutus/CBOR-key order, to prevent a future reader from reusing
this enum's Ord where ledger semantics are required.

---

## (b) Reward/stake GUARD verdict — does a Credential-Ord flip regress the Koios-validated reward/stake outputs?

**VERDICT: NO. A Credential-Ord flip (Key<Script → Script<Key) does NOT regress any
Koios-validated reward / stake / deposit / reserves / treasury total. The inversion is invisible
to the entire conservation pipeline.**

Decisive facts (code-verified):

1. **Enum erased at ingest.** `credential_to_hash(c) = c.to_typed_hash32()` (`state/mod.rs:2187-2188`)
   is the *only* path from `Credential` into ledger state. It writes the 28-byte hash into bytes
   [0..28] and the TYPE tag into byte 28 (`0x00` key / `0x01` script, `credentials.rs:32-39`). The
   Haskell-snapshot importer mirrors this exactly (`haskell_credential_to_hash32`, `mod.rs:2000-2007`).
   After this projection the `Credential` enum (and its `Ord`) no longer exists in ledger state.

2. **Every relevant map is `HashMap`/`ImblHashMap<Hash32|Hash28,_>`, never `BTreeMap<Credential>`:**
   `rewards` (`mod.rs:169`), `stake_map` (`:468`), `delegations` (`:526`), `vote_delegations` (`:282`),
   `reward_accounts`/`delegations` snapshot (`snapshot_format.rs:91,102`). The only `BTreeMap`s in
   `snapshot_format` are `pending/future_pp_updates` keyed by `EpochNo` — governance, not credentials.
   `grep` confirms **zero** `BTreeMap<Credential>` / `BTreeSet<Credential>` / `.cmp` / `sort` on
   `Credential` in `rewards.rs` or `epoch.rs`.

3. **Totals are commutative integer folds.** `total_distributed += delivered` over a `HashMap`
   iteration (`rewards.rs:528-540`); snapshot stake total = `stake_map.values().fold(0, saturating_add)`;
   apply-time registered/unregistered partition is `reward_accounts.contains_key()` set-membership
   (`epoch.rs`), not ordering. Integer `+` is associative/commutative ⇒ iteration order cannot move a total.

4. **The one intra-credential sort in rewards is NOT a Credential Ord.** At pv≤2, dugite's
   `Set.deleteFindMin` analog sorts entries by `(is_member, pool_id)`
   (`rewards.rs:535` `sort_unstable_by_key(|e| (e.0, e.1))`) — a `(bool, Hash28)` key, never a
   `Credential`. Structurally immune to a Credential-Ord flip.

5. **Haskell's own reward fold doesn't depend on Script-vs-Key either.** `aggregateRewards`/`sumRewards`
   is `fold`/`sum` over `Map (Credential Staking)` (order-agnostic); the only Haskell sort touching
   `Credential` Ord (`Set.deleteFindMin` in pv≤2 `filterRewards`) discriminates on the 28-byte hash in
   practice. Parity holds under either Ord choice.

**Conclusion:** the reward/stake regression guard is GREEN for a Credential-Ord change. The guard is
therefore *not* the blocker. The blocker is the opposite: a global enum-Ord flip would *change* the
phase-2 ScriptContext and the UpdateCommittee CBOR re-encode (which currently happen to align with
canonical CBOR / Plutus order), so the fix must be localized rather than applied to the shared enum.

---

## (c) Recommended fix level + exact sites + how_to_confirm

### Fix level: **per-consumer** (fix each phase-2 / governance-CBOR site to use the ledger Script<Key basis explicitly; leave the shared `Credential` enum's derived `Ord` untouched).

Rationale: the shared `dugite_primitives::Credential` enum's derived `Ord` (Key<Script) is *correct* for
its two natural roles — the Plutus `Credential` Data tag order and the canonical CBOR key-byte order —
and is *dead code* for ledger conservation. Flipping the derive would silently re-order the live
`BTreeMap<Credential,u64>` UpdateCommittee re-encode and any future `BTreeMap<Credential>`/`BTreeMap<Voter>`
consumer, trading one set of latent bugs for another. The robust fix introduces a ledger-ordered
comparator (Script<Key) at each phase-2/ledger-CBOR construction point.

### Exact sites to change

Root-cause enum (do NOT flip the derive; add a documented ledger-order comparator alongside it):
- `crates/dugite-primitives/src/credentials.rs:5-11` — keep `Ord` = Key<Script; add
  `fn cmp_ledger(&self,&self)` (Script<Key) for ledger/phase-2 consumers; augment the
  `test_credential_ord_key_before_script` comment (`:142-148`) to flag the ledger-vs-Plutus split.
- `crates/dugite-primitives/src/transaction.rs:501-506` (`Voter`) — provide a ledger-ordered
  comparator using `cmp_ledger` for the inner credential, for the phase-2 voter ordering / index.

Phase-2 ScriptContext / redeemer-index consumers (must emit/index in ledger Script<Key order):
- `crates/dugite-uplc/src/populate_gov.rs:119` (txInfoVotes), `:243` (TreasuryWithdrawals),
  members_to_add (UpdateCommittee map)
- `crates/dugite-uplc/src/populate_v3.rs:107-124` (txInfoWdrl V3)
- `crates/dugite-uplc/src/populate_v1_v2.rs` / `tx_info_populate.rs:569-583` (txInfoWdrl V1/V2)
- `crates/dugite-uplc/src/redeemer_resolve.rs:318` (Vote index), `:256` (Reward index) — index space
  must be the ledger `Set.elemAt` order, not raw blob / dugite-`Voter` BTreeMap order
- `crates/dugite-uplc/src/tx_info_populate.rs:481-485` + `populate_v3.rs:92` — **sort `txInfoSignatories`
  by raw 28-byte keyhash** (fixes the *missing-sort* divergence; independent of credential type)

Governance tx CBOR re-encode (consensus byte-exactness — only if matching ledger's non-canonical Map order):
- `crates/dugite-serialization/src/encode/governance.rs:182-186` (`members_to_add` via `encode_credential`)
  — emit in ledger Script<Key order to match cardano-ledger's `Map.toList` re-encode.

### how_to_confirm (byte-exact gate each fix must pass)

1. **Phase-2 ScriptContext dump-diff (primary gate).** With the dugite-uplc ScriptContext dump enabled,
   replay the captured preprod/preview phase-2 corpus (the same harness behind backlog #26/#27,
   `phase2-dumps-*`) and diff dugite's `serialiseData(ScriptContext)` bytes against the captured
   cardano-node ScriptContext bytes for each redeemer. PASS = byte-identical TxInfo for every
   `txInfoVotes`/`txInfoWdrl`/`TreasuryWithdrawals`/`UpdateCommittee`/`txInfoSignatories` case, AND the
   resolved redeemer (script+purpose) for every Vote/Reward redeemer matches the ledger's
   `redeemerPointerInverse`. To exercise the actually-inverted tie-break, include (or synthesize) a tx
   carrying both a key-cred and a script-cred voter / reward-account under the same body.

2. **Tx CBOR re-encode gate (governance site).** Re-encode a tx containing an `UpdateCommittee`
   `members_to_add` with ≥2 committee credentials and confirm `script_data_hash`, tx hash, and block
   body hash match the upstream golden / on-chain bytes. Cross-check against the cardano-ledger conway
   golden tx fixtures in the conformance corpus.

3. **Reward/stake non-regression (guard — must stay GREEN).** After the change, run the
   haskell-ledger-cross-validation epoch-diff harness from genesis and confirm reserves/treasury/fees/
   deposits/rewards/snapshots remain byte-exact vs `cardano-cli debug log-epoch-state` across all
   boundaries (expected: unchanged, since the ledger pipeline is Hash32-keyed and never touched). This
   gate proves the per-consumer fix did NOT leak into the conservation path.

4. **Unit/property coverage.** Add a same-hash-both-types proptest asserting dugite's phase-2 ordering
   == ledger Script<Key ordering for `txInfoVotes`/`txInfoWdrl`/UpdateCommittee, and a `txInfoSignatories`
   property that an unsorted wire array is emitted sorted. Keep
   `test_credential_ord_key_before_script` (enum Ord stays Key<Script) green to prove the shared enum
   was not flipped.
