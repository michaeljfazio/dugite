export const meta = {
  name: 'gauntlet-26b-gov-map-order',
  description: 'Refutation panel for #26b (V3 gov-action ScriptContext map ordering → ledger Script<Key in populate_gov.rs gov_action_to_data) before commit: per-field Plutus-vs-ledger order re-confirm (the #26-v1 trap) + over-strictness/scope + commit-safety',
  phases: [{ title: 'Gauntlet', detail: 'refute-by-default panel on the uncommitted #26b fix' }],
}

const VERDICT = {
  type: 'object',
  additionalProperties: false,
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean', description: 'true if the fix is shown wrong (wrong order on any field) / over-strict / out-of-scope / commit-unsafe via this lens; default true if uncertain' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

const CONTEXT =
  'Fix under test (UNCOMMITTED in the main tree; dugite backlog #26b, [M]/phase2-governance, the gov-action map-ordering sites '
  + 'DELIBERATELY EXCLUDED from the #26 credential-ord fix committed 4fe61ad011). Read the ACTUAL current code: '
  + '`git diff crates/dugite-uplc/src/populate_gov.rs` (the gov_action_to_data function + new tests).\n\n'
  + 'WHAT IT DOES: sorts the 3 V3 GovernanceAction ScriptContext fields into LEDGER order (Script<Key) before building Plutus '
  + 'Data, in gov_action_to_data:\n'
  + '  (1) TreasuryWithdrawals arm (~:263): REUSES `crate::tx_info_populate::ledger_ordered_withdrawals(withdrawals)?` (the '
  + '#26-gauntlet-PROVEN V3 helper; consumes the same &BTreeMap<Vec<u8> reward-blob,Lovelace>, returns Vec<(PrimCred,Lovelace)> '
  + 'sorted by Credential::cmp_ledger Script<Key), then maps each (stake,amount)→(credential_to_plutus(&stake).to_data(), '
  + 'Data::I(amount)) into the Constr 2 field-0 `Data::Map`.\n'
  + '  (2) UpdateCommittee members_to_add (~:312, Constr 4 field 2): collects the BTreeMap<Credential,u64> into a Vec, '
  + '`sort_by(|a,b| a.0.cmp_ledger(b.0))`, then builds the `Data::Map`.\n'
  + '  (3) UpdateCommittee members_to_remove (~:299, Constr 4 field 1): `sort_by(cmp_ledger)` + `dedup()` the Vec<Credential> '
  + 'before building the `Data::List`.\n'
  + 'Credential::cmp_ledger (credentials.rs:42) ranks Script=0<Key=1 then 28-byte hash asc. The credential Data TAGS '
  + '(credential_to_plutus: PubKey Constr 0 / Script Constr 1) are UNCHANGED — only the ENTRY ORDER of the Map/List changes. '
  + 'Diff = 1 file populate_gov.rs (+292/-26 incl tests), 1 crate dugite-uplc.\n\n'
  + 'CANONICAL HASKELL (diagnose w7bg9vcwg, conf 0.93, raw-source cardano-ledger ebed62de + plutus 1.65.0.0 — RE-CONFIRM '
  + 'INDEPENDENTLY from RAW source): Conway `transGovAction` (Cardano.Ledger.Conway.TxInfo) builds all 3 fields via '
  + '`transMap = PV3.unsafeFromList . map f . Map.toList` (TreasuryWithdrawals `Map AccountAddress Coin`, members_to_add '
  + '`Map (Credential ColdRole) EpochNo`) and `Set.toList` (members_to_remove `Set (Credential ColdRole)`). `Map.toList`/'
  + '`Set.toList` over a ledger `Map`/`Set` yields ascending LEDGER Credential Ord = ScriptHashObj < KeyHashObj = SCRIPT < KEY; '
  + '`PV3.unsafeFromList` is a bare AssocList wrapper and the Plutus `Map`/list ToData walks it WITHOUT re-sorting (plutus '
  + 'AssocMap.hs). So all 3 on-chain Data containers preserve ledger Script<Key. This is the SAME transMap mechanism as the V3 '
  + 'txInfoWdrl that #26 fixed (ledger_ordered_withdrawals, Script<Key). *** THE #26-v1 TRAP (the prior REFUTATION): V1/V2 '
  + 'txInfoWdrl uses a DIFFERENT builder (Alonzo transWithdrawals: Map.foldlWithKey into a FRESH Plutus Map → Plutus Key<Script) '
  + '— applying ledger Script<Key there was WRONG and got REFUTED 1/3. But GovernanceActions exist ONLY in V3 (Conway), so the '
  + 'V1/V2 Plutus-Key<Script case CANNOT apply to gov actions — ledger Script<Key is correct for all 3. CONFIRM this V3-only '
  + 'reasoning from raw source; do not let it be a hidden assumption.\n\n'
  + 'GREEN STATUS (engine-INDEPENDENTLY-re-verified, #438): only populate_gov.rs changed (1 crate dugite-uplc, +292/-26); '
  + 'cargo fmt(scoped) + clippy -p dugite-uplc --all-targets -D warnings = clean + nextest -p dugite-uplc = 469/469 (incl 6 new '
  + '#26b tests: 3 mixed-cred Script-first + 3 single-entry identity, + 2 pre-existing gov_action constr tests). 0/769 #730 '
  + 'corpus dumps carry any gov action (all Babbage spend/mint), and no live V3-gov-inspecting-script reference exists → the '
  + 'byte-exact gate is Haskell-Ord-by-construction + ordering tests (the standard accepted in #26/#29/#30); the TreasuryWithdrawals '
  + 'arm additionally carries the #26 gauntlet pedigree via the reused helper.'

const LENSES = [
  {
    key: 'per-field-plutus-vs-ledger-order',
    prompt: 'LENS (the decisive one — this is the class that REFUTED #26-v1): is EACH of the 3 fields built in the order Haskell '
      + 'actually uses? PERMALINK-RECONFIRM INDEPENDENTLY from RAW cardano-ledger + plutus source (raw.githubusercontent / gh api '
      + 'raw — NOT WebFetch; it hallucinated ordering rules in #26/#31). Read Cardano.Ledger.Conway.TxInfo `transGovAction` (the '
      + 'TreasuryWithdrawals + UpdateCommittee branches), the `transMap` helper, and plutus PlutusLedgerApi.V3.Contexts '
      + '(GovernanceAction makeIsDataSchemaIndexed + the field types) + PlutusTx.AssocMap (the Map ToData — does it re-sort?). '
      + 'Verify per field: (a) TreasuryWithdrawals `Map V2.Credential Lovelace` — is it transMap=unsafeFromList∘map∘Map.toList over '
      + 'the ledger `Map AccountAddress Coin` (⇒ ledger Credential Ord Script<Key, NO Plutus re-sort)? (b) members_to_add '
      + '`Map ColdCred Epoch` — same transMap over the ledger map (⇒ Script<Key)? (c) members_to_remove `[ColdCred]` — `Set.toList` '
      + '(⇒ ledger Set Ord Script<Key, deduped)? CRUCIAL: confirm these are NOT the V1/V2 fresh-Plutus-Map builder (Key<Script) — '
      + 'i.e. confirm GovernanceActions are V3-ONLY so the #26-v1 Plutus-Key<Script case cannot apply. If ANY field actually uses '
      + 'Plutus Key<Script (fresh-Map builder) or a different order than the fix applies (ledger Script<Key via cmp_ledger), '
      + 'refuted=true. Also confirm the credential Data TAGS are unchanged (PubKey Constr 0 / Script Constr 1) — only order, and '
      + 'that dugite\'s cmp_ledger (Script=0<Key=1 then hash asc) matches the ledger Credential derived Ord exactly.',
  },
  {
    key: 'over-strictness-scope',
    prompt: 'LENS: over-reach / over-strictness / scope. (a) Confirm ONLY gov_action_to_data in populate_gov.rs changed '
      + '(`git diff --stat` = 1 file) — the credential Data tags, the derived Credential Ord (credentials.rs), '
      + 'voting_procedures_to_plutus (already #26-cmp_ledger-sorted), the V1/V2 withdrawals_to_plutus (Plutus Key<Script, '
      + 'DELIBERATE — must NOT be touched), and the ParameterChange/HardForkInitiation/NoConfidence/NewConstitution/InfoAction arms '
      + 'are ALL untouched. (b) encode/governance.rs (the gov-action CBOR re-encode) was DEFERRED as INERT — independently verify '
      + 'that claim: does honest block ingest preserve raw_body_cbor (KeepRaw) so the re-encode never feeds a consensus hash (tx '
      + 'id / block-body hash / gov-action-id)? If encode/governance.rs is actually a LIVE consensus path (not inert), the fix is '
      + 'INCOMPLETE (refuted=true). (c) members_to_remove `dedup()`: is dedup SAFE — i.e. does the ledger reject duplicate/'
      + 'overlapping committee creds so dedup is a no-op for valid txs, AND does Set.toList dedup in Haskell so dugite matches? '
      + '(dedup after sort_by removes only ADJACENT equal creds — confirm that equals full-Set dedup since sort groups equals.) '
      + '(d) single-entry / already-sorted inputs: confirm the sort is a no-op for a 0- or 1-element field and for an already-'
      + 'canonically-ordered input (no over-sort / no spurious reordering of distinct-but-equal-rank creds — the hash tiebreak is '
      + 'deterministic). If any non-gov-action path was touched, encode/governance.rs is actually live, dedup is unsafe, or the '
      + 'sort over-reaches, refuted=true.',
  },
  {
    key: 'commit-safety',
    prompt: 'LENS: is committing #26b safe? (a) PURELY ORDERING: the diff only re-orders entries within the TreasuryWithdrawals / '
      + 'members_to_add / members_to_remove Data containers — no value, no credential bytes, no Constr tag, no other field '
      + 'changes. Confirm honest txs are unaffected EXCEPT for the corrected order (which is the POINT — dugite now matches '
      + 'Haskell). (b) The change makes dugite\'s V3 gov-action ScriptContext byte-MATCH Haskell for any tx carrying a '
      + 'TreasuryWithdrawals/UpdateCommittee proposal with >=2 mixed key+script entries (previously DIVERGED Key<Script vs '
      + 'ledger Script<Key) → a correctness alignment, not a regression; single/sorted inputs unchanged. (c) LATENT: there is no '
      + 'live divergence in the 769-dump corpus (0 gov-action dumps) and a V3 script inspecting a gov-action proposal is rare on-'
      + 'chain, so ZERO live impact on current preprod/mainnet sync today; this is byte-exactness defense for V3 governance '
      + 'scripts. A correctly-tracked latent fix grounded in canonical Haskell Ord is GOOD, not a refutation (the #26/#29/#30 '
      + 'standard: when 0 reference dumps exist, the canonical Haskell Ord IS the authoritative reference). (d) One crate '
      + '(dugite-uplc, populate_gov.rs); the unrelated dirty crates/dugite-ledger/src/eras/common.rs (ep246 scratch) + any '
      + 'proptest-regressions artifact must be EXCLUDED via explicit-filename staging. Refuted=true ONLY if committing breaks '
      + 'honest-tx eval, the change is not actually purely-ordering, or there is a real commit-safety problem. A correctly-'
      + 'excluded unrelated dirty file + a latent-but-correct by-construction fix are NOT refutations.',
  },
]

phase('Gauntlet')
const votes = await parallel(
  LENSES.map((l) => () =>
    agent('Adversarially REFUTE the #26b fix via this lens. Default refuted=true if uncertain. Read the real current code (git diff populate_gov.rs) + RAW Haskell cardano-ledger/plutus source before deciding.\n\n' + CONTEXT + '\n\n' + l.prompt,
      { label: 'refute:' + l.key, phase: 'Gauntlet', schema: VERDICT, model: 'opus' }
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens: l.key })
  )
)

const real = votes.filter(Boolean)
const refuteCount = real.filter((v) => v.refuted).length
const pass = refuteCount < Math.ceil(LENSES.length / 2)
return { pass, refuteCount, total: LENSES.length, votes: real }
