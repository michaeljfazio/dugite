---
name: issue-1091-cli-surface-second-wave
description: "#1091 closed 11/22 of #1008's deferred CLI shims (cip-format×4, byron key aliases×2, ping, debug×2, key verification-key/non-extended-key); found 2 real dugite-node handshake bugs and 1 phantom cardano-cli command along the way"
metadata:
  type: project
---

2026-08-21. `scripts/validation/cli-surface-known-gaps.txt` is the source of
truth; `scripts/validation/cli-surface-parity.sh` (no live node needed —
just `cardano-cli` on PATH + a built `dugite-cli`) is the authoritative
verifier — run it after ANY change to that file, it PASSes only with 0
uncovered gaps / 0 stale allowlist entries / 0 malformed lines.

## CLOSED (11), all live-verified against real cardano-cli 11.0.0.0 unless noted

- **cip-format cip-129 × 4** (drep/committee-cold-key/committee-hot-key/
  governance-action-id) — CIP-129 encoding added to
  `dugite-primitives/src/governance.rs`: header byte `(type<<4)|cred_kind`,
  type∈{0=CC hot,1=CC cold,2=DRep}, cred_kind∈{2=key,3=script}; HRP stays
  `drep`/`cc_cold`/`cc_hot` (header disambiguates, not the HRP — unlike the
  pre-existing non-CIP129 module which uses `drep`/`drep_script` etc.).
  governance-action-id: HRP `gov_action`, payload = `txid(32) ||
  index_u16_be(2)`, NO header byte at all — different shape from the
  credential forms. New command file `crates/dugite-cli/src/commands/
  cip_format.rs`. `--governance-action-file`/`--governance-action-bech32`
  are BOTH permanently-broken flags in real cardano-cli 11.0.0.0 itself
  ("TextEnvelope encoded Governance Action Id is not supported" / "Bech32
  encoded..." — always, unconditionally) — dugite matches by also
  rejecting them, not by inventing a shape upstream can't read.

- **key convert-byron-genesis-vkey / convert-byron-key** — the #1008 gaps
  file called these "naming-normalization aliases" of the pre-existing
  `byron key convert-byron-genesis-vkey`/`convert-byron-key` and rated them
  "low priority, alias only". **That framing was wrong**, discovered by
  actually testing real cardano-cli: these commands don't even exist under
  `byron key` in real cardano-cli 11 (confirmed via `cardano-cli byron key
  --help` — only keygen/to-verification/signing-key-public/
  signing-key-address/migrate-delegate-key-from). They live ONLY at
  top-level `key convert-byron-*`, with a DIFFERENT flag surface and
  DIFFERENT semantics than dugite's pre-existing `byron key` versions:
  - `key convert-byron-genesis-vkey` takes an INLINE Base64 string (not a
    file), truncates 64→32 bytes, outputs `GenesisVerificationKey_ed25519`
    — a different envelope type than `byron key convert-byron-genesis-vkey`'s
    `GenesisUTxOVerificationKey_ed25519` (AVVM/redemption key, not a
    genesis delegate key — not the same real-world object at all).
  - `key convert-byron-key` needs a `--byron-{payment,genesis,genesis-
    delegate}-key-type` selector (3 families × signing/verification = 6
    output type strings, ALL captured live, not derived from docs — see
    the table in `crates/dugite-cli/src/commands/key.rs`'s doc comment).
    It does NOT change the key's crypto type or bytes — confirmed
    empirically that feeding a payment key with `--byron-genesis-key-type`
    silently relabels it as a genesis key with ZERO validation. "Convert to
    Shelley format" in cardano-cli's own help text means "re-wrap into the
    modern JSON envelope", not "derive a different key type" — I initially
    misread this as a cryptographic conversion and burned real time on that
    wrong model before testing settled it.
  - `--legacy-byron-*-key-type` (raw cardano-sl on-disk binary format, not
    JSON/Base64) and `--password` (encrypted keys) are explicitly NOT
    implemented — this repo has no cardano-sl binary parser or key-
    decryption anywhere, and inventing one wasn't in scope. Both fail with
    a clear message rather than a parse error.
  - Old `byron key convert-byron-key`/`convert-byron-genesis-vkey` left
    UNTOUCHED (out of scope, and they solve a genuinely different problem
    — deriving a Shelley-spendable key from Byron key material — that real
    cardano-cli's `key convert-byron-key` does not solve at all).

- **ping** — full N2N/N2C connectivity probe. `--host`/`--unixsock`,
  `--query-versions`, `--tip`, `--count` (N2N-only KeepAlive loop, reuses
  the existing `KeepAliveClient`). Needed 2 NEW public functions in
  `dugite-network::handshake` (`query_n2n_versions`/`query_n2c_versions`)
  because the existing `run_n2{n,c}_handshake_client`/`HandshakeResult`
  collapses a query-mode reply to a single best-match version and drops
  each entry's per-version data (#880's design, correct for its own
  negotiation-diagnostic caller, insufficient for `ping -Q`'s full-table
  listing) — new code re-decodes the SAME `MsgQueryReply` shape rather
  than duplicating the CBOR parsing. `--tip` over N2C is a **documented
  no-op in real cardano-cli itself** — verified against BOTH dugite-node's
  own socket and a real cardano-node's: `--tip` over a unix socket ALWAYS
  prints `{ "tip": [] }`, no real fetch happens. `--tip` over N2N genuinely
  fetches the tip via `MsgFindIntersect` with an EMPTY point list (can
  never intersect, so the reply's own tip field is all you get — clean
  trick, no bulk sync needed).
  **Found 2 real dugite-node bugs along the way, NOT fixed (out of #1091's
  scope — dugite-node internals, not dugite-cli):**
  1. dugite-node's N2N handshake responder closed the bearer immediately
     after `network_rtt`, before completing the handshake, when probed
     with real `cardano-cli ping --host <dugite-node's own N2N port>`. A
     real cardano-node on the same host/session negotiated cleanly at the
     same moment. Worth its own issue.
  2. dugite-node's N2C handshake SERVER (`run_n2c_handshake_server` in
     `dugite-network`) never inspects the client's `query` flag at all —
     unlike its N2N sibling, which does. A query-mode N2C proposal against
     it gets a plain `MsgAcceptVersion` (one version) instead of
     `MsgQueryReply` (the real full V16-V23 table a real cardano-node
     returns). `query_n2c_versions` tolerates this (treats a lone accept
     as a one-entry table) because real `cardano-cli ping -Q` does the
     same when talking to dugite-node's own socket — verified live, not
     assumed.

- **debug transaction view** — replaces the `transaction.rs` hand-rolled
  ~200-line minicbor walker (kept untouched — it's a documented SUPERSET
  entry at the OLD name, `transaction view`, not this command) with the
  REAL production decoder, `dugite_serialization::decode::decode_transaction`
  — same decoder the node's sync path uses. `Transaction` and its nested
  types already derive `Serialize`, so the JSON dump is
  `serde_json::to_value(&tx)`, not a second hand-built schema — Conway
  governance fields (`voting_procedures`, `proposal_procedures`,
  `treasury_value`, `donation`) now decode for real instead of "Field N:
  <present>". `--tx-body-file` (body-only, no witnesses) is handled by
  wrapping the raw body CBOR in a SYNTHETIC standalone-tx array
  (`[body, {}, true, null]`, or `[body, {}, null]` for Dijkstra per
  CIP-0167's missing `is_valid`) rather than adding a second body-only
  decode entry point to dugite-serialization — the body bytes go through
  the identical body-parser either way. `--output-yaml` NOT implemented
  (bails clearly) — no YAML serializer anywhere in the workspace, judged
  out of proportion for one flag. Verified end-to-end against real
  dugite-cli-built tx-body and signed-tx files (not just unit tests).

- **debug check-node-configuration** — scoped to genesis file hash/path
  checking (the command's own one-line description), NOT a full parse of
  cardano-node's entire `NodeConfiguration` Aeson schema — confirmed real
  cardano-cli 11.0.0.0 itself requires the FULL schema (`RequiresNetworkMagic`
  etc.) even for this narrow check, which dugite's own `config/*/config.json`
  files don't fully match either. Being lenient outside genesis-hash-
  checking can't produce a false PASS on what the command is actually for.
  **Found a real bug in this exact process**: my first pass hashed Byron
  genesis as raw file bytes and got a FAIL against preprod's real
  `config.json` (`559db4de…` computed vs `d4b8de7a…` declared) — this
  matches [[reference_byron_genesis_hash.md]] exactly (Byron hashes
  CANONICAL JSON, sorted keys no whitespace, not raw bytes; the existing
  `dugite-node/src/config.rs::byron_genesis_hash`/`write_canonical_json`
  is `pub(crate)` so dugite-cli can't import it — the algorithm is copied,
  not duplicated by accident, and pinned against the same real preprod
  vector dugite-node's own test uses). This is a positive case of the
  "verify against ground truth, not just self-consistency" discipline
  catching a real mistake before it shipped.

- **key verification-key / key non-extended-key** — GENERIC key-envelope
  transforms, no BIP-39/HD-derivation infrastructure needed (unlike the
  rest of the `key convert-itn-*`/`derive-from-mnemonic`/
  `generate-mnemonic`/`convert-cardano-address-key` cluster, which does).
  `verification-key`: output `type` = input `type` with `"SigningKey"` →
  `"VerificationKey"` (literal substring replace) — verified live on BOTH
  a payment key and a DRep key, byte-identical output to real cardano-cli
  both times. Extended (BIP32) signing keys explicitly rejected with a
  clear error (dugite has no Ed25519-BIP32 scalar multiplication anywhere
  — same gap as the deferred mnemonic cluster). `non-extended-key`: 64→32
  byte truncation (drop chain code) + strip `"Extended"` from the type
  name — this one is NOT independently live-verified (no way to produce a
  real extended verification key without the BIP32 tooling that's
  deferred), disclosed as such in the doc comment rather than claimed as
  verified.

## WONTFIX correction: transaction signed-transaction does not exist

`transaction signed-transaction` was in #1008's DEFERRED list. Verified by
enumerating cardano-cli 11.0.0.0's ENTIRE `transaction`/`debug transaction`
subcommand tree (`--help` at every level) — no such command exists
anywhere, under any era prefix. Almost certainly a stale/incorrect artifact
of #1008's original surface-parity scrape. Moved to WONTFIX with that
reasoning recorded rather than silently deleted.

## DEFERRED (10), reasons UPDATED to reflect real investigation

- `debug log-epoch-state` — needs a live-node streaming loop that "will
  not terminate" (cardano-cli's own description) — different shape of
  work from the other two `debug` commands, not rushed alongside them.
- `key convert-cardano-address-key` / `convert-itn-{bip32-,extended-,}key`
  / `derive-from-mnemonic` / `generate-mnemonic` (6 commands) — ALL need
  real BIP-39 (wordlist/checksum/entropy) and/or Ed25519-BIP32 HD
  derivation (CIP-3/CIP-1852) this repo has NO implementation of anywhere.
  A genuine crypto feature wave, not a thin CLI shim — same judgment call
  as leaving Byron's raw-binary/legacy key formats out of scope above.
- `transaction build-estimate` — real complexity confirmed by reading its
  full `--help`: essentially the WHOLE `transaction build-raw` flag
  surface (~30 option groups — Plutus refscripts, certs, withdrawals,
  votes, proposals) reused with `--protocol-params-file`/
  `--total-utxo-value` instead of a live node. A near-full offline tx
  builder, not a shim.
- `transaction calculate-plutus-script-cost {online,offline}` — dugite HAS
  the CEK machine + cost models this needs (per the issue's own framing),
  but still needs the full tx-context/UTxO-set input plumbing
  (`--tx-file`/`--protocol-params-file`/utxo args) wired up — not
  attempted this session given time, but genuinely the most promising
  item left for a future pass given the existing CEK infrastructure.

## Process note

`scripts/validation/cli-surface-parity.sh` ran CLEAN after all edits: 0
uncovered gaps, 0 stale allowlist entries, 0 malformed lines, and
independently confirmed every closed command now matches cardano-cli's
real (recursively-walked) `--help` tree — not just my own claim that it
works. Worth running this after ANY future change to the gaps file or the
CLI surface; it needs no live node, just `cardano-cli` on PATH.
