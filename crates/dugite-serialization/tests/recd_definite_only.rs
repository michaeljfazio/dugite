//! Records Haskell decodes with `RecD` are DEFINITE-ONLY before decoder version 12.
//!
//! `Coders.hs`: `decodeCount (RecD cn) n = decodeRecordNamed "RecD" (const n) (pure (n, cn))`.
//! The inner decoder of that `decodeRecordNamed` is `pure`, so `decodeListLikeT` reads the break
//! right after the array head, BEFORE any field: `9f <fields> ff` fails with "Excess terms in
//! array". cardano-node 11.1.3 (cardano-ledger `f649f975`) decodes these types that way in
//! Conway; each switches to a field-reading `decodeRecordNamed` only at version 12.
//!
//! Types covered, each driven through a real block (`decode_block`, `decode_block_minimal`) or
//! the transaction-submission decode (`decode_transaction`):
//! the Mary value pair (`Mary/Value.hs:298`), `ExUnits`, `Prices`, `GovActionId`,
//! `VotingProcedure`, `ProposalProcedure`, `Constitution`, `Anchor` and the Praos header and
//! header body. The definite forms must still decode and the indefinite forms of the
//! `decodeRecordNamed` / `decodeRecordSum` records (e.g. TxIn, credential) must still be
//! accepted, so the two families are not confused again.

use dugite_primitives::era::Era;
use dugite_serialization::{
    decode_block, decode_block_minimal, decode_transaction, decode_transaction_output,
    decode_wrapped_block_header,
};

fn load_vector(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/test_vectors/{}.hex",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    hex::decode(std::fs::read_to_string(path).unwrap().trim()).unwrap()
}

