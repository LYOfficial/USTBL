use strum_macros::Display;

#[derive(Debug, Display)]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
pub enum MCPError {
  ToolNeedsConfirmation,
}

impl std::error::Error for MCPError {}
