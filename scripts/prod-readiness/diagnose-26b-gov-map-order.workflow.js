export const meta = {
  name: 'diagnose-26b-gov-map-order',
  description: 'DIAGNOSE #26b: pin the Haskell V3-ScriptContext ordering of the gov-action maps EXCLUDED from #26 — TreasuryWithdrawals (Map Credential Coin), UpdateCommittee members_to_add (Map ColdCred Epoch) + members_to_remove ([ColdCred]) — Plutus-order (Key<Script) vs ledger-order (Script<Key) per RAW Conway TxInfo.hs; AND whether encode/governance.rs gov-action CBOR re-encode is a LIVE consensus path or inert',
  phases: [{ title: 'Diagnose', detail: 'per-field Plutus-vs-ledger order from raw Conway TxInfo.hs; encode/governance.rs consensus reach; dugite gap + fix plan' }],
}

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['treasury_order', 'committee_add_order', 'committee_remove_order', 'encode_governance_reach', 'dugite_gap', 'fix_plan', 'tests', 'reference_availability', 'haskell_source', 'confidence', 'caveats'],
  properties: {
    treasury_order: { type: 'string', description: 'The EXACT order in which Haskell builds the V3 ScriptContext TreasuryWithdrawals `Map V2.Credential Lovelace` (gov-action Constr 2 field 0). Conway transGovAction/transTreasuryWithdrawals: does it (a) fold the ledger `Map RewardAccount Coin` into a FRESH Plutus Map keyed by V2.Credential then Map.toList → PLUTUS Credential Ord (makeIsDataSchemaIndexed PubKeyCredential=0 < ScriptCredential=1 ⇒ KEY<SCRIPT), or (b) transMap over the ledger Map preserving ledger RewardAccount/Credential Ord (ScriptHashObj<KeyHashObj ⇒ SCRIPT<KEY)? Quote the exact function + the Map construction. This is the #26-v1 trap (V1/V2 txInfoWdrl was Plutus Key<Script, V3 was ledger Script<Key) — gov actions are V3-ONLY so pin the V3 rule precisely.' },
    committee_add_order: { type: 'string', description: 'Same Plutus-vs-ledger order question for UpdateCommittee `members_to_add :: Map (Credential ColdCommitteeRole) EpochNo` → V3 Plutus `Map ColdCommitteeCredential Epoch` (gov-action Constr 4 field 2). Fresh Plutus Map (Key<Script) or ledger Map preserved (Script<Key)? Quote the transGovAction UpdateCommittee branch.' },
    committee_remove_order: { type: 'string', description: 'Order of UpdateCommittee `members_to_remove :: Set (Credential ColdCommitteeRole)` → V3 Plutus `[ColdCommitteeCredential]` (gov-action Constr 4 field 1). Set.toList → ledger Credential Ord (Script<Key) almost certainly; confirm from source + whether the list is sorted/deduped. Also note dugite Prim type of members_to_remove (Vec vs Set) and its current order.' },
    encode_governance_reach: { type: 'string', description: 'Is crates/dugite-serialization/src/encode/governance.rs (the gov-action CBOR re-encode: TreasuryWithdrawals withdrawals map :152, UpdateCommittee members_to_add BTreeMap :182, voting procedures :41) a LIVE CONSENSUS path, or INERT like #31-C/#28b (honest ingest preserves RAW wire bytes via KeepRaw; re-encode fires only for synthetic in-mem txs)? Trace: does honest block ingest / gov-action-id hash / proposal hashing re-encode gov actions, or hash the raw on-wire CBOR? Does block FORGING re-emit raw or re-encode? If LIVE, the BTreeMap<Credential> / BTreeMap<blob> iteration order must match the Haskell ledger EncCBOR Map order (encodeMap in key Ord order: ledger Credential Script<Key, RewardAccount its Ord) — state the gap. If INERT, say so (then it is a self-consistency/forge-only concern, not a live divergence).' },
    dugite_gap: { type: 'string', description: 'EXACT dugite gap with file:line. populate_gov.rs gov_action_to_data: TreasuryWithdrawals (~:263) builds Data::Map by iterating withdrawals:BTreeMap<Vec<u8> reward-blob,Lovelace> in RAW-header-byte order (0xE_ key < 0xF_ script = Key<Script), NOT re-sorted; UpdateCommittee members_to_add (~:305) iterates BTreeMap<Credential,epoch> in derived Credential Ord (Key<Script); members_to_remove (~:299) iterates in its Prim order. State, per field, whether dugite\'s current order MATCHES the pinned Haskell order or DIVERGES (and in which direction). Also encode/governance.rs:152/179/182 if LIVE.' },
    fix_plan: { type: 'string', description: 'The surgical Tier-A\' fix (dugite-uplc populate_gov.rs, + dugite-serialization encode/governance.rs ONLY IF its reach is LIVE). For each field that diverges: sort the entries by the pinned order before building Data::Map/Data::List (apply Credential::cmp_ledger Script<Key if ledger-order, or the derived/Plutus Ord Key<Script if Plutus-order — DO NOT blindly apply Script<Key; the #26-v1 fix was REFUTED for exactly that mistake on V1/V2). Specify the comparator per field. If a field already matches, leave it (record why). Keep populate_gov.rs (phase-2) and encode/governance.rs (serialization) as SEPARATE commits if both change (≤2 crates rule).' },
    tests: { type: 'string', description: 'Tests: a TreasuryWithdrawals gov action with a mixed key-stake + script-stake withdrawal → Data::Map order matches the pinned Haskell order; an UpdateCommittee with mixed key+script members_to_add/remove → matches; single-entry = identity (no-op). If encode/governance.rs is LIVE, a CBOR round-trip / golden vs a known on-chain gov-action CBOR. Note that (like #26) the #730 corpus has 0 gov-action dumps, so the byte-exact gate rests on the Haskell-Ord match + ordering tests unless a real preprod/mainnet gov-action tx is found.' },
    reference_availability: { type: 'string', description: 'Is there ANY real reference tx to verify against? Check: have TreasuryWithdrawals or UpdateCommittee gov actions been enacted on preprod/mainnet (a Plutus-script tx inspecting one would be the gold tie-break, like #26 sought)? Note the 0/769 #730-corpus gov-action absence (already known). State whether the gate must rest on canonical Haskell Ord (by-construction) like #26/#29/#30, or a real ScriptContext dump is attainable.' },
    haskell_source: { type: 'string', description: 'RAW source (raw.githubusercontent / gh api raw — NOT WebFetch; it hallucinated rules in #31/#26), permalink + SHA pinned: cardano-ledger Cardano.Ledger.Conway.TxInfo (transGovAction / transTxInfo proposal procedures) + the V3 ScriptContext GovernanceAction ToData; plutus PlutusLedgerApi.V3.Contexts (GovernanceAction makeIsDataSchemaIndexed). For encode reach: cardano-ledger Conway GovAction EncCBOR. Pin the exact lines for each ordering decision.' },
    confidence: { type: 'number' },
    caveats: { type: 'string' },
  },
}

