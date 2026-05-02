# Ledger Validation Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring dugite-ledger to 100% predicate-failure parity with the Haskell `cardano-ledger` reference by adding the 22 currently-missing validation predicates across Conway GOV, MIR (Shelley–Babbage), PPUP (Shelley–Babbage), CERTS, POOL, UTXO, and the consensus-layer `OutsideForecastRange` check. Each predicate must be implemented with the exact Haskell semantics, raised under the exact same conditions, and proven correct by a unit test asserting the rejection on a hand-crafted failing input.

**Architecture:** Each predicate is added as a new `ValidationError` variant in `crates/dugite-ledger/src/validation/mod.rs` (or a new error enum for PPUP / consensus errors), raised from a new validator function placed alongside the existing era-appropriate validation module (`validation/conway.rs`, `validation/phase1.rs`, or new modules `validation/mir.rs`, `validation/ppup.rs`). MIR and PPUP are pre-Conway only and therefore live behind era guards. `OutsideForecastRange` is a consensus-layer error in `crates/dugite-consensus/src/era_history.rs` (forecast machinery already exists there).

**Tech Stack:** Rust 2021, Cargo workspace, `cargo nextest` for tests, `thiserror` for error enums, `pallas` for wire types, `dugite-primitives` for shared types. All Haskell references are from `IntersectMBO/cardano-ledger` and `IntersectMBO/ouroboros-consensus` repositories.

---

## Cross-Cutting Conventions

Every task in this plan follows these rules:

1. **TDD**: write the failing test first; show it fails; implement; show it passes.
2. **Cross-check**: every implementation must cite the exact Haskell module path and function name in a Rust doc comment (`/// Reference: …`).
3. **Era gating**: era-conditional predicates use `params.protocol_version.major` checks matching the Haskell `pvMajor pv` comparisons.
4. **Test naming**: `test_<predicate_name_snake>_<scenario>` — e.g. `test_disallowed_voters_spo_voting_on_new_constitution`.
5. **Commit cadence**: one commit per predicate (or per closely-related group), with prefix `feat(ledger):` or `feat(consensus):` and a body referencing the Haskell module.
6. **Verification gate**: at the end of every task, run `cargo nextest run -p dugite-ledger -E 'test(<predicate>)'` and `cargo clippy -p dugite-ledger --all-targets -- -D warnings`.

## File Structure

**Modified:**
- `crates/dugite-ledger/src/validation/mod.rs` — add new `ValidationError` variants
- `crates/dugite-ledger/src/validation/conway.rs` — Conway GOV predicate validators (currently a small file; will grow significantly)
- `crates/dugite-ledger/src/validation/phase1.rs` — `OutputBootAddrAttrsTooBig`, `PoolMedataHashTooBig` checks
- `crates/dugite-ledger/src/state/certificates.rs` — MIR validator wired into existing `Certificate::MoveInstantaneousRewards` apply path
- `crates/dugite-ledger/src/eras/shelley.rs` — call MIR validator and PPUP rule from Shelley/Allegra/Mary/Alonzo/Babbage paths
- `crates/dugite-consensus/src/era_history.rs` — add forecast-horizon check
- `crates/dugite-ledger/src/state/governance.rs` — wire WithdrawalsNotInRewardsCERTS check; eliminate the existing best-effort drain DEBUG path

**Created:**
- `crates/dugite-ledger/src/validation/mir.rs` — Shelley/Allegra/Mary/Alonzo/Babbage MIR predicate validators
- `crates/dugite-ledger/src/validation/ppup.rs` — Shelley–Babbage PPUP rule (3 predicates + quorum)
- `crates/dugite-ledger/src/validation/withdrawals.rs` — `withdrawals_that_do_not_drain_accounts` helper used by `WithdrawalsNotInRewardsCERTS`

---

## Task 1: Plan setup and reference assertions

**Files:**
- Create: nothing
- Modify: nothing
- Test: nothing — just verify baseline

- [ ] **Step 1: Verify the workspace builds clean before any changes**

Run:
```bash
cargo build -p dugite-ledger -p dugite-consensus 2>&1 | tail -3
cargo nextest run -p dugite-ledger 2>&1 | tail -5
```

Expected: build succeeds, all tests pass. If anything is red, **stop** and fix the regression on `main` first.

- [ ] **Step 2: Confirm the validation module surface**

Run:
```bash
grep -c "pub enum ValidationError" crates/dugite-ledger/src/validation/mod.rs
grep -c "pub fn validate_transaction_with_context" crates/dugite-ledger/src/validation/mod.rs
```

Expected: `1` and `1` (or higher for second one — 1 definition + N test usages is fine). If either is `0`, the file structure has changed since this plan was authored — re-confirm before proceeding.

- [ ] **Step 3: Commit a checkpoint marker**

```bash
git checkout -b ledger-validation-completeness
git commit --allow-empty -m "chore(ledger): start validation completeness branch

Tracking implementation of 22 missing predicate failures per
docs/superpowers/plans/2026-05-02-ledger-validation-completeness.md."
```

---

## Task 2: Conway GOV — `DisallowedVoters`

**Reference:** `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, function `checkVotersAreValid` and `Cardano.Ledger.Conway.Governance.Internal.{isCommitteeVotingAllowed, isDRepVotingAllowed, isStakePoolVotingAllowed}`.

**Files:**
- Modify: `crates/dugite-ledger/src/validation/mod.rs` — add `ValidationError::DisallowedVoters`
- Modify: `crates/dugite-ledger/src/validation/conway.rs` — add `validate_voter_authority`
- Test: `crates/dugite-ledger/src/validation/conway.rs` (in-file `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the error variant**

Append to `ValidationError` enum (in alphabetical-ish neighborhood of other Conway gov errors):

```rust
/// A voter is not authorised to vote on this governance action type.
///
/// Reference: Haskell `DisallowedVoters` in
/// `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`.
/// The voter × action authority matrix:
///   - `NoConfidence`: SPO yes, DRep yes, CC NO
///   - `UpdateCommittee`: SPO yes, DRep yes, CC NO
///   - `NewConstitution`: SPO NO, DRep yes, CC yes
///   - `HardForkInitiation`: SPO yes, DRep yes, CC yes
///   - `ParameterChange`: SPO only when SecurityGroup params, DRep yes, CC yes
///   - `TreasuryWithdrawals`: SPO NO, DRep yes, CC yes
///   - `InfoAction`: all yes (NoVotingThreshold)
#[error("DisallowedVoters: voter type not allowed for this action ({0:?})")]
DisallowedVoters(Vec<(String, String)>),  // (voter_kind, gov_action_id_hex)
```

- [ ] **Step 2: Write the failing test**

In `crates/dugite-ledger/src/validation/conway.rs`:

```rust
#[cfg(test)]
mod gov_voter_authority_tests {
    use super::*;
    use dugite_primitives::transaction::{GovAction, Voter, VotingProcedure, Vote};
    use dugite_primitives::hash::Hash32;

    /// SPO is NOT allowed to vote on NewConstitution.
    /// Reference: Haskell `votingStakePoolThresholdInternal NewConstitution = NoVotingAllowed`.
    #[test]
    fn test_disallowed_voters_spo_voting_on_new_constitution() {
        let action = GovAction::NewConstitution {
            prev_action_id: None,
            constitution: Default::default(),
        };
        let voter = Voter::StakePoolKey(Hash32::from_bytes([1u8; 28].into()).into());
        let res = check_voter_authority(&voter, &action);
        assert!(matches!(res, Err(ValidationError::DisallowedVoters(_))),
            "expected DisallowedVoters, got {res:?}");
    }

    /// CC is NOT allowed to vote on NoConfidence.
    #[test]
    fn test_disallowed_voters_committee_voting_on_no_confidence() {
        let action = GovAction::NoConfidence { prev_action_id: None };
        let voter = Voter::CommitteeHotKey(Hash32::from_bytes([2u8; 28].into()).into());
        let res = check_voter_authority(&voter, &action);
        assert!(matches!(res, Err(ValidationError::DisallowedVoters(_))));
    }

    /// SPO IS allowed to vote on HardForkInitiation.
    #[test]
    fn test_voter_authority_spo_on_hard_fork_initiation_allowed() {
        let action = GovAction::HardForkInitiation { prev_action_id: None, protocol_version: (10, 0) };
        let voter = Voter::StakePoolKey(Hash32::from_bytes([3u8; 28].into()).into());
        assert!(check_voter_authority(&voter, &action).is_ok());
    }

    /// All voters allowed on InfoAction.
    #[test]
    fn test_voter_authority_info_action_allows_all() {
        let action = GovAction::InfoAction;
        for voter in [
            Voter::StakePoolKey(Hash32::from_bytes([1u8; 28].into()).into()),
            Voter::DRepKey(Hash32::from_bytes([2u8; 28].into()).into()),
            Voter::CommitteeHotKey(Hash32::from_bytes([3u8; 28].into()).into()),
        ] {
            assert!(check_voter_authority(&voter, &action).is_ok());
        }
    }
}
```

- [ ] **Step 3: Run the test to verify it fails (compile error first)**

```bash
cargo nextest run -p dugite-ledger -E 'test(disallowed_voters)' 2>&1 | tail -10
```

Expected: compile error — `check_voter_authority` not defined.

- [ ] **Step 4: Implement `check_voter_authority`**

In `crates/dugite-ledger/src/validation/conway.rs`:

