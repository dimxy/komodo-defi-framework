use crate::lp_swap::swap_v2_common::SwapStateMachineError;
use crate::lp_swap::swap_v2_rpcs::MySwapStatusError;
use crate::lp_swap::taker_swap_v2;
use crate::lp_swap::CheckBalanceError;
use coins::CoinFindError;
use derive_more::Display;
use enum_derives::EnumFromStringify;
use ethereum_types::U256;
use mm2_number::BigDecimal;
use trading_api::one_inch_api::errors::OneInchError;

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
    OneInchError(OneInchError), // TODO: do not attach the whole error but extract only message
    StateError(String),
    BestLrSwapNotFound {
        candidates: u32,
    },
    AtomicSwapError(String),
    AtomicSwapAborted(taker_swap_v2::AbortReason),
    #[from_stringify("serde_json::Error")]
    ResponseParseError(String),
    #[from_stringify("coins::TransactionErr")]
    TransactionError(String),
    #[from_stringify("coins::RawTransactionError")]
    SignTransactionError(String),
    InternalError(String),
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
    TransportError(String),
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

impl From<OneInchError> for LrSwapError {
    fn from(error: OneInchError) -> Self {
        match error {
            OneInchError::InvalidParam(error) => LrSwapError::InvalidParam(error),
            OneInchError::OutOfBounds { param, value, min, max } => LrSwapError::OutOfBounds { param, value, min, max },
            OneInchError::TransportError(_)
            | OneInchError::ParseBodyError { .. }
            | OneInchError::GeneralApiError { .. } => LrSwapError::OneInchError(error),
            OneInchError::AllowanceNotEnough { allowance, amount, .. } => {
                LrSwapError::OneInchAllowanceNotEnough { allowance, amount }
            },
        }
    }
}

impl From<CheckBalanceError> for LrSwapError {
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
