//! Read treasury and reserves from the LEDGER (`NewEpochState[3][0]`), not from
//! the Prometheus gauge.
//!
//! ```text
//! cargo run -p dugite-network --example capture_pots -- <socket> <magic>
//! ```
//!
//! A mainnet replay showed `dugite_reserves_lovelace` bit-identical across two
//! epoch boundaries while treasury grew — which is either a conservation defect
//! or a stale gauge. Those demand completely different fixes, and a metric is
//! not ledger state, so the question has to be asked of the ledger.
//!
//! `cardano-cli query ledger-state` cannot answer it at mainnet scale: the
//! reply is large enough that the CLI's JSON rendering times out. This walks
//! straight to `EpochState[0] = ChainAccountState = [treasury, reserves]` and
//! reads two integers.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let socket: PathBuf = args
        .next()
        .ok_or("usage: capture_pots <socket-path> <network-magic>")?
        .into();
    let magic: u64 = args
        .next()
        .ok_or("usage: capture_pots <socket-path> <network-magic>")?
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

    let mut d = minicbor::Decoder::new(&raw);
    d.array()?; // MsgResult array(2)
    let _tag = d.u32()?;
    d.array()?; // HFC array(1)
    let n = d.array()?.ok_or("NewEpochState not a definite array")?;
    if n != 7 {
        return Err(format!("expected NewEpochState array(7), got array({n})").into());
    }
    d.skip()?; // [0] EpochNo
    d.skip()?; // [1] BlocksMade prev
    d.skip()?; // [2] BlocksMade cur
    d.array()?; // [3] EpochState array(4)
    d.array()?; // [3][0] ChainAccountState array(2)
    let treasury = d.u64()?;
    let reserves = d.u64()?;

    println!(
        "treasury={treasury} reserves={reserves} sum={}",
        treasury + reserves
    );
    Ok(())
}
