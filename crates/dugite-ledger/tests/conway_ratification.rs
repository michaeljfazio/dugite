//! Integration tests validating `ratify_proposals()` against committed
//! Koios-captured fixtures and synthetic correctness gates.

mod common;

use common::ratification_fixture::{
    assert_not_ratified, assert_ratified, parse_gov_action_id, RatificationFixture,
};

// ---------------------------------------------------------------------------
// Real preview fixtures (captured via capture-ratification-fixture)
// ---------------------------------------------------------------------------

/// Real preview ParameterChange enacted at epoch 1095.  PV=9 (bootstrap),
/// 3 SPOs voted Yes with real preview stakes (so `pvt_pp_security_group`
/// is exercised against real data, not bypassed), 3 CC members voted Yes.
#[test]
fn ratifies_first_positive_preview_proposal() {
    let path = format!(
        "{}/../../fixtures/conway-ratification/preview-pparam-1096.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let fixture = RatificationFixture::load(&path);
    let expected_bucket = fixture.expected_outcome.enacted_bucket;
    let expected_id = parse_gov_action_id(
        fixture
            .expected_outcome
            .enacted_id
            .as_deref()
            .expect("positive fixture must carry enacted_id"),
    );
    let mut ledger = fixture.into_ledger_state();
    ledger.ratify_proposals();
    assert_ratified(&ledger, expected_bucket, &expected_id);
}

/// Real preview ParameterChange (`committeeMinSize: 5`) dropped at epoch
/// 1216.  PV=9 (bootstrap), `parent_enacted.PParamUpdate` is set to the
/// prior 69c948 PParamChange so `prev_action_as_expected` succeeds; the
/// proposal then fails CC approval against the empty fixture committee.
#[test]
fn drops_preview_pparam_change_1216() {
    let path = format!(
        "{}/../../fixtures/conway-ratification/preview-pparam-dropped-1216.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let fixture = RatificationFixture::load(&path);
    assert!(
        !fixture.expected_outcome.ratified,
        "negative fixture must carry ratified=false"
    );
    assert!(
        fixture.expected_outcome.enacted_id.is_none(),
        "negative fixture must carry enacted_id=null"
    );
    let proposal_id = parse_gov_action_id(&fixture.proposal.gov_action_id);
    let mut ledger = fixture.into_ledger_state();
    ledger.ratify_proposals();
    assert_not_ratified(&ledger, &proposal_id);
}

// ---------------------------------------------------------------------------
// Synthetic correctness gates
// ---------------------------------------------------------------------------
//
// These fixtures are NOT captured from chain — they construct the smallest
// proposal/voter shape that distinguishes a correct implementation from a
// plausible but wrong one.  Each one is asymmetric: ratification succeeds
// only when the rule under test is wired correctly.

/// Gate B1 — `defaultStakePoolVote` (post-bootstrap).
///
/// Setup: 1 voting pool (Yes), 4 non-voting pools each delegated to
/// `DRepAlwaysAbstain` via `vote_delegations`.  All five pools have equal
/// stake.
///
/// If `default_spo_vote_from` is wired correctly, the 4 non-voting pools
/// resolve to `DefaultVote::Abstain` → excluded from the SPO denominator,
/// SPO ratio = 1/1 = 1.0 ≥ 0.51 (`pvt_pp_security_group`) → ratified.
///
/// If the rule is broken (e.g. always returns `DefaultVote::No`), the 4
/// non-voters fall through to the No bucket → SPO denominator = 5,
/// SPO ratio = 1/5 = 0.2 < 0.51 → not ratified, test fails.
#[test]
fn ratifies_post_bootstrap_with_abstain_delegated_pools() {
    let fixture: RatificationFixture = serde_json::from_str(POST_BOOTSTRAP_DEFAULT_VOTE_FIXTURE)
        .expect("synthetic B1 fixture must parse");
    let expected_id = parse_gov_action_id(
        fixture
            .expected_outcome
            .enacted_id
            .as_deref()
            .expect("synthetic B1 fixture must carry enacted_id"),
    );
    let expected_bucket = fixture.expected_outcome.enacted_bucket;
    let mut ledger = fixture.into_ledger_state();
    ledger.ratify_proposals();
    assert_ratified(&ledger, expected_bucket, &expected_id);
}

/// Gate B2 — bootstrap SPO non-voter rule.
///
/// Setup: 1 voting pool (Yes), 4 non-voting pools at PV=9 with no
/// `vote_delegations` populated.  PPU touches `maxBlockBodySize`
/// (Network + Security) so SPOs vote against `pvt_pp_security_group`.
///
/// During bootstrap, non-voting SPOs are counted as Abstain (excluded
/// from the SPO denominator).  If the rule is wired correctly, the
/// SPO ratio collapses to 1/1 = 1.0 ≥ 0.51 → ratified.  If non-voters
/// were instead counted as No (the wrong rule that's correct
/// post-bootstrap), the ratio would be 1/5 = 0.2 < 0.51 → not ratified.
#[test]
fn ratifies_bootstrap_with_non_voting_pools_as_abstain() {
    let fixture: RatificationFixture = serde_json::from_str(BOOTSTRAP_NON_VOTER_ABSTAIN_FIXTURE)
        .expect("synthetic B2 fixture must parse");
    let expected_id = parse_gov_action_id(
        fixture
            .expected_outcome
            .enacted_id
            .as_deref()
            .expect("synthetic B2 fixture must carry enacted_id"),
    );
    let expected_bucket = fixture.expected_outcome.enacted_bucket;
    let mut ledger = fixture.into_ledger_state();
    ledger.ratify_proposals();
    assert_ratified(&ledger, expected_bucket, &expected_id);
}

// ---------------------------------------------------------------------------
// Synthetic fixture data
// ---------------------------------------------------------------------------

const POST_BOOTSTRAP_DEFAULT_VOTE_FIXTURE: &str = r#"{
  "proposal": {
    "gov_action_id": "0000000000000000000000000000000000000000000000000000000000000001#0",
    "action": {
      "tag": "ParameterChange",
      "contents": [
        null,
        { "maxBlockBodySize": 90112 },
        null
      ]
    },
    "deposit": 100000000000,
    "return_addr_hex": "e0000000000000000000000000000000000000000000000000000000000000",
    "expiration": 999999,
    "anchor": null
  },
  "proposed_epoch": 99,
  "votes": [
    { "voter_type": "DRepKeyHash",                       "voter_id": "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a", "vote": "Yes" },
    { "voter_type": "StakePoolKeyHash",                  "voter_id": "10101010101010101010101010101010101010101010101010101010", "vote": "Yes" },
    { "voter_type": "ConstitutionalCommitteeHotKeyHash", "voter_id": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd", "vote": "Yes" }
  ],
  "drep_power": {
    "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a00000000": 1000000000000
  },
  "drep_no_confidence": 0,
  "drep_abstain": 0,
  "spo_stake": {
    "10101010101010101010101010101010101010101010101010101010": 1000000000000,
    "20202020202020202020202020202020202020202020202020202020": 1000000000000,
    "30303030303030303030303030303030303030303030303030303030": 1000000000000,
    "40404040404040404040404040404040404040404040404040404040": 1000000000000,
    "50505050505050505050505050505050505050505050505050505050": 1000000000000
  },
  "pool_reward_accounts": {
    "20202020202020202020202020202020202020202020202020202020": "e021212121212121212121212121212121212121212121212121212121",
    "30303030303030303030303030303030303030303030303030303030": "e031313131313131313131313131313131313131313131313131313131",
    "40404040404040404040404040404040404040404040404040404040": "e041414141414141414141414141414141414141414141414141414141",
    "50505050505050505050505050505050505050505050505050505050": "e051515151515151515151515151515151515151515151515151515151"
  },
  "vote_delegations": {
    "2121212121212121212121212121212121212121212121212121212100000000": { "tag": "Abstain" },
    "3131313131313131313131313131313131313131313131313131313100000000": { "tag": "Abstain" },
    "4141414141414141414141414141414141414141414141414141414100000000": { "tag": "Abstain" },
    "5151515151515151515151515151515151515151515151515151515100000000": { "tag": "Abstain" }
  },
  "no_confidence": false,
  "committee": {
    "members": [
      {
        "cold_key": "ababababababababababababababababababababababababababababab000000",
        "hot_key":  "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd00000000",
        "expiration": 999999
      }
    ],
    "threshold": { "numerator": 1, "denominator": 2 },
    "resigned": []
  },
  "pparams_epoch": 100,
  "pparams": {
    "protocol_version_major": 10,
    "committee_min_size": 1,
    "committee_max_term_length": 365,
    "dvt_pp_network_group":       { "numerator": 67, "denominator": 100 },
    "dvt_pp_economic_group":      { "numerator": 67, "denominator": 100 },
    "dvt_pp_technical_group":     { "numerator": 67, "denominator": 100 },
    "dvt_pp_gov_group":           { "numerator": 75, "denominator": 100 },
    "dvt_hard_fork":              { "numerator": 60, "denominator": 100 },
    "dvt_no_confidence":          { "numerator": 67, "denominator": 100 },
    "dvt_committee_normal":       { "numerator": 67, "denominator": 100 },
    "dvt_committee_no_confidence":{ "numerator": 60, "denominator": 100 },
    "dvt_constitution":           { "numerator": 75, "denominator": 100 },
    "dvt_treasury_withdrawal":    { "numerator": 67, "denominator": 100 },
    "pvt_motion_no_confidence":   { "numerator": 51, "denominator": 100 },
    "pvt_committee_normal":       { "numerator": 51, "denominator": 100 },
    "pvt_committee_no_confidence":{ "numerator": 51, "denominator": 100 },
    "pvt_hard_fork":              { "numerator": 51, "denominator": 100 },
    "pvt_pp_security_group":      { "numerator": 51, "denominator": 100 }
  },
  "expected_outcome": {
    "ratified": true,
    "enacted_bucket": "PParamUpdate",
    "enacted_epoch": 100,
    "enacted_id": "0000000000000000000000000000000000000000000000000000000000000001#0"
  },
  "parent_enacted": {
    "PParamUpdate": null,
    "HardFork": null,
    "Committee": null,
    "Constitution": null
  }
}"#;

const BOOTSTRAP_NON_VOTER_ABSTAIN_FIXTURE: &str = r#"{
  "proposal": {
    "gov_action_id": "0000000000000000000000000000000000000000000000000000000000000002#0",
    "action": {
      "tag": "ParameterChange",
      "contents": [
        null,
        { "maxBlockBodySize": 90112 },
        null
      ]
    },
    "deposit": 100000000000,
    "return_addr_hex": "e0000000000000000000000000000000000000000000000000000000000000",
    "expiration": 999999,
    "anchor": null
  },
  "proposed_epoch": 99,
  "votes": [
    { "voter_type": "StakePoolKeyHash",                  "voter_id": "10101010101010101010101010101010101010101010101010101010", "vote": "Yes" },
    { "voter_type": "ConstitutionalCommitteeHotKeyHash", "voter_id": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd", "vote": "Yes" }
  ],
  "drep_power": {},
  "drep_no_confidence": 0,
  "drep_abstain": 0,
  "spo_stake": {
    "10101010101010101010101010101010101010101010101010101010": 1000000000000,
    "20202020202020202020202020202020202020202020202020202020": 1000000000000,
    "30303030303030303030303030303030303030303030303030303030": 1000000000000,
    "40404040404040404040404040404040404040404040404040404040": 1000000000000,
    "50505050505050505050505050505050505050505050505050505050": 1000000000000
  },
  "pool_reward_accounts": {},
  "vote_delegations": {},
  "no_confidence": false,
  "committee": {
    "members": [
      {
        "cold_key": "ababababababababababababababababababababababababababababab000000",
        "hot_key":  "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd00000000",
        "expiration": 999999
      }
    ],
    "threshold": { "numerator": 1, "denominator": 2 },
    "resigned": []
  },
  "pparams_epoch": 100,
  "pparams": {
    "protocol_version_major": 9,
    "committee_min_size": 0,
    "committee_max_term_length": 365,
    "dvt_pp_network_group":       { "numerator": 67, "denominator": 100 },
    "dvt_pp_economic_group":      { "numerator": 67, "denominator": 100 },
    "dvt_pp_technical_group":     { "numerator": 67, "denominator": 100 },
    "dvt_pp_gov_group":           { "numerator": 75, "denominator": 100 },
    "dvt_hard_fork":              { "numerator": 60, "denominator": 100 },
    "dvt_no_confidence":          { "numerator": 67, "denominator": 100 },
    "dvt_committee_normal":       { "numerator": 67, "denominator": 100 },
    "dvt_committee_no_confidence":{ "numerator": 60, "denominator": 100 },
    "dvt_constitution":           { "numerator": 75, "denominator": 100 },
    "dvt_treasury_withdrawal":    { "numerator": 67, "denominator": 100 },
    "pvt_motion_no_confidence":   { "numerator": 51, "denominator": 100 },
    "pvt_committee_normal":       { "numerator": 51, "denominator": 100 },
    "pvt_committee_no_confidence":{ "numerator": 51, "denominator": 100 },
    "pvt_hard_fork":              { "numerator": 51, "denominator": 100 },
    "pvt_pp_security_group":      { "numerator": 51, "denominator": 100 }
  },
  "expected_outcome": {
    "ratified": true,
    "enacted_bucket": "PParamUpdate",
    "enacted_epoch": 100,
    "enacted_id": "0000000000000000000000000000000000000000000000000000000000000002#0"
  },
  "parent_enacted": {
    "PParamUpdate": null,
    "HardFork": null,
    "Committee": null,
    "Constitution": null
  }
}"#;
