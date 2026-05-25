//! `QueryService` — read-only ledger queries.
//!
//! M2 (this commit) implements:
//!
//! * `ReadParams` — `LedgerContext::params_at_tip` + `pparams_to_proto`
//!   + `tip` for the ledger_tip ChainPoint.
//! * `ReadUtxos` — `utxo_by_ref` for each requested `TxoRef`. Returns
//!   the parsed Cardano `TxOutput` + a `ChainPoint` ledger tip.
//! * `ReadGenesis` — `LedgerContext::genesis` minimum-viable envelope
//!   with network_magic / start_time / security_param. Full Genesis
//!   message (Byron + Shelley + Alonzo + Conway fields) lands in M2.B
//!   alongside the full mapping modules.
//! * `ReadEraSummary` — projects `EraHistoryView` into the proto
//!   EraSummaries shape.
//!
//! `SearchUtxos`, `ReadData`, `ReadTx`, `ReadState` remain
//! `UNIMPLEMENTED` until M2.B fills the remaining mapping modules.

use tonic::{Request, Response, Status};

use super::ServiceState;
use crate::context::LedgerContext;
use crate::map::block::block_ref_from_tip;
use crate::map::pparams::pparams_to_proto;
use crate::map::tx::tx_output_to_proto;
use crate::proto::{v1alpha, v1beta};

const UNIMPL: &str = "M2.B — full mapping required";

/// Hard cap on UTxOs returned per SearchUtxos request — protects the
/// node from runaway scans when an unindexed pattern matches a large
/// address set.
const SEARCH_UTXOS_HARD_CAP: usize = 5_000;

/// Extract an exact-address match from a v1beta SearchUtxos predicate
/// if and only if the predicate boils down to a single
/// `match.cardano.address.exact_address` lookup. Anything more complex
/// (asset patterns, `payment_part` / `delegation_part`, `not` / `all_of`
/// / `any_of` composition) yields `None`, and the service surfaces
/// UNIMPLEMENTED so clients see a clear signal instead of silently
/// truncated results.
fn predicate_exact_address(p: Option<&v1beta::query::UtxoPredicate>) -> Option<Vec<u8>> {
    let p = p?;
    if !p.not.is_empty() || !p.all_of.is_empty() || !p.any_of.is_empty() {
        return None;
    }
    let any = p.r#match.as_ref()?;
    let utxo_pattern = any.utxo_pattern.as_ref()?;
    let v1beta::query::any_utxo_pattern::UtxoPattern::Cardano(out_pattern) = utxo_pattern;
    if out_pattern.asset.is_some() {
        return None;
    }
    let addr = out_pattern.address.as_ref()?;
    if addr.payment_part.is_some() || addr.delegation_part.is_some() {
        return None;
    }
    addr.exact_address.clone()
}

async fn search_utxos_response_beta(
    ctx: &std::sync::Arc<dyn LedgerContext>,
    request: v1beta::query::SearchUtxosRequest,
) -> Result<v1beta::query::SearchUtxosResponse, Status> {
    let Some(exact_addr_bytes) = predicate_exact_address(request.predicate.as_ref()) else {
        return Err(Status::unimplemented(
            "SearchUtxos: only `predicate.match.cardano.address.exact_address` is \
             supported in this build. Payment-credential / delegation / asset / \
             composite (not/all_of/any_of) patterns are deferred to a follow-up.",
        ));
    };

    let parsed_addr = dugite_primitives::address::Address::from_bytes(&exact_addr_bytes)
        .map_err(|e| Status::invalid_argument(format!("exact_address bytes invalid: {e}")))?;

    let mut snaps = ctx
        .utxos_by_address(&parsed_addr)
        .await
        .map_err(Status::from)?;
    let cap = request
        .max_items
        .map(|n| (n as usize).min(SEARCH_UTXOS_HARD_CAP))
        .unwrap_or(SEARCH_UTXOS_HARD_CAP)
        .max(1);
    if snaps.len() > cap {
        snaps.truncate(cap);
    }

    let ledger_tip = ledger_tip_chain_point(ctx).await?;
    let items: Vec<v1beta::query::AnyUtxoData> = snaps
        .iter()
        .map(|snap| {
            let tx_output = tx_output_to_proto(&snap.output);
            v1beta::query::AnyUtxoData {
                native_bytes: snap.output.raw_cbor.clone().unwrap_or_default(),
                txo_ref: Some(v1beta::query::TxoRef {
                    hash: snap.ref_.transaction_id.as_ref().to_vec(),
                    index: snap.ref_.index,
                }),
                parsed_state: Some(v1beta::query::any_utxo_data::ParsedState::Cardano(
                    tx_output,
                )),
                block_ref: None,
            }
        })
        .collect();

    Ok(v1beta::query::SearchUtxosResponse {
        items,
        ledger_tip: Some(ledger_tip),
        // Pagination (next_token) is M3-of-Search; the hard cap above
        // bounds memory for the single-page response.
        next_token: None,
    })
}

