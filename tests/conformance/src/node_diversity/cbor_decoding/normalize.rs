//! The dataset's CBOR normalisation, in Rust.
//!
//! The reference is `app/Generation/Normalize.hs` and the table in `VERIFICATION.md` of the
//! pinned cardano-cbor-dataset.  A re-encoding is compared with `.expected.cbor` only after both
//! sides are normalised, so that the container form an encoder happens to choose is not
//! observable, and nothing else is:
//!
//! | from | to |
//! |---|---|
//! | indefinite array / map | definite, same elements in the same order |
//! | chunked byte / text string | one definite string |
//! | non-minimal integer, tag or length head | the minimal head |
//! | bignum (tag 2 / 3) whose value fits a native head | the native uint / nint |
//! | bignum that does not fit | kept, leading zero bytes of the magnitude dropped |
//!
//! Map key order, duplicate keys, tags, float widths and every integer value survive.  The
//! normaliser is checked two ways: against the 20 hand-verified pairs under
//! `dataset/normalization`, and against the dataset itself (for every valid sample of a rule that
//! is not byte-exact, `normalize(input) == expected`).

/// A CBOR data item, with every head already decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Uint(u64),
    /// A native negative integer; the value is `-1 - n`.
    Nint(u64),
    Bytes(Vec<u8>),
    /// UTF-8 bytes, kept as bytes: validity is the decoder's business, not the normaliser's.
    Text(Vec<u8>),
    Array(Vec<Item>),
    Map(Vec<(Item, Item)>),
    Tag(u64, Box<Item>),
    /// A simple value other than the floats: `false` (20), `true` (21), `null` (22), ...
    Simple(u8),
    /// Float of 2, 4 or 8 bytes, kept as its bits so that a width is never changed.
    Float(Vec<u8>),
}

const MAX_DEPTH: usize = 512;

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| format!("truncated at byte {}", self.pos))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    /// The argument of a head; `None` for the indefinite marker (additional info 31).
    fn argument(&mut self, info: u8) -> Result<Option<u64>, String> {
        Ok(Some(match info {
            0..=23 => u64::from(info),
            24 => u64::from(self.byte()?),
            25 => u64::from(u16::from_be_bytes(self.take(2)?.try_into().unwrap())),
            26 => u64::from(u32::from_be_bytes(self.take(4)?.try_into().unwrap())),
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            31 => return Ok(None),
            _ => return Err(format!("reserved additional information {info}")),
        }))
    }

    fn length(&mut self, arg: u64) -> Result<usize, String> {
        usize::try_from(arg).map_err(|_| "length does not fit".to_string())
    }

    /// The chunks of an indefinite-length string of major type `major`.
    fn chunks(&mut self, major: u8) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            let head = self.byte()?;
            if head == 0xff {
                return Ok(out);
            }
            if head >> 5 != major {
                return Err("indefinite string chunk of the wrong type".into());
            }
            match self.argument(head & 0x1f)? {
                Some(n) => {
                    let n = self.length(n)?;
                    out.extend_from_slice(self.take(n)?);
                }
                None => return Err("nested indefinite string".into()),
            }
        }
    }

    fn item(&mut self, depth: usize) -> Result<Item, String> {
        if depth > MAX_DEPTH {
            return Err("nesting too deep".into());
        }
        let head = self.byte()?;
        let (major, info) = (head >> 5, head & 0x1f);
        match major {
            0 => Ok(Item::Uint(
                self.argument(info)?.ok_or("indefinite integer")?,
            )),
            1 => Ok(Item::Nint(
                self.argument(info)?.ok_or("indefinite integer")?,
            )),
            2 | 3 => {
                let data = match self.argument(info)? {
                    Some(n) => {
                        let n = self.length(n)?;
                        self.take(n)?.to_vec()
                    }
                    None => self.chunks(major)?,
                };
                Ok(if major == 2 {
                    Item::Bytes(data)
                } else {
                    Item::Text(data)
                })
            }
            4 => {
                let mut items = Vec::new();
                match self.argument(info)? {
                    Some(n) => {
                        for _ in 0..n {
                            items.push(self.item(depth + 1)?);
                        }
                    }
                    None => {
                        while self.bytes.get(self.pos) != Some(&0xff) {
                            items.push(self.item(depth + 1)?);
                        }
                        self.pos += 1;
                    }
                }
                Ok(Item::Array(items))
            }
            5 => {
                let mut entries = Vec::new();
                match self.argument(info)? {
                    Some(n) => {
                        for _ in 0..n {
                            let key = self.item(depth + 1)?;
                            entries.push((key, self.item(depth + 1)?));
                        }
                    }
                    None => {
                        while self.bytes.get(self.pos) != Some(&0xff) {
                            let key = self.item(depth + 1)?;
                            entries.push((key, self.item(depth + 1)?));
                        }
                        self.pos += 1;
                    }
                }
                Ok(Item::Map(entries))
            }
            6 => {
                let tag = self.argument(info)?.ok_or("indefinite tag")?;
                Ok(Item::Tag(tag, Box::new(self.item(depth + 1)?)))
            }
            _ => match info {
                0..=23 => Ok(Item::Simple(info)),
                24 => Ok(Item::Simple(self.byte()?)),
                25 => Ok(Item::Float(self.take(2)?.to_vec())),
                26 => Ok(Item::Float(self.take(4)?.to_vec())),
                27 => Ok(Item::Float(self.take(8)?.to_vec())),
                _ => Err(format!("unexpected simple value information {info}")),
            },
        }
    }
}

