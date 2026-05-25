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
        _request: Request<v1alpha::query::SearchUtxosRequest>,
    ) -> Result<Response<v1alpha::query::SearchUtxosResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
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
        _request: Request<v1beta::query::SearchUtxosRequest>,
    ) -> Result<Response<v1beta::query::SearchUtxosResponse>, Status> {
        Err(Status::unimplemented(UNIMPL))
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
