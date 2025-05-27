use crate::lp_swap::swap_v2_common::SwapStateMachineError;
use crate::lp_swap::swap_v2_rpcs::MySwapStatusError;
use coins::CoinFindError;
use enum_derives::EnumFromStringify;
use ethereum_types::U256;
use trading_api::one_inch_api::errors::ApiClientError;

#[derive(Debug, Display, EnumFromStringify)]
pub enum LrSwapError {
    NoSuchCoin {
        coin: String,
    },
    CoinTypeError,
    NftProtocolNotSupported,
    ChainNotSupported,
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
        allowance: U256,
        amount: U256,
    },
    OneInchError(ApiClientError),
    StateError(String),
    BestLrSwapNotFound,
    AtomicSwapError(String),
    #[from_stringify("serde_json::Error")]
    ResponseParseError(String),
    #[from_stringify("coins::TransactionErr")]
    TransactionError(String),
    #[from_stringify("coins::RawTransactionError")]
    SignTransactionError(String),
    InternalError(String),
}

impl From<CoinFindError> for LrSwapError {
    fn from(err: CoinFindError) -> Self {
        match err {
            CoinFindError::NoSuchCoin { coin } => LrSwapError::NoSuchCoin { coin },
        }
    }
}

// Implement conversion from lower-level errors
impl From<SwapStateMachineError> for LrSwapError {
    fn from(e: SwapStateMachineError) -> Self { LrSwapError::StateError(e.to_string()) }
}

impl From<MySwapStatusError> for LrSwapError {
    fn from(e: MySwapStatusError) -> Self {
        match e {
            MySwapStatusError::NoSwapWithUuid(uuid) => {
                LrSwapError::AtomicSwapError(format!("No swap with UUID {}", uuid))
            },
            _ => LrSwapError::InternalError(e.to_string()),
        }
    }
}

impl From<ApiClientError> for LrSwapError {
    fn from(error: ApiClientError) -> Self {
        match error {
            ApiClientError::InvalidParam(error) => LrSwapError::InvalidParam(error),
            ApiClientError::OutOfBounds { param, value, min, max } => {
                LrSwapError::OutOfBounds { param, value, min, max }
            },
            ApiClientError::TransportError(_)
            | ApiClientError::ParseBodyError { .. }
            | ApiClientError::GeneralApiError { .. } => LrSwapError::OneInchError(error),
            ApiClientError::AllowanceNotEnough { allowance, amount, .. } => {
                LrSwapError::OneInchAllowanceNotEnough { allowance, amount }
            },
        }
    }
}