/// Parse exactly one item; anything after it is an error.
pub fn parse(bytes: &[u8]) -> Result<Item, String> {
    let mut parser = Parser { bytes, pos: 0 };
    let item = parser.item(0)?;
    if parser.pos != bytes.len() {
        return Err(format!(
            "{} trailing bytes after the top-level item",
            bytes.len() - parser.pos
        ));
    }
    Ok(item)
}

/// The shape rewrite: definite containers and strings, and bignums folded when they fit.
pub fn normalize_item(item: Item) -> Item {
    match item {
        Item::Array(items) => Item::Array(items.into_iter().map(normalize_item).collect()),
        Item::Map(entries) => Item::Map(
            entries
                .into_iter()
                .map(|(k, v)| (normalize_item(k), normalize_item(v)))
                .collect(),
        ),
        Item::Tag(tag @ (2 | 3), inner) => match *inner {
            Item::Bytes(magnitude) => fold_bignum(tag == 3, magnitude),
            other => Item::Tag(tag, Box::new(normalize_item(other))),
        },
        Item::Tag(30, inner) => match normalize_item(*inner) {
            Item::Array(parts) => match reduce_ratio(&parts) {
                Some(reduced) => Item::Tag(30, Box::new(Item::Array(reduced))),
                None => Item::Tag(30, Box::new(Item::Array(parts))),
            },
            other => Item::Tag(30, Box::new(other)),
        },
        Item::Tag(tag, inner) => Item::Tag(tag, Box::new(normalize_item(*inner))),
        other => other,
    }
}

/// The integer an item denotes, when it is an integer (native or bignum, already folded).
fn as_bigint(item: &Item) -> Option<num_bigint::BigInt> {
    use num_bigint::{BigInt, Sign};
    match item {
        Item::Uint(n) => Some(BigInt::from(*n)),
        Item::Nint(n) => Some(-BigInt::from(*n) - 1),
        Item::Tag(2, inner) => match inner.as_ref() {
            Item::Bytes(b) => Some(BigInt::from_bytes_be(Sign::Plus, b)),
            _ => None,
        },
        Item::Tag(3, inner) => match inner.as_ref() {
            Item::Bytes(b) => Some(-BigInt::from_bytes_be(Sign::Plus, b) - 1),
            _ => None,
        },
        _ => None,
    }
}

/// The shortest item for an integer: a native head when the argument fits 64 bits.
fn from_bigint(n: &num_bigint::BigInt) -> Item {
    use num_bigint::Sign;
    if n.sign() == Sign::Minus {
        let magnitude = (-n.clone() - 1u8).to_bytes_be().1;
        fold_bignum(true, magnitude)
    } else {
        fold_bignum(false, n.to_bytes_be().1)
    }
}

/// `[n, d]` with a non-zero denominator, in lowest terms (Haskell builds a `Rational` with `%`).
fn reduce_ratio(parts: &[Item]) -> Option<Vec<Item>> {
    let [n, d] = parts else { return None };
    let (n, d) = (as_bigint(n)?, as_bigint(d)?);
    if d == num_bigint::BigInt::from(0) {
        return None;
    }
    let zero = num_bigint::BigInt::from(0);
    let (mut a, mut b) = (n.clone(), d.clone());
    while b != zero {
        let r = &a % &b;
        a = std::mem::replace(&mut b, r);
    }
    let g = if a < zero { -a } else { a };
    let (mut n, mut d) = (n / &g, d / &g);
    if d < num_bigint::BigInt::from(0) {
        (n, d) = (-n, -d);
    }
    Some(vec![from_bigint(&n), from_bigint(&d)])
}

