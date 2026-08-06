---
name: issue-1050-1051-collateral-refinput-wire-fixes
description: CollateralContainsNonADA/InsufficientCollateral zero-arm fix + BabbageNonDisjointRefInputs spurious Set-tag fix, with the exact CollateralContainsNonADA firing/payload formula
type: reference
---

Fixed live-reachable (PV10) N2C LocalTxSubmission wire bugs found by tx-zoo
18a/18b/18f, in `crates/dugite-ledger/src/validation/{mod,collateral}.rs`,
`crates/dugite-network/src/lib.rs` + `protocol/local_tx_submission/encode.rs`,
`crates/dugite-node/src/node/serve.rs`.

## #1050a — InsufficientCollateral (UTXO tag 12), zero encoder arm

`ValidationError::InsufficientCollateral` was a unit variant → `TxValidationError::InsufficientCollateral`
was a unit variant → **no encoder arm existed at all** (4th confirmation of
the #1025 "carries no data → falls to generic ConwayMempoolFailure" pattern,
after MissingRedeemer/MalformedProposal/CollateralHasTokens/StakePoolNotRegisteredOnKeyPOOL/BabbageOutputTooSmallUTxO).

Fix: both now carry `{ balance: i128, required: u64 }`. `balance` = the
signed `effective_collateral` dugite already computes (inputs minus
`collateral_return`, can be negative). `required` = `ceil(fee *
collateral_percentage / 100)`, computed by a new `required_collateral()`
helper in collateral.rs (u128 multiply to avoid overflow, `.div_ceil(100)`).//
Wire: `array(3)[12, balance_i64, required_u64]` — `DeltaCoin` (`newtype
DeltaCoin = DeltaCoin Integer`, newtype-derived `EncCBOR`) is a **bare signed
CBOR int, no wrapper** — encode with `enc.i64()`, not the `Coin`/u64 encoder.
Oracle: `Sum InsufficientCollateral 12 !> To a !> To b`, `a::DeltaCoin`
(balance) first, `b::Coin` (required) second — matches dugite's own N2C
decoder (`n2c_client.rs` tag 12) which already read `[delta, required]` in
that order (decoder was silently correct the whole time; only the encoder
was missing).

Scope note: dugite's `body.collateral.is_empty()` early-return also raises
`InsufficientCollateral` (balance=0). Haskell actually has a distinct
fieldless `NoCollateralInputs` (tag 19) for that case — **not fixed here**,
out of scope; only made the existing (arguably-wrong-predicate) payload
wire-correct.

## #1050b — CollateralHasTokens → CollateralContainsNonADA (UTXO tag 15), zero encoder arm + WRONG intended payload

Same zero-arm bug, but the payload is the interesting part — **my first
attempt at the trigger/payload formula was wrong and got corrected by the
oracle mid-task**. Record precisely, this is easy to get backwards:

**Haskell (`Cardano.Ledger.Babbage.Rules.Utxo`, `validateCollateralContainsNonADA`,
unchanged in Conway) `CollateralContainsNonADA (Value era)` payload construction:**

```haskell
collateralBalance = sumAllValue utxoCollateral   -- RAW sum of collateral INPUTS' Value (coin+multiasset), never netted
valueWithNonAda =
  case txBody ^. collateralReturnTxBodyL of
    SNothing -> collateralBalance
    SJust retTxOut ->
      if utxoCollateralHasOnlyAda   -- INPUTS alone (not inputs+return) are ada-only
        then retTxOut ^. valueTxOutL   -- report the RETURN's own Value
        else collateralBalance          -- report the raw INPUT sum, unreturned
```

**Firing condition** (do NOT conflate with the payload branch above — it's a
*different* boolean): reject unless `utxoCollateralAndReturnHaveOnlyAda OR
isAdaOnly(collateralBalance <-> retValue)`. Proof this SIMPLIFIES to exactly
dugite's pre-existing netted `has_net_tokens` check (inputs-minus-return,
prune zero-diff entries, any nonzero entry left ⇒ fire): if inputs+return are
both ada-only, the netted diff is trivially ada-only too, so the OR's first
clause is redundant — the fire condition collapses to "netted value is not
ada-only after pruning". **So the TRIGGER did not need to change, only the
PAYLOAD did.** Confirmed against all 4 existing test scenarios in
collateral.rs (no-return-has-tokens, return-absorbs-all-tokens,
return-over-declares-a-phantom-token → negative residual, inputs-ada-only-
return-has-tokens): every one still fires/doesn't-fire identically under the
old netted trigger.

