#[derive(Debug, thiserror::Error)]
pub enum LsbxError {
    #[error("usage: {0}")]
    Usage(String),
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("contract violated: {0}")]
    ContractViolated(String),
    #[error("lock contention: {0}")]
    LockContention(String),
    #[error("auth failed: {0}")]
    AuthFailed(String),
    #[error("interrupted: {0}")]
    Interrupted(String),
}

impl LsbxError {
    pub fn exit_code(&self) -> crate::exit_code::ExitCode {
        match self {
            Self::Usage(_) => crate::exit_code::ExitCode::Usage,
            Self::BackendUnavailable(_) => crate::exit_code::ExitCode::BackendUnavailable,
            Self::NotFound(_) => crate::exit_code::ExitCode::NotFound,
            Self::ContractViolated(_) => crate::exit_code::ExitCode::ContractViolated,
            Self::LockContention(_) => crate::exit_code::ExitCode::LockContention,
            Self::AuthFailed(_) => crate::exit_code::ExitCode::AuthFailed,
            Self::Interrupted(_) => crate::exit_code::ExitCode::Interrupted,
        }
    }
}
