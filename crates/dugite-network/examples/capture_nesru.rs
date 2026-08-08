//! Capture `NewEpochState[4]` — `nesRu`, the pulsing reward update — from a
//! running node's N2C socket.
//!
//! ```text
//! cargo run -p dugite-network --example capture_nesru -- <socket> <magic>
//! ```
//!
//! # The window this must be run in
//!
//! `nesRu` takes three shapes over an epoch, and only one of them is visible in
//! cardano-cli's JSON:
//!
//! ```text
//! slots 0..first+4k/f      SNothing          -> array(0)     JSON: null
//! just after the mark      Pulsing s p       -> [ [0,s,p] ]  JSON: null   <-- !
//! once the fold finishes   Complete r        -> [ [1,r] ]    JSON: object
//! ```
//!
//! `instance ToJSON PulsingRewUpdate` renders `Pulsing` as `Null`
//! (`RewardUpdate.hs:359-365`), identically to `SNothing`. **So the JSON view
//! cannot tell you which state you captured.** Drive the sampling by SLOT and
//! read the verdict from the CBOR — that is exactly the trap #1067's capture
//! had before epoch 3, where an empty map would have "confirmed" a hardcoded
//! empty map.
//!
//! On the devnet (`epochLength=400`, `k=40`, `f=0.5`, `sr=320`) the useful
//! sampling points are ~slot 323 for `Pulsing` and ~slot 350+ for `Complete`.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let socket: PathBuf = args
        .next()
        .ok_or("usage: capture_nesru <socket-path> <network-magic>")?
        .into();
    let magic: u64 = args
        .next()
        .ok_or("usage: capture_nesru <socket-path> <network-magic>")?
        .parse()?;

    let rt = tokio::runtime::Runtime::new()?;
    let raw = rt.block_on(async {
        let mut c = dugite_network::N2CClient::connect(&socket, magic).await?;
        c.acquire().await?;
        let r = c.query_debug_new_epoch_state().await;
        c.release().await.ok();
        c.done().await.ok();
        r
    })?;

    eprintln!("MsgResult payload: {} bytes", raw.len());

    let (start, end) = locate_nesru(&raw).ok_or("could not locate NewEpochState[4]")?;
    let slice = &raw[start..end];
    eprintln!(
        "nesRu at [{start}..{end}] ({} bytes) — state: {}",
        end - start,
        classify(slice)
    );
    println!("{}", hex::encode(slice));
    Ok(())
}

/// Name the constructor from the CBOR, since the JSON cannot.
fn classify(b: &[u8]) -> &'static str {
    match b {
        [0x80, ..] => "SNothing (array(0))",
        // SJust wraps in array(1); the inner sum tag distinguishes the arms.
        [0x81, x, 0x00, ..] if *x >= 0x80 => "SJust Pulsing (sum tag 0)",
        [0x81, x, 0x01, ..] if *x >= 0x80 => "SJust Complete (sum tag 1)",
        [0x81, ..] => "SJust (unrecognised inner shape — inspect by hand)",
        _ => "unrecognised",
    }
}

/// `NewEpochState` is `array(7)`; `nesRu` is element [4].
///
/// Walks the first four elements rather than pattern-matching, because
/// `[1]`/`[2]` are `BlocksMade` maps and `[3]` is the whole `EpochState` —
/// none of which have a fixed byte length.
fn locate_nesru(data: &[u8]) -> Option<(usize, usize)> {
    let mut d = minicbor::Decoder::new(data);
    // MsgResult: array(2)[4, ...]
    d.array().ok()?;
    let _tag = d.u32().ok()?;
    // HFC success wrapper: array(1)
    d.array().ok()?;
    // NewEpochState = array(7)
    let n = d.array().ok()??;
    if n != 7 {
        eprintln!("expected NewEpochState array(7), got array({n})");
        return None;
    }
    for _ in 0..4 {
        d.skip().ok()?;
    }
    let start = d.position();
    d.skip().ok()?;
    Some((start, d.position()))
}