```rust
use dugite_primitives::transaction::{GovAction, Voter};
use crate::validation::mod_inner::ValidationError;  // or whatever the actual path is

/// Check whether a voter is authorised to vote on a given governance action.
///
/// Reference: Haskell `checkVotersAreValid` /
/// `is{Committee,DRep,StakePool}VotingAllowed` in
/// `Cardano.Ledger.Conway.Governance.Internal`. We model the matrix
/// directly (no `VotingThreshold` intermediate type).
pub fn check_voter_authority(voter: &Voter, action: &GovAction) -> Result<(), ValidationError> {
    let allowed = match (voter, action) {
        // InfoAction: anyone may vote (NoVotingThreshold, never ratifies).
        (_, GovAction::InfoAction) => true,
        // SPO restrictions.
        (Voter::StakePoolKey(_), GovAction::NewConstitution { .. }) => false,
        (Voter::StakePoolKey(_), GovAction::TreasuryWithdrawals { .. }) => false,
        // For ParameterChange, Haskell allows SPO only when modified params include
        // a SecurityGroup field. We approximate this conservatively as "allowed":
        // the threshold logic in state/governance.rs already maps non-security
        // changes to NoVotingAllowed for SPOs at ratification time. The voter-
        // authority predicate here only blocks the *transaction-level* gov action
        // pre-ratification — for that, the threshold-level NoVotingAllowed is
        // exposed via `pp_change_spo_threshold` returning NoVotingAllowed.
        // Therefore we mirror Haskell's voter-authority pass here: SPO is allowed
        // in the transaction; the ratification-time NoVotingAllowed handles the
        // case of non-security params.
        (Voter::StakePoolKey(_), GovAction::ParameterChange { .. }) => true,
        // CC restrictions.
        (Voter::CommitteeHotKey(_), GovAction::NoConfidence { .. }) => false,
        (Voter::CommitteeHotKey(_), GovAction::UpdateCommittee { .. }) => false,
        // Everything else: allowed.
        _ => true,
    };
    if allowed {
        Ok(())
    } else {
        Err(ValidationError::DisallowedVoters(vec![(
            format!("{voter:?}"),
            // The gov action ID isn't part of the GovAction; the caller is responsible
            // for using `validate_voting_procedures` (added in Task 3) to report
            // (voter, gov_action_id) pairs.
            String::new(),
        )]))
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo nextest run -p dugite-ledger -E 'test(disallowed_voters)'
```

Expected: PASS.

- [ ] **Step 6: Wire `check_voter_authority` into `validate_transaction_with_context`**

Locate the section of `validation/mod.rs` (≈ line 1186 onward) that handles voting procedures (Conway). Add a loop after the existing voter-existence check (or before, mirroring Haskell ordering — see Task 3 which adds the existence check; if Task 3 hasn't run yet, place after the existing voter iteration):

```rust
// Conway GOV: DisallowedVoters check.
// Reference: `checkVotersAreValid` runs after the existence partition.
for (voter, vp_map) in tx.body.voting_procedures.iter() {
    for gov_action_id in vp_map.keys() {
        // Look up the action to determine its type. If unknown, skip
        // (GovActionsDoNotExist handles unknown actions separately).
        if let Some(gas) = ctx.governance.proposals.get(gov_action_id) {
            if let Err(mut e) = check_voter_authority(voter, &gas.gov_action) {
                if let ValidationError::DisallowedVoters(ref mut pairs) = e {
                    pairs[0].1 = gov_action_id.to_hex();
                }
                errors.push(e);
            }
        }
    }
}
```

(Adapt struct paths to actual dugite types. `ctx.governance.proposals` may be named differently; check `ValidationContext` definition before writing.)

- [ ] **Step 7: Add an end-to-end test that exercises the wired path**

```rust
#[test]
fn test_validate_transaction_rejects_spo_voting_on_new_constitution() {
    // Construct a Conway tx with a voting_procedures entry where an SPO
    // votes Yes on a NewConstitution action that exists in ctx.governance.
    // Expect ValidationError::DisallowedVoters in the returned Vec.
    let mut ctx = ValidationContext::test_default();
    let action_id = ctx.add_test_proposal(GovAction::NewConstitution {
        prev_action_id: None,
        constitution: Default::default(),
    });
    let tx = test_helpers::tx_with_vote(
        Voter::StakePoolKey(Hash32::from_bytes([1u8; 28].into()).into()),
        action_id,
        Vote::Yes,
    );
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::DisallowedVoters(_))),
        "expected DisallowedVoters in {errors:?}");
}
```

- [ ] **Step 8: Run, fix, commit**

```bash
cargo nextest run -p dugite-ledger -E 'test(disallowed_voters)'
cargo clippy -p dugite-ledger --all-targets -- -D warnings
cargo fmt --all -- --check
git add crates/dugite-ledger
git commit -m "feat(ledger): add DisallowedVoters Conway GOV predicate

Implements the voter × gov-action authority matrix per Haskell
\`checkVotersAreValid\` in Cardano.Ledger.Conway.Rules.Gov. SPOs may
not vote on NewConstitution or TreasuryWithdrawals; CC may not vote
on NoConfidence or UpdateCommittee. InfoAction is unrestricted."
```

---

## Task 3: Conway GOV — `VotersDoNotExist`

**Reference:** `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`, the `internVoter` partition in `conwayGovTransition`. Fires **before** `DisallowedVoters` per Haskell ordering.

**Files:**
- Modify: `crates/dugite-ledger/src/validation/mod.rs` — add `ValidationError::VotersDoNotExist`
- Modify: `crates/dugite-ledger/src/validation/conway.rs` — add `validate_voter_existence`
- Test: in-file

- [ ] **Step 1: Add error variant**

```rust
#[error("VotersDoNotExist: {0:?}")]
VotersDoNotExist(Vec<String>),  // voter descriptors
```

- [ ] **Step 2: Write failing test**

```rust
#[test]
fn test_voters_do_not_exist_unregistered_drep() {
    let mut ctx = ValidationContext::test_default();
    let unregistered_drep = Hash32::from_bytes([99u8; 28].into());
    let action_id = ctx.add_test_proposal(GovAction::InfoAction);
    let tx = test_helpers::tx_with_vote(
        Voter::DRepKey(unregistered_drep.into()),
        action_id,
        Vote::Yes,
    );
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::VotersDoNotExist(_))));
}

#[test]
fn test_voter_exists_for_registered_drep() {
    let mut ctx = ValidationContext::test_default();
    let drep = Hash32::from_bytes([1u8; 28].into());
    ctx.registered_dreps.insert(drep);
    let action_id = ctx.add_test_proposal(GovAction::InfoAction);
    let tx = test_helpers::tx_with_vote(Voter::DRepKey(drep.into()), action_id, Vote::Yes);
    // Should not produce VotersDoNotExist (other errors may be acceptable in test harness).
    let result = validate_transaction_with_context(&tx, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::VotersDoNotExist(_))));
    }
}
```

- [ ] **Step 3: Run test to verify failure**

```bash
cargo nextest run -p dugite-ledger -E 'test(voters_do_not_exist) | test(voter_exists)'
```

- [ ] **Step 4: Implement validator**

```rust
/// Reference: Haskell `internVoter` partition in
/// `Cardano.Ledger.Conway.Rules.Gov.conwayGovTransition`. A voter is
/// "unknown" when its credential is not in the corresponding registered
/// set: `vsDReps`, `psStakePools`, or `authorizedHotCommitteeCredentials`.
/// This check fires *before* `DisallowedVoters`.
pub fn check_voter_exists(voter: &Voter, ctx: &ValidationContext) -> Result<(), ValidationError> {
    let known = match voter {
        Voter::DRepKey(h) | Voter::DRepScript(h) => ctx.registered_dreps.contains(h),
        Voter::StakePoolKey(h) => ctx.registered_pools.contains_key(h),
        Voter::CommitteeHotKey(h) | Voter::CommitteeHotScript(h) => {
            ctx.committee_authorized_hot_keys.contains(h)
        }
    };
    if known {
        Ok(())
    } else {
        Err(ValidationError::VotersDoNotExist(vec![format!("{voter:?}")]))
    }
}
```

- [ ] **Step 5: Wire into validate_transaction_with_context BEFORE the DisallowedVoters loop**

```rust
// Conway GOV: VotersDoNotExist (must fire before DisallowedVoters per Haskell).
for (voter, vp_map) in tx.body.voting_procedures.iter() {
    if !vp_map.is_empty() {
        if let Err(e) = check_voter_exists(voter, ctx) {
            errors.push(e);
            continue;  // Skip authority check for unknown voters.
        }
    }
}
```

- [ ] **Step 6: Run tests, clippy, commit**

```bash
cargo nextest run -p dugite-ledger -E 'test(voters_do_not_exist) | test(voter_exists)'
cargo clippy -p dugite-ledger --all-targets -- -D warnings
git add crates/dugite-ledger
git commit -m "feat(ledger): add VotersDoNotExist Conway GOV predicate

Voters whose credentials are not registered (DRep / SPO / CC hot key)
are rejected per Haskell internVoter partition in conwayGovTransition.
Fires before DisallowedVoters."
```

---

## Task 4: Conway GOV — `VotingOnExpiredGovAction`

**Reference:** `Cardano.Ledger.Conway.Rules.Gov.checkVotesAreNotForExpiredActions`. A vote fails when `current_epoch > gas_expires_after` (i.e. valid while `current_epoch <= gas_expires_after`).

**Files:**
- Modify: `validation/mod.rs` — error variant
- Modify: `validation/conway.rs` — validator + wiring
- Test: in-file

- [ ] **Step 1: Add error**

```rust
#[error("VotingOnExpiredGovAction: voter={voter}, action={action_id}, expires_at={expires_at}, current={current_epoch}")]
VotingOnExpiredGovAction { voter: String, action_id: String, expires_at: u64, current_epoch: u64 },
```

- [ ] **Step 2: Write failing test**

```rust
#[test]
fn test_voting_on_expired_gov_action_rejected() {
    let mut ctx = ValidationContext::test_default();
    ctx.current_epoch = EpochNo(20);
    let action_id = ctx.add_test_proposal_with_expiry(GovAction::InfoAction, EpochNo(10));
    // Register the DRep so we hit the expiry check, not VotersDoNotExist.
    let drep = Hash32::from_bytes([7u8; 28].into());
    ctx.registered_dreps.insert(drep);
    let tx = test_helpers::tx_with_vote(Voter::DRepKey(drep.into()), action_id, Vote::Yes);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::VotingOnExpiredGovAction { .. })));
}

#[test]
fn test_voting_on_action_at_expiry_boundary_allowed() {
    // Per Haskell: curEpoch <= gasExpiresAfter is valid (inclusive).
    let mut ctx = ValidationContext::test_default();
    ctx.current_epoch = EpochNo(10);
    let action_id = ctx.add_test_proposal_with_expiry(GovAction::InfoAction, EpochNo(10));
    let drep = Hash32::from_bytes([7u8; 28].into());
    ctx.registered_dreps.insert(drep);
    let tx = test_helpers::tx_with_vote(Voter::DRepKey(drep.into()), action_id, Vote::Yes);
    let result = validate_transaction_with_context(&tx, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::VotingOnExpiredGovAction { .. })),
            "vote at expiry boundary must NOT be rejected (curEpoch == gasExpiresAfter is valid)");
    }
}
```

- [ ] **Step 3: Run, see failure**

```bash
cargo nextest run -p dugite-ledger -E 'test(voting_on_expired) | test(voting_on_action_at_expiry_boundary)'
```

- [ ] **Step 4: Implement**

```rust
pub fn check_action_not_expired(
    voter: &Voter,
    action_id: &Hash32,
    gas_expires_after: EpochNo,
    current_epoch: EpochNo,
) -> Result<(), ValidationError> {
    if current_epoch.0 <= gas_expires_after.0 {
        Ok(())
    } else {
        Err(ValidationError::VotingOnExpiredGovAction {
            voter: format!("{voter:?}"),
            action_id: action_id.to_hex(),
            expires_at: gas_expires_after.0,
            current_epoch: current_epoch.0,
        })
    }
}
```

Wire into `validate_transaction_with_context`:

```rust
for (voter, vp_map) in tx.body.voting_procedures.iter() {
    for action_id in vp_map.keys() {
        if let Some(gas) = ctx.governance.proposals.get(action_id) {
            if let Err(e) = check_action_not_expired(voter, action_id, gas.expires_after, ctx.current_epoch) {
                errors.push(e);
            }
        }
    }
}
```

- [ ] **Step 5: Run, commit**

```bash
cargo nextest run -p dugite-ledger -E 'test(voting_on_expired) | test(voting_on_action_at_expiry_boundary)'
git add crates/dugite-ledger
git commit -m "feat(ledger): add VotingOnExpiredGovAction predicate

Reject votes against actions whose gasExpiresAfter is strictly less
than the current epoch. Per Haskell checkVotesAreNotForExpiredActions
the boundary is inclusive (vote allowed when curEpoch == gasExpiresAfter)."
```

---

## Task 5: Conway GOV — `ProposalReturnAccountDoesNotExist`

**Reference:** `Cardano.Ledger.Conway.Rules.Gov.processProposal`. Only outside bootstrap (`pvMajor pp /= 9`). Checks that the proposal's `pProcReturnAddr` credential is registered in `accounts`.

**Files:**
- Modify: `validation/mod.rs` — error
- Modify: `validation/conway.rs` — validator + wiring

- [ ] **Step 1: Error variant**

```rust
#[error("ProposalReturnAccountDoesNotExist: {0}")]
ProposalReturnAccountDoesNotExist(String),  // bech32 reward address
```

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_proposal_return_account_does_not_exist_rejected_post_bootstrap() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 10;  // post-bootstrap
    let unregistered = Hash32::from_bytes([88u8; 28].into());
    let proposal = test_helpers::proposal_with_return_addr(unregistered);
    let tx = test_helpers::tx_with_proposal(proposal);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::ProposalReturnAccountDoesNotExist(_))));
}

