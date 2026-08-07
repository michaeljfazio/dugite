//! Capture `EpochState.esNonMyopic` from a running node's N2C socket.
//!
//! This is the tool that produced the byte fixture pinned by
//! `non_myopic_bytes_match_a_cardano_node_11_0_1_capture` in
//! `dugite-node`'s `n2c_query::encoding`. It exists so that "RE-CAPTURE from
//! the new node" is a command someone can run, rather than an instruction to
//! rebuild an ad-hoc socket proxy from scratch.
//!
//! ```text
//! cargo run -p dugite-network --example capture_non_myopic -- \
//!     /tmp/ld-$(id -u)/cbp.sock 42
//! ```
//!
//! IMPORTANT: run it against a node whose `likelihoodsNM` is NON-EMPTY. The map
//! is legitimately empty until the go snapshot first carries pools — about
//! epoch 3 on a fresh devnet (slot ~1200 at `epochLength=400`) — and capturing
//! before then yields `a0`, which would "confirm" the old hardcoded empty map
//! by accident. Check first with:
//!
//! ```text
//! cardano-cli query ledger-state --testnet-magic 42 \
//!   | jq '.stateBefore.esNonMyopic.likelihoodsNM | length'
//! ```
//!
//! Note that cardano-cli's JSON renders `exp(LogWeight)`, not the stored value —
//! it shows `1`, `1.4e305` and `+inf` for a field that is a `Float`. Use it to
//! decide WHEN to capture, never to check WHAT was captured.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let socket: PathBuf = args
        .next()
        .ok_or("usage: capture_non_myopic <socket-path> <network-magic>")?
        .into();
    let magic: u64 = args
        .next()
        .ok_or("usage: capture_non_myopic <socket-path> <network-magic>")?
        .parse()?;

    let rt = tokio::runtime::Runtime::new()?;
    let raw = rt.block_on(async {
        let mut client = dugite_network::N2CClient::connect(&socket, magic).await?;
        client.acquire().await?;
        let raw = client.query_debug_new_epoch_state().await;
        client.release().await.ok();
        client.done().await.ok();
        raw
    })?;

    eprintln!("MsgResult payload: {} bytes", raw.len());

    let (start, end) = locate_non_myopic(&raw).ok_or(
        "no NonMyopic found — is likelihoodsNM empty? see the module docs on \
         capturing before epoch 3",
    )?;

    eprintln!("NonMyopic at [{start}..{end}] ({} bytes)", end - start);
    println!("{}", hex::encode(&raw[start..end]));
    Ok(())
}

/// Locate the `NonMyopic` record by its unmistakable wire signature.
///
/// Rather than walk the whole `NewEpochState`, find a `Likelihood` — an
/// indefinite array (`0x9f`) of exactly 100 CBOR float32s (`0xfa` + 4 bytes)
/// closed by `0xff` — then step back over its `bstr(28)` key (`0x58 0x1c`) and
/// the enclosing `map`/`array(2)` headers.
fn locate_non_myopic(data: &[u8]) -> Option<(usize, usize)> {
    let mut first_key: Option<usize> = None;
    let mut last_end: Option<usize> = None;

    let mut i = 0usize;
    while i + 1 < data.len() {
        if data[i] == 0x9f && data[i + 1] == 0xfa {
            let mut j = i + 1;
            let mut n = 0;
            while j + 4 < data.len() && data[j] == 0xfa {
                j += 5;
                n += 1;
            }
            if n == 100 && j < data.len() && data[j] == 0xff {
                let key = i.checked_sub(30)?;
                if data[key] == 0x58 && data[key + 1] == 0x1c {
                    first_key.get_or_insert(key);
                    last_end = Some(j + 1);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }

    let first_key = first_key?;
    let mut end = last_end?;

    // `rewardPotNM` follows the map: a CBOR uint, 1..9 bytes.
    end += match data.get(end)? {
        0x00..=0x17 => 1,
        0x18 => 2,
        0x19 => 3,
        0x1a => 5,
        0x1b => 9,
        _ => return None,
    };

    // Step back over the map header (definite `0xa0..0xb7` for <=23 entries)
    // and the enclosing `array(2)` (`0x82`).
    let map_hdr = first_key.checked_sub(1)?;
    let start = if (0xa0..=0xb7).contains(&data[map_hdr]) && data[map_hdr - 1] == 0x82 {
        map_hdr - 1
    } else {
        return None;
    };

    Some((start, end))
}