fn bstr(b: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    if b.len() < 24 {
        v.push(0x40 + b.len() as u8);
    } else {
        v.extend([0x58, b.len() as u8]);
    }
    v.extend_from_slice(b);
    v
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

/// An array of already-encoded items, definite or indefinite.
fn arr(indef: bool, items: &[Vec<u8>]) -> Vec<u8> {
    let mut v = if indef {
        vec![0x9f]
    } else {
        assert!(items.len() < 24);
        vec![0x80 + items.len() as u8]
    };
    for i in items {
        v.extend_from_slice(i);
    }
    if indef {
        v.push(0xff);
    }
    v
}

fn addr() -> Vec<u8> {
    let mut a = vec![0x61];
    a.extend([0x11u8; 28]);
    bstr(&a)
}

fn anchor(indef: bool) -> Vec<u8> {
    arr(indef, &[vec![0x61, b'a'], bstr(&[0x44; 32])])
}

fn gov_action_id(indef: bool) -> Vec<u8> {
    arr(indef, &[bstr(&[0x55; 32]), vec![0x00]])
}

fn body(extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut b = vec![0xa3 + extra.len() as u8, 0x00, 0x80, 0x01, 0x80, 0x02, 0x00];
    for (k, v) in extra {
        b.push(*k);
        b.extend_from_slice(v);
    }
    b
}

fn tx(body: &[u8], wits: &[u8]) -> Vec<u8> {
    cat(&[&[0x84], body, wits, &[0xf5, 0xf6]])
}

fn block(body: &[u8], wits: &[u8]) -> Vec<u8> {
    let full = load_vector("conway");
    let mut d = minicbor::Decoder::new(&full);
    d.array().unwrap();
    let era = d.u64().unwrap();
    d.array().unwrap();
    let hs = d.position();
    d.skip().unwrap();
    cat(&[
        &[0x82, era as u8, 0x85],
        &full[hs..d.position()],
        &[0x81],
        body,
        &[0x81],
        wits,
        &[0xa0, 0x80],
    ])
}

/// Assert: indefinite form refused on the tx-submission path and in both block modes
/// (`minimal` only when the type lives in the body, which that mode still parses);
/// definite form accepted everywhere.
fn check(
    name: &str,
    definite: (Vec<u8>, Vec<u8>),
    indefinite: (Vec<u8>, Vec<u8>),
    body_borne: bool,
) {
    let (db, dw) = definite;
    let (ib, iw) = indefinite;
    decode_transaction(6, &tx(&db, &dw)).unwrap_or_else(|e| panic!("{name}: definite tx: {e}"));
    decode_block(&block(&db, &dw)).unwrap_or_else(|e| panic!("{name}: definite block: {e}"));
    decode_block_minimal(&block(&db, &dw))
        .unwrap_or_else(|e| panic!("{name}: definite minimal block: {e}"));

    assert!(
        decode_transaction(6, &tx(&ib, &iw)).is_err(),
        "{name}: indefinite form must be refused on tx submission"
    );
    assert!(
        decode_block(&block(&ib, &iw)).is_err(),
        "{name}: indefinite form must be refused by decode_block"
    );
    if body_borne {
        assert!(
            decode_block_minimal(&block(&ib, &iw)).is_err(),
            "{name}: indefinite form must be refused by decode_block_minimal"
        );
    }
}

/// `{0: addr, 1: <value>}` as the single output.
fn body_with_value(value: Vec<u8>) -> Vec<u8> {
    let out = cat(&[&[0xa2, 0x00], &addr(), &[0x01], &value]);
    let mut b = vec![0xa3, 0x00, 0x80, 0x01, 0x81];
    b.extend(out);
    b.extend([0x02, 0x00]);
    b
}

#[test]
fn mary_value_pair_is_definite_only() {
    let ma = |_: bool| {
        // { policy(28) => { "A" => 1 } }
        cat(&[&[0xa1], &bstr(&[0x22; 28]), &[0xa1, 0x41, 0x41, 0x01]])
    };
    let value = |indef: bool| {
        let mut v = vec![if indef { 0x9f } else { 0x82 }, 0x01];
        v.extend(ma(indef));
        if indef {
            v.push(0xff);
        }
        v
    };
    check(
        "value pair",
        (body_with_value(value(false)), vec![0xa0]),
        (body_with_value(value(true)), vec![0xa0]),
        true,
    );
    // the exact output from the review, through the standalone output decoder
    let out = cat(&[&[0xa2, 0x00], &addr(), &[0x01], &value(true)]);
    for era in [5u16, 6, 7] {
        assert!(decode_transaction_output(era, &out).is_err(), "era {era}");
    }
}

#[test]
fn ex_units_in_a_redeemer_is_definite_only() {
    // witness set {5: {[0, 0]: [0, <ex_units>]}}
    let wits = |indef: bool| {
        cat(&[
            &[0xa1, 0x05, 0xa1, 0x82, 0x00, 0x00, 0x82, 0x00],
            &arr(indef, &[vec![0x01], vec![0x01]]),
        ])
    };
    check(
        "ex_units",
        (body(&[]), wits(false)),
        (body(&[]), wits(true)),
        false,
    );
}

fn proposal(action: Vec<u8>, anchor_bytes: Vec<u8>, indef: bool) -> Vec<u8> {
    arr(
        indef,
        &[
            vec![0x1a, 0x00, 0x0f, 0x42, 0x40],
            addr_reward(),
            action,
            anchor_bytes,
        ],
    )
}

fn addr_reward() -> Vec<u8> {
    let mut a = vec![0xe1];
    a.extend([0x11u8; 28]);
    bstr(&a)
}

fn body_with_proposal(p: Vec<u8>) -> Vec<(u8, Vec<u8>)> {
    vec![(0x14, arr(false, &[p]))]
}

#[test]
fn ex_unit_prices_are_definite_only() {
    // ParameterChange [0, null, {19: prices}, null]
    let prices = |indef: bool| {
        let r = |n: u8, d: u8| cat(&[&[0xd8, 0x1e], &arr(false, &[vec![n], vec![d]])]);
        arr(indef, &[r(1, 2), r(3, 4)])
    };
    let action = |indef: bool| {
        arr(
            false,
            &[
                vec![0x00],
                vec![0xf6],
                cat(&[&[0xa1, 0x13], &prices(indef)]),
                vec![0xf6],
            ],
        )
    };
    check(
        "ex_unit_prices",
        (
            body(&body_with_proposal(proposal(
                action(false),
                anchor(false),
                false,
            ))),
            vec![0xa0],
        ),
        (
            body(&body_with_proposal(proposal(
                action(true),
                anchor(false),
                false,
            ))),
            vec![0xa0],
        ),
        true,
    );
}

#[test]
fn tag30_rationals_accept_the_indefinite_pair_with_two_elements() {
    // `decodeRationalWithTag` reads the pair with `decodeCollectionWithLen decodeListLenOrIndef
    // decodeInteger` and requires exactly two elements.
    let prices = |rat_indef: bool, n_elems: usize| {
        let mut elems = vec![vec![0x01], vec![0x02]];
        if n_elems == 3 {
            elems.push(vec![0x03]);
        }
        if n_elems == 1 {
            elems.pop();
        }
        let r = cat(&[&[0xd8, 0x1e], &arr(rat_indef, &elems)]);
        arr(false, &[r.clone(), r])
    };
    let action = |p: Vec<u8>| {
        arr(
            false,
            &[
                vec![0x00],
                vec![0xf6],
                cat(&[&[0xa1, 0x13], &p]),
                vec![0xf6],
            ],
        )
    };
    let mk = |p: Vec<u8>| {
        body(&body_with_proposal(proposal(
            action(p),
            anchor(false),
            false,
        )))
    };
    // indefinite, two elements: accepted
    let good = tx(&mk(prices(true, 2)), &[0xa0]);
    decode_transaction(6, &good).expect("indefinite rational pair");
    decode_block(&block(&mk(prices(true, 2)), &[0xa0])).expect("block with indefinite rational");
    // wrong element count stays refused in both forms
    for (indef, n) in [(true, 3), (true, 1), (false, 3), (false, 1)] {
        assert!(
            decode_transaction(6, &tx(&mk(prices(indef, n)), &[0xa0])).is_err(),
            "indef={indef} n={n}"
        );
    }
}

#[test]
fn gov_action_id_voting_procedure_are_definite_only() {
    // voting_procedures {voter => {gov_action_id => voting_procedure}}
    let voter = arr(false, &[vec![0x00], bstr(&[0x66; 28])]);
    let vp = |indef: bool| arr(indef, &[vec![0x00], vec![0xf6]]);
    let vps = |gid_indef: bool, vp_indef: bool| {
        cat(&[
            &[0xa1],
            &voter,
            &[0xa1],
            &gov_action_id(gid_indef),
            &vp(vp_indef),
        ])
    };
    let mk = |g: bool, v: bool| body(&[(0x13, vps(g, v))]);
    check(
        "gov_action_id",
        (mk(false, false), vec![0xa0]),
        (mk(true, false), vec![0xa0]),
        true,
    );
    check(
        "voting_procedure",
        (mk(false, false), vec![0xa0]),
        (mk(false, true), vec![0xa0]),
        true,
    );
    // the voting procedure from the review: `9f 00 f6 ff`
    assert_eq!(vp(true), vec![0x9f, 0x00, 0xf6, 0xff]);
}

#[test]
fn proposal_procedure_constitution_anchor_are_definite_only() {
    let info = || arr(false, &[vec![0x06]]);
    check(
        "proposal_procedure",
        (
            body(&body_with_proposal(proposal(info(), anchor(false), false))),
            vec![0xa0],
        ),
        (
            body(&body_with_proposal(proposal(info(), anchor(false), true))),
            vec![0xa0],
        ),
        true,
    );
    check(
        "anchor",
        (
            body(&body_with_proposal(proposal(info(), anchor(false), false))),
            vec![0xa0],
        ),
        (
            body(&body_with_proposal(proposal(info(), anchor(true), false))),
            vec![0xa0],
        ),
        true,
    );
    // NewConstitution [5, null, [anchor, null]]
    let constitution = |indef: bool| arr(indef, &[anchor(false), vec![0xf6]]);
    let new_constitution = |indef: bool| arr(false, &[vec![0x05], vec![0xf6], constitution(indef)]);
    check(
        "constitution",
        (
            body(&body_with_proposal(proposal(
                new_constitution(false),
                anchor(false),
                false,
            ))),
            vec![0xa0],
        ),
        (
            body(&body_with_proposal(proposal(
                new_constitution(true),
                anchor(false),
                false,
            ))),
            vec![0xa0],
        ),
        true,
    );
}

/// Records that Haskell decodes with `decodeRecordNamed`/`decodeRecordSum` KEEP accepting the
/// indefinite form (the contrast to everything above).
#[test]
fn decode_record_named_records_still_accept_indefinite() {
    // TxIn `9f txid ix ff`, credential `9f 0 h ff` (certificate 0), gov action `9f 06 ff`
    let txin = arr(true, &[bstr(&[0xAA; 32]), vec![0x00]]);
    let cert = arr(
        true,
        &[vec![0x00], arr(true, &[vec![0x00], bstr(&[0x33; 28])])],
    );
    let b = cat(&[
        &[0xa4, 0x00, 0x81],
        &txin,
        &[0x01, 0x80, 0x02, 0x00, 0x04, 0x81],
        &cert,
    ]);
    decode_transaction(6, &tx(&b, &[0xa0])).expect("indefinite TxIn / certificate / credential");
    decode_block(&block(&b, &[0xa0])).expect("block");
    let info = arr(true, &[vec![0x06]]);
    let b = body(&body_with_proposal(proposal(info, anchor(false), false)));
    decode_transaction(6, &tx(&b, &[0xa0])).expect("indefinite gov action");
}

/// The Praos header (`HeaderRaw`) and header body (`HeaderBody`) are `RecD` too.
#[test]
fn praos_header_and_header_body_are_definite_only() {
    let full = load_vector("conway");
    let mut d = minicbor::Decoder::new(&full);
    d.array().unwrap();
    let era = d.u64().unwrap();
    d.array().unwrap();
    let hs = d.position();
    d.skip().unwrap();
    let header = full[hs..d.position()].to_vec();
    // header = 82 <body> <kes sig>
    assert_eq!(header[0], 0x82);
    let mut hd = minicbor::Decoder::new(&header);
    hd.array().unwrap();
    let body_start = hd.position();
    hd.skip().unwrap();
    let body_end = hd.position();
    let header_body = header[body_start..body_end].to_vec();
    let sig = header[body_end..].to_vec();
    assert_eq!(header_body[0], 0x8a);

    let indef_header = cat(&[&[0x9f], &header_body, &sig, &[0xff]]);
    let indef_body = cat(&[&[0x9f], &header_body[1..], &[0xff]]);
    let indef_both_body_header = cat(&[&[0x82], &indef_body, &sig]);

    let wrap = |h: &[u8]| {
        // [era_tag, tag24(bytes(header))]
        let mut inner = vec![0xd8, 0x18, 0x59];
        inner.extend((h.len() as u16).to_be_bytes());
        inner.extend_from_slice(h);
        cat(&[&[0x82, era as u8], &inner])
    };
    decode_wrapped_block_header(&wrap(&header)).expect("definite header");
    assert!(
        decode_wrapped_block_header(&wrap(&indef_header)).is_err(),
        "indefinite header array"
    );
    assert!(
        decode_wrapped_block_header(&wrap(&indef_both_body_header)).is_err(),
        "indefinite header body"
    );

    // and inside a full block
    let mk_block = |h: &[u8]| {
        cat(&[
            &[0x82, era as u8, 0x85],
            h,
            &[0x81],
            &body(&[]),
            &[0x81, 0xa0, 0xa0, 0x80],
        ])
    };
    decode_block(&mk_block(&header)).expect("definite header block");
    assert!(decode_block(&mk_block(&indef_header)).is_err());
    assert!(decode_block_minimal(&mk_block(&indef_header)).is_err());
    assert!(decode_block(&mk_block(&indef_both_body_header)).is_err());
}

/// A Dijkstra header carries an optional 11th header-body element; only the Dijkstra era
/// dispatch accepts it, a Conway one must not.
#[test]
fn dijkstra_header_body_with_prev_nonce_decodes_only_as_dijkstra() {
    let full = load_vector("conway");
    let mut d = minicbor::Decoder::new(&full);
    d.array().unwrap();
    d.u64().unwrap();
    d.array().unwrap();
    let hs = d.position();
    d.skip().unwrap();
    let header = full[hs..d.position()].to_vec();
    let mut hd = minicbor::Decoder::new(&header);
    hd.array().unwrap();
    let body_start = hd.position();
    hd.skip().unwrap();
    let body_end = hd.position();
    let header_body = &header[body_start..body_end];
    let sig = &header[body_end..];
    // header body with a null prevNonce appended: array(11)
    let eleven = cat(&[&[0x8b], &header_body[1..], &[0xf6]]);
    let h = cat(&[&[0x82], &eleven, sig]);
    let wrap = |era: u8| {
        let mut inner = vec![0xd8, 0x18, 0x59];
        inner.extend((h.len() as u16).to_be_bytes());
        inner.extend_from_slice(&h);
        cat(&[&[0x82, era], &inner])
    };
    let dij = decode_wrapped_block_header(&wrap(8)).expect("Dijkstra 11-element header body");
    assert!(dij.prev_nonce.is_none());
    assert!(
        decode_wrapped_block_header(&wrap(7)).is_err(),
        "Conway must refuse 11 elements"
    );
    let _ = Era::Dijkstra;
}