// ─── shared helpers ──────────────────────────────────────────────────────

async fn ledger_tip_chain_point(
    ctx: &std::sync::Arc<dyn LedgerContext>,
) -> Result<v1beta::query::ChainPoint, Status> {
    let tip = ctx.tip().await.map_err(Status::from)?;
    let r = block_ref_from_tip(&tip);
    Ok(v1beta::query::ChainPoint {
        slot: r.slot,
        hash: r.hash,
        height: r.height,
        timestamp: r.timestamp,
    })
}

async fn read_params_response_beta(
    ctx: &std::sync::Arc<dyn LedgerContext>,
) -> Result<v1beta::query::ReadParamsResponse, Status> {
    let params_view = ctx.params_at_tip().await.map_err(Status::from)?;
    let cardano = pparams_to_proto(&params_view.params);
    let ledger_tip = ledger_tip_chain_point(ctx).await?;
    Ok(v1beta::query::ReadParamsResponse {
        values: Some(v1beta::query::AnyChainParams {
            params: Some(v1beta::query::any_chain_params::Params::Cardano(cardano)),
        }),
        ledger_tip: Some(ledger_tip),
    })
}

async fn read_utxos_response_beta(
    ctx: &std::sync::Arc<dyn LedgerContext>,
    refs: Vec<v1beta::query::TxoRef>,
) -> Result<v1beta::query::ReadUtxosResponse, Status> {
    // Map TxoRef → TransactionInput list.
    let mut inputs = Vec::with_capacity(refs.len());
    for r in &refs {
        if r.hash.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&r.hash);
            inputs.push(dugite_primitives::transaction::TransactionInput {
                transaction_id: dugite_primitives::hash::Hash32::from_bytes(arr),
                index: r.index,
            });
        }
    }

    let snapshots = ctx.utxo_by_ref(&inputs).await.map_err(Status::from)?;
    let ledger_tip = ledger_tip_chain_point(ctx).await?;

    let items: Vec<v1beta::query::AnyUtxoData> = snapshots
        .iter()
        .map(|snap| {
            let tx_output = tx_output_to_proto(&snap.output);
            v1beta::query::AnyUtxoData {
                native_bytes: snap.output.raw_cbor.clone().unwrap_or_default(),
                txo_ref: Some(v1beta::query::TxoRef {
                    hash: snap.ref_.transaction_id.as_ref().to_vec(),
                    index: snap.ref_.index,
                }),
                parsed_state: Some(v1beta::query::any_utxo_data::ParsedState::Cardano(
                    tx_output,
                )),
                block_ref: snap.slot.map(|s| v1beta::query::ChainPoint {
                    slot: s,
                    hash: Vec::new(),
                    height: 0,
                    timestamp: 0,
                }),
            }
        })
        .collect();

    Ok(v1beta::query::ReadUtxosResponse {
        items,
        ledger_tip: Some(ledger_tip),
    })
}

async fn read_genesis_response_beta(
    ctx: &std::sync::Arc<dyn LedgerContext>,
) -> Result<v1beta::query::ReadGenesisResponse, Status> {
    let view = ctx.genesis().await.map_err(Status::from)?;
    let cardano = v1beta::cardano::Genesis {
        network_magic: view.network_magic,
        system_start: if view.system_start_unix > 0 {
            view.system_start_unix.to_string()
        } else {
            String::new()
        },
        security_param: view.security_param,
        ..Default::default()
    };
    Ok(v1beta::query::ReadGenesisResponse {
        genesis: Vec::new(),
        caip2: String::new(),
        config: Some(v1beta::query::read_genesis_response::Config::Cardano(
            cardano,
        )),
    })
}

async fn read_era_summary_response_beta(
    ctx: &std::sync::Arc<dyn LedgerContext>,
) -> Result<v1beta::query::ReadEraSummaryResponse, Status> {
    let view = ctx.era_history().await.map_err(Status::from)?;
    let summaries: Vec<v1beta::cardano::EraSummary> = view
        .summaries
        .iter()
        .map(|s| v1beta::cardano::EraSummary {
            name: format!("{:?}", s.era).to_lowercase(),
            start: Some(v1beta::cardano::EraBoundary {
                time: 0,
                slot: s.first_slot,
                epoch: 0,
            }),
            end: None,
            protocol_params: None,
        })
        .collect();
    Ok(v1beta::query::ReadEraSummaryResponse {
        summary: Some(v1beta::query::read_era_summary_response::Summary::Cardano(
            v1beta::cardano::EraSummaries { summaries },
        )),
    })
}

