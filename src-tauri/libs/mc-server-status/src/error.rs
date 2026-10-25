// Copyright (c) 2025 pynickle. This is a fork of Original Crate. Original copyright: Copyright (c) 2025 NameOfShadow

use thiserror::Error;

#[derive(Error, Debug)]
pub enum McError {
    #[error("DNS resolution failed: {0}")]
    DnsError(String),

    #[error("Connection failed: {0}")]
    ConnectionError(String),

    #[error("Timeout occurred")]
    Timeout,

    #[error("Invalid server response: {0}")]
    InvalidResponse(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("UTF-8 conversion error: {0}")]
    Utf8Error(#[from] std::string::FromUtf8Error),

    #[error("Base64 decoding error: {0}")]
    Base64Error(#[from] base64::DecodeError),

    #[error("Invalid edition: {0}")]
    InvalidEdition(String),

    #[error("Invalid port: {0}")]
    InvalidPort(String),

    #[error("Invalid address format: {0}")]
    InvalidAddress(String),
}