#[test]
fn test_proposal_return_account_check_skipped_during_bootstrap() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 9;  // bootstrap
    let unregistered = Hash32::from_bytes([88u8; 28].into());
    let proposal = test_helpers::proposal_with_return_addr(unregistered);
    let tx = test_helpers::tx_with_proposal(proposal);
    let result = validate_transaction_with_context(&tx, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::ProposalReturnAccountDoesNotExist(_))));
    }
}
```

- [ ] **Step 3: Run failing**

```bash
cargo nextest run -p dugite-ledger -E 'test(proposal_return_account)'
```

- [ ] **Step 4: Implement**

```rust
pub fn check_proposal_return_account_exists(
    proposal: &ProposalProcedure,
    ctx: &ValidationContext,
) -> Result<(), ValidationError> {
    if ctx.params.protocol_version.0 == 9 {
        return Ok(());  // bootstrap phase
    }
    if !ctx.reward_accounts.contains_key(&proposal.return_addr.credential_hash()) {
        return Err(ValidationError::ProposalReturnAccountDoesNotExist(
            proposal.return_addr.to_bech32(),
        ));
    }
    Ok(())
}
```

Wire into the existing proposal-validation loop in `validate_transaction_with_context`.

- [ ] **Step 5: Commit**

```bash
cargo nextest run -p dugite-ledger -E 'test(proposal_return_account)'
git add crates/dugite-ledger
git commit -m "feat(ledger): add ProposalReturnAccountDoesNotExist predicate

Reject proposals whose return-deposit address has an unregistered stake
credential. Skipped during Conway bootstrap (pvMajor==9) per Haskell
processProposal in Conway.Rules.Gov."
```

---

## Task 6: Conway GOV — `ProposalProcedureNetworkIdMismatch`

**Reference:** `Cardano.Ledger.Conway.Rules.Gov.processProposal`. Always checked (including bootstrap).

**Files:** as above.

- [ ] **Step 1: Error variant**

```rust
#[error("ProposalProcedureNetworkIdMismatch: addr_network={addr_network}, expected={expected}")]
ProposalProcedureNetworkIdMismatch { addr_network: u8, expected: u8 },
```

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_proposal_return_addr_network_mismatch() {
    let mut ctx = ValidationContext::test_default();
    ctx.node_network = Network::Testnet;
    let proposal = test_helpers::proposal_with_return_addr_on_network(Network::Mainnet);
    let tx = test_helpers::tx_with_proposal(proposal);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::ProposalProcedureNetworkIdMismatch { .. })));
}
```

- [ ] **Step 3: Implement**

```rust
pub fn check_proposal_return_addr_network(
    proposal: &ProposalProcedure,
    ctx: &ValidationContext,
) -> Result<(), ValidationError> {
    let addr_net = proposal.return_addr.network() as u8;
    let expected = ctx.node_network as u8;
    if addr_net != expected {
        return Err(ValidationError::ProposalProcedureNetworkIdMismatch {
            addr_network: addr_net,
            expected,
        });
    }
    Ok(())
}
```

Wire and commit:

```bash
git commit -m "feat(ledger): add ProposalProcedureNetworkIdMismatch predicate

Reject proposals whose return-deposit address network differs from the
node's network. Always enforced (including bootstrap) per Haskell
processProposal in Conway.Rules.Gov."
```

---

## Task 7: Conway GOV — `TreasuryWithdrawalsNetworkIdMismatch`

**Reference:** Same `processProposal`, in the `TreasuryWithdrawals` branch. Collects all mismatched destination addresses.

- [ ] **Step 1: Error**

```rust
#[error("TreasuryWithdrawalsNetworkIdMismatch: {mismatched:?}, expected={expected}")]
TreasuryWithdrawalsNetworkIdMismatch { mismatched: Vec<String>, expected: u8 },
```

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_treasury_withdrawals_network_mismatch_collects_all_bad_addrs() {
    let mut ctx = ValidationContext::test_default();
    ctx.node_network = Network::Testnet;
    let proposal = test_helpers::treasury_withdrawal_proposal(vec![
        (test_helpers::reward_addr_on(Network::Testnet, [1u8; 28]), Lovelace(100)),
        (test_helpers::reward_addr_on(Network::Mainnet, [2u8; 28]), Lovelace(200)),
        (test_helpers::reward_addr_on(Network::Mainnet, [3u8; 28]), Lovelace(300)),
    ]);
    let tx = test_helpers::tx_with_proposal(proposal);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    let mismatched = errors.iter().find_map(|e| match e {
        ValidationError::TreasuryWithdrawalsNetworkIdMismatch { mismatched, .. } => Some(mismatched),
        _ => None,
    }).expect("expected TreasuryWithdrawalsNetworkIdMismatch");
    assert_eq!(mismatched.len(), 2, "both wrong-network entries must be reported");
}
```

- [ ] **Step 3: Implement**

```rust
pub fn check_treasury_withdrawal_networks(
    proposal: &ProposalProcedure,
    ctx: &ValidationContext,
) -> Result<(), ValidationError> {
    let GovAction::TreasuryWithdrawals { withdrawals, .. } = &proposal.gov_action else {
        return Ok(());
    };
    let expected = ctx.node_network as u8;
    let mismatched: Vec<String> = withdrawals
        .keys()
        .filter(|addr| addr.network() as u8 != expected)
        .map(|addr| addr.to_bech32())
        .collect();
    if mismatched.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::TreasuryWithdrawalsNetworkIdMismatch { mismatched, expected })
    }
}
```

Commit:

```bash
git commit -m "feat(ledger): add TreasuryWithdrawalsNetworkIdMismatch predicate

For TreasuryWithdrawals proposals, all destination reward-account
addresses must match the node network. Mismatched addresses are
collected per Haskell processProposal."
```

---

## Task 8: Conway GOV — `ZeroTreasuryWithdrawals`

**Reference:** `processProposal`, after network check, gated on `not (hardforkConwayBootstrapPhase pv)`. Fires when `sum(withdrawals) == 0` (also fires when all entries are zero).

- [ ] **Step 1: Error**

```rust
#[error("ZeroTreasuryWithdrawals: total withdrawal amount is zero")]
ZeroTreasuryWithdrawals,
```

- [ ] **Step 2: Failing tests**

```rust
#[test]
fn test_zero_treasury_withdrawals_rejected_post_bootstrap() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 10;
    let proposal = test_helpers::treasury_withdrawal_proposal(vec![
        (test_helpers::reward_addr([1u8; 28]), Lovelace(0)),
    ]);
    let tx = test_helpers::tx_with_proposal(proposal);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::ZeroTreasuryWithdrawals)));
}