/// `#6.2 magnitude` / `#6.3 magnitude` as the shortest item: a native head when the argument
/// fits 64 bits, otherwise the bignum with its leading zero bytes dropped.
fn fold_bignum(negative: bool, magnitude: Vec<u8>) -> Item {
    let start = magnitude
        .iter()
        .position(|b| *b != 0)
        .unwrap_or(magnitude.len());
    let trimmed = &magnitude[start..];
    if trimmed.len() <= 8 {
        let mut arg = [0u8; 8];
        arg[8 - trimmed.len()..].copy_from_slice(trimmed);
        let n = u64::from_be_bytes(arg);
        return if negative {
            Item::Nint(n)
        } else {
            Item::Uint(n)
        };
    }
    Item::Tag(
        if negative { 3 } else { 2 },
        Box::new(Item::Bytes(trimmed.to_vec())),
    )
}

fn head(out: &mut Vec<u8>, major: u8, arg: u64) {
    let m = major << 5;
    match arg {
        0..=23 => out.push(m | arg as u8),
        24..=0xff => out.extend([m | 24, arg as u8]),
        0x100..=0xffff => {
            out.push(m | 25);
            out.extend((arg as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(m | 26);
            out.extend((arg as u32).to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend(arg.to_be_bytes());
        }
    }
}

/// Encode with minimal heads and definite lengths throughout.
pub fn encode_item(item: &Item, out: &mut Vec<u8>) {
    match item {
        Item::Uint(n) => head(out, 0, *n),
        Item::Nint(n) => head(out, 1, *n),
        Item::Bytes(b) => {
            head(out, 2, b.len() as u64);
            out.extend_from_slice(b);
        }
        Item::Text(t) => {
            head(out, 3, t.len() as u64);
            out.extend_from_slice(t);
        }
        Item::Array(items) => {
            head(out, 4, items.len() as u64);
            items.iter().for_each(|i| encode_item(i, out));
        }
        Item::Map(entries) => {
            head(out, 5, entries.len() as u64);
            for (k, v) in entries {
                encode_item(k, out);
                encode_item(v, out);
            }
        }
        Item::Tag(tag, inner) => {
            head(out, 6, *tag);
            encode_item(inner, out);
        }
        Item::Simple(v) if *v < 24 => out.push(0xe0 | v),
        Item::Simple(v) => out.extend([0xf8, *v]),
        Item::Float(bits) => {
            out.push(match bits.len() {
                2 => 0xf9,
                4 => 0xfa,
                _ => 0xfb,
            });
            out.extend_from_slice(bits);
        }
    }
}

/// `normalize(bytes)`: parse, rewrite, re-encode.  An input that is not one well-formed item is
/// an error, like the reference's `decodeWholeTerm`.
pub fn normalize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let item = normalize_item(parse(bytes)?);
    let mut out = Vec::with_capacity(bytes.len());
    encode_item(&item, &mut out);
    Ok(out)
}

/// Sort every map's entries by the encoded bytes of the key (stable, so duplicate keys keep
/// their order).  NOT part of the dataset's normalisation: it is used only to tell, when
/// checking the dataset against this module, a rule whose reference re-encoding sorted a map
/// (the Haskell type is a `Map`) from a genuine disagreement.
pub fn sort_maps(item: Item) -> Item {
    match item {
        Item::Array(items) => Item::Array(items.into_iter().map(sort_maps).collect()),
        Item::Map(entries) => {
            let mut keyed: Vec<(Vec<u8>, Item, Item)> = entries
                .into_iter()
                .map(|(k, v)| {
                    let k = sort_maps(k);
                    (sort_key(&k), k, sort_maps(v))
                })
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            Item::Map(keyed.into_iter().map(|(_, k, v)| (k, v)).collect())
        }
        Item::Tag(tag, inner) => Item::Tag(tag, Box::new(sort_maps(*inner))),
        other => other,
    }
}

/// The order Haskell's `Ord` gives the keys the dataset uses: byte strings compare by their
/// bytes (a shorter prefix first), everything else by its encoding.
fn sort_key(item: &Item) -> Vec<u8> {
    match item {
        Item::Bytes(b) => {
            let mut key = vec![2u8];
            key.extend_from_slice(b);
            key
        }
        other => {
            let mut key = vec![0u8];
            encode_item(other, &mut key);
            key
        }
    }
}

/// `normalize`, then sort every map: equal for two encodings that differ only in map entry order.
pub fn normalize_sorted(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let item = sort_maps(normalize_item(parse(bytes)?));
    let mut out = Vec::with_capacity(bytes.len());
    encode_item(&item, &mut out);
    Ok(out)
}

/// A deliberately weak form for the dataset self-check only: every array and map ordered, tag 258
/// removed, set elements de-duplicated.  Two encodings equal in this form hold the same leaf
/// values and differ only in ordering, set tags and duplicates (what a Haskell `Set` or `Map`
/// re-encoding changes).
pub fn loose(item: Item) -> Item {
    match item {
        // Embedded CBOR (`#6.24(bytes .cbor x)`): the reference re-encodes what is inside.
        Item::Tag(24, inner) => match *inner {
            Item::Bytes(b) => match parse(&b) {
                Ok(embedded) => {
                    let mut bytes = Vec::new();
                    encode_item(&loose(normalize_item(embedded)), &mut bytes);
                    Item::Tag(24, Box::new(Item::Bytes(bytes)))
                }
                Err(_) => Item::Tag(24, Box::new(Item::Bytes(b))),
            },
            other => Item::Tag(24, Box::new(loose(other))),
        },
        Item::Tag(258, inner) => match loose(*inner) {
            Item::Array(mut xs) => {
                xs.dedup();
                Item::Array(xs)
            }
            other => other,
        },
        Item::Array(items) => {
            // The reference drops a null element of a Dijkstra pool registration.
            let mut items: Vec<Item> = items
                .into_iter()
                .map(loose)
                .filter(|i| *i != Item::Simple(22))
                .collect();
            // A value with no assets, `[coin, {}]`, is re-encoded as the bare coin.
            if let [Item::Uint(_), Item::Map(assets)] = items.as_slice() {
                if assets.is_empty() {
                    return items.swap_remove(0);
                }
            }
            let mut xs: Vec<(Vec<u8>, Item)> =
                items.into_iter().map(|i| (sort_key(&i), i)).collect();
            xs.sort_by(|a, b| a.0.cmp(&b.0));
            Item::Array(xs.into_iter().map(|(_, i)| i).collect())
        }
        Item::Map(entries) => {
            let mut es: Vec<(Vec<u8>, Item, Item)> = entries
                .into_iter()
                .map(|(k, v)| {
                    let k = loose(k);
                    (sort_key(&k), k, loose(v))
                })
                // A policy with no assets is dropped from a multi-asset map.
                .filter(|(_, _, v)| !matches!(v, Item::Map(m) if m.is_empty()))
                .collect();
            es.sort_by(|a, b| a.0.cmp(&b.0));
            // A map-form output `{0: address, 1: value}` is re-encoded in the array form.
            if let [(Item::Uint(0), a), (Item::Uint(1), v)] = es
                .iter()
                .map(|(_, k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
                .as_slice()
            {
                return loose(Item::Array(vec![a.clone(), v.clone()]));
            }
            // ... and with a datum hash, `{0: a, 1: v, 2: [0, h]}` as `[a, v, h]`.
            if let [(Item::Uint(0), a), (Item::Uint(1), v), (Item::Uint(2), d)] = es
                .iter()
                .map(|(_, k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
                .as_slice()
            {
                let datum_hash = match d {
                    Item::Array(parts) => match parts.as_slice() {
                        [Item::Uint(0), h @ Item::Bytes(_)] => Some(h.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(h) = datum_hash {
                    return loose(Item::Array(vec![a.clone(), v.clone(), h]));
                }
            }
            Item::Map(es.into_iter().map(|(_, k, v)| (k, v)).collect())
        }
        Item::Tag(tag, inner) => Item::Tag(tag, Box::new(loose(*inner))),
        other => other,
    }
}

/// `normalize`, then [`loose`].
pub fn normalize_loose(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let item = loose(normalize_item(parse(bytes)?));
    let mut out = Vec::with_capacity(bytes.len());
    encode_item(&item, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(hex_in: &str) -> String {
        hex::encode(normalize(&hex::decode(hex_in).unwrap()).unwrap())
    }

    /// The table of `dataset/normalization/README.md`, every row.
    #[test]
    fn the_twenty_reference_rows() {
        let rows = [
            ("9f0102ff", "820102"),
            ("bf0102ff", "a10102"),
            ("5f420102420304ff", "4401020304"),
            ("7f626162626364ff", "6461626364"),
            ("9f9f01ffbf0102ffff", "828101a10102"),
            ("9fff", "80"),
            ("bfff", "a0"),
            ("1805", "05"),
            ("a202010102", "a202010102"),
            ("a201020103", "a201020103"),
            ("1bffffffffffffffff", "1bffffffffffffffff"),
            ("d81e820102", "d81e820102"),
            ("d90103a10102", "d90103a10102"),
            ("d9050f01", "d9050f01"),
            ("d81e9f0102ff", "d81e820102"),
            ("f93c00", "f93c00"),
            ("c24296be", "1996be"),
            ("c3435e0f31", "3a005e0f31"),
            ("c2420000", "00"),
            ("c249010000000000000000", "c249010000000000000000"),
        ];
        for (input, expected) in rows {
            assert_eq!(n(input), expected, "normalize({input})");
        }
    }

    #[test]
    fn trailing_bytes_and_truncation_are_errors() {
        assert!(normalize(&[0x01, 0x02]).is_err());
        assert!(normalize(&[0x82, 0x01]).is_err());
        assert!(normalize(&[]).is_err());
    }
}
