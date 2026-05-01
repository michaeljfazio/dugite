# Conway ratification fixtures

Offline JSON fixtures consumed by
`crates/dugite-ledger/tests/conway_ratification.rs`. Two committed fixtures
are real preview captures (`preview-pparam-1096.json` and
`preview-pparam-dropped-1216.json`); the test file also embeds two
synthetic correctness-gate fixtures inline. **No live network access at
test time.**

## Fixture schema

The schema is a faithful projection of the Haskell `RatifyEnv` /
`RatifyState` inputs onto JSON, defined in
`crates/dugite-ledger/tests/common/ratification_fixture.rs`:

| Field | Maps to (Haskell) | Notes |
|---|---|---|
| `proposal` | `GovActionState.gasProposalProcedure` | `action` is the Koios `proposal_description` blob, fed through a fail-closed reconstructor. |
| `votes` | `Voter -> VotingProcedure` | One entry per voter; the loader builds `votes_by_action`. |
| `drep_power` | `RatifyEnv.reDRepDistr` (DRepCredential keys) | Map of typed-Hash32 hex (28-byte hash + `0x00`/`0x01` discriminator + 3 zero pad) → lovelace. |
| `drep_no_confidence` | `RatifyEnv.reDRepDistr[DRepAlwaysNoConfidence]` | Aggregate. |
| `drep_abstain` | `RatifyEnv.reDRepDistr[DRepAlwaysAbstain]` | Aggregate. |
| `spo_stake` | `RatifyEnv.reStakePoolDistr.unPoolDistr` | Map of raw 28-byte pool hex → `individualTotalPoolStake`. |
| `pool_reward_accounts` | `RatifyEnv.reStakePools` | Pool ID hex → 29-byte reward account hex. Required for `defaultStakePoolVote`. |
| `vote_delegations` | `RatifyEnv.reAccounts.dRepDelegationAccountStateL` | Stake credential typed-Hash32 hex → DRep variant. |
| `no_confidence` | `EnactState.no_confidence` (live) | Read live by the `UpdateCommittee` branch (NOT from snapshot). |
| `committee` | `RatifyEnv.reCommitteeState` + `EnactState.ensCommittee.committeeThreshold` | Cold/hot keys are typed-Hash32 hex; threshold is a `{numerator, denominator}` rational. |
| `pparams` | Subset of `EnactState.ensCurPParams` actually read by RATIFY | Every `dvt_*` and `pvt_*` threshold, plus `protocol_version_major`, `committee_min_size`, `committee_max_term_length`. |
| `parent_enacted` | `EnactState.ensPrevGovActionIds` | Four optional roots (PParamUpdate / HardFork / Committee / Constitution). |
| `expected_outcome` | RATIFY result | Drives test assertions. |
| `provenance` | n/a | Capture metadata; not consumed by the loader. |

## Capturing a new fixture

> ### Koios free-tier daily cap (read this first)
>
> The default capture flow fans out one `drep_voting_power_history` request
> per registered DRep — ~8800 calls on preview as of epoch 1283.  The
> public Koios endpoint (`https://preview.koios.rest`) enforces a **5000
> request / 24-hour daily tier limit** in addition to a short-window burst
> limit.  A full post-bootstrap (PV ≥ 10) capture exhausts the daily cap
> before completing the DRep snapshot and panics with
> `Exceeded Tier Limit, count was N` on the first 429 that fails 8 retries.
>
> Workarounds:
>
> 1. **Bootstrap-era fixtures (PV = 9):** pass `--skip-drep-snapshot`.
>    Bootstrap auto-passes every DRep threshold so the snapshot is unread
>    by `ratify_proposals`; the capture completes in ~1 minute and only
>    burns ~10 requests.  This is what `preview-pparam-1096.json` and
>    `preview-pparam-dropped-1216.json` use.
>
> 2. **Post-bootstrap fixtures (planned, not yet wired):** swap the per-DRep
>    fan-out for a single `proposal_voting_summary` call that returns the
>    aggregate yes/no/abstain DRep stake for the proposal.  The loader will
>    synthesize an equivalent `drep_distribution_snapshot` (one Yes-cred,
>    one No-cred, one Abstain-cred + the always-no-confidence /
>    always-abstain pseudo-DRep aggregates) that produces the same
>    `drep_yes / drep_total` ratio as the real per-DRep iteration.  The
>    fixture loses per-DRep granularity but preserves the ratification
>    outcome — and uses **one** Koios request instead of thousands.
>
> 3. **Authenticated Koios tier:** with a paid Koios API key the daily cap
>    is lifted; the capture binary's existing per-DRep flow then runs to
>    completion in ~20 minutes.  No code changes needed; just point the
>    binary at an authenticated endpoint via `KOIOS_BASE` (env override
>    not yet wired — easy follow-up).
>
> The capture binary's existing throttling defaults (`--drep-concurrency 2
> --inter-request-ms 250`) keep us under the **burst** limit but cannot
> escape the **daily** cap; this is a Koios-side constraint, not a binary
> bug.

```bash
cargo build -p dugite-cli --bin capture-ratification-fixture
./target/debug/capture-ratification-fixture \
    --network preview \
    --proposal-id <tx_hex>#<proposal_index> \
    --output fixtures/conway-ratification/<name>.json
