#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    Usage = 2,
    BackendUnavailable = 3,
    NotFound = 4,
    ContractViolated = 5,
    LockContention = 6,
    AuthFailed = 7,
    Interrupted = 8,
}

impl From<ExitCode> for i32 {
    fn from(val: ExitCode) -> Self {
        val as i32
    }
}
