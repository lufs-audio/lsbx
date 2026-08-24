use crate::error::LsbxError;

#[derive(serde::Serialize)]
#[serde(tag = "status")]
pub enum Envelope<T: serde::Serialize> {
    #[serde(rename = "success")]
    Success { data: T },
    #[serde(rename = "error")]
    Error { code: i32, message: String },
}

impl<T: serde::Serialize> Envelope<T> {
    pub fn from_result(r: Result<T, LsbxError>) -> Self {
        match r {
            Ok(data) => Self::Success { data },
            Err(e) => Self::Error {
                code: e.exit_code() as i32,
                message: e.to_string(),
            },
        }
    }
}