```

The capture binary queries (in order):
`proposal_list`, `proposal_votes`, `epoch_params`,
`pool_voting_power_history`, `pool_info` (per voting pool, for the reward
account), `committee_info`, `drep_list`, `drep_voting_power_history`
(per registered DRep, with bounded concurrency + retry), and
`drep_delegators` (for the always-abstain / always-no-confidence pseudo-
DReps). It transforms each response into the canonical schema above and
writes a single JSON file.

The DRep snapshot is the dominant cost — capturing all preview DReps
takes a few minutes against the public Koios free tier. Pass
`--skip-drep-snapshot` to omit it for bootstrap-era fixtures (PV=9
auto-passes all DRep thresholds, so the snapshot is unread).

After capture, add a `#[test]` in
`crates/dugite-ledger/tests/conway_ratification.rs` that loads the new
file and asserts the expected outcome.

## Synthetic correctness gates

Two synthetic fixtures live inline in
`crates/dugite-ledger/tests/conway_ratification.rs`:

* `ratifies_post_bootstrap_with_abstain_delegated_pools` — exercises the
  Haskell `defaultStakePoolVote` rule. 1 voting pool (Yes), 4 non-voting
  pools whose reward accounts delegate to `DRepAlwaysAbstain`. Ratifies
  iff non-voters resolve to `DefaultVote::Abstain` and are excluded from
  the SPO denominator. A broken implementation that returns
  `DefaultVote::No` causes the SPO ratio to collapse to 1/5 and the test
  fails.

* `ratifies_bootstrap_with_non_voting_pools_as_abstain` — exercises the
  bootstrap (PV=9) non-voter rule for non-HardFork actions. Same setup
  as above but no `vote_delegations`. Ratifies iff bootstrap non-voters
  count as Abstain. A broken implementation that counts them as No
  fails the SPO ratio.

These tests are intentionally asymmetric — the rule under test is the
only thing that distinguishes pass from fail, so a regression that
touches either path is caught immediately.

## Coverage gates

`crates/dugite-ledger/tests/common/ratification_fixture.rs` includes:

* `full_ppu_decoder_covers_every_known_field` — asserts that every PPU
  JSON key emitted by Koios is decoded into a `Some(_)` field on
  `ProtocolParamUpdate`. Acts as a regression test against silent PPU
  coverage gaps.
* `unknown_ppu_field_is_fail_closed` — asserts that an unrecognized PPU
  key panics rather than being silently ignored. Critical to prevent
  new ledger fields from quietly producing wrong group classification.

When a new PPU field is added in cardano-ledger, both
`koios_protocol_param_update` and `KNOWN_PPU_FIELDS` in the loader
must be extended; the second test will fail until the first is updated.

## Hash encodings — quick reference

Two byte-formats appear repeatedly:

* **Typed Hash32** (used for credentials — DRep, CC, stake): 28-byte
  Blake2b-224 hash, followed by `0x00` (key) or `0x01` (script), followed
  by 3 zero bytes. 64 hex characters total. Matches
  `Credential::to_typed_hash32`. Used for `drep_power` keys, all
  `committee.*` keys, and `vote_delegations` keys.
* **Raw Hash28** (used for pool IDs): 28-byte Blake2b-224 hash, no
  discriminator. 56 hex characters total. Used for `spo_stake` keys and
  `pool_reward_accounts` keys.

The capture binary handles all encoding internally; hand-edited fixtures
must follow the same convention or the loader will silently miss the
lookup (e.g. CC votes appear to be from members nobody recognizes).