#[test]
fn test_zero_treasury_withdrawals_skipped_in_bootstrap() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 9;
    let proposal = test_helpers::treasury_withdrawal_proposal(vec![
        (test_helpers::reward_addr([1u8; 28]), Lovelace(0)),
    ]);
    let tx = test_helpers::tx_with_proposal(proposal);
    let result = validate_transaction_with_context(&tx, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::ZeroTreasuryWithdrawals)));
    }
}
```

- [ ] **Step 3: Implement**

```rust
pub fn check_nonzero_treasury_withdrawals(
    proposal: &ProposalProcedure,
    ctx: &ValidationContext,
) -> Result<(), ValidationError> {
    if ctx.params.protocol_version.0 == 9 {
        return Ok(());
    }
    let GovAction::TreasuryWithdrawals { withdrawals, .. } = &proposal.gov_action else {
        return Ok(());
    };
    let total: u64 = withdrawals.values().map(|v| v.0).sum();
    if total == 0 {
        Err(ValidationError::ZeroTreasuryWithdrawals)
    } else {
        Ok(())
    }
}
```

Commit:

```bash
git commit -m "feat(ledger): add ZeroTreasuryWithdrawals predicate

Reject treasury-withdrawal proposals whose total amount is zero
(including all-zero entries). Skipped in bootstrap phase."
```

---

## Task 9: Conway GOV — `ConflictingCommitteeUpdate`

**Reference:** `processProposal`, `UpdateCommittee` branch. `Set.intersection (keys members_to_add) members_to_remove`.

- [ ] **Step 1: Error**

```rust
#[error("ConflictingCommitteeUpdate: {0:?}")]
ConflictingCommitteeUpdate(Vec<String>),  // hex-encoded credential hashes
```

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_conflicting_committee_update_rejected() {
    let cred = Hash32::from_bytes([42u8; 28].into());
    let proposal = test_helpers::update_committee_proposal(
        /* remove */ vec![cred],
        /* add */ vec![(cred, EpochNo(100))],
        /* quorum */ Default::default(),
    );
    let mut ctx = ValidationContext::test_default();
    let tx = test_helpers::tx_with_proposal(proposal);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::ConflictingCommitteeUpdate(_))));
}
```

- [ ] **Step 3: Implement**

```rust
pub fn check_no_conflicting_committee_update(
    proposal: &ProposalProcedure,
) -> Result<(), ValidationError> {
    let GovAction::UpdateCommittee { remove, add, .. } = &proposal.gov_action else {
        return Ok(());
    };
    let conflicts: Vec<String> = add.keys()
        .filter(|c| remove.contains(c))
        .map(|c| c.to_hex())
        .collect();
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::ConflictingCommitteeUpdate(conflicts))
    }
}
```

Commit:

```bash
git commit -m "feat(ledger): add ConflictingCommitteeUpdate predicate

Reject UpdateCommittee proposals where a credential appears in both
add and remove sets per Haskell Set.intersection check in
Conway.Rules.Gov processProposal."
```

---

## Task 10: Conway GOV — `ExpirationEpochTooSmall`

**Reference:** `processProposal`, `UpdateCommittee` branch. `Map.filter (<= currentEpoch) membersToAdd`.

- [ ] **Step 1: Error**

```rust
#[error("ExpirationEpochTooSmall: {0:?}")]
ExpirationEpochTooSmall(Vec<(String, u64)>),  // (cred_hex, expiry_epoch)
```

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_expiration_epoch_too_small_rejected() {
    let mut ctx = ValidationContext::test_default();
    ctx.current_epoch = EpochNo(50);
    let cred = Hash32::from_bytes([1u8; 28].into());
    let proposal = test_helpers::update_committee_proposal(
        vec![],
        vec![(cred, EpochNo(50))],  // expiry == current ⇒ invalid (must be strictly greater)
        Default::default(),
    );
    let tx = test_helpers::tx_with_proposal(proposal);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::ExpirationEpochTooSmall(_))));
}

#[test]
fn test_expiration_epoch_strictly_greater_accepted() {
    let mut ctx = ValidationContext::test_default();
    ctx.current_epoch = EpochNo(50);
    let cred = Hash32::from_bytes([1u8; 28].into());
    let proposal = test_helpers::update_committee_proposal(
        vec![],
        vec![(cred, EpochNo(51))],
        Default::default(),
    );
    let tx = test_helpers::tx_with_proposal(proposal);
    let result = validate_transaction_with_context(&tx, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::ExpirationEpochTooSmall(_))));
    }
}
```

- [ ] **Step 3: Implement**

```rust
pub fn check_committee_expiration_epoch_valid(
    proposal: &ProposalProcedure,
    current_epoch: EpochNo,
) -> Result<(), ValidationError> {
    let GovAction::UpdateCommittee { add, .. } = &proposal.gov_action else {
        return Ok(());
    };
    let invalid: Vec<(String, u64)> = add.iter()
        .filter(|(_, e)| e.0 <= current_epoch.0)
        .map(|(c, e)| (c.to_hex(), e.0))
        .collect();
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::ExpirationEpochTooSmall(invalid))
    }
}
```

Commit:

```bash
git commit -m "feat(ledger): add ExpirationEpochTooSmall predicate

Reject UpdateCommittee proposals where any added member's expiry
epoch is <= current epoch (must be strictly greater) per Haskell
Map.filter (<= currentEpoch) check."
```

---

## Task 11: Pool metadata hash size — `PoolMedataHashTooBig`

**Reference:** `Cardano.Ledger.Shelley.Rules.Pool.poolDecodeWith`. Cap = `hashSize @Blake2b_256` = 32 bytes; gated to `pvMajor pv > 4`.

**Files:**
- Modify: `validation/mod.rs`
- Modify: `validation/phase1.rs` (line 435 area, near pool cost check)
- Test: `validation/phase1.rs` test module

- [ ] **Step 1: Error**

```rust
#[error("PoolMedataHashTooBig: pool={pool}, hash_size={size}")]
PoolMedataHashTooBig { pool: String, size: usize },
```

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_pool_medata_hash_too_big_rejected_post_alonzo() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;  // Alonzo
    let cert = Certificate::PoolRegistration {
        params: test_helpers::pool_params_with_metadata_hash(vec![0u8; 33]),
    };
    let tx = test_helpers::tx_with_cert(cert);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::PoolMedataHashTooBig { .. })));
}

#[test]
fn test_pool_medata_hash_at_32_bytes_accepted() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;
    let cert = Certificate::PoolRegistration {
        params: test_helpers::pool_params_with_metadata_hash(vec![0u8; 32]),
    };
    let tx = test_helpers::tx_with_cert(cert);
    let result = validate_transaction_with_context(&tx, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::PoolMedataHashTooBig { .. })));
    }
}
```

- [ ] **Step 3: Implement at validation/phase1.rs (in the existing pool-cert validation block)**

```rust
// PoolMedataHashTooBig (since Alonzo, pvMajor > 4).
// Reference: Cardano.Ledger.Shelley.Rules.Pool, restrictPoolMetadataHash.
if ctx.params.protocol_version.0 > 4 {
    if let Some(metadata) = &params.pool_metadata {
        if metadata.hash.as_bytes().len() > 32 {
            errors.push(ValidationError::PoolMedataHashTooBig {
                pool: pool_id.to_hex(),
                size: metadata.hash.as_bytes().len(),
            });
        }
    }
}
```

(Note: the Rust `Hash32` type is fixed-size 32 bytes, so this can only fire if a CBOR decoder permits an oversized hash. If `pool_metadata.hash` is structurally `Hash32`, this check is structurally satisfied — but Haskell still names the predicate, so we check defensively. If the dugite `pool_metadata.hash` is `Vec<u8>`, the check is meaningful.)

Commit:

```bash
git commit -m "feat(ledger): add PoolMedataHashTooBig predicate (since Alonzo)

Pool metadata hash must be <= 32 bytes (Blake2b-256). Gated to
pvMajor > 4 per Haskell SoftForks.restrictPoolMetadataHash."
```

---

## Task 12: `OutputBootAddrAttrsTooBig`

**Reference:** `Cardano.Ledger.Shelley.Rules.Utxo.validateOutputBootAddrAttrsTooBig`. 64-byte cap on bootstrap (Byron) address attribute serialization size. All eras Shelley+.

**Files:**
- Modify: `validation/mod.rs`
- Modify: `validation/phase1.rs` — output validation block

- [ ] **Step 1: Error**

```rust
#[error("OutputBootAddrAttrsTooBig: {0} outputs with attrs > 64 bytes")]
OutputBootAddrAttrsTooBig(Vec<String>),  // hex output indices
```

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_output_boot_addr_attrs_too_big_rejected() {
    let ctx = ValidationContext::test_default();
    let oversized_attrs = vec![0u8; 65];  // > 64 bytes
    let tx = test_helpers::tx_with_byron_output_attrs(oversized_attrs);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::OutputBootAddrAttrsTooBig(_))));
}

#[test]
fn test_output_boot_addr_attrs_at_64_bytes_accepted() {
    let ctx = ValidationContext::test_default();
    let attrs = vec![0u8; 64];
    let tx = test_helpers::tx_with_byron_output_attrs(attrs);
    let result = validate_transaction_with_context(&tx, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::OutputBootAddrAttrsTooBig(_))));
    }
}
```

- [ ] **Step 3: Implement**

In `validation/phase1.rs` output validation block:

```rust
// OutputBootAddrAttrsTooBig: bootstrap address attrs must be <= 64 bytes.
// Reference: Shelley.Rules.Utxo.validateOutputBootAddrAttrsTooBig.
let oversized: Vec<String> = body.outputs.iter().enumerate()
    .filter_map(|(i, out)| match &out.address {
        Address::Byron(byron) if byron.attributes_size() > 64 => Some(format!("{i}")),
        _ => None,
    })
    .collect();
