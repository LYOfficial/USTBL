use serde::Serialize;
use std::error::Error;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct USTBLError(pub String);

pub type USTBLResult<T> = Result<T, USTBLError>;

impl<T> From<T> for USTBLError
where
  T: Error,
{
  fn from(err: T) -> Self {
    USTBLError(err.to_string())
  }
}