phase('Diagnose')
const d = await agent(
  'DIAGNOSE dugite backlog #26b — the gov-action MAP ORDERING sites DELIBERATELY EXCLUDED from the #26 credential-ord fix '
  + '(committed 4fe61ad011). #26 fixed txInfoVotes (Voter::cmp_ledger), V3 txInfoWdrl (ledger Script<Key), V1/V2 txInfoWdrl '
  + '(Plutus Key<Script — the REFUTED-then-corrected nuance), and the Reward/Vote redeemer indices. It EXCLUDED three V3 '
  + 'gov-action ScriptContext map fields + a gov-action CBOR re-encode, filed as #26b. RESOLVE them now (root-cause only — ONE '
  + 'state-machine step NEW→ROOT-CAUSED).\n\n'
  + 'THE EXCLUDED SITES (HEAD-verified this wake):\n'
  + '  (1) crates/dugite-uplc/src/populate_gov.rs gov_action_to_data TreasuryWithdrawals arm (~:263-284): builds '
  + '`Data::Map(entries)` for the V3 GovernanceAction Constr 2 field 0 (`Map V2.Credential Lovelace`) by iterating '
  + '`withdrawals: BTreeMap<Vec<u8> reward-account-blob, Lovelace>` in RAW 29-byte-blob order ([header||hash28], so 0xE_ '
  + 'key-stake < 0xF_ script-stake = KEY<SCRIPT), NOT re-sorted by credential.\n'
  + '  (2) populate_gov.rs UpdateCommittee arm (~:298-313): `members_to_add` → `Data::Map(add_map)` (Constr 4 field 2, '
  + '`Map ColdCred Epoch`) iterates `BTreeMap<Credential, epoch>` in DERIVED Credential Ord (Key=0<Script=1 = KEY<SCRIPT); '
  + '`members_to_remove` → `Data::List(remove_list)` (Constr 4 field 1, `[ColdCred]`) iterates its Prim order.\n'
  + '  (3) crates/dugite-serialization/src/encode/governance.rs: the gov-action CBOR re-encode — TreasuryWithdrawals '
  + '`withdrawals` map (:152-153), UpdateCommittee `members_to_add` BTreeMap<Credential,epoch> (:182-183) + `members_to_remove` '
  + '(:178-179), voting procedures BTreeMap<Voter,..> (:41) — all iterated in Rust derived Ord.\n\n'
  + 'THE CRITICAL QUESTION (this is the #26-v1 REFUTATION trap — do NOT repeat it): for EACH V3 ScriptContext field, does '
  + 'Haskell build the Plutus Data Map in (a) PLUTUS order = fresh Plutus Map keyed by V2.Credential then Map.toList → '
  + 'makeIsDataSchemaIndexed PubKeyCredential=0 < ScriptCredential=1 = KEY<SCRIPT (the order dugite\'s BTreeMap ALREADY '
  + 'produces — so NO fix needed), or (b) LEDGER order = transMap over the ledger Map preserving Credential Ord ScriptHashObj<'
  + 'KeyHashObj = SCRIPT<KEY (so dugite DIVERGES and must sort by Credential::cmp_ledger)? In #26 the V3 txInfoWdrl turned out '
  + 'to be LEDGER order (Script<Key) while V1/V2 was Plutus order (Key<Script). Gov actions are V3-ONLY, but you MUST pin EACH '
  + 'field from RAW source — do not assume V3==ledger for all of them.\n\n'
  + 'RESOLVE (RAW Conway source, permalink + SHA pinned — read raw.githubusercontent / gh api raw, NEVER WebFetch):\n'
  + '  - cardano-ledger Cardano.Ledger.Conway.TxInfo: transGovAction / the proposal-procedure translation feeding txInfo '
  + 'ProposalProcedures — how TreasuryWithdrawals `Map RewardAccount Coin`, UpdateCommittee `Map ColdCred EpochNo` (add) + '
  + '`Set ColdCred` (remove) become Plutus Data, and in WHAT ORDER (fresh Plutus Map.toList vs ledger-Map-preserved). Quote the '
  + 'exact Map/Set construction per field.\n'
  + '  - plutus PlutusLedgerApi.V3.Contexts: GovernanceAction makeIsDataSchemaIndexed (confirm the Constr tags 0-6 + field '
  + 'shapes dugite\'s doc comment claims) + V2.Credential ToData tag order (PubKey 0 / Script 1).\n'
  + '  - For site (3) reach: is encode/governance.rs a LIVE consensus path or INERT? Trace honest block ingest + gov-action-id '
  + 'hashing + block forging in dugite — do they re-encode gov actions or preserve RAW wire (KeepRaw, like #31-C tx-body / '
  + 'script_data_hash and #28b)? If INERT, #26b\'s site-3 is a forge-only/self-consistency concern, not a live divergence (lower '
  + 'priority, still align if cheap). If LIVE, the BTreeMap iteration order must match the ledger EncCBOR encodeMap key Ord '
  + '(Credential Script<Key) — confirm against Conway GovAction EncCBOR.\n\n'
  + 'Use the in-project refs FIRST if relevant '
  + '(.claude/skills/haskell-ledger-cross-validation/references/era-rules/conway.md), then RAW plutus + cardano-ledger source. '
  + 'PIN per-field the order (Plutus Key<Script vs ledger Script<Key), the exact dugite gap (which fields DIVERGE vs already '
  + 'MATCH), the surgical fix (comparator PER FIELD — never blind-flip), tests, the reference availability (likely 0 corpus, '
  + 'gate rests on Haskell-Ord match like #26/#29/#30), confidence, caveats. This is [L]/latent (V3-gov-inspecting scripts; no '
  + 'gov-action dumps in the corpus) but a REAL phase-2 ScriptContext byte-exactness item. Return the StructuredOutput.',
  { label: 'diagnose:26b', phase: 'Diagnose', schema: SCHEMA, model: 'opus' }
)
return { d }
