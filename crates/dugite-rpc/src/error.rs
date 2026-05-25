//! RPC-layer error type + conversion to `tonic::Status`.
//!
//! Service impls return `Result<_, RpcError>` and the outer tower middleware
//! (or a manual `.map_err(Into::into)`) converts to the gRPC status code that
//! the wire actually carries. Centralising this means service code never
//! constructs `tonic::Status` directly — it just classifies the failure.

use thiserror::Error;
use tonic::Status;

/// Failures the RPC layer can report to a client.
///
/// Variant choice mirrors the gRPC status taxonomy
/// (<https://grpc.io/docs/guides/status-codes/>) so [`From`] is mechanical.
/// Service code should reach for the most specific variant — defaulting to
/// `Internal` silently masks real bugs.
#[derive(Debug, Error)]
pub enum RpcError {
    /// The client supplied a malformed or contradictory request payload
    /// (e.g. a `BlockRef` with neither `slot` nor `hash`, an unparseable
    /// CBOR pattern, a `FieldMask` referencing an unknown path).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// The requested resource does not exist (block hash not in
    /// ChainDB, tx hash not in mempool when `WaitForTx` was asked to fail
    /// fast, etc.).
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation is not implemented in this milestone — used by every
    /// service stub during M1.A until later milestones land.
    #[error("unimplemented: {0}")]
    Unimplemented(&'static str),

    /// The client (or some shared resource) is over a configured limit:
    /// a slow streaming client whose per-stream buffer overflowed, an
    /// asset-pattern scan that exceeded the safety cap, etc.
    #[error("resource exhausted: {0}")]
    ResourceExhausted(String),

    /// Transaction submission was rejected by ledger validation. The
    /// payload is the structured `TxValidationError` message rendered
    /// to a string so cardano-node per-rule semantics survive the
    /// trip across the gRPC boundary.
    #[error("transaction rejected: {0}")]
    TxRejected(String),

    /// The request was cancelled by the client (stream dropped, deadline
    /// expired) — surfaced only when the service can distinguish
    /// cancellation from completion.
    #[error("cancelled")]
    Cancelled,

    /// Catch-all for unexpected node-internal failures. Service code
    /// should NOT construct this for known-rejection scenarios — prefer
    /// a specific variant so clients can react.
    #[error("internal: {0}")]
    Internal(String),
}

impl From<RpcError> for Status {
    fn from(err: RpcError) -> Self {
        match err {
            RpcError::InvalidArgument(msg) => Status::invalid_argument(msg),
            RpcError::NotFound(msg) => Status::not_found(msg),
            RpcError::Unimplemented(what) => {
                Status::unimplemented(format!("{what} is not implemented in this build"))
            }
            RpcError::ResourceExhausted(msg) => Status::resource_exhausted(msg),
            RpcError::TxRejected(msg) => {
                // gRPC has no "ledger validation failed" code; FAILED_PRECONDITION
                // best matches the semantic ("a logical precondition for the call
                // — the tx must validate — was not met"). Clients reading the
                // message get the structured rejection reason verbatim.
                Status::failed_precondition(msg)
            }
            RpcError::Cancelled => Status::cancelled("client cancelled"),
            RpcError::Internal(msg) => Status::internal(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    #[test]
    fn status_codes_round_trip() {
        let cases = [
            (RpcError::InvalidArgument("x".into()), Code::InvalidArgument),
            (RpcError::NotFound("x".into()), Code::NotFound),
            (RpcError::Unimplemented("EvalTx"), Code::Unimplemented),
            (
                RpcError::ResourceExhausted("x".into()),
                Code::ResourceExhausted,
            ),
            (RpcError::TxRejected("x".into()), Code::FailedPrecondition),
            (RpcError::Cancelled, Code::Cancelled),
            (RpcError::Internal("x".into()), Code::Internal),
        ];
        for (err, expected) in cases {
            let status: Status = err.into();
            assert_eq!(status.code(), expected);
        }
    }
}
