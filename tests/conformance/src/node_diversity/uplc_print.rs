//! Result read-back and textual UPLC printing for the `uplc-conformance` scenario.
//!
//! The scenario asks for "the CEK result, as textual UPLC".  The CEK machine returns a
//! [`Value`]; this module turns it back into a closed term (the Haskell `dischargeCekValue`
//! read-back) and prints it in the syntax the corpus' expected results use:
//!
//! * binders are named by their depth, `i-0`, `i-1`, ... (a variable prints as the binder it
//!   refers to);
//! * an application is binary, `[ f a ]`; a `data` constant is parenthesised only at the top
//!   level of a `con`, bare when nested in a list, pair or array; strings are escaped the way
//!   Haskell `show` escapes them, except that non-ASCII characters print as themselves.
//!
//! The scenario's scorer ignores whitespace around punctuation (never inside a string), so the
//! output is a single line.  None of this is production code: it only reads a result back for a
//! report, so a defect here is a harness defect.

use std::rc::Rc;

use dugite_uplc::machine::env::Env;
use dugite_uplc::machine::value::rc_into_term;
use dugite_uplc::machine::Value;
use dugite_uplc::term::{Constant, TypeTag};
use dugite_uplc::{Data, Program, Term};

/// Read a CEK value back into a closed term.
///
/// A closure captures an environment; every free de Bruijn index in its body is replaced by the
/// read-back of the captured value.  Each substituent is closed, so no index shifting is needed.
pub fn readback(value: Value) -> Result<Term, String> {
    match value {
        Value::Const(c) => Ok(Term::Const(c)),
        Value::Lambda { body, env } => {
            // Entering the binder makes Var(1) the lambda's own parameter; Var(i > 1) indexes the
            // captured environment.
            Ok(Term::Lam(Rc::new(quote(rc_into_term(body), &env, 1)?)))
        }
        Value::Delay { body, env } => Ok(Term::Delay(Rc::new(quote(rc_into_term(body), &env, 0)?))),
        Value::Builtin { id, forces, args } => {
            let mut t = Term::Builtin(id);
            for _ in 0..forces {
                t = Term::Force(Rc::new(t));
            }
            for arg in args {
                t = Term::App(Rc::new(t), Rc::new(readback(arg)?));
            }
            Ok(t)
        }
        Value::Constr { tag, args } => {
            let mut out = Vec::with_capacity(args.len());
            for a in args {
                out.push(Rc::new(readback(a)?));
            }
            Ok(Term::Constr { tag, args: out })
        }
    }
}

fn quote(term: Term, env: &Env, depth: u64) -> Result<Term, String> {
    let sub = |t: Rc<Term>, d: u64| -> Result<Rc<Term>, String> {
        Ok(Rc::new(quote(rc_into_term(t), env, d)?))
    };
    Ok(match term {
        Term::Var(i) if i <= depth => Term::Var(i),
        Term::Var(i) => {
            let captured = env
                .lookup(i - depth)
                .map_err(|e| format!("captured Var({i}) lookup failed: {e}"))?
                .clone();
            readback(captured)?
        }
        Term::Lam(b) => Term::Lam(sub(b, depth + 1)?),
        Term::App(f, a) => Term::App(sub(f, depth)?, sub(a, depth)?),
        Term::Delay(b) => Term::Delay(sub(b, depth)?),
        Term::Force(b) => Term::Force(sub(b, depth)?),
        Term::Const(c) => Term::Const(c),
        Term::Error => Term::Error,
        Term::Builtin(id) => Term::Builtin(id),
        Term::Constr { tag, args } => Term::Constr {
            tag,
            args: args
                .into_iter()
                .map(|a| sub(a, depth))
                .collect::<Result<_, _>>()?,
        },
        Term::Case {
            scrutinee,
            branches,
        } => Term::Case {
            scrutinee: sub(scrutinee, depth)?,
            branches: branches
                .into_iter()
                .map(|b| sub(b, depth))
                .collect::<Result<_, _>>()?,
        },
    })
}

/// Print `(program <version> <term>)` for a closed term.
pub fn print_program(input: &Program, term: &Term) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(&format!(
        "(program {}.{}.{} ",
        input.version.0, input.version.1, input.version.2
    ));
    print_term(term, 0, &mut out)?;
    out.push(')');
    Ok(out)
}