// ─── v1alpha-specific recoders ───────────────────────────────────────────

fn recode_pparams_to_alpha(beta: v1beta::cardano::PParams) -> v1alpha::cardano::PParams {
    use prost::Message;
    let bytes = beta.encode_to_vec();
    v1alpha::cardano::PParams::decode(bytes.as_slice())
        .expect("v1alpha PParams subset-compatible with v1beta")
}

fn recode_tx_output_to_alpha(beta: v1beta::cardano::TxOutput) -> v1alpha::cardano::TxOutput {
    use prost::Message;
    let bytes = beta.encode_to_vec();
    v1alpha::cardano::TxOutput::decode(bytes.as_slice())
        .expect("v1alpha TxOutput subset-compatible with v1beta")
}

fn beta_chain_point_to_alpha(b: v1beta::query::ChainPoint) -> v1alpha::query::ChainPoint {
    v1alpha::query::ChainPoint {
        slot: b.slot,
        hash: b.hash,
        height: b.height,
        timestamp: b.timestamp,
    }
}

// ─── v1alpha ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct QuerySvcAlpha {
    state: ServiceState,
}

impl QuerySvcAlpha {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1alpha::query::query_service_server::QueryService for QuerySvcAlpha {
    async fn read_params(
        &self,
        _request: Request<v1alpha::query::ReadParamsRequest>,
    ) -> Result<Response<v1alpha::query::ReadParamsResponse>, Status> {
        let beta = read_params_response_beta(&self.state.context).await?;
        let cardano = beta
            .values
            .and_then(|v| v.params)
            .map(|p| match p {
                v1beta::query::any_chain_params::Params::Cardano(c) => c,
            })
            .map(recode_pparams_to_alpha);
        Ok(Response::new(v1alpha::query::ReadParamsResponse {
            values: Some(v1alpha::query::AnyChainParams {
                params: cardano.map(v1alpha::query::any_chain_params::Params::Cardano),
            }),
            ledger_tip: beta.ledger_tip.map(beta_chain_point_to_alpha),
        }))
    }

    async fn read_utxos(
        &self,
        request: Request<v1alpha::query::ReadUtxosRequest>,
    ) -> Result<Response<v1alpha::query::ReadUtxosResponse>, Status> {
        let beta_refs: Vec<v1beta::query::TxoRef> = request
            .into_inner()
            .keys
            .into_iter()
            .map(|r| v1beta::query::TxoRef {
                hash: r.hash,
                index: r.index,
            })
            .collect();
        let beta = read_utxos_response_beta(&self.state.context, beta_refs).await?;
        let items = beta
            .items
            .into_iter()
            .map(|d| v1alpha::query::AnyUtxoData {
                native_bytes: d.native_bytes,
                txo_ref: d.txo_ref.map(|r| v1alpha::query::TxoRef {
                    hash: r.hash,
                    index: r.index,
                }),
                parsed_state: d.parsed_state.map(|p| match p {
                    v1beta::query::any_utxo_data::ParsedState::Cardano(c) => {
                        v1alpha::query::any_utxo_data::ParsedState::Cardano(
                            recode_tx_output_to_alpha(c),
                        )
                    }
                }),
                block_ref: d.block_ref.map(beta_chain_point_to_alpha),
            })
            .collect();
        Ok(Response::new(v1alpha::query::ReadUtxosResponse {
            items,
            ledger_tip: beta.ledger_tip.map(beta_chain_point_to_alpha),
        }))
    }

    async fn search_utxos(
        &self,
        request: Request<v1alpha::query::SearchUtxosRequest>,
    ) -> Result<Response<v1alpha::query::SearchUtxosResponse>, Status> {
        // Recode the v1alpha request → v1beta to share the matcher, then
        // recode the response back. v1beta is a superset for our use
        // case at v0.19.2.
        use prost::Message;
        let req = request.into_inner();
        let bytes = req.encode_to_vec();
        let beta_req = v1beta::query::SearchUtxosRequest::decode(bytes.as_slice())
            .expect("v1alpha SearchUtxosRequest subset-compatible with v1beta");
        let beta_resp = search_utxos_response_beta(&self.state.context, beta_req).await?;
        let resp_bytes = beta_resp.encode_to_vec();
        let alpha_resp = v1alpha::query::SearchUtxosResponse::decode(resp_bytes.as_slice())
            .expect("v1alpha SearchUtxosResponse subset-compatible with v1beta");
        Ok(Response::new(alpha_resp))
    }