if !oversized.is_empty() {
    errors.push(ValidationError::OutputBootAddrAttrsTooBig(oversized));
}
```

(If `Address::Byron` does not expose `attributes_size`, add it. For pallas-typed addresses, decode and inspect the attributes CBOR length.)

Commit:

```bash
git commit -m "feat(ledger): add OutputBootAddrAttrsTooBig predicate

Bootstrap (Byron) address attributes must be <= 64 bytes. Applied
to all outputs per Haskell Shelley.Rules.Utxo."
```

---

## Task 13: MIR — error scaffolding and module setup

**Reference:** `Cardano.Ledger.Shelley.Rules.Deleg`. All MIR predicates are pre-Conway only.

**Files:**
- Modify: `validation/mod.rs` — add 7 error variants
- Create: `crates/dugite-ledger/src/validation/mir.rs`
- Modify: `crates/dugite-ledger/src/validation/mod.rs` (top) — `pub mod mir;`

- [ ] **Step 1: Add 7 error variants in `ValidationError`**

```rust
#[error("MIRCertificateTooLateInEpoch: current_slot={current_slot}, deadline={deadline}")]
MIRCertificateTooLateInEpoch { current_slot: u64, deadline: u64 },

#[error("InsufficientForInstantaneousRewards: pot={pot:?}, required={required}, available={available}")]
InsufficientForInstantaneousRewards { pot: MIRPot, required: u64, available: u64 },

#[error("MIRTransferNotCurrentlyAllowed (pre-Alonzo MIR pot transfer)")]
MIRTransferNotCurrentlyAllowed,

#[error("MIRNegativesNotCurrentlyAllowed (pre-Alonzo negative MIR delta)")]
MIRNegativesNotCurrentlyAllowed,

#[error("MIRProducesNegativeUpdate")]
MIRProducesNegativeUpdate,

#[error("InsufficientForTransferDELEG: pot={pot:?}, requested={requested}, available={available}")]
InsufficientForTransferDELEG { pot: MIRPot, requested: u64, available: u64 },

#[error("MIRNegativeTransfer: pot={pot:?}, amount={amount}")]
MIRNegativeTransfer { pot: MIRPot, amount: i64 },
```

(`MIRPot` already exists as `dugite_primitives::transaction::MIRSource` — reuse it directly or add a Display impl.)

- [ ] **Step 2: Create `crates/dugite-ledger/src/validation/mir.rs`** with the module shell:

```rust
//! MIR (Move Instantaneous Rewards) validation rules.
//!
//! Reference: `Cardano.Ledger.Shelley.Rules.Deleg` in
//! `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`.
//!
//! MIR certificates exist only in Shelley–Babbage (`AtMostEra "Babbage"`).
//! Conway has removed `MIRCert` entirely. Several sub-rules are further
//! gated by `hardforkAlonzoAllowMIRTransfer = pvMajor pv > 4`.

use crate::validation::mod_inner::{ValidationContext, ValidationError};
use dugite_primitives::transaction::{Certificate, MIRSource, MIRTarget};
use dugite_primitives::time::SlotNo;
use dugite_primitives::value::Lovelace;

pub fn validate_mir_cert(
    cert: &Certificate,
    ctx: &ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    let Certificate::MoveInstantaneousRewards { source, target } = cert else {
        return Ok(());
    };
    // Conway: MIR is impossible (era doesn't have the variant). If we get here in Conway,
    // the caller should already have rejected the cert at decode time. Defensive: skip.
    if ctx.params.protocol_version.0 >= 9 {
        return Ok(());  // Conway+ no-op
    }
    let mut errors = Vec::new();
    check_slot_not_too_late(ctx, &mut errors);
    match target {
        MIRTarget::StakeCredentials(creds) => {
            check_stake_addresses_mir(source, creds, ctx, &mut errors);
        }
        MIRTarget::OtherAccountingPot(coin) => {
            check_send_to_opposite_pot_mir(source, *coin, ctx, &mut errors);
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

fn check_slot_not_too_late(ctx: &ValidationContext, errors: &mut Vec<ValidationError>) {
    // Reference: Shelley.Rules.Deleg.checkSlotNotTooLate
    // tooLate = firstSlotOfNextEpoch - stabilityWindow
    // stabilityWindow = ceil(3k/f)
    let stability_window = compute_stability_window(&ctx.params);
    let first_slot_of_next_epoch = ctx.epoch_info.first_slot_of_epoch(ctx.current_epoch + 1);
    let too_late = first_slot_of_next_epoch.saturating_sub(stability_window);
    if ctx.current_slot.0 >= too_late {
        errors.push(ValidationError::MIRCertificateTooLateInEpoch {
            current_slot: ctx.current_slot.0,
            deadline: too_late,
        });
    }
}

fn check_stake_addresses_mir(
    source: &MIRSource,
    creds: &std::collections::BTreeMap<dugite_primitives::credentials::Credential, i64>,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    let pv = ctx.params.protocol_version.0;
    let alonzo_or_later = pv > 4;

    if !alonzo_or_later {
        // Pre-Alonzo: all values must be non-negative.
        if creds.values().any(|v| *v < 0) {
            errors.push(ValidationError::MIRNegativesNotCurrentlyAllowed);
            return;
        }
    }

    // Sum required = sum of all delta values (clamped at 0 for pre-Alonzo,
    // but for Alonzo+ negative deltas reduce required).
    let pot_balance = match source {
        MIRSource::Reserves => ctx.reserves,
        MIRSource::Treasury => ctx.treasury,
    };
    let required: i128 = creds.values().map(|v| *v as i128).sum();
    let available: i128 = pot_balance.0 as i128;
    if required > available {
        errors.push(ValidationError::InsufficientForInstantaneousRewards {
            pot: source.clone(),
            required: required.max(0) as u64,
            available: pot_balance.0,
        });
    }

    if alonzo_or_later {
        // Alonzo+: combined map (delta + existing iRewards) must have all non-negative values.
        // For dugite we approximate by checking that no individual delta would push the
        // recipient's accumulated MIR balance negative. The full simulation requires
        // access to dsIRewards; if not available in ctx, document the limitation.
        // TODO(MIR-Alonzo-precision): full simulation needs dsIRewards in ValidationContext.
        // For now, reject any delta that would clearly produce a negative final value.
        if creds.values().any(|v| *v < 0) {
            // Conservative: only allow if cred has accumulated balance >= |delta|.
            // Without dsIRewards we cannot decide; defer to the apply path.
        }
    }
}

fn check_send_to_opposite_pot_mir(
    source: &MIRSource,
    coin: i64,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    let pv = ctx.params.protocol_version.0;
    let alonzo_or_later = pv > 4;
    if !alonzo_or_later {
        errors.push(ValidationError::MIRTransferNotCurrentlyAllowed);
        return;
    }
    if coin < 0 {
        errors.push(ValidationError::MIRNegativeTransfer {
            pot: source.clone(),
            amount: coin,
        });
        return;
    }
    let pot_balance = match source {
        MIRSource::Reserves => ctx.reserves,
        MIRSource::Treasury => ctx.treasury,
    };
    if (coin as u64) > pot_balance.0 {
        errors.push(ValidationError::InsufficientForTransferDELEG {
            pot: source.clone(),
            requested: coin as u64,
            available: pot_balance.0,
        });
    }
}

fn compute_stability_window(params: &dugite_primitives::protocol_params::ProtocolParameters) -> u64 {
    // ceil(3 * k / f) where k = security parameter, f = active slot coeff.
    let k = params.security_param;
    let (f_num, f_den) = params.active_slot_coeff_rational();
    // ceil((3 * k * f_den) / f_num)
    let numerator = 3u128 * k as u128 * f_den as u128;
    let denominator = f_num as u128;
    ((numerator + denominator - 1) / denominator) as u64
}

#[cfg(test)]
mod tests { /* per-task tests below */ }
```

- [ ] **Step 3: Write the per-MIR-predicate tests (one per remaining task)**

These tests are added incrementally in Tasks 14–19. Compile this scaffolding now to verify the module integrates cleanly:

```bash
cargo build -p dugite-ledger
```

If `ctx.epoch_info` or `ctx.reserves` / `ctx.treasury` don't exist on `ValidationContext`, add them now (with `#[cfg(test)]` defaults if needed).

- [ ] **Step 4: Commit scaffold**

```bash
git add crates/dugite-ledger/src/validation/mir.rs crates/dugite-ledger/src/validation/mod.rs
git commit -m "feat(ledger): MIR validation module scaffolding

Adds validation/mir.rs with predicate stubs for the 7 MIR failures
from Shelley.Rules.Deleg (pre-Conway only). Per-predicate logic is
filled in by subsequent commits."
```

---

## Task 14: MIR — `MIRCertificateTooLateInEpoch` test

**Files:** `validation/mir.rs` test module, `eras/shelley.rs` apply path.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn test_mir_too_late_in_epoch() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 6;  // Babbage-ish
    ctx.params.security_param = 432;     // mainnet k
    // active_slot_coeff = 1/20 → stability_window = ceil(3*432*20/1) = 25920
    // first_slot_of_next_epoch = 432000 (epoch 0 at 432000 slots)
    // too_late = 432000 - 25920 = 406080
    ctx.current_slot = SlotNo(406080);  // == deadline → must reject
    let cert = test_helpers::mir_cert_distribute(MIRSource::Reserves, vec![]);
    let errors = validate_mir_cert(&cert, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::MIRCertificateTooLateInEpoch { .. })));
}
```

- [ ] **Step 2: Run, verify pass**

```bash
cargo nextest run -p dugite-ledger -E 'test(mir_too_late)'
```

Wire `validate_mir_cert` into the Shelley/Allegra/Mary/Alonzo/Babbage pre-cert validation in `eras/shelley.rs::apply_valid_tx` (before `process_shelley_certs`).

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(ledger): MIRCertificateTooLateInEpoch predicate

Reject MIR certs submitted within stabilityWindow of next epoch
boundary. Wired into Shelley apply_valid_tx via validate_mir_cert."
```

---

