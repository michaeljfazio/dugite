export const meta = {
  name: 'prod-readiness-muscle',
  description: 'Analyze, fix, and gauntlet-verify one production-readiness item',
  phases: [
    { title: 'Research' },
    { title: 'RootCause' },
    { title: 'Fix' },
    { title: 'Gauntlet' },
  ],
}

// args = { item, mode, net, reference, tokenBudget }
const { item, mode, net, reference } = args || {}

// ---- structured-output schemas ----
const ROOTCAUSE = {
  type: 'object',
  required: ['hypothesis', 'evidence', 'haskell_source', 'spec_cite', 'field', 'confidence'],
  properties: {
    hypothesis: { type: 'string' },
    evidence: { type: 'string' },
    haskell_source: { type: 'string' },   // permalink + verbatim snippet
    spec_cite: { type: 'string' },
    field: { type: 'string' },            // the exact diverging field/epoch/account
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

if (mode === 'analyze') {
  phase('Research')
  const research = await agent(
    `Item: ${item}\nNetwork: ${net}.\n`
    + `Read .claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md FIRST. `
    + `Then consult cardano-haskell-oracle for the canonical IntersectMBO/cardano-ledger `
    + `source for this calculation, then the ledger spec. `
    + `Return: the canonical Haskell calc (permalink + verbatim snippet) and the spec citation.`,
    { label: 'research', phase: 'Research' },
  )
  phase('RootCause')
  const rootcause = await agent(
    `Given this research:\n${research}\n\nRoot-cause "${item}". `
    + `Use the diff-vs-Koios dump already produced for this item (see engine-state.md `
    + `Running jobs; reference: ${reference}). Be specific: the exact field, epoch, `
    + `account/pool, and lovelace delta. Do NOT propose a fix yet.`,
    { label: 'root-cause', phase: 'RootCause', schema: ROOTCAUSE },
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
    { label: 'fix', phase: 'Fix', isolation: 'worktree', schema: FIX },
  )
  return { mode, fix }
}

if (mode === 'gauntlet') {
  // Only reached AFTER a VERIFYING replay reproduced `reference` with the divergence gone.
  phase('Gauntlet')
  const N = (args && args.refuterN) || 3
  const lensPool = ['haskell-semantics', 'edge-epoch', 'compounding-feedback', 'integer-rounding']
  const lenses = lensPool.slice(0, Math.max(3, Math.min(N, lensPool.length)))
  const votes = await parallel(lenses.map((lens) => () =>
    agent(
      `Adversarially REFUTE the fix for "${item}" via the ${lens} lens. `
      + `Context: a byte-exact replay reproduced ${reference} with the divergence gone, `
      + `and the fix quotes the canonical Haskell source. Try hard to find a case where `
      + `the fix is still wrong (an edge epoch, a compounding feedback, a rounding path, `
      + `a Haskell-semantics mismatch). Default refuted=true if you are uncertain.`,
      { label: `refute:${lens}`, phase: 'Gauntlet', schema: VERDICT },
    ).then((v) => v || { refuted: true, reason: 'agent-skipped', lens })))

  const real = votes.filter(Boolean)
  const refuteCount = real.filter((v) => v.refuted).length
  const pass = refuteCount < Math.ceil(lenses.length / 2)
  return { mode, pass, refuteCount, votes: real }
}

return { error: `unknown mode: ${mode}` }