Fix in `check_collateral`: snapshot `collateral_multi_asset` (raw, i128,
still non-negative) as `collateral_balance_value` (a real
`dugite_primitives::value::Value`) via a new `multi_asset_i128_to_value()`
helper, BEFORE folding `collateral_return` into the netted accumulator used
for the trigger. Also compute `inputs_have_only_ada` from that same
pre-netting snapshot. Then on fire: `match collateral_return { None =>
collateral_balance_value, Some(ret) => if inputs_have_only_ada { ret.value }
else { collateral_balance_value } }`.

Wire: `array(2)[15, value_bytes]` where `value_bytes` = whatever
`dugite_serialization::encode_value()` (the SAME encoder tx outputs use)
produces — bare uint for ada-only, `array(2)[coin, multiasset_map]`
otherwise. Reused verbatim, not re-implemented, so the two paths can't drift
(#932/#938 lesson).

**Trap for next time**: my first draft assumed the payload was simply "the
net collateral value with tokens" (the netted balance) — plausible-sounding
and WRONG. The oracle's first answer only quoted the payload-construction
code and I nearly shipped it without separately verifying the firing
condition was unaffected; the coordinator caught the ambiguity and a
follow-up round trip nailed down that `valueWithNonAda` and the trigger
boolean are two different things computed from overlapping-but-distinct
subexpressions. **Always get both the trigger condition AND the payload
formula quoted verbatim, never assume the payload is "the same expression
that fired the check."**

## #1051 — BabbageNonDisjointRefInputs (UTXO tag 22), spurious tag-258 Set wrapper

`ReferenceInputOverlapsInput`'s encoder wrapped its `NonEmpty TxIn` payload in
`enc.tag(CBOR_TAG_SET)` — cardano-cli's Haskell-derived decoder hard-crashes
on it (`DeserialiseFailure "expected list len or indef"`); dugite's OWN
`n2c_client.rs` decoder used a generic `decoder.skip()` for this tag and
tolerated either shape, which is exactly why the bug was invisible to every
existing same-crate round-trip test. Oracle-confirmed: `BabbageNonDisjointRefInputs
(NonEmpty TxIn)`, `EncCBOR (NonEmpty a) = encCBOR . toList`, a bare
`variableListLenEncoding` list — tag 258 belongs exclusively to `Set`'s own
`EncCBOR` instance and never applies to `NonEmpty`. Fix: replaced
`enc.tag(CBOR_TAG_SET); enc.array(1); ...` with `list_open(enc,1); ...;
list_close(enc,1)` (the same helper already used elsewhere in this file for
`variableListLenEncoding`-shaped payloads).

**The pre-existing golden test (`test_encode_reference_input_overlaps`)
PINNED the bug** — it decoded only as far as the tag number (22) and never
inspected the payload bytes, so it passed identically before and after the
fix. Renamed to `test_encode_reference_input_overlaps_golden` with full
byte-exact assertion, plus a dedicated
`reference_input_overlaps_never_emits_set_tag` regression pin that scans for
the `[0xd9,0x01,0x02]` tag-258 prefix anywhere in the output. #948-class
trap, confirmed again.

## Round-trip methodology note

`n2c_client.rs`'s decode helpers are all private `fn` except
`pub(crate) fn decode_reject_reason(decoder: &mut Decoder) -> Option<String>`
— the ONE crate-visible entry point, takes the full `encode_apply_tx_err`
output (`[[era_id,[failures]]]`) verbatim. Used it for genuine cross-module
(same-crate) round-trip tests in `encode.rs`'s test module via
`crate::n2c_client::decode_reject_reason`. This is the only practical
round-trip surface — everything else in n2c_client.rs is module-private and
not worth widening visibility for.