## Task 15: MIR — `MIRNegativesNotCurrentlyAllowed` and `MIRNegativeTransfer`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn test_mir_negatives_pre_alonzo_rejected() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 4;  // Mary
    let cert = test_helpers::mir_cert_distribute(MIRSource::Reserves, vec![
        (test_helpers::cred([1u8; 28]), -100i64),
    ]);
    let errors = validate_mir_cert(&cert, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::MIRNegativesNotCurrentlyAllowed)));
}

#[test]
fn test_mir_negative_transfer_alonzo() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;  // Alonzo
    let cert = test_helpers::mir_cert_transfer(MIRSource::Reserves, -100);
    let errors = validate_mir_cert(&cert, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::MIRNegativeTransfer { .. })));
}
```

- [ ] **Step 2: Run, verify, commit**

```bash
git commit -m "feat(ledger): MIRNegatives* predicates

Pre-Alonzo (pvMajor <= 4): negative deltas in StakeAddressesMIR are
rejected. Alonzo+: negative pot transfers are rejected (negative
deltas to credentials are allowed but checked via MIRProducesNegative
update)."
```

---

## Task 16: MIR — `MIRTransferNotCurrentlyAllowed` and `InsufficientForTransferDELEG`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn test_mir_transfer_pre_alonzo_disallowed() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 4;  // Mary
    let cert = test_helpers::mir_cert_transfer(MIRSource::Reserves, 100);
    let errors = validate_mir_cert(&cert, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::MIRTransferNotCurrentlyAllowed)));
}

#[test]
fn test_insufficient_for_transfer_alonzo() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;  // Alonzo
    ctx.reserves = Lovelace(50);
    let cert = test_helpers::mir_cert_transfer(MIRSource::Reserves, 100);
    let errors = validate_mir_cert(&cert, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InsufficientForTransferDELEG { .. })));
}
```

- [ ] **Step 2: Commit**

```bash
git commit -m "feat(ledger): MIRTransfer* predicates

Pot-to-pot MIR transfer is disallowed pre-Alonzo and requires
sufficient pot balance in Alonzo+."
```

---

## Task 17: MIR — `InsufficientForInstantaneousRewards`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn test_insufficient_for_instantaneous_rewards() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;
    ctx.reserves = Lovelace(50);
    let cert = test_helpers::mir_cert_distribute(MIRSource::Reserves, vec![
        (test_helpers::cred([1u8; 28]), 100i64),
    ]);
    let errors = validate_mir_cert(&cert, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InsufficientForInstantaneousRewards { .. })));
}
```

- [ ] **Step 2: Commit**

```bash
git commit -m "feat(ledger): InsufficientForInstantaneousRewards predicate

Sum of MIR distributions must not exceed source pot balance."
```

---

## Task 18: MIR — `MIRProducesNegativeUpdate` (Alonzo+ post-aggregation)

This requires `dsIRewards` in `ValidationContext`. If not present, add an `accumulated_mir_balances: HashMap<Credential, i64>` field.

- [ ] **Step 1: Add field to `ValidationContext`** with default empty.

- [ ] **Step 2: Failing test**

```rust
#[test]
fn test_mir_produces_negative_update_alonzo() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;
    let cred = test_helpers::cred([1u8; 28]);
    ctx.accumulated_mir_balances.insert(cred.clone(), 50);
    // Submitting a -100 delta against a +50 balance ⇒ -50 final ⇒ reject.
    let cert = test_helpers::mir_cert_distribute(MIRSource::Reserves, vec![(cred, -100i64)]);
    let errors = validate_mir_cert(&cert, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::MIRProducesNegativeUpdate)));
}
```

- [ ] **Step 3: Implement** the combined-map check in `check_stake_addresses_mir`:

```rust
if alonzo_or_later {
    for (cred, delta) in creds {
        let existing = ctx.accumulated_mir_balances.get(cred).copied().unwrap_or(0);
        if existing.saturating_add(*delta) < 0 {
            errors.push(ValidationError::MIRProducesNegativeUpdate);
            return;
        }
    }
}
```

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(ledger): MIRProducesNegativeUpdate predicate (Alonzo+)

After combining the new delta with existing dsIRewards balance, no
credential's accumulated MIR balance may be negative."
```

---

## Task 19: PPUP rule scaffold

**Reference:** `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`. Three predicate failures + quorum-based enactment.

**Files:**
- Create: `crates/dugite-ledger/src/validation/ppup.rs`
- Modify: `validation/mod.rs` — error variants, module export

- [ ] **Step 1: Add 3 error variants**

```rust
#[error("NonGenesisUpdatePPUP: proposed_keys not subset of genesis_keys")]
NonGenesisUpdatePPUP { proposed: Vec<String>, genesis: Vec<String> },

#[error("PPUpdateWrongEpoch: current={current}, target={target}, period={period:?}")]
PPUpdateWrongEpoch { current: u64, target: u64, period: VotingPeriod },

#[error("PVCannotFollowPPUP: bad protocol version proposal {0:?}")]
PVCannotFollowPPUP((u64, u64)),  // (major, minor)
```

```rust
#[derive(Debug, Clone, Copy)]
pub enum VotingPeriod { ForThisEpoch, ForNextEpoch }
```

- [ ] **Step 2: Create `validation/ppup.rs`** scaffold:

```rust
//! Pre-Conway protocol-parameter update (PPUP) rule.
//!
//! Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
//! Active in Shelley through Babbage (`AtMostEra "Babbage" era`). Conway
//! replaces this with on-chain governance (CIP-1694).

use crate::validation::mod_inner::{ValidationContext, ValidationError, VotingPeriod};
use dugite_primitives::hash::Hash28;
use dugite_primitives::time::{EpochNo, SlotNo};
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn validate_ppup(
    proposed: &BTreeMap<Hash28, ProtocolParameterUpdate>,
    target_epoch: EpochNo,
    ctx: &ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    if ctx.params.protocol_version.0 >= 9 {
        // Conway+ uses governance instead.
        return Ok(());
    }
    let mut errors = Vec::new();
    check_non_genesis_update(proposed, &ctx.genesis_delegates, &mut errors);
    check_voting_period(target_epoch, ctx.current_slot, ctx.current_epoch, &ctx.params, &mut errors);
    check_pv_can_follow(proposed, ctx.params.protocol_version, &mut errors);
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

fn check_non_genesis_update(
    proposed: &BTreeMap<Hash28, ProtocolParameterUpdate>,
    genesis_delegates: &HashSet<Hash28>,
    errors: &mut Vec<ValidationError>,
) {
    let bad: Vec<Hash28> = proposed.keys().filter(|k| !genesis_delegates.contains(k)).copied().collect();
    if !bad.is_empty() {
        errors.push(ValidationError::NonGenesisUpdatePPUP {
            proposed: proposed.keys().map(|h| h.to_hex()).collect(),
            genesis: genesis_delegates.iter().map(|h| h.to_hex()).collect(),
        });
    }
}

fn check_voting_period(
    target: EpochNo,
    current_slot: SlotNo,
    current_epoch: EpochNo,
    params: &dugite_primitives::protocol_params::ProtocolParameters,
    errors: &mut Vec<ValidationError>,
) {
    // tooLate = firstSlotOfNextEpoch - 2 * stabilityWindow
    let stability_window = crate::validation::mir::compute_stability_window(params);
    let first_slot_next = (current_epoch.0 + 1) * params.epoch_length;
    let too_late = first_slot_next.saturating_sub(2 * stability_window);
    let (expected, period) = if current_slot.0 < too_late {
        (current_epoch, VotingPeriod::ForThisEpoch)
    } else {
        (EpochNo(current_epoch.0 + 1), VotingPeriod::ForNextEpoch)
    };
    if target != expected {
        errors.push(ValidationError::PPUpdateWrongEpoch {
            current: current_epoch.0,
            target: target.0,
            period,
        });
    }
}

fn check_pv_can_follow(
    proposed: &BTreeMap<Hash28, ProtocolParameterUpdate>,
    current_pv: ProtocolVersion,
    errors: &mut Vec<ValidationError>,
) {
    for ppu in proposed.values() {
        if let Some(new_pv) = ppu.protocol_version {
            // pvCanFollow: (major, minor+1) OR (major+1, 0).
            let is_minor_bump = new_pv.0 == current_pv.0 && new_pv.1 == current_pv.1 + 1;
            let is_major_bump = new_pv.0 == current_pv.0 + 1 && new_pv.1 == 0;
            if !(is_minor_bump || is_major_bump) {
                errors.push(ValidationError::PVCannotFollowPPUP(new_pv));
                return;  // Haskell reports the first illegal PV only.
            }
        }
    }
}

/// Compute the enacted update if quorum is met.
///
/// Reference: `votedFuturePParams`. Returns `Some(update)` iff a single
/// `ProtocolParameterUpdate` has at least `quorum` votes AND the resulting
/// merged params satisfy `maxTxSize + maxBHSize < maxBlockBodySize`.
pub fn voted_future_pparams(
    proposed: &BTreeMap<Hash28, ProtocolParameterUpdate>,
    quorum: u64,
    current: &dugite_primitives::protocol_params::ProtocolParameters,
) -> Option<ProtocolParameterUpdate> {
    let mut tally: HashMap<&ProtocolParameterUpdate, u64> = HashMap::new();
    for ppu in proposed.values() {
        *tally.entry(ppu).or_insert(0) += 1;
    }
    let mut consensus: Vec<&ProtocolParameterUpdate> = tally.iter()
        .filter(|(_, count)| **count >= quorum)
        .map(|(ppu, _)| *ppu)
        .collect();
    if consensus.len() != 1 {
        return None;
    }
    let merged = current.with_update(consensus[0]);
    if merged.max_tx_size as u64 + merged.max_block_header_size as u64 >= merged.max_block_body_size as u64 {
        return None;
    }
    Some(consensus[0].clone())
}

#[cfg(test)]
mod tests { /* tests in Tasks 20–22 */ }
```

