//! Errors when accessing external trading providers

use crate::lp_swap::CheckBalanceError;
use crate::lr_swap::lr_errors::LrSwapError;
use coins::{CoinFindError, NumConversError, UnexpectedDerivationMethod};
use common::{HttpStatusCode, StatusCode};
use ethereum_types::U256;
use ser_error_derive::SerializeErrorType;
use serde::Serialize;
use trading_api::one_inch_api::errors::OneInchError;

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ExtApiRpcError {
    NoSuchCoin {
        coin: String,
    },
    #[display(fmt = "EVM token needed")]
    CoinTypeError,
    #[display(fmt = "NFT not supported")]
    NftProtocolNotSupported,
    #[display(fmt = "Chain not supported")]
    ChainNotSupported,
    #[display(fmt = "Must be same chain")]
    DifferentChains,
    MyAddressError(String),
    ConversionError(String),
    InvalidParam(String),
    #[display(fmt = "Parameter {param} out of bounds, value: {value}, min: {min} max: {max}")]
    OutOfBounds {
        param: String,
        value: String,
        min: String,
        max: String,
    },
    #[display(fmt = "allowance not enough for 1inch contract, available: {allowance}, needed: {amount}")]
    OneInchAllowanceNotEnough {
        allowance: U256,
        amount: U256,
    },
    #[display(fmt = "1inch API error: {}", _0)]
    OneInchError(OneInchError),
    #[display(fmt = "1inch API data parse error: {}", _0)]
    OneInchDataError(String),
    InternalError(String),
    ResponseParseError(String),
    #[display(fmt = "Transaction error {}", _0)]
    TransactionError(String),
    #[display(fmt = "Sign transaction error {}", _0)]
    SignTransactionError(String),
    #[display(fmt = "best liquidity routing swap not found")]
    BestLrSwapNotFound,
    CheckBalanceError(String),
}

impl HttpStatusCode for ExtApiRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            ExtApiRpcError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            ExtApiRpcError::CoinTypeError
            | ExtApiRpcError::NftProtocolNotSupported
            | ExtApiRpcError::ChainNotSupported
            | ExtApiRpcError::DifferentChains
            | ExtApiRpcError::MyAddressError(_)
            | ExtApiRpcError::InvalidParam(_)
            | ExtApiRpcError::OutOfBounds { .. }
            | ExtApiRpcError::OneInchAllowanceNotEnough { .. }
            | ExtApiRpcError::ConversionError(_)
            | ExtApiRpcError::CheckBalanceError(_) => StatusCode::BAD_REQUEST,
            ExtApiRpcError::OneInchError(_)
            | ExtApiRpcError::BestLrSwapNotFound
            | ExtApiRpcError::OneInchDataError(_)
            | ExtApiRpcError::TransactionError(_) => StatusCode::BAD_GATEWAY,
            ExtApiRpcError::ResponseParseError(_)
            | ExtApiRpcError::SignTransactionError(_)
            | ExtApiRpcError::InternalError { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<OneInchError> for ExtApiRpcError {
    fn from(error: OneInchError) -> Self {
        match error {
            OneInchError::InvalidParam(error) => ExtApiRpcError::InvalidParam(error),
            OneInchError::OutOfBounds { param, value, min, max } => {
                ExtApiRpcError::OutOfBounds { param, value, min, max }
            },
            OneInchError::TransportError(_)
            | OneInchError::ParseBodyError { .. }
            | OneInchError::GeneralApiError { .. } => ExtApiRpcError::OneInchError(error),
            OneInchError::AllowanceNotEnough { allowance, amount, .. } => {
                ExtApiRpcError::OneInchAllowanceNotEnough { allowance, amount }
            },
        }
    }
}

impl From<CoinFindError> for ExtApiRpcError {
    fn from(err: CoinFindError) -> Self {
        match err {
            CoinFindError::NoSuchCoin { coin } => ExtApiRpcError::NoSuchCoin { coin },
        }
    }
}

impl From<CheckBalanceError> for ExtApiRpcError {
    fn from(err: CheckBalanceError) -> Self {
        ExtApiRpcError::CheckBalanceError(err.to_string())
    }
}

impl From<UnexpectedDerivationMethod> for ExtApiRpcError {
    fn from(err: UnexpectedDerivationMethod) -> Self { Self::MyAddressError(err.to_string()) }
}

/// Error aggregator for errors of conversion of api returned values
#[derive(Debug, Display, Serialize)]
pub(crate) struct FromApiValueError(String);

impl FromApiValueError {
    pub(crate) fn new(msg: String) -> Self { Self(msg) }
}

impl From<NumConversError> for FromApiValueError {
    fn from(err: NumConversError) -> Self { Self(err.to_string()) }
}

impl From<primitive_types::Error> for FromApiValueError {
    fn from(err: primitive_types::Error) -> Self { Self(format!("{:?}", err)) }
}

impl From<hex::FromHexError> for FromApiValueError {
    fn from(err: hex::FromHexError) -> Self { Self(err.to_string()) }
}

impl From<ethereum_types::FromDecStrErr> for FromApiValueError {
    fn from(err: ethereum_types::FromDecStrErr) -> Self { Self(err.to_string()) }
}

impl From<LrSwapError> for ExtApiRpcError {
    fn from(err: LrSwapError) -> Self {
        match err {
            LrSwapError::NoSuchCoin { coin } => ExtApiRpcError::NoSuchCoin { coin },
            LrSwapError::StateError(msg) | LrSwapError::AtomicSwapError(msg) | LrSwapError::InternalError(msg) => {
                ExtApiRpcError::InternalError(msg)
            },
            LrSwapError::CoinTypeError => ExtApiRpcError::CoinTypeError,
            LrSwapError::NftProtocolNotSupported => ExtApiRpcError::NftProtocolNotSupported,
            LrSwapError::ChainNotSupported => ExtApiRpcError::ChainNotSupported,
            LrSwapError::DifferentChains => ExtApiRpcError::DifferentChains,
            LrSwapError::InvalidParam(msg) => ExtApiRpcError::InvalidParam(msg),
            LrSwapError::MyAddressError(msg) => ExtApiRpcError::MyAddressError(msg),
            LrSwapError::BestLrSwapNotFound => ExtApiRpcError::BestLrSwapNotFound,
            LrSwapError::OutOfBounds { param, value, min, max } => {
                ExtApiRpcError::OutOfBounds { param, value, min, max }
            },
            LrSwapError::OneInchAllowanceNotEnough { allowance, amount } => {
                ExtApiRpcError::OneInchAllowanceNotEnough { allowance, amount }
            },
            LrSwapError::OneInchError(msg) => ExtApiRpcError::OneInchError(msg),
            LrSwapError::ConversionError(msg) => ExtApiRpcError::ConversionError(msg),
            LrSwapError::ResponseParseError(msg) => ExtApiRpcError::ResponseParseError(msg),
            LrSwapError::TransactionError(msg) => ExtApiRpcError::TransactionError(msg),
            LrSwapError::SignTransactionError(msg) => ExtApiRpcError::SignTransactionError(msg),
            LrSwapError::CheckBalanceError(msg) => ExtApiRpcError::CheckBalanceError(msg),
        }
    }
}