fn print_term(term: &Term, depth: u64, out: &mut String) -> Result<(), String> {
    match term {
        Term::Var(i) => {
            if *i == 0 || *i > depth {
                return Err(format!("free variable {i} at depth {depth} in a result"));
            }
            out.push_str(&format!("i-{}", depth - i));
        }
        Term::Lam(body) => {
            out.push_str(&format!("(lam i-{depth} "));
            print_term(body, depth + 1, out)?;
            out.push(')');
        }
        Term::App(f, a) => {
            out.push_str("[ ");
            print_term(f, depth, out)?;
            out.push(' ');
            print_term(a, depth, out)?;
            out.push_str(" ]");
        }
        Term::Const(c) => {
            out.push_str("(con ");
            out.push_str(&type_text(&c.type_tag()));
            out.push(' ');
            constant_text(c, true, out)?;
            out.push(')');
        }
        Term::Delay(b) => {
            out.push_str("(delay ");
            print_term(b, depth, out)?;
            out.push(')');
        }
        Term::Force(b) => {
            out.push_str("(force ");
            print_term(b, depth, out)?;
            out.push(')');
        }
        Term::Error => out.push_str("(error)"),
        Term::Builtin(id) => out.push_str(&format!("(builtin {})", id.name())),
        Term::Constr { tag, args } => {
            out.push_str(&format!("(constr {tag}"));
            for a in args {
                out.push(' ');
                print_term(a, depth, out)?;
            }
            out.push(')');
        }
        Term::Case {
            scrutinee,
            branches,
        } => {
            out.push_str("(case ");
            print_term(scrutinee, depth, out)?;
            for b in branches {
                out.push(' ');
                print_term(b, depth, out)?;
            }
            out.push(')');
        }
    }
    Ok(())
}

/// The type of a constant, as written after `con`.
pub fn type_text(t: &TypeTag) -> String {
    match t {
        TypeTag::Integer => "integer".into(),
        TypeTag::ByteString => "bytestring".into(),
        TypeTag::String => "string".into(),
        TypeTag::Unit => "unit".into(),
        TypeTag::Bool => "bool".into(),
        TypeTag::Data => "data".into(),
        TypeTag::List(e) => format!("(list {})", type_text(e)),
        TypeTag::Pair(a, b) => format!("(pair {} {})", type_text(a), type_text(b)),
        TypeTag::Array(e) => format!("(array {})", type_text(e)),
        TypeTag::Bls12_381G1Element => "bls12_381_G1_element".into(),
        TypeTag::Bls12_381G2Element => "bls12_381_G2_element".into(),
        TypeTag::Bls12_381MlResult => "bls12_381_mlresult".into(),
        TypeTag::Value => "value".into(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `top` is true for the constant directly under `con`, where a `data` is parenthesised.
fn constant_text(c: &Constant, top: bool, out: &mut String) -> Result<(), String> {
    match c {
        Constant::Integer(n) => out.push_str(&n.to_string()),
        Constant::ByteString(b) => {
            out.push('#');
            out.push_str(&hex(b));
        }
        Constant::String(s) => out.push_str(&string_literal(s)),
        Constant::Unit => out.push_str("()"),
        Constant::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Constant::ProtoList { elements, .. } | Constant::Array { elements, .. } => {
            out.push('[');
            for (i, e) in elements.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                constant_text(e, false, out)?;
            }
            out.push(']');
        }
        Constant::ProtoPair { a, b, .. } => {
            out.push('(');
            constant_text(a, false, out)?;
            out.push_str(", ");
            constant_text(b, false, out)?;
            out.push(')');
        }
        Constant::Data(d) => {
            if top {
                out.push('(');
            }
            data_text(d, out);
            if top {
                out.push(')');
            }
        }
        Constant::Bls12_381G1Element(b) => {
            out.push_str("0x");
            out.push_str(&hex(&b[..]));
        }
        Constant::Bls12_381G2Element(b) => {
            out.push_str("0x");
            out.push_str(&hex(&b[..]));
        }
        Constant::Bls12_381MlResult(_) => {
            return Err("a bls12_381_mlresult constant has no textual form".into())
        }
        Constant::Value(map) => {
            out.push('[');
            for (i, (policy, tokens)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("(#{}, [", hex(policy)));
                for (j, (name, amount)) in tokens.iter().enumerate() {
                    if j > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&format!("(#{}, {amount})", hex(name)));
                }
                out.push_str("])");
            }
            out.push(']');
        }
    }
    Ok(())
}