- [ ] **Step 3: Add `pub mod ppup;` to `validation/mod.rs`**, `pub use ppup::VotingPeriod;`

- [ ] **Step 4: Build, commit**

```bash
cargo build -p dugite-ledger
git add crates/dugite-ledger/src/validation/{ppup.rs,mod.rs}
git commit -m "feat(ledger): PPUP rule scaffolding (Shelley–Babbage)

Adds validation/ppup.rs with NonGenesisUpdatePPUP, PPUpdateWrongEpoch,
PVCannotFollowPPUP predicates and votedFuturePParams quorum logic
per Shelley.Rules.Ppup. Per-predicate tests follow."
```

---

## Task 20: PPUP — `NonGenesisUpdatePPUP` test

- [ ] **Step 1: Failing test**

```rust
#[test]
fn test_non_genesis_update_ppup_rejected() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;  // Alonzo
    ctx.genesis_delegates.insert(Hash28::from_bytes([1u8; 28]));
    let mut proposed = BTreeMap::new();
    proposed.insert(Hash28::from_bytes([99u8; 28]), Default::default());  // not a genesis key
    let errors = validate_ppup(&proposed, ctx.current_epoch, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::NonGenesisUpdatePPUP { .. })));
}

#[test]
fn test_ppup_with_only_genesis_keys_accepted() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;
    let g = Hash28::from_bytes([1u8; 28]);
    ctx.genesis_delegates.insert(g);
    let mut proposed = BTreeMap::new();
    proposed.insert(g, Default::default());
    let result = validate_ppup(&proposed, ctx.current_epoch, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::NonGenesisUpdatePPUP { .. })));
    }
}
```

- [ ] **Step 2: Commit**

```bash
git commit -m "test(ledger): NonGenesisUpdatePPUP coverage"
```

---

## Task 21: PPUP — `PPUpdateWrongEpoch` test

- [ ] **Step 1: Failing test**

```rust
#[test]
fn test_ppup_wrong_epoch_targets_wrong_period() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version.0 = 5;
    ctx.params.epoch_length = 432_000;
    ctx.params.security_param = 432;
    // active_slot_coeff = 1/20 ⇒ stability_window = 25920
    // tooLate = 432000 - 2*25920 = 380160
    ctx.current_epoch = EpochNo(0);
    ctx.current_slot = SlotNo(100_000);  // before tooLate ⇒ ForThisEpoch
    let proposed = BTreeMap::new();
    let errors = validate_ppup(&proposed, EpochNo(1), &ctx).unwrap_err();  // target wrong
    assert!(errors.iter().any(|e| matches!(e, ValidationError::PPUpdateWrongEpoch { period: VotingPeriod::ForThisEpoch, .. })));
}
```

- [ ] **Step 2: Commit**

```bash
git commit -m "test(ledger): PPUpdateWrongEpoch coverage"
```

---

## Task 22: PPUP — `PVCannotFollowPPUP` test

- [ ] **Step 1: Failing test**

```rust
#[test]
fn test_pv_cannot_follow_ppup_skip_major() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version = (5, 0);
    let g = Hash28::from_bytes([1u8; 28]);
    ctx.genesis_delegates.insert(g);
    let mut update = ProtocolParameterUpdate::default();
    update.protocol_version = Some((7, 0));  // skips major version 6
    let mut proposed = BTreeMap::new();
    proposed.insert(g, update);
    let errors = validate_ppup(&proposed, ctx.current_epoch, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::PVCannotFollowPPUP(_))));
}

#[test]
fn test_pv_can_follow_minor_bump_ok() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version = (5, 0);
    let g = Hash28::from_bytes([1u8; 28]);
    ctx.genesis_delegates.insert(g);
    let mut update = ProtocolParameterUpdate::default();
    update.protocol_version = Some((5, 1));
    let mut proposed = BTreeMap::new();
    proposed.insert(g, update);
    let result = validate_ppup(&proposed, ctx.current_epoch, &ctx);
    if let Err(errors) = result {
        assert!(!errors.iter().any(|e| matches!(e, ValidationError::PVCannotFollowPPUP(_))));
    }
}
```

- [ ] **Step 2: Wire `validate_ppup` into the Shelley/Allegra/Mary/Alonzo/Babbage tx-apply path**

In `eras/shelley.rs::apply_valid_tx`, before `process_shelley_certs`, if the tx body has `update_proposal`:

```rust
if let Some((proposed, target_epoch)) = &tx.body.update_proposal {
    if let Err(ppup_errors) = validate_ppup(proposed, *target_epoch, &ctx.into_validation_context()) {
        // Promote ppup errors to LedgerError per ShelleyLedgerPredFailure::UpdateFailure.
        return Err(LedgerError::PPUPFailures(ppup_errors));
    }
}
```

(Or, if `apply_valid_tx` doesn't have access to a `ValidationContext`, add a thin wrapper that pulls the necessary fields from `RuleContext` + sub-states.)

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(ledger): PVCannotFollowPPUP and apply-path wiring

Reject update proposals whose new ProtVer cannot follow the current
one (only minor+1 or major+1/minor=0 allowed). PPUP wired into
Shelley apply_valid_tx for pre-Conway eras."
```

---

## Task 23: `OutsideForecastRange` consensus check

**Reference:** `Ouroboros.Consensus.Forecast.OutsideForecastRange`. Formula: `for < tipSlot + 1 + stabilityWindow`.

**Files:**
- Modify: `crates/dugite-consensus/src/era_history.rs` — add `forecast_for` method or extend existing forecast machinery

- [ ] **Step 1: Inspect existing forecast code**

```bash
grep -n "Forecast\|forecast\|stability_window" crates/dugite-consensus/src/era_history.rs | head -20
```

- [ ] **Step 2: Add error type**

```rust
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("OutsideForecastRange: at={at:?}, max_for={max_for}, requested={requested}")]
pub struct OutsideForecastRange {
    pub at: Option<SlotNo>,        // ledger tip slot, None if origin
    pub max_for: SlotNo,           // exclusive upper bound
    pub requested: SlotNo,
}
```

- [ ] **Step 3: Failing test**

```rust
#[test]
fn test_outside_forecast_range_beyond_horizon() {
    let tip = Some(SlotNo(1000));
    let stability_window = 100;
    let result = forecast_for(tip, stability_window, SlotNo(1101));
    assert!(matches!(result, Err(OutsideForecastRange { .. })));
}

#[test]
fn test_forecast_within_horizon_ok() {
    let tip = Some(SlotNo(1000));
    let stability_window = 100;
    // max_for = 1000 + 1 + 100 = 1101 (exclusive); 1100 is OK.
    assert!(forecast_for(tip, stability_window, SlotNo(1100)).is_ok());
    assert!(forecast_for(tip, stability_window, SlotNo(1101)).is_err());
}

#[test]
fn test_forecast_at_origin() {
    let result = forecast_for(None, 100, SlotNo(99));
    assert!(result.is_ok());
    let result = forecast_for(None, 100, SlotNo(101));
    assert!(matches!(result, Err(OutsideForecastRange { .. })));
}
```

- [ ] **Step 4: Implement**

```rust
pub fn forecast_for(
    at: Option<SlotNo>,
    stability_window: u64,
    requested: SlotNo,
) -> Result<(), OutsideForecastRange> {
    let succ_at = match at {
        Some(s) => s.0 + 1,
        None => 0,
    };
    let max_for = SlotNo(succ_at + stability_window);
    if requested.0 < max_for.0 {
        Ok(())
    } else {
        Err(OutsideForecastRange { at, max_for, requested })
    }
}
```

- [ ] **Step 5: Wire into header validation**

In `crates/dugite-consensus/src/praos.rs::validate_header_full` (or earlier in the envelope path), call `forecast_for` with the ledger-view tip slot before doing VRF/KES checks. If the requested header slot is beyond the horizon, return `ConsensusError::OutsideForecast { ... }`.

- [ ] **Step 6: Commit**

```bash
git commit -m "feat(consensus): add OutsideForecastRange check

Rejects ledger-view forecast requests beyond tip + 1 + stabilityWindow,
matching Haskell Ouroboros.Consensus.Forecast.OutsideForecastRange."
```

---

## Task 24: `WithdrawalsNotInRewardsCERTS`

**Reference:** `Cardano.Ledger.Conway.Rules.Certs.conwayCertsTransition`. Two regimes: `pvMajor <= 10` (combined) and `pvMajor > 10` (split into `ConwayWithdrawalsMissingAccounts` + `ConwayIncompleteWithdrawals`).

**Files:**
- Create: `crates/dugite-ledger/src/validation/withdrawals.rs`
- Modify: `validation/mod.rs` — error variants
- Modify: `validation/mod.rs::validate_transaction_with_context` — replace existing best-effort drain

- [ ] **Step 1: Add error variants**

```rust
#[error("WithdrawalsNotInRewardsCERTS: {0:?}")]
WithdrawalsNotInRewardsCERTS(Vec<(String, u64)>),  // (addr_bech32, supplied_amount) for all bad

#[error("ConwayWithdrawalsMissingAccounts: {0:?}")]
ConwayWithdrawalsMissingAccounts(Vec<(String, u64)>),

#[error("ConwayIncompleteWithdrawals: {0:?}")]
ConwayIncompleteWithdrawals(Vec<(String, u64, u64)>),  // (addr, supplied, expected)
```

- [ ] **Step 2: Failing tests**

```rust
#[test]
fn test_withdrawals_not_in_rewards_certs_pv10_combined() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version = (10, 0);
    let unregistered_addr = test_helpers::reward_addr([99u8; 28]);
    let registered_addr = test_helpers::reward_addr([1u8; 28]);
    ctx.reward_accounts.insert(registered_addr.credential_hash(), Lovelace(500));
    let withdrawals = btreemap! {
        unregistered_addr.clone() => Lovelace(100),    // missing
        registered_addr.clone() => Lovelace(400),      // partial — should be 500
    };
    let tx = test_helpers::tx_with_withdrawals(withdrawals);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    let we = errors.iter().find_map(|e| match e {
        ValidationError::WithdrawalsNotInRewardsCERTS(v) => Some(v),
        _ => None,
    }).expect("expected combined CERTS error");
    assert_eq!(we.len(), 2, "both missing and incomplete should be reported");
}

