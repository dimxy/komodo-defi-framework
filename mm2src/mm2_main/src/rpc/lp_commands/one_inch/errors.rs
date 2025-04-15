use coins::{eth::u256_to_big_decimal, CoinFindError, NumConversError};
use common::{HttpStatusCode, StatusCode};
use enum_derives::EnumFromStringify;
use mm2_number::BigDecimal;
use ser_error_derive::SerializeErrorType;
use serde::Serialize;
use trading_api::one_inch_api::errors::ApiClientError;

#[derive(Debug, Display, Serialize, SerializeErrorType, EnumFromStringify)]
#[serde(tag = "error_type", content = "error_data")]
pub enum ApiIntegrationRpcError {
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
    #[from_stringify("coins::UnexpectedDerivationMethod")]
    MyAddressError(String),
    #[from_stringify("ethereum_types::FromDecStrErr", "coins::NumConversError", "hex::FromHexError")]
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
        allowance: BigDecimal,
        amount: BigDecimal,
    },
    #[display(fmt = "1inch API error: {}", _0)]
    OneInchError(ApiClientError),
    ApiDataError(String),
    InternalError(String),
    #[display(fmt = "liquidity routing swap not found")]
    LrSwapNotFound,
    #[from_stringify("serde_json::Error")]
    ResponseParseError(String),
    #[from_stringify("coins::TransactionErr")]
    #[display(fmt = "Transaction error {}", _0)]
    TransactionError(String),
    #[from_stringify("coins::RawTransactionError")]
    #[display(fmt = "Sign transaction error {}", _0)]
    SignTransactionError(String),
}

impl HttpStatusCode for ApiIntegrationRpcError {
    fn status_code(&self) -> StatusCode {
        match self {
            ApiIntegrationRpcError::NoSuchCoin { .. } => StatusCode::NOT_FOUND,
            ApiIntegrationRpcError::CoinTypeError
            | ApiIntegrationRpcError::NftProtocolNotSupported
            | ApiIntegrationRpcError::ChainNotSupported
            | ApiIntegrationRpcError::DifferentChains
            | ApiIntegrationRpcError::MyAddressError(_)
            | ApiIntegrationRpcError::InvalidParam(_)
            | ApiIntegrationRpcError::OutOfBounds { .. }
            | ApiIntegrationRpcError::OneInchAllowanceNotEnough { .. }
            | ApiIntegrationRpcError::InternalError { .. }
            | ApiIntegrationRpcError::ConversionError(_)
            | ApiIntegrationRpcError::LrSwapNotFound => StatusCode::BAD_REQUEST,
            ApiIntegrationRpcError::OneInchError(_)
            | ApiIntegrationRpcError::ApiDataError(_)
            | ApiIntegrationRpcError::TransactionError(_) => StatusCode::BAD_GATEWAY,
            ApiIntegrationRpcError::ResponseParseError(_) | ApiIntegrationRpcError::SignTransactionError(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

impl ApiIntegrationRpcError {
    pub(crate) fn from_api_error(error: ApiClientError, decimals: Option<u8>) -> Self {
        match error {
            ApiClientError::InvalidParam(error) => ApiIntegrationRpcError::InvalidParam(error),
            ApiClientError::OutOfBounds { param, value, min, max } => {
                ApiIntegrationRpcError::OutOfBounds { param, value, min, max }
            },
            ApiClientError::TransportError(_)
            | ApiClientError::ParseBodyError { .. }
            | ApiClientError::GeneralApiError { .. } => ApiIntegrationRpcError::OneInchError(error),
            ApiClientError::AllowanceNotEnough { allowance, amount, .. } => {
                ApiIntegrationRpcError::OneInchAllowanceNotEnough {
                    allowance: u256_to_big_decimal(allowance, decimals.unwrap_or_default()).unwrap_or_default(),
                    amount: u256_to_big_decimal(amount, decimals.unwrap_or_default()).unwrap_or_default(),
                }
            },
        }
    }
}

impl From<CoinFindError> for ApiIntegrationRpcError {
    fn from(err: CoinFindError) -> Self {
        match err {
            CoinFindError::NoSuchCoin { coin } => ApiIntegrationRpcError::NoSuchCoin { coin },
        }
    }
}

/// Error aggregator for errors of conversion of api returned values
#[derive(Debug, Display, Serialize)]
pub(crate) struct FromApiValueError(pub String);

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