fn data_text(d: &Data, out: &mut String) {
    match d {
        Data::Constr(tag, args) => {
            out.push_str(&format!("Constr {tag} ["));
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                data_text(a, out);
            }
            out.push(']');
        }
        Data::Map(kvs) => {
            out.push_str("Map [");
            for (i, (k, v)) in kvs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('(');
                data_text(k, out);
                out.push_str(", ");
                data_text(v, out);
                out.push(')');
            }
            out.push(']');
        }
        Data::List(xs) => {
            out.push_str("List [");
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                data_text(x, out);
            }
            out.push(']');
        }
        Data::I(n) => out.push_str(&format!("I {n}")),
        Data::B(b) => {
            out.push_str("B #");
            out.push_str(&hex(b));
        }
    }
}

/// A string constant as written in a program: Haskell `show` escapes for ASCII, characters
/// above 127 as themselves.
pub fn string_literal(s: &str) -> String {
    const CONTROL: [&str; 32] = [
        "NUL", "SOH", "STX", "ETX", "EOT", "ENQ", "ACK", "a", "b", "t", "n", "v", "f", "r", "SO",
        "SI", "DLE", "DC1", "DC2", "DC3", "DC4", "NAK", "SYN", "ETB", "CAN", "EM", "SUB", "ESC",
        "FS", "GS", "RS", "US",
    ];
    let mut out = String::from("\"");
    let mut prev_so = false;
    for ch in s.chars() {
        let code = ch as u32;
        // `show` separates `\SO` from a following `H` so it is not read back as `\SOH`.
        if prev_so && ch == 'H' {
            out.push_str("\\&");
        }
        prev_so = code == 14;
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ if code < 32 => {
                out.push('\\');
                out.push_str(CONTROL[code as usize]);
            }
            '\u{7f}' => out.push_str("\\DEL"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    fn show(term: &Term) -> String {
        let mut s = String::new();
        print_term(term, 0, &mut s).unwrap();
        s
    }

    #[test]
    fn variables_print_as_the_binder_they_refer_to() {
        // \x -> \y -> y x
        let t = Term::Lam(Rc::new(Term::Lam(Rc::new(Term::App(
            Rc::new(Term::Var(1)),
            Rc::new(Term::Var(2)),
        )))));
        assert_eq!(show(&t), "(lam i-0 (lam i-1 [ i-1 i-0 ]))");
    }

    #[test]
    fn a_free_variable_is_an_error_not_a_guess() {
        let mut s = String::new();
        assert!(print_term(&Term::Var(1), 0, &mut s).is_err());
    }

    #[test]
    fn strings_escape_as_haskell_show_does() {
        assert_eq!(
            string_literal("\t\"Success!\"\n"),
            "\"\\t\\\"Success!\\\"\\n\""
        );
        assert_eq!(string_literal("\x7f"), "\"\\DEL\"");
        assert_eq!(string_literal("\x08\n\x07\t"), "\"\\b\\n\\a\\t\"");
        assert_eq!(string_literal("€¬"), "\"€¬\"");
        assert_eq!(string_literal("\\"), "\"\\\\\"");
        assert_eq!(string_literal("\x0eH"), "\"\\SO\\&H\"");
    }

    #[test]
    fn data_is_parenthesised_only_directly_under_con() {
        let d = Constant::Data(Rc::new(Data::List(vec![
            Data::I(BigInt::from(-1)),
            Data::B(vec![0xab]),
        ])));
        assert_eq!(
            show(&Term::Const(d)),
            "(con data (List [I -1, B #ab]))".to_string()
        );
        let list = Constant::ProtoList {
            elem_type: TypeTag::Data,
            elements: vec![Constant::Data(Rc::new(Data::I(BigInt::from(0))))],
        };
        assert_eq!(show(&Term::Const(list)), "(con (list data) [I 0])");
    }

    #[test]
    fn pairs_lists_and_unit_print_in_the_corpus_syntax() {
        let p = Constant::ProtoPair {
            a_type: TypeTag::Integer,
            b_type: TypeTag::Pair(Box::new(TypeTag::Unit), Box::new(TypeTag::Bool)),
            a: Box::new(Constant::Integer(BigInt::from(12345))),
            b: Box::new(Constant::ProtoPair {
                a_type: TypeTag::Unit,
                b_type: TypeTag::Bool,
                a: Box::new(Constant::Unit),
                b: Box::new(Constant::Bool(true)),
            }),
        };
        assert_eq!(
            show(&Term::Const(p)),
            "(con (pair integer (pair unit bool)) (12345, ((), True)))"
        );
    }
}
