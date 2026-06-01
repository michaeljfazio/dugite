//! Reproducer for issue #656 — Aiken-built PlutusV3 always-true validator
//! fails phase-2 evaluation with `De Bruijn index 2 out of range (env depth 0)`.
//!
//! The script bytes are vendored from the `aiken` 1.1.22 toolchain via
//! `testnet/local-devnet/tx-zoo/lib/build-plutus.sh`. The decoded shape
//! (per `aiken uplc decode --cbor`) is:
//!
//! ```text
//! (program 1.1.0
//!   (lam i_0
//!     (case (constr 0 [tailList, headList, sndPair, fstPair, ifThenElse])
//!       (lam i_1 (lam i_2 (lam i_3 (lam i_4 (lam i_5 …))))))))
//! ```
//!
//! i.e. it first binds the 5 list/pair/ifThenElse builtins as locals via
//! `case`-on-`Constr`, then enters the script body. Applied to a `Data`
//! ctx, it should reduce without raising `CekFailure`.
//!
//! This test passes on `aiken uplc eval` for the same flat bytes. It is
//! expected to FAIL on dugite until #656 is fixed; once a fix lands, the
//! `#[ignore]` attribute should be removed.

use dugite_uplc::{Constant, Data, Program, Term};
use std::rc::Rc;

/// Inner flat bytes (post-CBOR-unwrap) of the canonical Aiken-built
/// PlutusV3 always-true validator. Source:
/// `testnet/local-devnet/tx-zoo/lib/plutus/always-true-v3.plutus`,
/// cborHex = `588d588b<139 bytes>` → strip outer CBOR (58 8d) and inner
/// CBOR (58 8b) to get the 139-byte flat program. This constant inlines
/// the inner-CBOR-wrapped form (141 bytes) so we can use
/// `Program::from_cbor`.
const AIKEN_V3_ALWAYS_TRUE_CBOR: &[u8] = &[
    0x58, 0x8b, 0x01, 0x01, 0x00, 0x29, 0x80, 0x0a, 0xba, 0x2a, 0xba, 0x1a, 0xab, 0x9e, 0xaa, 0xb9,
    0xda, 0xb9, 0xa4, 0x88, 0x88, 0x96, 0x60, 0x02, 0x64, 0x64, 0x65, 0x30, 0x01, 0x30, 0x05, 0x37,
    0x54, 0x00, 0x33, 0x00, 0x70, 0x03, 0x98, 0x03, 0x80, 0x12, 0x44, 0x4b, 0x30, 0x01, 0x33, 0x70,
    0xe9, 0x00, 0x00, 0x01, 0xc4, 0xc9, 0x28, 0x9b, 0xae, 0x30, 0x0a, 0x30, 0x09, 0x37, 0x54, 0x00,
    0x91, 0x59, 0x80, 0x09, 0x9b, 0x87, 0x48, 0x00, 0x80, 0x0e, 0x26, 0x46, 0x64, 0x49, 0x44, 0xc0,
    0x2c, 0x00, 0x4c, 0x02, 0xcc, 0x03, 0x00, 0x04, 0xc0, 0x24, 0xdd, 0x50, 0x02, 0x45, 0x66, 0x00,
    0x26, 0x6e, 0x1d, 0x20, 0x04, 0x00, 0x38, 0x99, 0x25, 0x13, 0x00, 0xa3, 0x00, 0x93, 0x75, 0x40,
    0x09, 0x16, 0x40, 0x1c, 0x80, 0x39, 0x00, 0x70, 0xc0, 0x18, 0xc0, 0x1c, 0x00, 0x4c, 0x01, 0x80,
    0x04, 0xc0, 0x0c, 0xdd, 0x50, 0x03, 0x45, 0x26, 0x89, 0xb2, 0xb2, 0x00, 0x21,
];

#[test]
fn decode_succeeds() {
    let p = Program::from_cbor(AIKEN_V3_ALWAYS_TRUE_CBOR)
        .expect("Aiken-built V3 always-true must decode cleanly after #41b7a036a");
    assert_eq!(p.version, (1, 1, 0));
    // Top-level term is a single Lambda (binds ctx).
    assert!(matches!(p.term, Term::Lam(_)));
}

/// CEK can traverse the Aiken-generated env-management code without
/// raising a Var-lookup error.  We don't assert the *result* here —
/// Aiken's always-true unpacks the ScriptContext via builtins
/// (`unConstrData`, `headList`, `sndPair`, …) and a stub `Data::Constr`
/// is too thin to satisfy those builtins, so the CEK genuinely surfaces
/// a `BuiltinFailure` against this mock.  What this test pins down is
/// the regression mode from #656: a malformed AST (from missing
/// Constr/Case flat decoding) used to surface as
/// `De Bruijn index 2 out of range (env depth 0)` *before* any
/// builtin ran.  A `BuiltinFailure` against this stub is the post-fix
/// signature — exactly because we now reach the script body intact.
#[test]
fn cek_reaches_body_no_var_lookup_failure() {
    let p = Program::from_cbor(AIKEN_V3_ALWAYS_TRUE_CBOR)
        .expect("decode canonical Aiken V3 always-true");
    let stub_ctx = Data::Constr(0, vec![Data::Constr(0, vec![]); 4]);
    let applied = Term::App(
        Rc::new(p.term),
        Rc::new(Term::Const(Constant::Data(stub_ctx))),
    );

    let err = dugite_uplc::machine::step::evaluate(applied).expect_err(
        "stub ctx is too thin to satisfy unConstrData/headList — expect a builtin error",
    );
    let msg = format!("{err:?}");
    assert!(
        !msg.contains("De Bruijn"),
        "post-#656 the CEK must reach the script body without any Var-lookup \
         failure; got: {msg}"
    );
    assert!(
        msg.contains("headList") || msg.contains("Builtin"),
        "expected the failure to come from a builtin against the stub ctx; got: {msg}"
    );
}
