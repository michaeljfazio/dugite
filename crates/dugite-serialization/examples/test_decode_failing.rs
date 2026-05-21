// Probe layers of the failing preprod PV11 block decode to localize the bug.
use minicbor::data::Type;
use minicbor::decode::Decoder;

fn main() {
    let data = std::fs::read("/tmp/preprod-block.cbor").unwrap();
    println!("bytes: {}", data.len());

    // Full decode (expected to fail).
    match dugite_serialization::decode_block_with_byron_epoch_length(&data, 21600) {
        Ok(blk) => println!(
            "decode_block OK: slot={} block_no={}",
            blk.slot().0,
            blk.block_number().0
        ),
        Err(e) => println!("decode_block ERR: {e}"),
    }

    // Manual walk to pinpoint failure position.
    let mut d = Decoder::new(&data);
    println!("--- manual walk ---");
    println!("pos={} type={:?}", d.position(), d.datatype().unwrap());
    let n = d.array().unwrap();
    println!("envelope array len={n:?}");
    let era = d.u64().unwrap();
    println!("era={era}");
    let block_n = d.array().unwrap();
    println!("block array len={block_n:?} pos={}", d.position());
    // Now header
    let hdr_n = d.array().unwrap();
    println!("header array len={hdr_n:?} pos={}", d.position());
    let hb_n = d.array().unwrap();
    println!("header_body array len={hb_n:?} pos={}", d.position());
    // Walk header_body fields
    for i in 0..hb_n.unwrap() {
        let pos = d.position();
        let ty = d.datatype().unwrap();
        match ty {
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                let v = d.u64().unwrap();
                println!("  field {i} @ {pos}: u64={v}");
            }
            Type::Bytes => {
                let b = d.bytes().unwrap();
                println!("  field {i} @ {pos}: bytes(len={})", b.len());
            }
            Type::Array | Type::ArrayIndef => {
                let n = d.array().unwrap();
                println!("  field {i} @ {pos}: array(len={n:?}) — recursing");
                if let Some(nn) = n {
                    for j in 0..nn {
                        let p = d.position();
                        let t = d.datatype().unwrap();
                        match t {
                            Type::Bytes => {
                                let b = d.bytes().unwrap();
                                println!("    sub {j} @ {p}: bytes(len={})", b.len());
                            }
                            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                                let v = d.u64().unwrap();
                                println!("    sub {j} @ {p}: u64={v}");
                            }
                            other => {
                                println!("    sub {j} @ {p}: type {other:?} — STOPPING");
                                return;
                            }
                        }
                    }
                }
            }
            Type::Null => {
                d.null().unwrap();
                println!("  field {i} @ {pos}: null");
            }
            other => {
                println!("  field {i} @ {pos}: UNEXPECTED type {other:?} — STOPPING");
                return;
            }
        }
    }
    // After header_body, kes_sig
    let pos = d.position();
    let ty = d.datatype().unwrap();
    println!("after header_body: pos={pos} type={ty:?}");
    if ty == Type::Bytes {
        let b = d.bytes().unwrap();
        println!("kes_sig: bytes(len={})", b.len());
    } else if ty == Type::Array || ty == Type::ArrayIndef {
        let n = d.array().unwrap();
        println!("UNEXPECTED: kes_sig is array(len={n:?})");
        if let Some(nn) = n {
            for j in 0..nn {
                let p = d.position();
                let t = d.datatype().unwrap();
                match t {
                    Type::Bytes => {
                        let b = d.bytes().unwrap();
                        println!("  kes-sub {j} @ {p}: bytes(len={})", b.len());
                    }
                    Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                        let v = d.u64().unwrap();
                        println!("  kes-sub {j} @ {p}: u64={v}");
                    }
                    other => {
                        println!("  kes-sub {j} @ {p}: type {other:?}");
                        return;
                    }
                }
            }
        }
    }
}
