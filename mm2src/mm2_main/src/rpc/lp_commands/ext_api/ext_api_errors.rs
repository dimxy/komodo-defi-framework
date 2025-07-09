//! Errors when accessing external trading providers

use crate::lp_swap::CheckBalanceError;
use crate::lr_swap::lr_errors::LrSwapError;
use coins::{CoinFindError, NumConversError, TradePreimageError, UnexpectedDerivationMethod};
use common::{HttpStatusCode, StatusCode};
use ethereum_types::U256;
use mm2_number::BigDecimal;
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
    #[display(fmt = "Internal error: no token info in params for liquidity routing")]
    NoLrTokenInfo,
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
    #[display(fmt = "best liquidity routing swap not found, candidates: {}", candidates)]
    BestLrSwapNotFound {
        candidates: u32,
    },
    #[display(
        fmt = "Not enough {} for swap: available {}, required at least {}, locked by swaps {:?}",
        coin,
        available,
        required,
        locked_by_swaps
    )]
    NotSufficientBalance {
        coin: String,
        available: BigDecimal,
        required: BigDecimal,
        locked_by_swaps: Option<BigDecimal>,
    },
    #[display(
        fmt = "The volume {} of the {} coin less than minimum transaction amount {}",
        volume,
        coin,
        threshold
    )]
    VolumeTooLow {
        coin: String,
        volume: BigDecimal,
        threshold: BigDecimal,
    },
    #[display(fmt = "Transport error {}", _0)]
    TransportError(String),
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
            | ExtApiRpcError::NoLrTokenInfo
            | ExtApiRpcError::InvalidParam(_)
            | ExtApiRpcError::OutOfBounds { .. }
            | ExtApiRpcError::OneInchAllowanceNotEnough { .. }
            | ExtApiRpcError::ConversionError(_)
            | ExtApiRpcError::NotSufficientBalance { .. }
            | ExtApiRpcError::VolumeTooLow { .. } => StatusCode::BAD_REQUEST,
            ExtApiRpcError::OneInchError(_)
            | ExtApiRpcError::BestLrSwapNotFound { .. }
            | ExtApiRpcError::OneInchDataError(_)
            | ExtApiRpcError::TransactionError(_)
            | ExtApiRpcError::TransportError(_) => StatusCode::BAD_GATEWAY,
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
        match err {
            CheckBalanceError::NotSufficientBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            } => Self::NotSufficientBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            },
            CheckBalanceError::NotSufficientBaseCoinBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            } => Self::NotSufficientBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            },
            CheckBalanceError::VolumeTooLow {
                coin,
                volume,
                threshold,
            } => Self::VolumeTooLow {
                coin,
                volume,
                threshold,
            },
            CheckBalanceError::Transport(nested_err) => Self::TransportError(nested_err),
            CheckBalanceError::InternalError(nested_err) => Self::InternalError(nested_err),
        }
    }
}

impl From<UnexpectedDerivationMethod> for ExtApiRpcError {
    fn from(err: UnexpectedDerivationMethod) -> Self { Self::MyAddressError(err.to_string()) }
}

impl From<NumConversError> for ExtApiRpcError {
    fn from(err: NumConversError) -> Self { Self::ConversionError(err.to_string()) }
}

impl From<TradePreimageError> for ExtApiRpcError {
    fn from(err: TradePreimageError) -> Self {
        match err {
            TradePreimageError::AmountIsTooSmall { amount, threshold } => Self::VolumeTooLow {
                coin: "".to_owned(),
                volume: amount,
                threshold,
            },
            TradePreimageError::NotSufficientBalance {
                coin,
                available,
                required,
            } => Self::NotSufficientBalance {
                coin,
                available,
                required,
                locked_by_swaps: Default::default(),
            },
            TradePreimageError::Transport(nested_err) => Self::TransportError(nested_err),
            TradePreimageError::InternalError(nested_err) => Self::InternalError(nested_err),
            TradePreimageError::NftProtocolNotSupported => Self::NftProtocolNotSupported,
            TradePreimageError::NoSuchCoin { coin } => Self::NoSuchCoin { coin },
        }
    }
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
            LrSwapError::BestLrSwapNotFound { candidates } => ExtApiRpcError::BestLrSwapNotFound { candidates },
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
            LrSwapError::NotSufficientBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            } => ExtApiRpcError::NotSufficientBalance {
                coin,
                available,
                required,
                locked_by_swaps,
            },
            LrSwapError::VolumeTooLow {
                coin,
                volume,
                threshold,
            } => ExtApiRpcError::VolumeTooLow {
                coin,
                volume,
                threshold,
            },
            LrSwapError::TransportError(nested_err) => ExtApiRpcError::TransportError(nested_err),
        }
    }
}
