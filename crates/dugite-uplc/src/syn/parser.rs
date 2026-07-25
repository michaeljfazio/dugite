//! Recursive-descent body for the [`super`] textual parser.
//!
//! Style: explicit cursor over a borrowed `&str`. No external parser
//! library, no panics, no `unwrap`/`expect`. Every public function on
//! [`Parser`] either advances the cursor and returns a successful
//! parse, or leaves the cursor wherever the error was first detected
//! and returns [`super::ParseError`].

use super::{builtin_from_name, parse_signed_bigint, ParseError};
use crate::data::Data;
use crate::term::{Constant, Term, TypeTag};
use crate::Program;
use num_bigint::BigInt;
use std::rc::Rc;

/// Stateful cursor over the source plus a binder stack for named
/// → De-Bruijn conversion.
pub(super) struct Parser<'a> {
    src: &'a str,
    /// Byte offset into `src`.
    pos: usize,
    /// Stack of binder names — outermost first. A `Var` reference is
    /// resolved by scanning right-to-left for the matching name, then
    /// converting the position to a De Bruijn index (`stack.len() - idx`).
    binders: Vec<&'a str>,
    /// Plutus Core program version (`major.minor.patch`).  Latched in
    /// `parse_program_top` so version-gated grammar (`constr`, `case`,
    /// `array`, `value`, … — anything introduced in PV1.1.0) can be
    /// rejected pre-version.  `None` for the standalone-term / typed-
    /// constant / data-expression entry points where no version
    /// preamble exists; those default to permissive behaviour because
    /// the caller controls the language level.
    program_version: Option<(u64, u64, u64)>,
}

