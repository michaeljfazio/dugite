export const meta = {
  name: 'fix-26b-gov-map-order',
  description: 'FIXING #26b: sort the 3 V3 gov-action ScriptContext fields (TreasuryWithdrawals map, UpdateCommittee members_to_add map, members_to_remove list) by Credential::cmp_ledger (ledger Script<Key) in populate_gov.rs gov_action_to_data — mirrors the gauntlet-proven #26 ledger_ordered_withdrawals; 1 crate dugite-uplc; NO commit',
  phases: [{ title: 'Fix', detail: '3 sort-by-cmp_ledger edits in gov_action_to_data + mixed-cred ordering tests; fmt/clippy/nextest' }],
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['treasury_edit', 'committee_add_edit', 'committee_remove_edit', 'scope_guard', 'tests', 'checks', 'caveats', 'completed'],
  properties: {
    treasury_edit: { type: 'string', description: 'The TreasuryWithdrawals arm edit in gov_action_to_data (populate_gov.rs ~:263-284). PREFER reusing the gauntlet-proven helper: `let ordered = crate::tx_info_populate::ledger_ordered_withdrawals(withdrawals)?;` (it takes the SAME &BTreeMap<Vec<u8>,Lovelace>, returns Vec<(PrimCred,Lovelace)> sorted by cmp_ledger Script<Key), then map each (stake, amount) -> (credential_to_plutus(&stake).to_data(), Data::I(BigInt::from(amount.0))) into `entries`, before `Data::Constr(2, vec![Data::Map(entries), maybe_script_hash(...)])`. Confirm ledger_ordered_withdrawals is `pub` (tx_info_populate.rs:593) + importable. Quote the before/after.' },
    committee_add_edit: { type: 'string', description: 'The UpdateCommittee members_to_add edit (~:305-313): collect the BTreeMap<Credential,epoch> entries into a Vec, `sort_by(|a,b| a.0.cmp_ledger(b.0))` (Script<Key), THEN build `add_map: Vec<(Data,Data)>` mapping each (c, epoch) -> (credential_to_plutus(c).to_data(), Data::I(BigInt::from(epoch))). Quote before/after + confirm the epoch type.' },
    committee_remove_edit: { type: 'string', description: 'The UpdateCommittee members_to_remove edit (~:299-303): the Vec<Credential> must be SORTED by cmp_ledger (Script<Key) + deduped (Set parity, no-op for valid txs) BEFORE building `remove_list: Vec<Data>`. Quote before/after.' },
    scope_guard: { type: 'string', description: 'Evidence the change is populate_gov.rs gov_action_to_data ONLY (1 crate dugite-uplc): the credential Data TAGS (credential_to_plutus: PubKey Constr 0 / Script Constr 1) are UNCHANGED — only ENTRY ORDER changes; NO change to the derived Credential Ord, to encode/governance.rs (INERT, separate crate, deliberately deferred), to voting_procedures_to_plutus (already cmp_ledger-sorted by #26), or to any V1/V2 path. ParameterChange/HardFork/NoConfidence/NewConstitution/InfoAction arms UNTOUCHED.' },
    tests: { type: 'string', description: 'populate_gov.rs tests (reuse the existing gov_action test helpers): (1) gov_action_treasury_withdrawals_orders_script_before_key — TWO reward blobs, a key-stake (header 0xE_) + a script-stake (0xF_); assert the Data::Map entries[0]=Script(Constr 1), entries[1]=Key(Constr 0) even though the input BTreeMap iterates key-blob first. (2) gov_action_update_committee_add_orders_script_before_key — members_to_add with a key + a script cred; assert add[0]=Constr 1, add[1]=Constr 0. (3) gov_action_update_committee_remove_orders_script_before_key — members_to_remove vec![key, script] in KEY-first order; assert Data::List = [Constr 1 (Script), Constr 0 (Key)]. (4) single-entry identity for each (no over-sort regression). Mirror the existing ledger_ordered_withdrawals_puts_script_before_key test (tx_info_populate.rs:964) for blob construction (vary credential hash byte[1..29], header byte[0]=0xE0 key / 0xF0 script).' },
    checks: {
      type: 'object', additionalProperties: false, required: ['fmt', 'clippy', 'nextest'],
      properties: { fmt: { type: 'boolean' }, clippy: { type: 'boolean' }, nextest: { type: 'boolean', description: 'cargo nextest run -p dugite-uplc green incl the 3 new mixed-cred ordering tests + the single-entry identities + all existing gov_action/populate_gov tests unchanged' } },
    },
    caveats: { type: 'string' },
    completed: { type: 'boolean' },
  },
}

