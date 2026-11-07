use carbide_uuid::machine::MachineIdParseError;
use thiserror::Error;

/// Top-level RVS error type.
#[derive(Debug, Error)]
pub enum RvsError {
    /// gRPC call to NICo failed.
    #[error("NICo RPC error: {0}")]
    Rpc(#[from] tonic::Status),

    /// Tray ID string couldn't be parsed as MachineId.
    #[error("failed to parse machine ID: {0}")]
    InvalidMachineId(#[from] MachineIdParseError),

    /// An ID string couldn't be parsed as a UUID-based type.
    #[error("failed to parse ID: {0}")]
    InvalidId(String),

    /// NICo returned an unexpected number of machines for a single-ID query.
    #[error("expected 1 machine for tray {tray_id}, got {count}")]
    UnexpectedMachineCount { tray_id: String, count: usize },

    /// A required gRPC message field was missing (would carry invalid data forward).
    #[error("{0}")]
    MissingField(&'static str),

    /// Invalid or missing command-line argument.
    #[error("invalid argument: {0}")]
    InvalidArg(String),

    /// Configuration loading failed.
    #[error("config error: {0}")]
    Config(String),

    /// I/O error (e.g. binding a TCP listener).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// An operation exceeded its deadline.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Downloaded file digest does not match the server-advertised checksum.
    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
}
