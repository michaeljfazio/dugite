---
name: kb-table-files-missing-use-live-github
description: The oracle_ledger_*.md files named in this agent's own system-prompt "Knowledge Base" table (architecture/state/validation/certificates/epoch_transitions/governance/types_crypto/wire_format/block_pipeline) do NOT exist on disk as of 2026-07-07. Read this before wasting a turn trying to Read them.
metadata:
  type: project
---

Checked 2026-07-07 (`find /Users/michaelfazio/Source/dugite -iname "oracle_ledger*"` returns nothing
anywhere on the filesystem). The actual persistent memory directory
(`.claude/agent-memory/cardano-ledger-oracle/`) only contains narrow, topic-specific files built up
from individual past investigations (see this directory's `MEMORY.md` for the real index) — there is
no comprehensive pre-built KB matching the 9-file table in this agent's system prompt.

**Why:** unclear — either the KB was never actually generated, or it was generated in a different
location/session and lost. Either way, don't assume it exists.

**How to apply:** when a question isn't covered by an existing topic file in this directory, do NOT
report "beyond my knowledge base, escalate to cardano-haskell-oracle" as a dead end — instead, this
agent has `Bash` access and `gh` is installed and authenticated (confirmed working 2026-07-07,
`gh auth status` → logged in as michaeljfazio). Use it directly:

```bash
gh api search/code -X GET -f q='SYMBOL_OR_STRING repo:IntersectMBO/cardano-ledger' --jq '.items[].path'
gh api repos/IntersectMBO/cardano-ledger/contents/PATH/TO/File.hs --jq '.content' | base64 -d > /path/scratch/File.hs
```
`gh api search/code` requires exact identifier/string matches (it's GitHub's code-search index, not
fuzzy) — search for a distinctive function/constructor name mentioned in the question, then fetch
the full file via `contents` + base64 decode. This is exactly as authoritative as what
`cardano-haskell-oracle` would produce (same source, same live GitHub read) — no need to hand off
unless the question is actually outside cardano-ledger (e.g. ouroboros-consensus/network, Plutus CEK
internals, cardano-node config), per the existing escalation rule.

After doing this kind of live research, persist genuinely durable, broadly-reusable findings as new
topic memory files here (as done for [[reapply-validatenone-predicate-skip-mechanics]] and
[[reward-calc-floor-chain-and-sigma-vs-sigmaA]], both from the 2026-07-07 session that produced this
note) so the next session doesn't have to re-derive them from scratch.