impl<'a> Parser<'a> {
    pub(super) fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            binders: Vec::new(),
            program_version: None,
        }
    }

    /// True if the program version is strictly older than the cutoff
    /// `(major, minor, patch)`.  A `None` program_version (term/const
    /// entry point) is treated as not-older for permissiveness.
    fn version_below(&self, major: u64, minor: u64, patch: u64) -> bool {
        match self.program_version {
            None => false,
            Some((maj, min, pat)) => (maj, min, pat) < (major, minor, patch),
        }
    }

    // ─── entry points ────────────────────────────────────────────────

    pub(super) fn parse_program_top(&mut self) -> Result<Program, ParseError> {
        self.skip_trivia();
        self.expect_char('(')?;
        self.skip_trivia();
        self.expect_keyword("program")?;
        self.skip_trivia();
        let version = self.parse_version()?;
        // Latch the version so version-gated grammar can refer to it.
        self.program_version = Some(version);
        self.skip_trivia();
        let term = self.parse_term()?;
        self.skip_trivia();
        self.expect_char(')')?;
        // `Program.version` is `BigUint`-typed (#842 residual — the flat
        // decoder needs arbitrary precision to match Haskell's unbounded
        // `Natural`). This textual parser is dev/test-only tooling (not
        // on the consensus wire-format path), so its own version-gating
        // (`program_version`/`version_below`) stays `u64`-based for
        // simplicity; only the final `Program` value needs the widened
        // type.
        Ok(Program {
            version: (
                num_bigint::BigUint::from(version.0),
                num_bigint::BigUint::from(version.1),
                num_bigint::BigUint::from(version.2),
            ),
            term,
        })
    }

    pub(super) fn parse_term_top(&mut self) -> Result<Term, ParseError> {
        self.skip_trivia();
        self.parse_term()
    }

    pub(super) fn parse_typed_constant(&mut self) -> Result<(TypeTag, Constant), ParseError> {
        self.skip_trivia();
        let ty = self.parse_type()?;
        self.skip_trivia();
        let c = self.parse_constant_for_type(&ty)?;
        Ok((ty, c))
    }

    pub(super) fn parse_data_expr(&mut self) -> Result<Data, ParseError> {
        self.skip_trivia();
        self.parse_data()
    }

    /// Verify no non-trivia content remains; returns the position of
    /// the first unexpected byte if any.
    pub(super) fn finish(&mut self) -> Result<(), ParseError> {
        self.skip_trivia();
        if self.pos != self.src.len() {
            return Err(ParseError::at(
                self.pos,
                format!("trailing input after parse: {:?}", self.peek_snippet()),
            ));
        }
        Ok(())
    }

    // ─── version triple ──────────────────────────────────────────────

    fn parse_version(&mut self) -> Result<(u64, u64, u64), ParseError> {
        let maj = self.parse_uint_u64()?;
        self.expect_char('.')?;
        let min = self.parse_uint_u64()?;
        self.expect_char('.')?;
        let patch = self.parse_uint_u64()?;
        Ok((maj, min, patch))
    }

    // ─── terms ───────────────────────────────────────────────────────

    fn parse_term(&mut self) -> Result<Term, ParseError> {
        self.skip_trivia();
        let here = self.pos;
        match self.peek_char() {
            None => Err(ParseError::at(here, "unexpected end of input".into())),
            Some('[') => self.parse_application(),
            Some('(') => self.parse_paren_term(),
            Some(_) => self.parse_var(),
        }
    }

    fn parse_application(&mut self) -> Result<Term, ParseError> {
        self.expect_char('[')?;
        self.skip_trivia();
        let mut term = self.parse_term()?;
        loop {
            self.skip_trivia();
            match self.peek_char() {
                Some(']') => {
                    self.pos += 1;
                    return Ok(term);
                }
                Some(_) => {
                    let arg = self.parse_term()?;
                    term = Term::App(Rc::new(term), Rc::new(arg));
                }
                None => {
                    return Err(ParseError::at(
                        self.pos,
                        "unterminated application: expected `]`".into(),
                    ));
                }
            }
        }
    }

    fn parse_paren_term(&mut self) -> Result<Term, ParseError> {
        let open = self.pos;
        self.expect_char('(')?;
        self.skip_trivia();
        let kw_start = self.pos;
        let kw = self.parse_ident()?;
        self.skip_trivia();

        let result = match kw {
            "lam" => self.parse_lam(),
            "delay" => self.parse_delay(),
            "force" => self.parse_force(),
            "error" => Ok(Term::Error),
            "builtin" => self.parse_builtin(),
            "constr" => self.parse_constr_term(),
            "case" => self.parse_case_term(),
            "con" => self.parse_con(),
            other => Err(ParseError::at(
                kw_start,
                format!("unknown special form `{other}` at byte {open}"),
            )),
        };
        let term = result?;
        self.skip_trivia();
        self.expect_char(')')?;
        Ok(term)
    }

    fn parse_lam(&mut self) -> Result<Term, ParseError> {
        let name = self.parse_ident()?;
        self.binders.push(name);
        self.skip_trivia();
        let body = self.parse_term();
        self.binders.pop();
        let body = body?;
        Ok(Term::Lam(Rc::new(body)))
    }

    fn parse_delay(&mut self) -> Result<Term, ParseError> {
        let body = self.parse_term()?;
        Ok(Term::Delay(Rc::new(body)))
    }

    fn parse_force(&mut self) -> Result<Term, ParseError> {
        let body = self.parse_term()?;
        Ok(Term::Force(Rc::new(body)))
    }

    fn parse_builtin(&mut self) -> Result<Term, ParseError> {
        let start = self.pos;
        let name = self.parse_ident()?;
        match builtin_from_name(name) {
            Some(id) => Ok(Term::Builtin(id)),
            None => Err(ParseError::at(start, format!("unknown builtin `{name}`"))),
        }
    }

    fn parse_constr_term(&mut self) -> Result<Term, ParseError> {
        // SOP `constr` was introduced in Plutus Core 1.1.0 (CIP-0085).
        // The reference parser rejects it in older programs; mirror.
        if self.version_below(1, 1, 0) {
            return Err(ParseError::at(
                self.pos,
                "`constr` requires Plutus Core ≥ 1.1.0".into(),
            ));
        }
        let tag = self.parse_uint_u64()?;
        let mut args = Vec::new();
        loop {
            self.skip_trivia();
            if matches!(self.peek_char(), Some(')')) {
                break;
            }
            args.push(Rc::new(self.parse_term()?));
        }
        Ok(Term::Constr { tag, args })
    }

    fn parse_case_term(&mut self) -> Result<Term, ParseError> {
        if self.version_below(1, 1, 0) {
            return Err(ParseError::at(
                self.pos,
                "`case` requires Plutus Core ≥ 1.1.0".into(),
            ));
        }
        let scrutinee = self.parse_term()?;
        let mut branches = Vec::new();
        loop {
            self.skip_trivia();
            if matches!(self.peek_char(), Some(')')) {
                break;
            }
            branches.push(Rc::new(self.parse_term()?));
        }
        Ok(Term::Case {
            scrutinee: Rc::new(scrutinee),
            branches,
        })
    }

    fn parse_var(&mut self) -> Result<Term, ParseError> {
        let _start = self.pos;
        let name = self.parse_ident()?;
        // Resolve right-to-left. The innermost binder is at the top
        // of `binders`; its De Bruijn index is 1.
        for (depth, bname) in self.binders.iter().rev().enumerate() {
            if *bname == name {
                // depth=0 → innermost → De Bruijn 1
                return Ok(Term::Var((depth as u64) + 1));
            }
        }
        // Free variable. The Haskell reference parser does NOT reject
        // this — variable scoping is a property checked by the
        // evaluator, not the parser. Emit De Bruijn index 0, which the
        // CEK machine treats as out-of-range and fails on at lookup
        // time. This matches the conformance corpus's `term/var/`
        // expectation: parse succeeds, evaluation fails.
        Ok(Term::Var(0))
    }

    fn parse_con(&mut self) -> Result<Term, ParseError> {
        let ty = self.parse_type()?;
        self.skip_trivia();
        let c = self.parse_constant_for_type(&ty)?;
        Ok(Term::Const(c))
    }

    // ─── types ───────────────────────────────────────────────────────

    fn parse_type(&mut self) -> Result<TypeTag, ParseError> {
        self.skip_trivia();
        match self.peek_char() {
            Some('(') => {
                self.pos += 1;
                self.skip_trivia();
                let head = self.parse_ident()?;
                self.skip_trivia();
                let result = match head {
                    "list" => {
                        let inner = self.parse_type()?;
                        TypeTag::List(Box::new(inner))
                    }
                    "pair" => {
                        let a = self.parse_type()?;
                        self.skip_trivia();
                        let b = self.parse_type()?;
                        TypeTag::Pair(Box::new(a), Box::new(b))
                    }
                    "array" => {
                        let inner = self.parse_type()?;
                        TypeTag::Array(Box::new(inner))
                    }
                    other => {
                        return Err(ParseError::at(
                            self.pos,
                            format!("unknown compound type head `{other}`"),
                        ))
                    }
                };
                self.skip_trivia();
                self.expect_char(')')?;
                Ok(result)
            }
            Some(_) => {
                let start = self.pos;
                let name = self.parse_ident()?;
                match name {
                    "integer" => Ok(TypeTag::Integer),
                    "bytestring" => Ok(TypeTag::ByteString),
                    "string" => Ok(TypeTag::String),
                    "unit" => Ok(TypeTag::Unit),
                    "bool" => Ok(TypeTag::Bool),
                    "data" => Ok(TypeTag::Data),
                    "bls12_381_G1_element" => Ok(TypeTag::Bls12_381G1Element),
                    "bls12_381_G2_element" => Ok(TypeTag::Bls12_381G2Element),
                    "bls12_381_mlresult" => Ok(TypeTag::Bls12_381MlResult),
                    "value" => Ok(TypeTag::Value),
                    other => Err(ParseError::at(start, format!("unknown type `{other}`"))),
                }
            }
            None => Err(ParseError::at(
                self.pos,
                "expected type tag, got end of input".into(),
            )),
        }
    }

    // ─── typed constant literals ─────────────────────────────────────

    fn parse_constant_for_type(&mut self, ty: &TypeTag) -> Result<Constant, ParseError> {
        match ty {
            TypeTag::Integer => Ok(Constant::Integer(self.parse_signed_int()?)),
            TypeTag::ByteString => Ok(Constant::ByteString(self.parse_hash_bytes()?)),
            TypeTag::String => Ok(Constant::String(self.parse_string_literal()?)),
            TypeTag::Unit => {
                self.expect_char('(')?;
                self.skip_trivia();
                self.expect_char(')')?;
                Ok(Constant::Unit)
            }
            TypeTag::Bool => {
                let start = self.pos;
                let name = self.parse_ident()?;
                match name {
                    "True" => Ok(Constant::Bool(true)),
                    "False" => Ok(Constant::Bool(false)),
                    other => Err(ParseError::at(
                        start,
                        format!("expected `True` or `False`, got `{other}`"),
                    )),
                }
            }
            TypeTag::Data => Ok(Constant::Data(std::rc::Rc::new(self.parse_data()?))),
            TypeTag::List(elem) => {
                let elems = self.parse_list_literal(elem)?;
                Ok(Constant::ProtoList {
                    elem_type: (**elem).clone(),
                    elements: elems,
                })
            }
            TypeTag::Pair(a_ty, b_ty) => {
                self.expect_char('(')?;
                self.skip_trivia();
                let a = self.parse_constant_for_type(a_ty)?;
                self.skip_trivia();
                self.expect_char(',')?;
                self.skip_trivia();
                let b = self.parse_constant_for_type(b_ty)?;
                self.skip_trivia();
                self.expect_char(')')?;
                Ok(Constant::ProtoPair {
                    a_type: (**a_ty).clone(),
                    b_type: (**b_ty).clone(),
                    a: Box::new(a),
                    b: Box::new(b),
                })
            }
            TypeTag::Bls12_381G1Element => {
                let here = self.pos;
                let bytes = self.parse_0x_hex_bytes()?;
                // CIP-0381 mandates that compressed G1 element literals
                // decompress to a point on the curve AND in the prime-
                // order subgroup.  The Plutus reference enforces this
                // at parse — bad-zero, off-curve, and out-of-group
                // encodings all surface as `parse error`.
                crate::builtin::bls::validate_g1_compressed(&bytes)
                    .map_err(|reason| ParseError::at(here, reason))?;
                let mut arr = [0u8; 48];
                arr.copy_from_slice(&bytes);
                Ok(Constant::Bls12_381G1Element(Box::new(arr)))
            }
            TypeTag::Bls12_381G2Element => {
                let here = self.pos;
                let bytes = self.parse_0x_hex_bytes()?;
                crate::builtin::bls::validate_g2_compressed(&bytes)
                    .map_err(|reason| ParseError::at(here, reason))?;
                let mut arr = [0u8; 96];
                arr.copy_from_slice(&bytes);
                Ok(Constant::Bls12_381G2Element(Box::new(arr)))
            }
            TypeTag::Bls12_381MlResult => {
                // The Haskell reference has no textual literal syntax for
                // `BLS12_381.Pairing.MlResult` (no `Parsable`/`Read`
                // instance in `Pairing.hs`) — an `MlResult` can only ever
                // be produced at runtime via `bls12_381_millerLoop` /
                // `bls12_381_mulMlResult`. Accepting a `0x...` literal
                // here would let a hand-written script inject 576
                // unvalidated bytes directly into `blst_fp12` arithmetic
                // (limbs `>= p` are possible) — reject to match the
                // reference parser (#843). This is textual-only: the
                // flat/CBOR on-chain path already rejects BLS constant
                // literals outright (see `flat/term.rs`).
                Err(ParseError::at(
                    self.pos,
                    "bls12_381_mlresult has no literal syntax".to_string(),
                ))
            }
            TypeTag::Array(elem) => {
                // `(array T)` literal uses the same `[e1, e2, ...]`
                // bracket syntax as `(list T)`.
                let elems = self.parse_list_literal(elem)?;
                Ok(Constant::Array {
                    elem_type: (**elem).clone(),
                    elements: elems,
                })
            }
            TypeTag::Value => {
                // `value` literal: `[( #policy, [( #token, amount ), ...] ), ...]`
                // Outer `[...]` is the outer list; inner `[...]` is the
                // token map for each policy.  We normalise in place:
                // sort both maps, remove zero-amount entries, remove
                // empty inner maps.
                self.parse_value_literal()
            }
        }
    }

    fn parse_list_literal(&mut self, elem: &TypeTag) -> Result<Vec<Constant>, ParseError> {
        self.expect_char('[')?;
        let mut out = Vec::new();
        self.skip_trivia();
        if matches!(self.peek_char(), Some(']')) {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            self.skip_trivia();
            let c = self.parse_constant_for_type(elem)?;
            out.push(c);
            self.skip_trivia();
            match self.peek_char() {
                Some(',') => {
                    self.pos += 1;
                    continue;
                }
                Some(']') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(other) => {
                    return Err(ParseError::at(
                        self.pos,
                        format!("expected `,` or `]` in list literal, got `{other}`"),
                    ))
                }
                None => return Err(ParseError::at(self.pos, "unterminated list literal".into())),
            }
        }
    }

    /// Parse a `value` literal.
    ///
    /// Syntax: `[ (#currencyID, [ (#tokenID, amount), ... ]), ... ]`
    ///
    /// The literal must already be in canonical form; the parser REJECTS
    /// anything else rather than normalising it. Per the Plutus conformance
    /// corpus (`plutus-conformance/test-cases/uplc/evaluation/builtin/
    /// constant/value/`, IntersectMBO/plutus 1.66.0.0):
    ///
    ///   - `currencyID`s are strictly ascending      (`currencyIDs-unordered`)
    ///   - `tokenID`s are strictly ascending within a currency, which also
    ///     rules out duplicates      (`tokenIDs-unordered`, `duplicate-tokenIDs`)
    ///   - every token map is non-empty                     (`empty-tokens`)
    ///   - every amount is non-zero                          (`zero-asset`)
    ///   - `currencyID`/`tokenID` are ≤ 32 bytes
    ///     (`currencyID-too-long-*`, `tokenID-too-long-*`)
    ///   - amounts fit in `i128`                       (`overflow`/`underflow`)
    ///
    /// An empty outer list (`(con value [])`) IS valid (`empty-value`).
    ///
    /// Ordering is plain lexicographic byte ordering — `Vec<u8>`'s `Ord` —
    /// so `#` < `#00` < `#0000` < `#000001` < `#11`. Hex digits are
    /// case-insensitive on input and compare as decoded bytes, so case never
    /// affects ordering.
    ///
    /// Note: plutus ≤ 1.65.0.0 *normalised* instead (summing duplicate keys and
    /// dropping zero/empty entries); 1.66.0.0 renamed these cases
    /// (`key-*` → `currencyID-*`/`tokenID-*`) and made them hard parse errors.
    fn parse_value_literal(&mut self) -> Result<Constant, ParseError> {
        use std::collections::BTreeMap;
        const MAX_KEY_LEN: usize = 32;
        // Outer `[...]` — list of (currencyID, token_map) pairs.
        self.expect_char('[')?;
        let mut outer: BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, i128>> = BTreeMap::new();
        let mut prev_currency: Option<Vec<u8>> = None;
        self.skip_trivia();
        if matches!(self.peek_char(), Some(']')) {
            self.pos += 1;
            return Ok(Constant::Value(outer));
        }
        loop {
            self.skip_trivia();
            // Each entry is `( #currencyID, [ ... ] )`
            self.expect_char('(')?;
            self.skip_trivia();
            let policy = self.parse_hash_bytes()?;
            if policy.len() > MAX_KEY_LEN {
                return Err(ParseError::at(
                    self.pos,
                    format!(
                        "value: currencyID exceeds {MAX_KEY_LEN} bytes (got {})",
                        policy.len()
                    ),
                ));
            }
            if let Some(prev) = &prev_currency {
                if policy <= *prev {
                    return Err(ParseError::at(
                        self.pos,
                        format!(
                            "value: currencyIDs must be strictly ascending —                              {} does not follow {}",
                            hex_lower(&policy),
                            hex_lower(prev)
                        ),
                    ));
                }
            }
            self.skip_trivia();
            self.expect_char(',')?;
            self.skip_trivia();
            // Inner `[...]` — list of (tokenID, amount) pairs.
            self.expect_char('[')?;
            let mut inner_map: BTreeMap<Vec<u8>, i128> = BTreeMap::new();
            let mut prev_token: Option<Vec<u8>> = None;
            self.skip_trivia();
            if !matches!(self.peek_char(), Some(']')) {
                loop {
                    self.skip_trivia();
                    self.expect_char('(')?;
                    self.skip_trivia();
                    let token = self.parse_hash_bytes()?;
                    if token.len() > MAX_KEY_LEN {
                        return Err(ParseError::at(
                            self.pos,
                            format!(
                                "value: tokenID exceeds {MAX_KEY_LEN} bytes (got {})",
                                token.len()
                            ),
                        ));
                    }
                    if let Some(prev) = &prev_token {
                        if token <= *prev {
                            return Err(ParseError::at(
                                self.pos,
                                format!(
                                    "value: tokenIDs must be strictly ascending within a                                      currency (no duplicates) — {} does not follow {}",
                                    hex_lower(&token),
                                    hex_lower(prev)
                                ),
                            ));
                        }
                    }
                    self.skip_trivia();
                    self.expect_char(',')?;
                    self.skip_trivia();
                    let amount = self.parse_signed_int_i128()?;
                    if amount == 0 {
                        return Err(ParseError::at(
                            self.pos,
                            format!(
                                "value: amount must be non-zero (currencyID {}, tokenID {})",
                                hex_lower(&policy),
                                hex_lower(&token)
                            ),
                        ));
                    }
                    self.skip_trivia();
                    self.expect_char(')')?;
                    prev_token = Some(token.clone());
                    inner_map.insert(token, amount);
                    self.skip_trivia();
                    match self.peek_char() {
                        Some(',') => {
                            self.pos += 1;
                            continue;
                        }
                        Some(']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(other) => {
                            return Err(ParseError::at(
                                self.pos,
                                format!("expected `,` or `]` in value token list, got `{other}`"),
                            ))
                        }
                        None => {
                            return Err(ParseError::at(
                                self.pos,
                                "unterminated value token list".into(),
                            ))
                        }
                    }
                }
            } else {
                self.pos += 1; // consume `]`
            }
            if inner_map.is_empty() {
                return Err(ParseError::at(
                    self.pos,
                    format!(
                        "value: currencyID {} has an empty token map",
                        hex_lower(&policy)
                    ),
                ));
            }
            prev_currency = Some(policy.clone());
            outer.insert(policy, inner_map);
            self.skip_trivia();
            self.expect_char(')')?;
            self.skip_trivia();
            match self.peek_char() {
                Some(',') => {
                    self.pos += 1;
                    continue;
                }
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                Some(other) => {
                    return Err(ParseError::at(
                        self.pos,
                        format!("expected `,` or `]` in value policy list, got `{other}`"),
                    ))
                }
                None => {
                    return Err(ParseError::at(
                        self.pos,
                        "unterminated value policy list".into(),
                    ))
                }
            }
        }
        // No normalisation: a non-canonical literal was already rejected above.
        Ok(Constant::Value(outer))
    }

    /// Parse a signed integer into an `i128` (used for `value` amounts
    /// where amounts must fit in i128 / Haskell's Integer that is
    /// checked for ≤ 2^127−1 overflow per the conformance corpus).
    fn parse_signed_int_i128(&mut self) -> Result<i128, ParseError> {
        let bi = self.parse_signed_int()?;
        // The Haskell reference caps amounts at 2^127 - 1 and floors at
        // -(2^127 - 1).  Any value outside that range is canonically
        // stored but the parser itself is not supposed to reject it at
        // this level — the Plutus spec leaves rejection to evaluation
        // time for insertCoin etc.  We use i128 internally and clamp
        // during conversion; values that actually overflow i128 (larger
        // than 2^127) will be rejected by the builtin denotation.
        use num_traits::ToPrimitive;
        bi.to_i128().ok_or_else(|| {
            ParseError::at(self.pos, format!("value amount {bi} exceeds i128 range"))
        })
    }

    fn parse_signed_int(&mut self) -> Result<BigInt, ParseError> {
        self.skip_trivia();
        let start = self.pos;
        let bytes = self.src.as_bytes();
        let mut end = start;
        if let Some(&c) = bytes.get(end) {
            if c == b'-' || c == b'+' {
                end += 1;
            }
        }
        let digits_from = end;
        while let Some(&c) = bytes.get(end) {
            if c.is_ascii_digit() {
                end += 1;
            } else {
                break;
            }
        }
        if end == digits_from {
            return Err(ParseError::at(self.pos, "expected integer literal".into()));
        }
        let slice = &self.src[start..end];
        let n = parse_signed_bigint(slice).map_err(|e| ParseError::at(start, e.message))?;
        self.pos = end;
        Ok(n)
    }

    fn parse_string_literal(&mut self) -> Result<String, ParseError> {
        self.expect_char('"')?;
        let bytes = self.src.as_bytes();
        let mut out = String::new();
        while let Some(&b) = bytes.get(self.pos) {
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let esc = bytes
                        .get(self.pos)
                        .copied()
                        .ok_or_else(|| ParseError::at(self.pos, "string ended in escape".into()))?;
                    self.pos += 1;
                    match esc {
                        b'\\' => out.push('\\'),
                        b'"' => out.push('"'),
                        b'\'' => out.push('\''),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'0' => out.push('\0'),
                        b'a' => out.push('\u{07}'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0C}'),
                        b'v' => out.push('\u{0B}'),
                        // Haskell-style decimal codepoint escape: \N (where N is a digit)
                        // e.g. \172 = ¬, \8712 = ∈
                        c if c.is_ascii_digit() => {
                            let dec_start = self.pos - 1; // already consumed first digit
                            let mut dec = String::new();
                            dec.push(c as char);
                            while let Some(&nc) = bytes.get(self.pos) {
                                if nc.is_ascii_digit() {
                                    dec.push(nc as char);
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            }
                            let n: u32 = dec.parse().map_err(|e| {
                                ParseError::at(dec_start, format!("bad decimal escape: {e}"))
                            })?;
                            out.push(char::from_u32(n).ok_or_else(|| {
                                ParseError::at(
                                    dec_start,
                                    format!("invalid \\N decimal escape: U+{n:04X}"),
                                )
                            })?);
                        }
                        b'x' => {
                            let h = self.read_hex_chars(2)?;
                            let n = u32::from_str_radix(&h, 16).map_err(|e| {
                                ParseError::at(self.pos, format!("bad hex escape: {e}"))
                            })?;
                            out.push(char::from_u32(n).ok_or_else(|| {
                                ParseError::at(self.pos, format!("invalid \\x escape: U+{n:04X}"))
                            })?);
                        }
                        b'u' => {
                            self.expect_char('{')?;
                            let mut hex = String::new();
                            while let Some(&c) = bytes.get(self.pos) {
                                if c == b'}' {
                                    break;
                                }
                                hex.push(c as char);
                                self.pos += 1;
                            }
                            self.expect_char('}')?;
                            let n = u32::from_str_radix(&hex, 16).map_err(|e| {
                                ParseError::at(self.pos, format!("bad \\u{{}} escape: {e}"))
                            })?;
                            out.push(char::from_u32(n).ok_or_else(|| {
                                ParseError::at(self.pos, format!("invalid \\u escape: U+{n:04X}"))
                            })?);
                        }
                        // Haskell-style octal escape: \oNNN
                        b'o' => {
                            let oct_start = self.pos;
                            let mut oct = String::new();
                            while let Some(&c) = bytes.get(self.pos) {
                                if c.is_ascii_digit() && c < b'8' {
                                    oct.push(c as char);
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            }
                            if oct.is_empty() {
                                return Err(ParseError::at(
                                    oct_start,
                                    "\\o escape requires octal digits".into(),
                                ));
                            }
                            let n = u32::from_str_radix(&oct, 8).map_err(|e| {
                                ParseError::at(oct_start, format!("bad octal escape: {e}"))
                            })?;
                            out.push(char::from_u32(n).ok_or_else(|| {
                                ParseError::at(oct_start, format!("invalid \\o escape: {n:#o}"))
                            })?);
                        }
                        // Haskell-style named escapes
                        // \NUL \SOH \STX \ETX \EOT \ENQ \ACK \BEL \BS \HT \LF \VT \FF \CR \SO \SI
                        // \DLE \DC1 \DC2 \DC3 \DC4 \NAK \SYN \ETB \CAN \EM \SUB \ESC \FS \GS \RS \US
                        // \SP \DEL
                        c if (c as char).is_ascii_uppercase() => {
                            // Back up: we already consumed `c` via the outer match —
                            // reconstruct the named escape by reading the rest of the
                            // identifier (starting from the char we already have).
                            let named_start = self.pos - 1; // pos was bumped past `c`
                            let _ = named_start; // keep for error context
                            let mut name = String::new();
                            name.push(c as char);
                            while let Some(&nc) = bytes.get(self.pos) {
                                if nc.is_ascii_uppercase() || nc.is_ascii_digit() {
                                    name.push(nc as char);
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            }
                            let ch = named_ascii_escape(&name).ok_or_else(|| {
                                ParseError::at(
                                    self.pos,
                                    format!("unknown named string escape `\\{name}`"),
                                )
                            })?;
                            out.push(ch);
                        }
                        other => {
                            return Err(ParseError::at(
                                self.pos,
                                format!("unknown string escape `\\{}`", other as char),
                            ))
                        }
                    }
                }
                _ => {
                    // Advance one Unicode codepoint, not one byte —
                    // strings in the corpus may contain multibyte UTF-8.
                    let ch_start = self.pos;
                    let ch = self.src[ch_start..]
                        .chars()
                        .next()
                        .ok_or_else(|| ParseError::at(ch_start, "unterminated string".into()))?;
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Err(ParseError::at(
            self.pos,
            "unterminated string literal".into(),
        ))
    }

    fn read_hex_chars(&mut self, n: usize) -> Result<String, ParseError> {
        let bytes = self.src.as_bytes();
        let mut s = String::with_capacity(n);
        for _ in 0..n {
            let c = bytes
                .get(self.pos)
                .copied()
                .ok_or_else(|| ParseError::at(self.pos, "short hex escape".into()))?;
            if !c.is_ascii_hexdigit() {
                return Err(ParseError::at(
                    self.pos,
                    format!("expected hex digit, got `{}`", c as char),
                ));
            }
            s.push(c as char);
            self.pos += 1;
        }
        Ok(s)
    }

    /// Bytestring literal: `#` followed by an even number of hex digits.
    /// `#` alone is a zero-length bytestring.
    fn parse_hash_bytes(&mut self) -> Result<Vec<u8>, ParseError> {
        self.expect_char('#')?;
        let bytes = self.src.as_bytes();
        let start = self.pos;
        while let Some(&c) = bytes.get(self.pos) {
            if c.is_ascii_hexdigit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        let hex_slice = &self.src[start..self.pos];
        if !hex_slice.len().is_multiple_of(2) {
            return Err(ParseError::at(
                start,
                format!(
                    "bytestring literal must have an even number of hex digits, got {}",
                    hex_slice.len()
                ),
            ));
        }
        decode_hex(hex_slice).map_err(|e| ParseError::at(start, e))
    }

    /// `0x` followed by hex digits — used for BLS element literals.
    fn parse_0x_hex_bytes(&mut self) -> Result<Vec<u8>, ParseError> {
        self.expect_char('0')?;
        self.expect_char('x')?;
        let bytes = self.src.as_bytes();
        let start = self.pos;
        while let Some(&c) = bytes.get(self.pos) {
            if c.is_ascii_hexdigit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        let hex_slice = &self.src[start..self.pos];
        if !hex_slice.len().is_multiple_of(2) {
            return Err(ParseError::at(
                start,
                format!(
                    "0x literal must have an even number of hex digits, got {}",
                    hex_slice.len()
                ),
            ));
        }
        decode_hex(hex_slice).map_err(|e| ParseError::at(start, e))
    }

    // ─── Data sub-language ────────────────────────────────────────────

    fn parse_data(&mut self) -> Result<Data, ParseError> {
        self.skip_trivia();
        // `(` here is grouping — the inner thing is still a Data atom
        // and may be parenthesised arbitrarily many times. The
        // conformance corpus uses `(I 5)` at the top of a `(con data
        // ...)` constant, but `I 5` (no outer parens) when the same
        // value appears inside `[...]` (list) or `(...)` (map pair).
        if matches!(self.peek_char(), Some('(')) {
            self.pos += 1;
            let v = self.parse_data()?;
            self.skip_trivia();
            self.expect_char(')')?;
            return Ok(v);
        }
        let head_start = self.pos;
        let head = self.parse_ident()?;
        self.skip_trivia();
        match head {
            "I" => Ok(Data::I(self.parse_signed_int()?)),
            "B" => Ok(Data::B(self.parse_hash_bytes()?)),
            "List" => Ok(Data::List(self.parse_data_list()?)),
            "Map" => Ok(Data::Map(self.parse_data_map()?)),
            "Constr" => {
                // The tag is an arbitrary-precision signed `Integer` in
                // Haskell (`Data = Constr Integer [Data] | ...`), matching
                // `Data::Constr`'s `BigInt` tag (#859) — use the same
                // signed-integer parser as the `I` atom rather than the
                // Word64-bounded `parse_uint_u64`, so the textual syntax
                // can express the full domain (including a transient
                // negative/oversized tag a script can construct pre-PV11).
                let tag = self.parse_signed_int()?;
                self.skip_trivia();
                let args = self.parse_data_list()?;
                Ok(Data::Constr(tag, args))
            }
            other => Err(ParseError::at(
                head_start,
                format!("unknown Data constructor `{other}`"),
            )),
        }
    }

    fn parse_data_list(&mut self) -> Result<Vec<Data>, ParseError> {
        self.skip_trivia();
        self.expect_char('[')?;
        let mut out = Vec::new();
        self.skip_trivia();
        if matches!(self.peek_char(), Some(']')) {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            self.skip_trivia();
            out.push(self.parse_data()?);
            self.skip_trivia();
            match self.peek_char() {
                Some(',') => {
                    self.pos += 1;
                    continue;
                }
                Some(']') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(other) => {
                    return Err(ParseError::at(
                        self.pos,
                        format!("expected `,` or `]` in Data list, got `{other}`"),
                    ))
                }
                None => return Err(ParseError::at(self.pos, "unterminated Data list".into())),
            }
        }
    }

    fn parse_data_map(&mut self) -> Result<Vec<(Data, Data)>, ParseError> {
        self.skip_trivia();
        self.expect_char('[')?;
        let mut out = Vec::new();
        self.skip_trivia();
        if matches!(self.peek_char(), Some(']')) {
            self.pos += 1;
            return Ok(out);
        }
        loop {
            self.skip_trivia();
            self.expect_char('(')?;
            self.skip_trivia();
            let k = self.parse_data()?;
            self.skip_trivia();
            self.expect_char(',')?;
            self.skip_trivia();
            let v = self.parse_data()?;
            self.skip_trivia();
            self.expect_char(')')?;
            out.push((k, v));
            self.skip_trivia();
            match self.peek_char() {
                Some(',') => {
                    self.pos += 1;
                    continue;
                }
                Some(']') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(other) => {
                    return Err(ParseError::at(
                        self.pos,
                        format!("expected `,` or `]` in Data map, got `{other}`"),
                    ))
                }
                None => return Err(ParseError::at(self.pos, "unterminated Data map".into())),
            }
        }
    }

    // ─── primitives ──────────────────────────────────────────────────

    fn skip_trivia(&mut self) {
        let bytes = self.src.as_bytes();
        loop {
            while let Some(&b) = bytes.get(self.pos) {
                if b.is_ascii_whitespace() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Line comment: `--` to end of line.
            if bytes.get(self.pos).copied() == Some(b'-')
                && bytes.get(self.pos + 1).copied() == Some(b'-')
            {
                self.pos += 2;
                while let Some(&b) = bytes.get(self.pos) {
                    self.pos += 1;
                    if b == b'\n' {
                        break;
                    }
                }
                continue;
            }
            break;
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_snippet(&self) -> &str {
        let end = (self.pos + 24).min(self.src.len());
        &self.src[self.pos..end]
    }

    fn expect_char(&mut self, want: char) -> Result<(), ParseError> {
        match self.peek_char() {
            Some(c) if c == want => {
                self.pos += c.len_utf8();
                Ok(())
            }
            Some(c) => Err(ParseError::at(
                self.pos,
                format!("expected `{want}`, got `{c}`"),
            )),
            None => Err(ParseError::at(
                self.pos,
                format!("expected `{want}`, got end of input"),
            )),
        }
    }

    fn expect_keyword(&mut self, kw: &str) -> Result<(), ParseError> {
        let start = self.pos;
        let id = self.parse_ident()?;
        if id != kw {
            return Err(ParseError::at(
                start,
                format!("expected `{kw}`, got `{id}`"),
            ));
        }
        Ok(())
    }

    /// Parse an identifier. The first character must be ASCII alpha or
    /// `_`; subsequent characters may be ASCII alphanumeric, `_`, `-`,
    /// or `'`. Returns a slice of the input — no allocation.
    fn parse_ident(&mut self) -> Result<&'a str, ParseError> {
        let bytes = self.src.as_bytes();
        let start = self.pos;
        match bytes.get(self.pos).copied() {
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                self.pos += 1;
            }
            Some(c) => {
                return Err(ParseError::at(
                    start,
                    format!("expected identifier, got `{}`", c as char),
                ))
            }
            None => {
                return Err(ParseError::at(
                    start,
                    "expected identifier, got end of input".into(),
                ))
            }
        }
        while let Some(&c) = bytes.get(self.pos) {
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'\'' {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(&self.src[start..self.pos])
    }

    fn parse_uint_u64(&mut self) -> Result<u64, ParseError> {
        let bytes = self.src.as_bytes();
        let start = self.pos;
        while let Some(&c) = bytes.get(self.pos) {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err(ParseError::at(start, "expected unsigned integer".into()));
        }
        self.src[start..self.pos]
            .parse::<u64>()
            .map_err(|e| ParseError::at(start, format!("invalid u64 literal: {e}")))
    }
}

// ─── helpers ────────────────────────────────────────────────────────

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = hex_nybble(bytes[i])?;
        let lo = hex_nybble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    if i != bytes.len() {
        return Err(format!("odd number of hex digits ({})", bytes.len()));
    }
    Ok(out)
}

fn hex_nybble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("invalid hex digit `{}`", b as char)),
    }
}

/// Haskell-style named ASCII/control character escapes.
/// These are the multi-character names used in Haskell string literals
/// after a backslash, e.g. `\NUL`, `\DEL`, `\SOH`, `\SO`, `\SI` etc.
fn named_ascii_escape(name: &str) -> Option<char> {
    // Two-letter codes that could conflict with longer codes first.
    // We match exact names only (the parser already collected the full
    // uppercase+digit run).
    match name {
        "NUL" => Some('\x00'),
        "SOH" => Some('\x01'),
        "STX" => Some('\x02'),
        "ETX" => Some('\x03'),
        "EOT" => Some('\x04'),
        "ENQ" => Some('\x05'),
        "ACK" => Some('\x06'),
        "BEL" | "a" => Some('\x07'),
        "BS" => Some('\x08'),
        "HT" => Some('\x09'),
        "LF" => Some('\x0A'),
        "VT" => Some('\x0B'),
        "FF" => Some('\x0C'),
        "CR" => Some('\x0D'),
        "SO" => Some('\x0E'),
        "SI" => Some('\x0F'),
        "DLE" => Some('\x10'),
        "DC1" => Some('\x11'),
        "DC2" => Some('\x12'),
        "DC3" => Some('\x13'),
        "DC4" => Some('\x14'),
        "NAK" => Some('\x15'),
        "SYN" => Some('\x16'),
        "ETB" => Some('\x17'),
        "CAN" => Some('\x18'),
        "EM" => Some('\x19'),
        "SUB" => Some('\x1A'),
        "ESC" => Some('\x1B'),
        "FS" => Some('\x1C'),
        "GS" => Some('\x1D'),
        "RS" => Some('\x1E'),
        "US" => Some('\x1F'),
        "SP" => Some(' '),
        "DEL" => Some('\x7F'),
        _ => None,
    }
}

/// Render bytes as a lower-case `#`-prefixed hex literal for diagnostics —
/// the same spelling the UPLC textual syntax uses for `bytestring`
/// / `value` keys.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(1 + bytes.len() * 2);
    s.push('#');
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
