# 19-era-negatives (#1034)

Conway wire-rejection of legacy-era artifacts. All six scripts here are
**parity-EXCLUDED** (see `denominators.json`, not edited by this category —
these transactions have no valid current-era counterpart, so there is
nothing for the bidirectional-parity oracle to compare against): a MIR
certificate, a genesis-key-delegation certificate, and a legacy Shelley
update proposal have no Conway equivalent at all, and dugite is *supposed*
to reject them.

## What's being pinned

`#1023` fixed dugite's Conway decoder to hard-reject certificate tags 5
(`GenesisKeyDelegation`) and 6 (`MoveInstantaneousRewards`) at CBOR-decode
time, matching cardano-ledger's own Conway decoder exactly:

```
| t == 5 -> fail "Genesis delegation certificates are no longer supported"
| t == 6 -> fail "MIR certificates are no longer supported"
```

(`crates/dugite-serialization/src/decode/era_conway.rs`.) This category is
that fix's permanent on-the-wire regression pin, plus three adjacent probes
of the SAME underlying question via a different wire path.

## Two genuinely different mechanisms — do not conflate them

### 19e / 19f — the CONFIRMED, currently-fixed path

Build a **valid Conway transaction** (one stake-registration certificate,
tag 7 `reg_deposit_cert`), then use `tx-cbor-tool.py`'s new
`splice-cert-tag` subcommand to overwrite *just that certificate's own tag
byte* — 7 → 6 for 19e, 7 → 5 for 19f — leaving every other byte of the
certificate (and its arity) untouched. cardano-ledger's Conway certificate
decoder dispatches on the tag integer **first** and fails immediately,
before it would ever look at the rest of the array, so a 3-element donor
certificate spliced onto a tag whose real shape is 2-element (MIR) or
4-element (GenesisKeyDelegation) is exactly the point: the decoder must
reject at the **tag**, not the arity.

This is solid, confirmed-in-code behavior — `#1023`'s fix landed in exactly
this code path — and was independently confirmed empirically while
authoring this category: `cardano-cli conway transaction txid --tx-file
<spliced>` itself hard-rejects with the **exact** cardano-ledger message

```
DeserialiseFailure 95 "MIR certificates are no longer supported"
```

**before ever opening a socket**, because cardano-cli 11.0.0.0 reads a
Conway-tagged tx file through the real ledger decoder. That is why 19e/19f
submit via `dugite-cli transaction submit` (raw cborHex forwarding, no
local decode — same precedent as `08-negative/08f-double-spend.sh`) rather
than `cardano-cli conway transaction submit`: we want the rejection to come
from the **node's** decoder, not the CLI's.

### 19a-19d — a DIFFERENT wire path than the "era-mismatch" framing suggests

These build a **genuine Shelley-era transaction** (`cardano-cli compatible
shelley transaction signed-transaction` — a real top-level `array(3)`
`[body, witness_set, aux_data]`, envelope type `TxSignedShelley`) carrying
constructs Conway's wire format cannot express at all: MIR certs (19a/19b),
a genesis-key-delegation cert (19c), a legacy protocol-parameters update in
tx-body key 6 (19d). `cardano-cli conway transaction submit` auto-detects
the envelope's declared era and forwards the bytes untouched — confirmed
empirically (it proceeds to open the socket rather than refusing the file
client-side, unlike the 19e/19f case above) — so no raw-socket fallback is
needed for submission itself.

**IMPORTANT SUBTLETY — read before trusting an "era mismatch" rejection
message from a live run.** The natural expectation (and this category's
original framing) is that rejection here comes from an HFC-style
*era-mismatch* check: the wire `era_id` the client declares (1 = Shelley)
doesn't match the chain's current era (Conway), so the node refuses the
submission without ever looking at the ledger rules. That is what
`ouroboros-consensus`'s `HardForkApplyTxErrWrongEra` does in Haskell. Three
things discovered while implementing this category mean dugite's actual
rejection mechanism — if any — is almost certainly **not** that, and the
scripts are written to assert generic rejection (any reason, from both
observers) rather than pattern-match a specific "era" or "mismatch" string:

1. **dugite has no wire-level era-vs-ledger-era check at all.**
   `LocalTxSubmissionServer::run` reads the client-declared `era_id` off the
   wire and passes it straight to `TxValidator::validate_tx(era_id, bytes)`
   with no comparison against the ledger's actual current era
   (`crates/dugite-network/src/protocol/local_tx_submission/server.rs`,
   `crates/dugite-node/src/node/serve.rs::LedgerTxValidator::validate_tx`).
   Unlike Haskell, there is no `WrongEra`-equivalent short-circuit.

2. **`decode_transaction` dispatches on the client-declared `era_id`,
   routing era_id=1 to a genuinely different decoder** —
   `era_shelley::decode_shelley_tx_standalone` — which does NOT hard-reject
   MIR or GenesisKeyDelegation certs (they were valid constructs in
   Shelley; `#1023`'s fix is scoped to the Conway decoder only,
   `era_conway.rs`). **That Shelley standalone decoder currently expects a
   top-level `array(4)`** (`crates/dugite-serialization/src/decode/era_shelley.rs`,
   `decode_shelley_tx_standalone`, comment "tx = [body, witness_set,
   is_valid, aux_data]"), but a genuine Shelley tx — confirmed by building
   one with `cardano-cli compatible shelley transaction signed-transaction`
   and inspecting the CBOR — is `array(3)` (`[body, witness_set,
   aux_data]`; Shelley predates the `is_valid` flag, which Alonzo
   introduced). **So the realistic outcome is a generic CBOR array-length
   decode error** ("expected array(4), got array(3)"), unrelated to MIR,
   genesis keys, or era semantics at all. Rejection should still occur —
   just via an unrelated code path than the task brief assumed.