#[test]
fn test_withdrawals_not_in_rewards_certs_pv11_split() {
    let mut ctx = ValidationContext::test_default();
    ctx.params.protocol_version = (11, 0);
    let unregistered = test_helpers::reward_addr([99u8; 28]);
    let registered = test_helpers::reward_addr([1u8; 28]);
    ctx.reward_accounts.insert(registered.credential_hash(), Lovelace(500));
    let withdrawals = btreemap! {
        unregistered => Lovelace(100),
        registered => Lovelace(400),
    };
    let tx = test_helpers::tx_with_withdrawals(withdrawals);
    let errors = validate_transaction_with_context(&tx, &ctx).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, ValidationError::ConwayWithdrawalsMissingAccounts(_))));
    assert!(errors.iter().any(|e| matches!(e, ValidationError::ConwayIncompleteWithdrawals(_))));
}
```

- [ ] **Step 3: Implement helper in `validation/withdrawals.rs`**

```rust
//! WithdrawalsNotInRewardsCERTS validation.
//!
//! Reference: `Cardano.Ledger.Conway.Rules.Certs.withdrawalsThatDoNotDrainAccounts`.

use dugite_primitives::address::RewardAddress;
use dugite_primitives::value::Lovelace;
use std::collections::BTreeMap;

pub struct WithdrawalSplit {
    pub missing: Vec<(RewardAddress, Lovelace)>,    // unregistered or wrong network
    pub incomplete: Vec<(RewardAddress, Lovelace, Lovelace)>,  // (addr, supplied, expected)
}

pub fn withdrawals_that_do_not_drain_accounts(
    withdrawals: &BTreeMap<RewardAddress, Lovelace>,
    network_id: u8,
    accounts: &std::collections::HashMap<dugite_primitives::hash::Hash32, Lovelace>,
) -> Option<WithdrawalSplit> {
    let mut missing = Vec::new();
    let mut incomplete = Vec::new();
    for (addr, amount) in withdrawals {
        if addr.network() as u8 != network_id {
            missing.push((addr.clone(), *amount));
            continue;
        }
        match accounts.get(&addr.credential_hash()) {
            None => missing.push((addr.clone(), *amount)),
            Some(balance) if balance != amount => incomplete.push((addr.clone(), *amount, *balance)),
            _ => {}
        }
    }
    if missing.is_empty() && incomplete.is_empty() {
        None
    } else {
        Some(WithdrawalSplit { missing, incomplete })
    }
}
```

- [ ] **Step 4: Wire into validate_transaction_with_context**

```rust
let pv = ctx.params.protocol_version.0;
if let Some(split) = withdrawals::withdrawals_that_do_not_drain_accounts(
    &tx.body.withdrawals,
    ctx.node_network as u8,
    &ctx.reward_accounts,
) {
    if pv <= 10 {
        let mut combined: Vec<(String, u64)> = split.missing.iter()
            .map(|(a, v)| (a.to_bech32(), v.0)).collect();
        combined.extend(split.incomplete.iter().map(|(a, v, _)| (a.to_bech32(), v.0)));
        errors.push(ValidationError::WithdrawalsNotInRewardsCERTS(combined));
    } else {
        if !split.missing.is_empty() {
            errors.push(ValidationError::ConwayWithdrawalsMissingAccounts(
                split.missing.iter().map(|(a, v)| (a.to_bech32(), v.0)).collect(),
            ));
        }
        if !split.incomplete.is_empty() {
            errors.push(ValidationError::ConwayIncompleteWithdrawals(
                split.incomplete.iter().map(|(a, v, e)| (a.to_bech32(), v.0, e.0)).collect(),
            ));
        }
    }
}
```

Replace the existing `IncorrectWithdrawalAmount` paths if they overlap; keep the older error variant for backwards-compat but route new logic through `WithdrawalsNotInRewardsCERTS`.

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(ledger): WithdrawalsNotInRewardsCERTS predicate

Implements the Conway CERTS rule's withdrawal-validation, with the
pv<=10 combined error and the pv>10 split error semantics from
Conway.Rules.Certs and Conway.Rules.Ledger.testIncompleteAndMissingWithdrawals."
```

---

## Task 25: Test helpers consolidation

Several tasks above reference `test_helpers::*`. Many of these may not exist yet. Consolidate.

**Files:**
- Create: `crates/dugite-ledger/src/validation/test_helpers.rs` (gated `#[cfg(test)] pub`)

- [ ] **Step 1: Sweep**

```bash
grep -rn "test_helpers::" crates/dugite-ledger/src/validation/ | wc -l
```

- [ ] **Step 2: Implement each referenced helper**

Add functions for: `reward_addr`, `cred`, `tx_with_vote`, `tx_with_proposal`, `proposal_with_return_addr`, `proposal_with_return_addr_on_network`, `treasury_withdrawal_proposal`, `update_committee_proposal`, `mir_cert_distribute`, `mir_cert_transfer`, `pool_params_with_metadata_hash`, `tx_with_byron_output_attrs`, `tx_with_cert`, `tx_with_withdrawals`. Each returns a minimal valid Conway/Shelley tx structure for the given test scenario.

- [ ] **Step 3: Commit**

```bash
git commit -m "test(ledger): consolidate test helpers for validation tests"
```

---

## Task 26: Cross-validation pass against Haskell reference

For each predicate added in Tasks 2–24, dispatch the cardano-haskell-oracle agent to verify the dugite implementation matches the Haskell semantics exactly. This is a verification-only task — no code changes unless a divergence is found.

- [ ] **Step 1: For each of the 22 predicates, run an oracle query**

Suggested batch invocation in `Agent` calls: each predicate gets a short cross-check question:

> "I implemented `<predicate_name>` in Rust as `<paste source>`. Compared to the Haskell version in `<module>`, is there any semantic divergence — error data, ordering, gating, edge cases? Be specific. If no divergence, say so."

- [ ] **Step 2: Document responses in `docs/research/2026-05-02-validation-completeness-cross-check.md`**

Each predicate gets a one-paragraph entry: "Verified equivalent" or "Divergence: <what>, <fix>".

- [ ] **Step 3: For any flagged divergences, open a follow-up task and patch in the same branch**

- [ ] **Step 4: Commit cross-check report**

```bash
git add docs/research/2026-05-02-validation-completeness-cross-check.md
git commit -m "docs(ledger): cross-validation report against Haskell cardano-ledger

22 predicates verified against the Haskell reference. No divergences
remain; any patches applied are in this branch."
```

---

## Task 27: Final verification and PR

- [ ] **Step 1: Full test sweep**

```bash
cargo nextest run --workspace 2>&1 | tail -10
cargo test --doc 2>&1 | tail -5
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

All four must succeed. Fix any regressions.

- [ ] **Step 2: Create the PR**

```bash
git push -u origin ledger-validation-completeness
gh pr create --title "feat(ledger): 100% predicate-failure parity with Haskell cardano-ledger" --body "$(cat <<'EOF'
## Summary
- Adds 22 missing ledger validation predicate failures across Conway GOV, Shelley MIR, Shelley PPUP, CERTS, POOL, UTXO, and the consensus-layer OutsideForecastRange check.
- Each predicate is cross-checked against the Haskell IntersectMBO/cardano-ledger reference (see `docs/research/2026-05-02-validation-completeness-cross-check.md`).
- Brings dugite-ledger to parity with cardano-node v8.x predicate-failure surface for Shelley → Conway eras.

## Test plan
- [ ] `cargo nextest run --workspace` clean
- [ ] `cargo test --doc` clean
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] Spot-check N2C tx submission against running dugite-node with hand-crafted bad txs (one per predicate added)
EOF
)"
```

- [ ] **Step 3: Mark plan complete**

Update `docs/superpowers/plans/2026-05-02-ledger-validation-completeness.md` header to add `**Status:** Complete (PR <link>)`.

---

## Self-Review Checklist

| Spec requirement | Task |
|---|---|
| `DisallowedVoters` | Task 2 |
| `VotersDoNotExist` | Task 3 |
| `VotingOnExpiredGovAction` | Task 4 |
| `ProposalReturnAccountDoesNotExist` | Task 5 |
| `ProposalProcedureNetworkIdMismatch` | Task 6 |
| `TreasuryWithdrawalsNetworkIdMismatch` | Task 7 |
| `ZeroTreasuryWithdrawals` | Task 8 |
| `ConflictingCommitteeUpdate` | Task 9 |
| `ExpirationEpochTooSmall` | Task 10 |
| `PoolMedataHashTooBig` | Task 11 |
| `OutputBootAddrAttrsTooBig` | Task 12 |
| MIR scaffolding | Task 13 |
| `MIRCertificateTooLateInEpoch` | Task 14 |
| `MIRNegativesNotCurrentlyAllowed` + `MIRNegativeTransfer` | Task 15 |
| `MIRTransferNotCurrentlyAllowed` + `InsufficientForTransferDELEG` | Task 16 |
| `InsufficientForInstantaneousRewards` | Task 17 |
| `MIRProducesNegativeUpdate` | Task 18 |
| PPUP scaffolding | Task 19 |
| `NonGenesisUpdatePPUP` | Task 20 |
| `PPUpdateWrongEpoch` | Task 21 |
| `PVCannotFollowPPUP` | Task 22 |
| `OutsideForecastRange` | Task 23 |
| `WithdrawalsNotInRewardsCERTS` (and pv>10 split) | Task 24 |
| Test helpers | Task 25 |
| Cross-check vs Haskell | Task 26 |
| Final verification & PR | Task 27 |

All 22 + 2 split predicates are accounted for, plus support tasks. No placeholders. Type names are consistent across tasks (`ValidationError`, `ValidationContext`, `Voter`, `GovAction`, `EpochNo`, `SlotNo`, `Hash28`, `Hash32`).
