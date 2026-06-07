export const meta = {
  name: 'prod-readiness-muscle',
  description: 'Diagnose, analyze, fix, and gauntlet-verify one production-readiness item',
  phases: [
    { title: 'Diagnose' },
    { title: 'Research' },
    { title: 'RootCause' },
    { title: 'Fix' },
    { title: 'Gauntlet' },
  ],
}

// A = { item, mode, net, reference, dumpPath, suspects?, refuterN?, tokenBudget? }
// mode ∈ { diagnose, analyze, fix, gauntlet }.  Every ANALYTICAL step of the engine
// runs through here so it is visible in /workflows; mechanical steps (clone, launch,
// poll) stay as direct shell in the runbook.
// `args` may arrive as an object OR a JSON string depending on the caller — parse defensively.
const A = (typeof args === 'string' ? (() => { try { return JSON.parse(args) } catch (e) { return {} } })() : args) || {}
const { item, mode, net, reference, dumpPath } = A

// Per-task model strategy (deterministic regardless of the launching session's model):
//   diagnose  -> Sonnet by DEFAULT : bounded, parallel, schema-constrained Koios-vs-dump
//               NUMERIC ledger comparison (reserves/treasury/stake vs Koios — the mode's
//               native use, e.g. #0/#11). Downstream Opus steps + the byte-exact replay
//               gauntlet validate it.
//               OVERRIDE to Opus via args.diagnoseModel:'opus' when the diagnose is
//               MECHANISM-HUNTING rather than numeric — deep root-causing where a wrong
//               conclusion burns a whole fix+gauntlet cycle (e.g. serialiseData byte-level
//               proof, input-provenance, UPLC/CBOR disassembly). Pick per task.
//   research / root-cause / fix / gauntlet -> Opus (ALWAYS) : deep ledger semantics,
//               byte-exact Rust, and the adversarial autonomy gate demand the strongest
//               reasoning (the #438 lesson: a wrong ledger fix that passes tests is the
//               failure mode). These are never weakened.
const VALID_MODELS = ['opus', 'sonnet', 'haiku']
const MODEL_DIAGNOSE = (VALID_MODELS.includes(A.diagnoseModel) ? A.diagnoseModel : 'sonnet')
const MODEL_REASON = 'opus'

// ---- structured-output schemas ----
const DIVERGENCE = {
  type: 'object',
  required: ['found', 'epoch', 'entity', 'field', 'delta_lovelace', 'evidence'],
  properties: {
    found: { type: 'boolean' },
    epoch: { type: 'integer' },
    entity: { type: 'string' },          // account/pool/global the divergence is on
    field: { type: 'string' },           // e.g. stake_distribution, reward, reserves
    delta_lovelace: { type: 'integer' }, // dugite - reference
    dugite_value: { type: 'string' },
    reference_value: { type: 'string' },
    evidence: { type: 'string' },
  },
}
const ROOTCAUSE = {
  type: 'object',
  required: ['hypothesis', 'evidence', 'haskell_source', 'spec_cite', 'field', 'confidence'],
  properties: {
    hypothesis: { type: 'string' },
    evidence: { type: 'string' },
    haskell_source: { type: 'string' },   // permalink + verbatim snippet
    spec_cite: { type: 'string' },
    field: { type: 'string' },
    confidence: { type: 'number' },
  },
}
const FIX = {
  type: 'object',
  required: ['files', 'diff_summary', 'tier', 'haskell_quote', 'checks_green'],
  properties: {
    files: { type: 'array', items: { type: 'string' } },
    diff_summary: { type: 'string' },
    tier: { type: 'string', enum: ['A', 'Aprime', 'B'] },
    haskell_quote: { type: 'string' },
    checks_green: { type: 'boolean' },    // fmt + clippy + nextest
  },
}
const VERDICT = {
  type: 'object',
  required: ['refuted', 'reason', 'lens'],
  properties: {
    refuted: { type: 'boolean' },
    reason: { type: 'string' },
    lens: { type: 'string' },
  },
}

