export const meta = {
  name: 'diagnose-31c-set-dedup',
  description: 'DIAGNOSE #31-C: Conway PV9+ Set duplicate-reject — exact read_set call sites + PV-threading mechanism + decodeSetEnforceNoDuplicates count-check',
  phases: [{ title: 'Diagnose', detail: 'enumerate read_set sites; how to thread PV>=9; the count-check; pre-PV9 lenient' }],
}

const SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['haskell_rule', 'haskell_source', 'read_set_sites', 'pv_threading', 'fix_plan', 'scope_eras', 'tests', 'confidence', 'caveats'],
  properties: {
    haskell_rule: { type: 'string', description: 'PV9+ decodeSetEnforceNoDuplicates: the exact count-check (when len/=count fail). Does it apply to ALL tag-258 Set fields (witness-set vkey/native/bootstrap/plutus + body inputs/certs/collateral/required_signers/reference_inputs)? PV-gated at exactly >=9? ordering NEVER enforced (Set)?' },
    haskell_source: { type: 'string', description: 'permalink-pinned cardano-ledger-binary decodeSet / decodeSetEnforceNoDuplicates / decodeListLikeEnforceNoDuplicates (the count-check), and the PV gate ifDecoderVersionAtLeast natVersion 9' },
    read_set_sites: { type: 'string', description: 'EVERY dugite read_set call site (file:line) and which era/field each is — separate the Conway PV9+ tag-258 sets (need strict dedup) from pre-Conway sets (lenient, accept dups). Note any read_set used for non-set or always-lenient data' },
    pv_threading: { type: 'string', description: 'how to make read_set PV-aware: does the Reader already carry protocol_major/era? is era/PV available at the call sites (era_conway decoder has era; reader.rs is generic)? options: (a) read_set_strict(item) variant called only by Conway PV9+ sites; (b) read_set(item, enforce_no_dups: bool); (c) thread protocol_major into Reader. Recommend the cleanest that does NOT break pre-Conway lenient sites' },
    fix_plan: { type: 'string', description: 'the precise fix: the count-check (decode N items, build a set, fail if sizes differ — mirror decodeListLikeEnforceNoDuplicates), applied to the Conway PV9+ read_set sites ONLY (pre-Conway stays lenient); how dups are compared (by the decoded item, e.g. raw bytes / the natural key); ordering NOT enforced' },
    scope_eras: { type: 'string', description: 'confirm pre-PV9 (Alonzo PV5-6 / Babbage PV7-8) MUST stay lenient (Haskell accepts dups there); only Conway PV9+ enforces. So the fix is Conway-decoder-scoped (era_conway read_set call sites)' },
    tests: { type: 'string', description: 'add: Conway set with a duplicate element -> decode Err (per affected field); pre-Conway set with a dup -> still Ok (lenient); a valid no-dup set -> Ok unchanged' },
    confidence: { type: 'number' },
    caveats: { type: 'string' },
  },
}

phase('Diagnose')
const d = await agent(
  'DIAGNOSE dugite backlog #31-C (Conway PV9+ Set duplicate-reject; the folded #30 fix-B). The #31 root-cause diagnose '
  + '(w2g366xg2, conf 0.9) already established the Haskell rule; this diagnose RESOLVES THE FIX ARCHITECTURE — the PV-threading '
  + 'and the exact read_set call sites — so the fix is clean and does NOT over-strict pre-Conway.\n\n'
  + 'ESTABLISHED (re-confirm + permalink): cardano-ledger-binary decodeSet is PV-gated: PV9+ uses decodeSetEnforceNoDuplicates '
  + '-> decodeListLikeEnforceNoDuplicates which does `when (len /= count) $ fail "...duplicate..."` (counts decoded items vs the '
  + 'dedup\'d Set size, HARD-FAILS on any duplicate element). Pre-PV9 (Alonzo/Babbage) is LENIENT (Set.fromList silently '
  + 'dedups, accepts dups). Ordering is NEVER enforced (it is a Set, not an OSet). dugite read_set (crates/dugite-serialization/'
  + 'src/decode/reader.rs:213-226) strips the optional tag-258 then read_array with NO dedup and NO count-check, so ALL tag-258 '
  + 'Set fields accept duplicates at PV9+ (a #539-class adversarial/latent consensus admission gap).\n\n'
  + 'RESOLVE:\n'
  + '  (1) Re-confirm the Haskell rule + PERMALINK-PIN (read RAW source — #31-A/#31-B caught WebFetch hallucinations on the '
  + 'version-gate; verify the ifDecoderVersionAtLeast (natVersion @9) gate and the count-check in '
  + 'cardano-ledger-binary Decoder.hs). Does the dedup count-check apply to ALL tag-258 Set fields (witness-set sets vkey/native/'
  + 'bootstrap/plutus_v1/v2/v3, AND body inputs/certs/collateral/required_signers/reference_inputs)? \n'
  + '  (2) Read dugite crates/dugite-serialization/src/decode/reader.rs read_set + ENUMERATE every call site of read_set across '
  + 'the decode crate (grep `read_set`). Classify each: Conway PV9+ tag-258 set (needs the strict count-check) vs pre-Conway set '
  + '(must stay lenient — Alonzo/Babbage accept dups) vs any non-set use. The Conway witness-set sites (era_conway.rs ~:2169-'
  + '2230) + Conway body set sites (inputs/certs/collateral/required_signers/reference_inputs) are the targets.\n'
  + '  (3) PV-threading: does the Reader carry protocol_major/era? Is the era/PV available at the Conway call sites (the Conway '
  + 'decoders know they are Conway/Dijkstra — both are PV9+)? The CLEANEST fix is likely a read_set_strict(item) variant '
  + '(enforces no-dups) called ONLY by the Conway PV9+ sites, since dugite\'s Conway decoders are statically Conway/Dijkstra '
  + '(both >=PV9) — so no runtime PV param is needed (Conway sites always strict, pre-Conway sites always lenient). CONFIRM this '
  + 'reasoning: are the Conway read_set sites ONLY ever used for PV9+ data? (Conway/Dijkstra are both PV9+, so yes — but verify '
  + 'no Conway read_set helper is shared with a pre-Conway decoder.)\n\n'
  + 'Use the in-project refs + cardano-ledger source. Give the exact fix plan (read_set_strict count-check; which call sites; '
  + 'how dups are compared — by the decoded item bytes), the scope (Conway-decoder-only; pre-Conway lenient), tests, confidence, '
  + 'and any caveat (does the witness-set vs body set use the same read_set? does making the Conway read_set sites strict risk '
  + 'an Allegra/Mary/Alonzo/Babbage site that reuses a Conway helper?). Return the StructuredOutput.',
  { label: 'diagnose:31c', phase: 'Diagnose', schema: SCHEMA, model: 'opus' }
)
return { d }