3. **MIR validation is a documented Phase-1 no-op at PV≥9**
   (`crates/dugite-ledger/src/validation/mir.rs`: "Conway has removed
   MIRCert entirely — at PV >= 9 every MIR predicate is a no-op (`Ok(())`)")
   and **GenesisKeyDelegation has full, era-unconditional apply-time
   support** (`crates/dugite-ledger/src/eras/common.rs`
   `enqueue_genesis_key_delegations`, `crates/dugite-ledger/src/state/certificates.rs`).
   **If dugite's Shelley decoder is ever fixed to accept `array(3)`**, a
   properly-witnessed MIR or genesis-key-delegation cert submitted this way
   would very plausibly be **ACCEPTED** — Phase-1 would not reject it, and
   the apply path would happily adopt a genesis delegation with no era
   gate. That would be a genuine, currently-latent compliance gap versus
   Haskell's HFC-level `WrongEra` rejection, distinct from and *not* fixed
   by `#1023`. Filing a follow-up issue once this is confirmed live is
   recommended — see "Open risks for live verification" below.

Because of all three points, 19a-19d today most likely collapse onto the
**same** underlying rejection mechanism (the array(3)/array(4) decode-shape
mismatch) regardless of their differing cert/body-field content. That
redundancy is not wasted coverage: if the Shelley decoder is ever fixed,
each script starts exercising a genuinely different downstream code path
(MIR no-op, GenesisKeyDelegation witness/apply, the body-key-6 shape
clash), and this category will need to be re-verified against the new
behavior at that point.

## The tag-0 vs PV10/PV11 trap — do NOT add this as a negative here

Conway certificate tag 0 (`reg_cert`, the deposit-less legacy stake
registration) inside a **Conway** transaction is still **VALID at PV10**.
Its deprecation (rejection) starts at **PV11**, as part of the hardfork
round — this devnet runs Conway PV10, so a tag-0 negative would be a false
positive here and belongs to the PV11 hardfork-round test suite instead,
not to this category. (Tags 5 and 6, by contrast, were removed at the
Shelley→Conway *era* boundary, which is unconditional and already past —
that is the whole reason `#1023`'s fix has no PV gate.)

## Splice mechanics (`tx-cbor-tool.py`)

`splice-cert-tag --in FILE --out FILE --index N --tag T` locates
certificate `N` in the tx-body's `certs` field (key 4, `OSet` under
`tag(258)`), then overwrites *only* that certificate's own leading tag
integer (its first array element — the constructor discriminator) with
`T`, using minimal-length CBOR head encoding. It does not touch the
certificate's remaining fields or the array's declared length — the
resulting arity intentionally does not match `T`'s real shape, which is the
point (see "19e / 19f" above). `show-certs --in FILE` prints each
certificate's tag as JSON, used by 19e/19f to confirm the splice landed on
the intended tag before the modified body is re-signed. Both are covered by
a standalone `python3` unit check against hand-built synthetic CBOR (no
devnet, no cardano-cli) exercising: correct single-byte splice (7→6, same
total length), correct multi-byte-growing splice (7→24, proves the tool
isn't accidentally relying on the lucky single-byte case that tags 5/6
happen to hit), byte-for-byte non-interference with everything outside the
spliced tag, out-of-range index, and a certs-less tx — all pass.

## Recording divergent reject reasons

Neither dugite nor a real cardano-node is required to answer with the same
wire shape to be correct — see `19e`'s header for the specific
`#925`-class caveat (Haskell may drop the connection rather than answer a
structured `MsgRejectTx` on certain decode failures). The zoo does not have
a symbol literally named `known_reject_reason_differences`; the closest
existing convention — followed here — is `16-cert-negatives/_cert-neg-helper.sh`'s
`expect_cert_rejection` and `08-negative/08f-double-spend.sh`'s `REASON`
variable: **record the observed text from both observers in the CSV detail
field rather than asserting they match.** `era_neg_assert_rejected_both` in
`_era-neg-helper.sh` does this: PASS requires both reachable observers to
refuse the transaction (any reason); the detail field always carries both
raw (truncated) responses side by side.

## Open risks for live verification

- **The core open question this category cannot answer statically**: does
  dugite's Shelley standalone-tx decoder's `array(4)` vs the real Shelley
  `array(3)` shape actually cause the rejection 19a-19d expect on a live
  run, and if the array-length check is ever "fixed" to accept `array(3)`,
  do 19a/19b (MIR) then get **accepted** — confirming the latent gap
  described above? A live run's *detail* field (not just PASS/FAIL) is the
  only way to tell; if the observed reason ever stops mentioning
  `array(4)`/CBOR decode and starts looking like a real Phase-1 verdict,
  re-read this README's mechanism section before trusting the result.
- **`run-all.sh`'s `ALL_CATEGORIES` guard.** `run-all.sh` (not edited by
  this change, per scope) FATALs on any `[0-9][0-9]-*/` directory not
  listed in its hardcoded `ALL_CATEGORIES` array — see the comment right
  above that array, which explicitly warns this is how `17-context-inspecting`
  went silently unrun for a time. `19-era-negatives` will trip that guard
  until `19-era-negatives` is added to `ALL_CATEGORIES` (and, separately,
  to `denominators.json` if this category should ever count toward a
  parity denominator — it should not, per the parity-EXCLUDED framing
  above). Until then this category can only be run directly, e.g.
  `./19-era-negatives/19a-compat-mir-treasury-reserves.sh`, not via
  `run-all.sh` or `run-all.sh 19-era-negatives`.
- **19c/19d's genesis-key dependency.** Both read
  `$LD_KEYS/genesis-keys/genesis1/{key.vkey,key.skey}`, provisioned by
  `setup.sh`'s `cardano-cli conway genesis create-testnet-data
  --genesis-keys 3` step and copied into `$LD_KEYS` at the end of
  `setup.sh`. If a devnet was brought up by an older `setup.sh` revision
  that didn't preserve this directory, both env-skip cleanly
  (`genesis1-keys-missing-under-...`) rather than failing.
- **19a/19b/19d were never run against a live devnet by this change** (the
  task explicitly scoped this to static implementation — `bash -n` plus the
  standalone Python splice check only). The `cardano-cli` command syntax
  they use (`compatible shelley transaction signed-transaction`'s
  single-token `ADDRESS+VALUE` form for `--tx-out`, `--certificate-file`
  and `--signing-key-file` both being repeatable flags, `create-mir-certificate`'s
  four subcommand forms) was verified interactively against the installed
  cardano-cli 11.0.0.0 binary and cross-checked by building real signed
  envelopes and inspecting their CBOR byte-for-byte, but the actual
  node-side submission outcome is unverified until a devnet round runs
  them.
