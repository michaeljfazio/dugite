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
        Ok(Program { version, term })
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
                    term = Term::App(Box::new(term), Box::new(arg));
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
        Ok(Term::Lam(Box::new(body)))
    }

    fn parse_delay(&mut self) -> Result<Term, ParseError> {
        let body = self.parse_term()?;
        Ok(Term::Delay(Box::new(body)))
    }

    fn parse_force(&mut self) -> Result<Term, ParseError> {
        let body = self.parse_term()?;
        Ok(Term::Force(Box::new(body)))
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
            args.push(self.parse_term()?);
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
            branches.push(self.parse_term()?);
        }
        Ok(Term::Case {
            scrutinee: Box::new(scrutinee),
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
            TypeTag::Data => Ok(Constant::Data(self.parse_data()?)),
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
                // No textual literal exists in the conformance corpus;
                // accept the same `0x...` form for symmetry.
                let bytes = self.parse_0x_hex_bytes()?;
                if bytes.len() != 576 {
                    return Err(ParseError::at(
                        self.pos,
                        format!("BLS12-381 ML result must be 576 bytes, got {}", bytes.len()),
                    ));
                }
                let mut arr = [0u8; 576];
                arr.copy_from_slice(&bytes);
                Ok(Constant::Bls12_381MlResult(Box::new(arr)))
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
                let tag = self.parse_uint_u64()?;
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
