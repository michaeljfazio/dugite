//! Capture `ConwayGovState.cgsDRepPulsingState` — the DRep pulser — from a
//! running node's N2C socket, and name its constructor.
//!
//! ```text
//! cargo run -p dugite-network --example capture_gov_pulser -- <socket> <magic>
//! ```
//!
//! # What this decides
//!
//! Phase 4 of the pulser-alignment design turns on one question the design
//! ASSERTED rather than measured: does cardano-node ever put `DRPulsing` on
//! the wire, or is `DRComplete` the only constructor a peer can observe?
//!
//! ```haskell
//! data DRepPulsingState era
//!   = DRPulsing !(DRepPulser era Identity (RatifyState era))
//!   | DRComplete !(PulsingSnapshot era) !(RatifyState era)
//! ```
//!
//! dugite emits `DRComplete` unconditionally. If cardano-node also only ever
//! emits `DRComplete` here, Phase 4 is an internal representation change with
//! no observable consequence — YAGNI, and the design says as much. If it emits
//! `DRPulsing` mid-epoch, that is a byte-level `GetGovState` divergence of
//! exactly the #1071 shape, and Phase 4 has a real justification.
//!
//! **cardano-cli cannot answer this.** `nextRatifyState` renders from tag 32,
//! not from the tag-24 embedded pulser (#992), so the JSON is identical either
//! way — the same trap that made `nesRu`'s `Pulsing` arm invisible until it was
//! read from CBOR. Hence a raw capture.
//!
//! Sample across a whole epoch: if `DRPulsing` appears at all, it appears
//! mid-epoch, and a single sample near a boundary would "confirm" the
//! convenient answer.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let socket: PathBuf = args
        .next()
        .ok_or("usage: capture_gov_pulser <socket-path> <network-magic>")?
        .into();
    let magic: u64 = args
        .next()
        .ok_or("usage: capture_gov_pulser <socket-path> <network-magic>")?
        .parse()?;

    let rt = tokio::runtime::Runtime::new()?;
    let raw = rt.block_on(async {
        let mut c = dugite_network::N2CClient::connect(&socket, magic).await?;
        c.acquire().await?;
        let r = c.query_gov_state().await;
        c.release().await.ok();
        c.done().await.ok();
        r
    })?;

    eprintln!("GetGovState payload: {} bytes", raw.len());
    match locate_pulser(&raw) {
        Some((start, end)) => {
            let slice = &raw[start..end];
            eprintln!(
                "cgsDRepPulsingState at [{start}..{end}] ({} bytes) — {}",
                end - start,
                classify(slice)
            );
            println!("{}", hex::encode(slice));
        }
        None => {
            eprintln!(
                "could not locate cgsDRepPulsingState — dumping the whole reply \
                 for offline decode rather than guessing at an offset"
            );
            println!("{}", hex::encode(&raw));
        }
    }
    Ok(())
}

/// Name the constructor from the CBOR, since the JSON cannot.
fn classify(b: &[u8]) -> String {
    match b.first() {
        // Sum encoding: array(n) then the tag.
        Some(h) if *h >= 0x80 && *h < 0x98 => match b.get(1) {
            Some(0x00) => "DRPulsing (sum tag 0) — Phase 4 HAS a wire form".into(),
            Some(0x01) => "DRComplete (sum tag 1) — matches what dugite emits".into(),
            Some(t) => format!("unrecognised sum tag {t:#04x} — decode by hand"),
            None => "truncated".into(),
        },
        Some(h) => format!("unexpected head {h:#04x} — decode by hand"),
        None => "empty".into(),
    }
}

/// `ConwayGovState` is `array(7)`; `cgsDRepPulsingState` is the LAST element.
///
/// Walking to the end rather than indexing a guessed offset: the preceding
/// elements are proposals, committee, constitution and two whole PParams
/// records, none of fixed length. Guessing here is how #1057 and #1067
/// happened.
fn locate_pulser(data: &[u8]) -> Option<(usize, usize)> {
    let mut d = minicbor::Decoder::new(data);
    // MsgResult: array(2)[4, ...]
    d.array().ok()?;
    let _tag = d.u32().ok()?;
    // HFC success wrapper: array(1)
    d.array().ok()?;
    let n = d.array().ok()??;
    eprintln!("ConwayGovState = array({n})");
    for _ in 0..n.saturating_sub(1) {
        d.skip().ok()?;
    }
    let start = d.position();
    d.skip().ok()?;
    Some((start, d.position()))
}