phase('Fix')
const fix = await agent(
  'Implement dugite backlog #26b (V3 gov-action ScriptContext map ordering) in the MAIN working tree (no worktree; do NOT git '
  + 'commit). Single crate: dugite-uplc, single file: crates/dugite-uplc/src/populate_gov.rs (the `gov_action_to_data` function '
  + 'ONLY). SOURCE-CONFIRMED (diagnose w7bg9vcwg, conf 0.93, raw-source cardano-ledger ebed62de + plutus 1.65.0.0): all 3 V3 '
  + 'GovernanceAction map/list fields are built in LEDGER order Script<Key in Haskell (Conway TxInfo.hs transGovAction: '
  + '`transMap = PV3.unsafeFromList . map f . Map.toList` preserves the ledger Map\'s Credential Ord ScriptHashObj<KeyHashObj; '
  + 'members_to_remove = `Set.toList` Script<Key, deduped; plutus AssocMap ToData walks the list with NO re-sort). dugite '
  + 'currently produces KEY<Script on all 3 → a live phase-2 ScriptContext byte-divergence for any V3 script inspecting a '
  + 'TreasuryWithdrawals/UpdateCommittee proposal. This is the SAME ledger-order mechanism as the V3 txInfoWdrl that #26 already '
  + 'fixed+gauntlet-passed via `ledger_ordered_withdrawals` (Script<Key). Gov actions are V3-ONLY, so use the LEDGER comparator '
  + '`Credential::cmp_ledger` (credentials.rs:42, Script=0<Key=1 then 28-byte hash asc) for ALL 3 — do NOT use the derived '
  + 'Plutus Ord here (that Key<Script case is V1/V2-only and does not apply to gov actions).\n\n'
  + 'THE 3 EDITS (gov_action_to_data only):\n'
  + '  1. TreasuryWithdrawals arm (~:263-284): the `withdrawals` field is `&BTreeMap<Vec<u8>, Lovelace>` — the SAME type '
  + '`crate::tx_info_populate::ledger_ordered_withdrawals` (pub, tx_info_populate.rs:593) consumes. REUSE it: '
  + '`let ordered = crate::tx_info_populate::ledger_ordered_withdrawals(withdrawals)?;` returns Vec<(PrimCred, Lovelace)> sorted '
  + 'by cmp_ledger (Script<Key). Then build `entries` by mapping each (stake, amount) -> '
  + '(credential_to_plutus(&stake).to_data(), Data::I(BigInt::from(amount.0))). Replaces the current manual blob-order loop. '
  + '(This reuses the gauntlet-proven #26 helper → byte-identical ledger order, minimal new code.)\n'
  + '  2. UpdateCommittee members_to_add (~:305-313): instead of iterating the BTreeMap<Credential,epoch> directly (derived '
  + 'Key<Script), collect entries into a Vec, `sort_by(|a, b| a.0.cmp_ledger(b.0))`, THEN map to `add_map`.\n'
  + '  3. UpdateCommittee members_to_remove (~:299-303): SORT the Vec<Credential> by cmp_ledger (Script<Key) + dedup (Set '
  + 'parity; no-op for valid txs) BEFORE building `remove_list`.\n\n'
  + '*** SCOPE GUARD (do NOT over-reach): ONLY gov_action_to_data in populate_gov.rs changes. The credential Data TAGS '
  + '(credential_to_plutus: PubKey Constr 0 / Script Constr 1) stay UNCHANGED — only ENTRY ORDER changes. Do NOT touch: the '
  + 'derived Credential Ord (credentials.rs — Key<Script is correct for its CBOR/Plutus roles); voting_procedures_to_plutus '
  + '(already cmp_ledger-sorted by #26); any V1/V2 txInfoWdrl path (those are Plutus Key<Script, deliberately); '
  + 'encode/governance.rs (INERT per the diagnose — raw-wire preserved on honest ingest; a SEPARATE optional dugite-serialization '
  + 'follow-up, NOT this commit); ParameterChange/HardForkInitiation/NoConfidence/NewConstitution/InfoAction arms.\n\n'
  + 'TESTS (add to populate_gov.rs tests; mirror ledger_ordered_withdrawals_puts_script_before_key at tx_info_populate.rs:964 for '
  + 'building mixed reward blobs — header byte[0]=0xE0 for key-stake / 0xF0 for script-stake, vary hash bytes[1..29]):\n'
  + '  - gov_action_treasury_withdrawals_orders_script_before_key: 2 withdrawals (one 0xE_ key-stake, one 0xF_ script-stake) → '
  + 'Data::Map entries[0]=Script(Constr 1), entries[1]=Key(Constr 0).\n'
  + '  - gov_action_update_committee_add_orders_script_before_key: members_to_add {key, script} → add[0]=Constr 1, add[1]=Constr 0.\n'
  + '  - gov_action_update_committee_remove_orders_script_before_key: members_to_remove vec![key, script] (key-first input) → '
  + 'Data::List [Constr 1 Script, Constr 0 Key].\n'
  + '  - single-entry identity for each field (no over-sort regression). Keep all existing gov_action/populate_gov tests green.\n\n'
  + 'BUILD (bounded): cargo fmt --all ; cargo clippy -p dugite-uplc --all-targets -- -D warnings ; cargo nextest run -p '
  + 'dugite-uplc. Report each pass/fail + counts. completed=true ONLY if the 3 sort edits are in gov_action_to_data, the credential '
  + 'tags + all other arms are untouched, the 3 mixed-cred + single-entry tests are added, and fmt+clippy+nextest are green. '
  + 'Green tests are NOT byte-exact proof — a gauntlet follows. Do NOT commit. NOTE: unrelated uncommitted '
  + 'crates/dugite-ledger/src/eras/common.rs (different crate, ep246 scratch) — do NOT touch.',
  { label: 'fix:26b', phase: 'Fix', schema: FIX_SCHEMA, model: 'opus' }
)
return { fix }