if (mode === 'diagnose') {
  // Localize the FIRST divergence in a completed replay dump vs the reference
  // (Koios, or a cardano-node dump). Parallel fan-out over candidate dimensions —
  // this is the REPRODUCING→ANALYZING work, now visible in /workflows.
  phase('Diagnose')
  const dims = (A.dimensions) || [
    'per-epoch reserves/treasury/fees vs Koios totals',
    'per-account reward_history vs Koios account_reward_history',
    'per-pool stake_distribution / active_stake vs Koios pool_delegators_history + pool_stake_snapshot',
  ]
  const finds = await parallel(dims.map((dim) => () =>
    agent(
      `Item: ${item} (network ${net}). The replay dump is at: ${dumpPath || '(see engine-state.md Running jobs)'}. `
      + `Reference: ${reference}. Compare the dump to the reference along this dimension: ${dim}. `
      + `Ground truth: use scripts/prod-readiness/lib/koios.sh ${net} <endpoint> '<json-body>' via Bash `
      + `(per-network REST: pool_history, pool_delegators_history, pool_stake_snapshot, `
      + `account_reward_history, epoch_info). Do NOT use the koios_* MCP tools — they were observed `
      + `serving the WRONG network (Preview when preprod was expected), which silently breaks `
      + `byte-exact comparison. Report the EARLIEST epoch where they diverge, the exact entity `
      + `(account/pool), the field, and the lovelace delta (dugite - reference). `
      + `If none diverge on this dimension, found=false.`,
      { label: `diff:${dim.split(' ')[1] || dim.slice(0, 12)}`, phase: 'Diagnose', schema: DIVERGENCE, model: MODEL_DIAGNOSE },
    ).then((d) => d || { found: false, epoch: 0, entity: '', field: dim, delta_lovelace: 0, evidence: 'agent-skipped' })))

  const real = finds.filter(Boolean).filter((d) => d.found)
  // earliest-epoch divergence is the one to chase (others are downstream)
  real.sort((a, b) => a.epoch - b.epoch)
  return { mode, divergence: real[0] || { found: false }, all: real }
}

if (mode === 'analyze') {
  phase('Research')
  const research = await agent(
    `Item: ${item}\nNetwork: ${net}.\n`
    + `Read .claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md FIRST. `
    + `Then consult cardano-haskell-oracle for the canonical IntersectMBO/cardano-ledger `
    + `source for this calculation, then the ledger spec. `
    + `Return: the canonical Haskell calc (permalink + verbatim snippet) and the spec citation.`,
    { label: 'research', phase: 'Research', model: MODEL_REASON },
  )
  phase('RootCause')
  const rootcause = await agent(
    `Given this research:\n${research}\n\nRoot-cause "${item}". `
    + `Use the localized divergence already found (see engine-state.md; reference: ${reference}). `
    + `Be specific: the exact field, epoch, account/pool, and lovelace delta. Do NOT propose a fix yet.`,
    { label: 'root-cause', phase: 'RootCause', schema: ROOTCAUSE, model: MODEL_REASON },
  )
  return { mode, research, rootcause }
}

if (mode === 'fix') {
  phase('Fix')
  const fix = await agent(
    `Implement the byte-exact fix for "${item}" in dugite (network ${net}), in an `
    + `isolated git worktree. You MUST quote the exact Haskell source you are matching. `
    + `Classify the tier: A (reward/snapshot/era-transition/governance/fee math), `
    + `Aprime (phase-2 ScriptContext schema), or B (other non-ledger). `
    + `Run: cargo fmt --all -- --check; cargo clippy --all-targets -- -D warnings; `
    + `cargo nextest run --workspace. Return files, diff summary, tier, the haskell quote, `
    + `and whether all checks are green. Remember: green tests are NOT proof of byte-exactness.`,
    { label: 'fix', phase: 'Fix', isolation: 'worktree', schema: FIX, model: MODEL_REASON },
  )
  return { mode, fix }
}

if (mode === 'gauntlet') {
  // Only reached AFTER a VERIFYING replay reproduced `reference` with the divergence gone.
  phase('Gauntlet')
  const N = (A.refuterN) || 3
  const lensPool = ['haskell-semantics', 'edge-epoch', 'compounding-feedback', 'integer-rounding']
  const lenses = lensPool.slice(0, Math.max(3, Math.min(N, lensPool.length)))
  const votes = await parallel(lenses.map((lens) => () =>
    agent(
      `Adversarially REFUTE the fix for "${item}" via the ${lens} lens. `
      + `Context: a byte-exact replay reproduced ${reference} with the divergence gone, `
      + `and the fix quotes the canonical Haskell source. Try hard to find a case where `
      + `the fix is still wrong (an edge epoch, a compounding feedback, a rounding path, `
      + `a Haskell-semantics mismatch). Default refuted=true if you are uncertain.`,
      { label: `refute:${lens}`, phase: 'Gauntlet', schema: VERDICT, model: MODEL_REASON },
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens })))

  const real = votes.filter(Boolean)
  const refuteCount = real.filter((v) => v.refuted).length
  const pass = refuteCount < Math.ceil(lenses.length / 2)
  return { mode, pass, refuteCount, votes: real }
}

return { error: `unknown mode: ${mode}` }