    async fn read_data(
        &self,
        _request: Request<v1alpha::query::ReadDataRequest>,
    ) -> Result<Response<v1alpha::query::ReadDataResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_tx(
        &self,
        _request: Request<v1alpha::query::ReadTxRequest>,
    ) -> Result<Response<v1alpha::query::ReadTxResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_genesis(
        &self,
        _request: Request<v1alpha::query::ReadGenesisRequest>,
    ) -> Result<Response<v1alpha::query::ReadGenesisResponse>, Status> {
        let beta = read_genesis_response_beta(&self.state.context).await?;
        let cardano_alpha = beta
            .config
            .map(|c| match c {
                v1beta::query::read_genesis_response::Config::Cardano(g) => g,
            })
            .map(|g| {
                use prost::Message;
                let bytes = g.encode_to_vec();
                v1alpha::cardano::Genesis::decode(bytes.as_slice())
                    .expect("v1alpha Genesis subset of v1beta for M2.A fields")
            });
        Ok(Response::new(v1alpha::query::ReadGenesisResponse {
            genesis: beta.genesis,
            caip2: beta.caip2,
            config: cardano_alpha.map(v1alpha::query::read_genesis_response::Config::Cardano),
        }))
    }

    async fn read_era_summary(
        &self,
        _request: Request<v1alpha::query::ReadEraSummaryRequest>,
    ) -> Result<Response<v1alpha::query::ReadEraSummaryResponse>, Status> {
        let beta = read_era_summary_response_beta(&self.state.context).await?;
        let alpha = beta
            .summary
            .map(|s| match s {
                v1beta::query::read_era_summary_response::Summary::Cardano(s) => s,
            })
            .map(|s| {
                use prost::Message;
                let bytes = s.encode_to_vec();
                v1alpha::cardano::EraSummaries::decode(bytes.as_slice())
                    .expect("v1alpha EraSummaries subset of v1beta")
            });
        Ok(Response::new(v1alpha::query::ReadEraSummaryResponse {
            summary: alpha.map(v1alpha::query::read_era_summary_response::Summary::Cardano),
        }))
    }
}

// ─── v1beta ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct QuerySvcBeta {
    state: ServiceState,
}

impl QuerySvcBeta {
    pub fn new(state: ServiceState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl v1beta::query::query_service_server::QueryService for QuerySvcBeta {
    async fn read_params(
        &self,
        _request: Request<v1beta::query::ReadParamsRequest>,
    ) -> Result<Response<v1beta::query::ReadParamsResponse>, Status> {
        Ok(Response::new(
            read_params_response_beta(&self.state.context).await?,
        ))
    }

    async fn read_utxos(
        &self,
        request: Request<v1beta::query::ReadUtxosRequest>,
    ) -> Result<Response<v1beta::query::ReadUtxosResponse>, Status> {
        Ok(Response::new(
            read_utxos_response_beta(&self.state.context, request.into_inner().keys).await?,
        ))
    }

    async fn search_utxos(
        &self,
        request: Request<v1beta::query::SearchUtxosRequest>,
    ) -> Result<Response<v1beta::query::SearchUtxosResponse>, Status> {
        Ok(Response::new(
            search_utxos_response_beta(&self.state.context, request.into_inner()).await?,
        ))
    }

    async fn read_data(
        &self,
        _request: Request<v1beta::query::ReadDataRequest>,
    ) -> Result<Response<v1beta::query::ReadDataResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_tx(
        &self,
        _request: Request<v1beta::query::ReadTxRequest>,
    ) -> Result<Response<v1beta::query::ReadTxResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }

    async fn read_genesis(
        &self,
        _request: Request<v1beta::query::ReadGenesisRequest>,
    ) -> Result<Response<v1beta::query::ReadGenesisResponse>, Status> {
        Ok(Response::new(
            read_genesis_response_beta(&self.state.context).await?,
        ))
    }

    async fn read_era_summary(
        &self,
        _request: Request<v1beta::query::ReadEraSummaryRequest>,
    ) -> Result<Response<v1beta::query::ReadEraSummaryResponse>, Status> {
        Ok(Response::new(
            read_era_summary_response_beta(&self.state.context).await?,
        ))
    }

    /// `v1beta`-only — ad-hoc CBOR-shaped state queries.
    async fn read_state(
        &self,
        _request: Request<v1beta::query::ReadStateRequest>,
    ) -> Result<Response<v1beta::query::ReadStateResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
    }
}
