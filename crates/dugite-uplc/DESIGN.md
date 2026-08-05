# dugite-uplc — Design synthesis

> **Status: pre-implementation.** The crate currently exposes only module
> skeletons + this design doc. The first commit lands the scaffolding +
> the synthesis here; subsequent commits add `flat`, `data`, `machine`,
> `builtin`, and the phase-2 wrapper in that order.

## Goal

Replace the `aiken-lang/uplc` dependency (and its transitive
`pallas-codec`/`pallas-primitives`/`pallas-addresses`/`pallas-crypto`/`pallas-traverse`
chain) with a first-party Rust implementation of:

1. Untyped Plutus Core AST.
2. The flat wire codec.
3. The PlutusData type + its CBOR codec.
4. The CEK abstract machine.
5. The full Cardano default-builtin suite (V1, V2, V3).
6. A V1/V2/V3 script-context (`TxInfo` / `ScriptContext`) builder.
7. A `phase_two_evaluate` façade that mirrors aiken's
   `tx::eval_phase_two_raw` API surface for drop-in replacement in
   `dugite-ledger/src/plutus.rs`.

Non-goals:

- A textual UPLC parser (only used for tests; we'll add one later if
  needed for conformance vectors).
- Program optimisation passes (the validation path doesn't run them).
- WASM / browser support.

## Hard requirements (all enforced by `#![deny]` lints at the crate root)

1. **Panic-free on adversarial input.** No `unwrap`, `expect`, `panic!`,
   `todo!`, `unimplemented!`, `unreachable!` reachable from any byte
   that comes off the wire. Adversaries control the witness-set script
   bytes via gossiped transactions — a panic is a remote DoS.
2. **Bounded allocation.** Every peer-supplied length header is
   sanity-clamped (the same `safe_alloc_capacity` pattern we landed in
   `dugite-serialization::decode::reader`).
3. **Bounded recursion.** No raw recursion over the term tree; the CEK
   machine carries an explicit heap-allocated continuation stack. The
   flat decoder and PlutusData decoder both thread an explicit depth
   counter.
4. **Bit-for-bit byte-exact compatibility** with cardano-node Haskell on
   the wire: flat-encoded scripts and CBOR-encoded `Data` round-trip
   identically; CEK evaluation produces identical `EvalResult` for
   identical inputs across platforms.
5. **No third-party UPLC dependencies.** No aiken-uplc, no
   pallas-anything, no amaru-uplc. The only runtime deps are
   `blake2`, `sha2`, `sha3`, `secp256k1` or `k256`, `blst`,
   `num-bigint`, `num-traits`, `minicbor`, `thiserror`, `tracing`,
   `hex`.

## Authoritative references

Normative (must match byte-for-byte):

| Topic | Source |
|---|---|
| UPLC language + CEK reduction rules | `IntersectMBO/plutus:plutus-core/docs/plutus-core-spec` (typeset PDF + LaTeX). |
| Flat encoding | Same spec, "Flat" appendix. |
| `PlutusData` CBOR | `IntersectMBO/plutus:plutus-core/plutus-core/src/PlutusCore/Data.hs`. |
| Default builtins | `IntersectMBO/plutus:plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs`. |
| BLS12-381 | CIP-0381 (zkcrypto/IETF serialisation, BLS12381G[12]\_XMD:SHA-256\_SSWU\_RO\_, strict subgroup checks). |
| Keccak-256 + Blake2b-224 | CIP-0101. |
| RIPEMD-160 | CIP-0127. |
| IntegerToByteString / ByteStringToInteger | CIP-0121. |
| ByteString logical builtins (and/or/xor/complement, readBit, writeBits, replicateByte) | CIP-0122. |
| ByteString bitwise builtins (shift/rotate, countSetBits, findFirstSetBit) | CIP-0123. |
| Explicit script return values (V3 must evaluate to unit) | CIP-0117. |
| ScriptContext V2 | CIP-0033. |
| ScriptContext V3 + governance | CIP-0035 + CIP-1694. |
| Cost-model parameter order | `IntersectMBO/plutus:plutus-core/cost-model/` + Conway-era `cardano-ledger-conway/cddl-files/conway.cddl`. |
| Conformance corpus | `IntersectMBO/plutus:plutus-conformance/test-cases/`. |

### Reference-implementation tie-break order

When two of our reference implementations disagree, resolve in this strict
priority (per user directive 2026-05-22):

  1. **`IntersectMBO/plutus` (Haskell)** — most authoritative. This is
     what runs on mainnet. If a Rust implementation diverges from
     Haskell, the Rust implementation is wrong by definition.
  2. **`pragma-org/uplc` (`amaru-uplc`)** — second. Useful for
     idiomatic Rust translation choices but supersedable by Haskell.
  3. **`aiken-lang/uplc`** — third. Useful as a corner-case oracle but
     known to carry adversarial-input panics we explicitly do *not*
     reproduce.

The UPLC-1 PlutusData encoder fix (#560) is the canonical example: pragma
and aiken disagreed on indefinite-length encoding of short `Constr` /
`List` payloads. The Haskell `Codec.Serialise.Class.encodeList`
implementation was inspected directly and used as ground truth; the
indefinite-length form pragma emits and the definite-length form aiken
historically used are both legal CBOR but only the Haskell form
round-trips byte-exactly with mainnet — so the Rust implementations'
disagreement was resolved in favour of Haskell.

Reference implementations to study (NOT to depend on):

| Source | What to steal | What to skip |
|---|---|---|
| `IntersectMBO/plutus` (Haskell) | Canonical CEK + cost-model + builtin denotations. Read line-by-line when designing each component. | — (reference only). |
| `pragma-org/uplc` (`amaru-uplc`) | Bumpalo arena + three-state CEK driver + slippage budget + linked Env/Context. Generic-over-binder pattern. Builtin enum with `#[repr(u8)]` + `force_count()`/`arity()`/`is_available_in()`. | Unbounded recursive `decode_term` (DoS), `Constr.tag: usize` truncation, `Runtime` arg-vec clone-per-apply (quadratic), string-keyed `HashMap` cost map, missing `phase_two` API. |
| `aiken-lang/uplc` | Field-by-field `TxInfoV1/V2/V3` constructors in `tx/script_context.rs`; redeemer iteration shape in `tx/eval.rs`. | `Rc<Term>` cloning hot path, pallas-codec dep, 30+ `unwrap`/`unreachable!`/`unimplemented!` sites on adversarial paths (catalogued in §6 below). |

## Architecture

```
crates/dugite-uplc/
├── src/
│   ├── lib.rs           // re-exports + crate docs
│   ├── error.rs         // UplcError taxonomy
│   ├── term.rs          // Term, Constant, TypeTag, BuiltinId
│   ├── data.rs          // PlutusData
│   ├── program.rs       // Program (CBOR ⇄ flat ⇄ AST boundary)
│   ├── flat/
│   │   ├── mod.rs       // shared types, FLAT_MAX_DEPTH
│   │   ├── decode.rs    // bit-stream → Term
│   │   └── encode.rs    // Term → bit-stream
│   ├── machine/
│   │   ├── mod.rs       // ExBudget, EvalResult
│   │   ├── env.rs       // De Bruijn env (cons-list)
│   │   ├── context.rs   // continuation frames
│   │   ├── value.rs     // CEK values
│   │   ├── step.rs      // step / compute / return_compute
│   │   └── cost.rs      // ExBudget arithmetic + step kinds
│   └── builtin/
│       ├── mod.rs       // entry points
│       ├── arity.rs     // force_count + arity per BuiltinId
│       ├── dispatch.rs  // BuiltinId → eval function
│       ├── sized.rs     // size estimators for costing
│       └── denotations.rs // actual builtin implementations
└── DESIGN.md            // (this file)
```

Crates added later (separate PRs):

```
crates/dugite-uplc-script-context/    // TxInfoV1/V2/V3 + ScriptContext builders
crates/dugite-uplc-phase-two/         // phase_two_evaluate(tx, utxos, ...) façade
```

(or — equivalent — additional modules inside `dugite-uplc` if we want a
single crate. We'll decide when the design lands; the modular split
keeps build times for the inner CEK loop bounded.)

## Component design

### Term & Constant (`src/term.rs`)

- De Bruijn indices end-to-end. No `Name` / `NamedDeBruijn` enum
  variants — the flat decoder emits `DeBruijn` directly and the CEK
  consumes it directly.
- `Term` recursion uses `Box<Term>` (not `Rc`, not arena indices). The
  CEK machine never walks the term tree recursively; the term tree is
  immutable once decoded, so a single allocation per node and no
  sharing is fine. The decoder threads a depth budget.
- `Constr.tag: u64` (NOT `usize` — wire spec is `Word64`; aiken's `usize`
  truncates on 32-bit hosts).
- `Constant::Bls12_381G1Element` / `G2Element` / `MlResult` are boxed
  to keep the enum size bounded.
- `BuiltinId` is a `#[repr(u8)]` `#[non_exhaustive]` enum whose
  discriminants match the Haskell `DefaultFun` ordering verbatim
  (normative — the flat encoding stores the builtin as a 7-bit
  discriminant). New entries are appended; never reordered.

### Error taxonomy (`src/error.rs`)

`UplcError` distinguishes:

- `FlatDecode(String)` — wire-format error.
- `CborDecode(String)` — outer envelope / `Data` CBOR error.
- `Encode(String)` — encoder error (kept fallible for API uniformity).
- `ScriptError` — the script evaluated to `Term::Error`.
- `BudgetExhausted { cpu_remaining, mem_remaining }`.
- `BuiltinTypeError { builtin, reason }` — argument shape mismatch
  (must not be reachable from the CEK's own checks for a well-typed
  program; reachable from adversarial `Data` lift).
- `BuiltinFailure { builtin, reason }` — builtin returned an error
  (e.g. division by zero, ed25519 verify failure).
- `NonUnitReturn` — V3 contract returned non-`Unit`.
- `Internal(String)` — invariant violation in dugite-uplc itself.
  Surfaced as a typed error rather than a panic so tests + monitoring
  can detect it; CI gates on zero occurrences.

### Flat decoder (`src/flat/decode.rs`)

Bit-stream reader: `struct BitReader<'b> { bytes: &'b [u8], byte_pos: usize, bit_pos: u8 }`.

Public entry points:

```rust
pub fn decode_program(bytes: &[u8]) -> FlatResult<Program>;
pub fn decode_term(bytes: &[u8]) -> FlatResult<Term>;
```

Hard invariants:

1. `ensure_bits(n)` is called before every `bits8` / `bool` / `bit`.
2. `word()` / `big_word()` reject varints whose total shift would
   exceed the target type's bit width (varint termination is bounded
   by `usize::BITS / 7 + 1` chunks).
3. `decode_term` threads a depth counter; exceeding `FLAT_MAX_DEPTH`
   (4096) returns `FlatDecode("depth limit exceeded")`.
4. Every `Vec::with_capacity(N)` uses `min(N, remaining_bits / 4)`.
5. Unknown term tags → `FlatDecode("unknown term tag {bits:#06b}")`.
6. Unknown builtin discriminants → `FlatDecode("unknown builtin id {n}")`.
7. Unknown constant universe-tag sequences → `FlatDecode("unknown universe tag {bits}")`.

### CEK machine (`src/machine/`)

Three-state driver (mirroring the Haskell reference):

```rust
enum MachineState {
    Compute(Context, Env, Term),
    Return(Context, Value),
    Done(Term),
}
```

- `Context` = explicit continuation stack (`Vec<Frame>`, heap-allocated,
  uncapped — depth is bounded only by `ExBudget` exhaustion, matching
  Haskell's `Context` exactly; see #817). Frame variants follow the
  spec: `AwaitArg`, `AwaitFunTerm`, `AwaitFunValue`, `Force`, `Constr`,
  `Cases`.
- `Env` = persistent cons-list of `Value`s. De Bruijn lookup is O(index)
  linear (matches Haskell exactly; mainnet scripts don't have
  pathologically deep envs).
- `Value` = `Con(Constant) | Lambda { body, env } | Builtin(Runtime) | Delay { term, env } | Constr { tag, args }`.
- Budget accounting uses the **slippage** scheme:
  `unbudgeted_steps: [u8; 10]`, `slippage = 200`. Every step bumps the
  per-kind counter + the global counter; we flush to `ExBudget` only
  when the global counter overflows or when `Context::NoFrame` is hit
  on return. This mirrors Haskell's optimisation byte-for-byte and is
  cheaper than per-step `ExBudget` subtraction.
- Cost-model lookup is `[i64; N]` keyed by `StepKind` enum — NOT a
  string-keyed `HashMap` like pragma-org/uplc. The parameter slice
  from protocol-params CBOR is loaded once at evaluation start and
  validated against the expected length for the script's language
  version + protocol version.

### Builtins (`src/builtin/`)

- Dispatch is a single `match` on `BuiltinId`, NOT a giant table of
  function pointers. The match expands to one arm per builtin (~88
  arms in PV11+). Each arm:
  1. Charges the budget via `costs.builtin(BuiltinId).cpu_for(arg_mem_sizes)` etc., BEFORE allocating result.
  2. Type-checks and unwraps args via copy-free `unwrap_*` helpers on `&Value`.
  3. Computes the result.
  4. Returns `Value::Con(result)` or `UplcError::BuiltinFailure`.
- A single `bigint_to_bounded(b: &BigInt, max: usize) -> Result<usize, UplcError>` chokepoint covers every place a script-controlled `BigInt` becomes a `usize` (`IndexByteString`, `SliceByteString`, `ShiftByteString`, `RotateByteString`, `ReadBit`, `WriteBits`, `ReplicateByte`, `FindFirstSetBit`, `IntegerToByteString`). The function caps at `min(64 KiB, mem_budget_remaining / 8)` so a script can't drain the mem budget via a single fat allocation.
- BLS12-381: `blst` is the implementation. Subgroup checks are wired through `Compressable::uncompress`. Inputs in the wrong subgroup → `UplcError::BuiltinFailure`. Cross-platform reproducibility is verified in CI on x86_64 + aarch64 via the IntersectMBO conformance corpus.
- ed25519: `ed25519-dalek`'s `verify_strict`. cardano-base implements Ed25519 DSIGN over libsodium's `crypto_sign_verify_detached`, which rejects small-order and non-canonical public keys and small-order `R`; the permissive `Verifier::verify` path accepts an identity-point key with a zero signature for *any* message. Fixed in #997 — do not relax this back to `verify`.
- secp256k1: `k256` (pure-Rust) used for both ECDSA and Schnorr to avoid the secp256k1-sys C dep. Will validate against CIP-49 test vectors.

### PlutusData (`src/data.rs` + `src/data/codec.rs`)

- `Data` is `Constr(u64, Vec<Data>) | Map(Vec<(Data, Data)>) | List(Vec<Data>) | I(BigInt) | B(Vec<u8>)`. Insertion order preserved (no sorting on decode — Haskell doesn't sort, and we must not change body hashes).
- CBOR encoder (using `minicbor`):
  - Constr tag in `0..=6` → CBOR tag `121 + i`, payload = fields array.
  - Constr tag in `7..=127` → CBOR tag `1280 + (i - 7)`, payload = fields array.
  - Constr tag outside that range → CBOR tag `102`, payload = `[i, fields]`.
  - `I(n)` for `n` in i64 range → major 0 / major 1. Otherwise → CBOR tag 2 (positive bignum) or tag 3 (negative bignum) wrapping a byte string.
  - `Map` and `List` and `B` follow the 64-element/byte definite-vs-indefinite cutoff on encode; decoder accepts either form.
- Decoder threads a depth counter (max 256).

### Phase-two wrapper (separate crate or module)

The drop-in replacement for `uplc::tx::eval_phase_two_raw` exposes:

```rust
pub fn evaluate_phase_two(
    tx: &dugite_primitives::transaction::Transaction,
    utxo_lookup: &dyn dugite_ledger::utxo::UtxoLookup,
    cost_models: &CostModels,
    initial_budget: ExBudget,
    slot_config: &SlotConfig,
) -> Result<Vec<RedeemerEvalResult>, UplcError>;
```

Internally:

1. Build the script-version map (which redeemers attach to V1 / V2 / V3
   scripts).
2. For each redeemer (tag + index):
   1. Construct the appropriate `TxInfo` per version (V1/V2/V3).
   2. Wrap into `ScriptContext` (V1/V2) or `ScriptInfo`-typed
      `ScriptContext` (V3).
   3. Load the script bytes; flat-decode into `Program`.
   4. Apply datum (V1/V2 spending only) + redeemer + context as
      arguments via `Program::apply`.
   5. Evaluate via `Machine::run`.
   6. Collect `RedeemerEvalResult { redeemer_cbor, ex_units, result }`.
3. Phase-1 (redeemer-script linkage) lives in `dugite-ledger`, not
   here. Aiken put it inside the uplc crate (`tx/phase_one.rs`); that
   was wrong — phase-1 is pure ledger logic.

### V3 Unit-return enforcement

V3 (PV9+) scripts must return `Unit`. Any other return value is treated
as `InvalidReturnValue`. Implementation: post-evaluation, the
phase-two wrapper checks the result `Term`; non-Unit → `UplcError::NonUnitReturn`.

## What we explicitly do not copy

From pragma-org/uplc:

- **Unbounded recursive `decode_term`** — stack-overflow DoS. We thread an explicit depth budget instead.
- **`Constr.tag: usize`** — truncates on 32-bit hosts. We use `u64`.
- **`Runtime::push` cloning the entire args vec on each apply** — quadratic in arity. We use a persistent cons-list for accumulated args, mirroring `Env`.
- **String-keyed `HashMap` cost map** — slow + lossy on missing keys. We use `[i64; N]` keyed by an enum, with length-checked loading.
- **`expect`/`unreachable!` in `runtime.rs`** — ~30 sites currently. Every one is replaced with `Result`-typed errors.
- **Two near-identical decode functions** (`decode_constant` vs `decode_constant_with_type`). Single parameterised function instead.
- **Public APIs leaking arena lifetimes** — we hide all allocation behind a façade.

From aiken-lang/uplc:

- **`Rc<Term>` cloning per CEK step** — measurable hot path. `Box<Term>` + arena-of-frames is sufficient.
- **`unimplemented!()` on non-Conway era in `eval_phase_two_raw`** — we return a typed error.
- **Pallas dependency** for all CBOR / tx / address / crypto. We use `dugite-serialization`, `dugite-primitives`, `dugite-crypto`.
- **Two-layer binder gymnastics** (`Name` / `NamedDeBruijn` / `FakeNamedDeBruijn` / `DeBruijn`). De Bruijn end-to-end.

## Catalogued panic sites in third-party UPLCs (do not reproduce)

Sites the dugite fuzz targets reach via adversarial witness scripts:

- `pallas-codec/src/flat/decode/decoder.rs:65` — unchecked `self.buffer[self.pos]` in `bool()` at EOF.
- `pallas-codec/src/flat/decode/decoder.rs:154` — unbounded shift in `word()`; debug = panic, release = silent corruption.
- `aiken-lang/uplc/src/tx.rs:181` — `unimplemented!()` on pre-Conway era.
- `aiken-lang/uplc/src/tx.rs:194` — `.unwrap()` on `PlutusData::decode_fragment(params_bytes)` returning `Err(EndOfInput)`; plus `unreachable!()` if the decoded form isn't `Array`.
- `aiken-lang/uplc/src/machine/runtime.rs` lines 549, 554, 574, 879, 1451, 1610, 1612, 1636, 1638, 1656, 1676, 1687, 1689, 1708, 1747 — `BigInt → usize/u64/i128` `.try_into().unwrap()` on script-controlled values.
- `aiken-lang/uplc/src/machine/runtime.rs` lines 875, 904, 910, 927, 1627 — `unreachable!()` guarded only by upstream type checks.
- `aiken-lang/uplc/src/machine/runtime.rs:963` — `c.any_constructor.unwrap()` on `Constr` with tag outside 121-127/1280-1400 and no fallback.
- `aiken-lang/uplc/src/tx/script_context.rs:36-37` — `Address::from_bytes().unwrap()` on output address from CBOR-valid bytes.
- `aiken-lang/uplc/src/tx/script_context.rs:646,1133` — `unreachable!("invalid reward address")`.

All of these are reachable from witness-set scripts on the gossip layer
and have produced libfuzzer crashes in dugite's fuzz CI. dugite-uplc
must demonstrably not have analogues — the fuzz matrix gates on it.

## Conformance gotchas from the Haskell reference

Surfaced by the cardano-haskell-oracle survey of `IntersectMBO/plutus`
HEAD; each is a place where the obvious-looking Rust translation
diverges from cardano-node and must be tested explicitly:

1. **Program version gate for `Constr`/`Case`.** Flat tags 8 and 9 are
   only legal when the program's outer version triple is `≥ 1.1.0`. A
   `1.0.0` program containing either node fails *phase-1* with
   `not allowed before version 1.1.0`. The flat decoder must thread
   the program version through so it can reject early.

2. **PlutusData 64-byte bytestring limit at decode.** Definite-length
   `B` leaves over 64 bytes are decode errors. Larger byte strings
   must use indefinite-length chunking. Integer bignums must have a
   ≤ 64 byte payload too. (Source: `decodeBoundedBytes` in
   `plutus-core/.../Data.hs`.)

3. **`Map` semantics for `Data`.** Duplicate keys are accepted; order
   is preserved as-is; no canonical-form enforcement. Scripts that
   need uniqueness enforce it themselves. Definite-length on encode.

4. **`Index = 0` is a sentinel free variable.** `mkTermToEvaluate`
   runs `checkScope` *before* evaluation and rejects programs with
   any free De Bruijn variable. `checkScope` failure is reported as
   a *phase-2* error (charged collateral), not a decode error.

5. **Slippage overshoot.** `defaultSlippage = 200` — scripts can
   exceed their budget by up to ~200 step-costs before the machine
   notices. The final returned `ExBudget` is the *remaining* budget
   (negative on overshoot); the ledger checks `final >= 0`. Don't
   short-circuit on exact-zero; honour the slippage.

6. **`DefaultUniValue` and the new `Value`-tagged builtins**
   (`InsertCoin`, `LookupCoin`, `UnionValue`, `ValueContains`,
   `ValueData`, `UnValueData`, `ScaleValue`) are on `IntersectMBO/plutus`
   master but **not** enabled in current mainnet protocol versions.
   Do not implement in the initial dugite-uplc target — but reserve
   the `BuiltinId` discriminants so we don't have to renumber later.

7. **V3-only fee/mint semantics.** `txInfoFee` is `Lovelace` (not
   `Value`) and `txInfoMint` is `MintValue` (the zero-Ada invariant is
   enforced at the ledger level: zero-quantity Ada entries never
   appear). The V1/V2 `Value`-typed fee field is V1/V2-only.

8. **V3 `ScriptContext` shape change.** V3 has three fields, not
   two: `txInfo`, `redeemer`, and `scriptInfo`. The third field
   (`scriptInfo`) replaces V1/V2's `scriptContextPurpose` with a
   richer type that carries the inline datum for `SpendingScript`.

9. **V3 redeemer iteration adds voting + proposing.** The
   `ScriptPurpose` enum gains `Voting Voter` and
   `Proposing Integer ProposalProcedure` variants on top of the V2
   four (`Minting`, `Spending`, `Rewarding`, `Certifying`).

10. **Reference script size fee** (Conway). `txNonDistinctRefScriptsSize`
    sums `originalBytesSize` of all reference scripts referenced by
    inputs ∪ reference inputs, *counting duplicates*, and is fed into
    minimum-fee computation. This is a fee rule (phase-1), not a
    script-budget rule.

## Open questions (to be answered in implementation PRs)

1. Single-crate or multi-crate split for the phase-two wrapper? (Likely
   single, with the wrapper in `crates/dugite-uplc/src/phase_two/`.)
2. ed25519 verifier strictness: dalek's `verify` vs `verify_strict` vs a
   custom relaxed verifier matching cardano-base. Conformance corpus
   will decide.
3. secp256k1: `k256` (pure-Rust) vs `secp256k1` (libsecp256k1 bindings).
   `k256` is more deterministic but ~2× slower; we'll benchmark before
   the V1 builtin lands.
4. Cost-model parameter ingestion: parse the protocol-params CBOR
   here, or take a `&[i64]` slice with length-checked loading? Likely
   the latter — the parsing belongs in `dugite-serialization`.

## Implementation order

1. **(this commit)** scaffold + design doc + module skeleton — landed.
2. PR 1: PlutusData CBOR codec + `Data` round-trip tests against
   IntersectMBO conformance fixtures.
3. PR 2: Flat decoder + encoder + round-trip property tests.
4. PR 3: CEK machine (no builtins yet — only `lam`/`app`/`var`/`force`/`delay`/`const`/`error`/`constr`/`case`).
5. PR 4: Builtin suite (V1) + cost model loader.
6. PR 5: Builtin suite (V2) + CIP-0033.
7. PR 6: Builtin suite (V3) + CIP-0035 + CIP-0117 + CIP-0121 + CIP-0122 + CIP-0123 + CIP-0127 + CIP-0101 + CIP-0381.
8. PR 7: ScriptContext V1/V2/V3 builders.
9. PR 8: phase-two façade + integration into `dugite-ledger/src/plutus.rs`.
10. PR 9: drop `uplc = { git = ... aiken-lang/aiken.git ... }` from workspace and confirm `cargo tree | grep pallas` is empty.

Conformance corpus from `IntersectMBO/plutus:plutus-conformance` runs
as an integration test on every PR after PR 3; the release gate is
100% pass.
