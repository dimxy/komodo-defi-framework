/******************************************************************************
 * Copyright © 2023 Pampex LTD and TillyHK LTD                                *
 *                                                                            *
 * See the CONTRIBUTOR-LICENSE-AGREEMENT, COPYING, LICENSE-COPYRIGHT-NOTICE   *
 * and DEVELOPER-CERTIFICATE-OF-ORIGIN files in the LEGAL directory in        *
 * the top-level directory of this distribution for the individual copyright  *
 * holder information and the developer policies on copyright and licensing.  *
 *                                                                            *
 * Unless otherwise agreed in a custom licensing agreement, no part of the    *
 * Komodo DeFi Framework software, including this file may be copied, modified, propagated *
 * or distributed except according to the terms contained in the              *
 * LICENSE-COPYRIGHT-NOTICE file.                                             *
 *                                                                            *
 * Removal or modification of this copyright notice is prohibited.            *
 *                                                                            *
 ******************************************************************************/
//
//  eth.rs
//  marketmaker
//
//  Copyright © 2023 Pampex LTD and TillyHK LTD. All rights reserved.
//
use super::eth::Action::{Call, Create};
use super::watcher_common::{validate_watcher_reward, REWARD_GAS_AMOUNT};
use super::*;
use crate::coin_balance::{EnableCoinBalanceError, EnabledCoinBalanceParams, HDAccountBalance, HDAddressBalance,
                          HDWalletBalance, HDWalletBalanceOps};
use crate::eth::eth_rpc::ETH_RPC_REQUEST_TIMEOUT;
use crate::eth::web3_transport::websocket_transport::{WebsocketTransport, WebsocketTransportNode};
use crate::hd_wallet::{DisplayAddress, HDAccountOps, HDCoinAddress, HDCoinWithdrawOps, HDConfirmAddress,
                       HDPathAccountToAddressId, HDWalletCoinOps, HDXPubExtractor};
use crate::lp_price::get_base_price_in_rel;
use crate::nft::nft_errors::ParseContractTypeError;
use crate::nft::nft_structs::{ContractType, ConvertChain, NftInfo, TransactionNftDetails, WithdrawErc1155,
                              WithdrawErc721};
use crate::nft::WithdrawNftResult;
use crate::rpc_command::account_balance::{AccountBalanceParams, AccountBalanceRpcOps, HDAccountBalanceResponse};
use crate::rpc_command::get_new_address::{GetNewAddressParams, GetNewAddressResponse, GetNewAddressRpcError,
                                          GetNewAddressRpcOps};
use crate::rpc_command::hd_account_balance_rpc_error::HDAccountBalanceRpcError;
use crate::rpc_command::init_account_balance::{InitAccountBalanceParams, InitAccountBalanceRpcOps};
use crate::rpc_command::init_create_account::{CreateAccountRpcError, CreateAccountState, CreateNewAccountParams,
                                              InitCreateAccountRpcOps};
use crate::rpc_command::init_scan_for_new_addresses::{InitScanAddressesRpcOps, ScanAddressesParams,
                                                      ScanAddressesResponse};
use crate::rpc_command::init_withdraw::{InitWithdrawCoin, WithdrawTaskHandleShared};
use crate::rpc_command::{account_balance, get_new_address, init_account_balance, init_create_account,
                         init_scan_for_new_addresses};
use crate::{coin_balance, scan_for_new_addresses_impl, BalanceResult, CoinWithDerivationMethod, DerivationMethod,
            DexFee, Eip1559Ops, MakerNftSwapOpsV2, ParseCoinAssocTypes, ParseNftAssocTypes, PayForGasParams,
            PrivKeyPolicy, RpcCommonOps, SendNftMakerPaymentArgs, SpendNftMakerPaymentArgs, ToBytes,
            ValidateNftMakerPaymentArgs, ValidateWatcherSpendInput, WatcherSpendType};
use async_trait::async_trait;
use bitcrypto::{dhash160, keccak256, ripemd160, sha256};
use common::custom_futures::repeatable::{Ready, Retry, RetryOnError};
use common::custom_futures::timeout::FutureTimerExt;
use common::executor::{abortable_queue::AbortableQueue, AbortSettings, AbortableSystem, AbortedError, SpawnAbortable,
                       Timer};
use common::log::{debug, error, info, warn};
use common::number_type_casting::SafeTypeCastingNumbers;
use common::{now_sec, small_rng, DEX_FEE_ADDR_RAW_PUBKEY};
use crypto::privkey::key_pair_from_secret;
use crypto::{Bip44Chain, CryptoCtx, CryptoCtxError, GlobalHDAccountArc, KeyPairPolicy};
use derive_more::Display;
use enum_derives::EnumFromStringify;

use compatible_time::Instant;
use ethabi::{Contract, Function, Token};
use ethcore_transaction::tx_builders::TxBuilderError;
use ethcore_transaction::{Action, TransactionWrapper, TransactionWrapperBuilder as UnSignedEthTxBuilder,
                          UnverifiedEip1559Transaction, UnverifiedEip2930Transaction, UnverifiedLegacyTransaction,
                          UnverifiedTransactionWrapper};
pub use ethcore_transaction::{SignedTransaction as SignedEthTx, TxType};
use ethereum_types::{Address, H160, H256, U256};
use ethkey::{public_to_address, sign, verify_address, KeyPair, Public, Signature};
use futures::compat::Future01CompatExt;
use futures::future::{join, join_all, select_ok, try_join_all, Either, FutureExt, TryFutureExt};
use futures01::Future;
use http::Uri;
use mm2_core::mm_ctx::{MmArc, MmWeak};
use mm2_number::bigdecimal_custom::CheckedDivision;
use mm2_number::{BigDecimal, BigUint, MmNumber};
use rand::seq::SliceRandom;
use rlp::{DecoderError, Encodable, RlpStream};
use rpc::v1::types::Bytes as BytesJson;
use secp256k1::PublicKey;
use serde_json::{self as json, Value as Json};
use serialization::{CompactInteger, Serializable, Stream};
use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::ops::Deref;
use std::str::from_utf8;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use web3::types::{Action as TraceAction, BlockId, BlockNumber, Bytes, CallRequest, FilterBuilder, Log, Trace,
                  TraceFilterBuilder, Transaction as Web3Transaction, TransactionId, U64};
use web3::{self, Web3};

cfg_wasm32! {
    use common::{now_ms, wait_until_ms};
    use crypto::MetamaskArc;
    use ethereum_types::{H264, H520};
    use mm2_metamask::MetamaskError;
    use web3::types::TransactionRequest;
}

use super::{coin_conf, lp_coinfind_or_err, AsyncMutex, BalanceError, BalanceFut, CheckIfMyPaymentSentArgs,
            CoinBalance, CoinProtocol, CoinTransportMetrics, CoinsContext, ConfirmPaymentInput, EthValidateFeeArgs,
            FeeApproxStage, FoundSwapTxSpend, HistorySyncState, IguanaPrivKey, MarketCoinOps, MmCoin, MmCoinEnum,
            MyAddressError, MyWalletAddress, NegotiateSwapContractAddrErr, NumConversError, NumConversResult,
            PaymentInstructionArgs, PaymentInstructions, PaymentInstructionsErr, PrivKeyBuildPolicy,
            PrivKeyPolicyNotAllowed, RawTransactionError, RawTransactionFut, RawTransactionRequest, RawTransactionRes,
            RawTransactionResult, RefundPaymentArgs, RewardTarget, RpcClientType, RpcTransportEventHandler,
            RpcTransportEventHandlerShared, SearchForSwapTxSpendInput, SendMakerPaymentSpendPreimageInput,
            SendPaymentArgs, SignEthTransactionParams, SignRawTransactionEnum, SignRawTransactionRequest,
            SignatureError, SignatureResult, SpendPaymentArgs, SwapOps, SwapTxFeePolicy, TradeFee, TradePreimageError,
            TradePreimageFut, TradePreimageResult, TradePreimageValue, Transaction, TransactionDetails,
            TransactionEnum, TransactionErr, TransactionFut, TransactionType, TxMarshalingErr,
            UnexpectedDerivationMethod, ValidateAddressResult, ValidateFeeArgs, ValidateInstructionsErr,
            ValidateOtherPubKeyErr, ValidatePaymentError, ValidatePaymentFut, ValidatePaymentInput, VerificationError,
            VerificationResult, WaitForHTLCTxSpendArgs, WatcherOps, WatcherReward, WatcherRewardError,
            WatcherSearchForSwapTxSpendInput, WatcherValidatePaymentInput, WatcherValidateTakerFeeInput, WeakSpawner,
            WithdrawError, WithdrawFee, WithdrawFut, WithdrawRequest, WithdrawResult, EARLY_CONFIRMATION_ERR_LOG,
            INVALID_CONTRACT_ADDRESS_ERR_LOG, INVALID_PAYMENT_STATE_ERR_LOG, INVALID_RECEIVER_ERR_LOG,
            INVALID_SENDER_ERR_LOG, INVALID_SWAP_ID_ERR_LOG};
pub use rlp;
cfg_native! {
    use std::path::PathBuf;
}

pub mod eth_balance_events;
mod eth_rpc;
#[cfg(test)] mod eth_tests;
#[cfg(target_arch = "wasm32")] mod eth_wasm_tests;
#[cfg(any(test, target_arch = "wasm32"))] mod for_tests;
pub(crate) mod nft_swap_v2;
mod web3_transport;
use web3_transport::{http_transport::HttpTransportNode, Web3Transport};

pub mod eth_hd_wallet;
use eth_hd_wallet::EthHDWallet;

#[path = "eth/v2_activation.rs"] pub mod v2_activation;
use v2_activation::{build_address_and_priv_key_policy, EthActivationV2Error};

mod eth_withdraw;
use eth_withdraw::{EthWithdraw, InitEthWithdraw, StandardEthWithdraw};

mod nonce;
use nonce::ParityNonce;

pub mod fee_estimation;
use fee_estimation::eip1559::{block_native::BlocknativeGasApiCaller, infura::InfuraGasApiCaller,
                              simple::FeePerGasSimpleEstimator, FeePerGasEstimated, GasApiConfig, GasApiProvider};

pub mod erc20;
use erc20::get_token_decimals;
pub(crate) mod eth_swap_v2;
use eth_swap_v2::{extract_id_from_tx_data, EthPaymentType, PaymentMethod, SpendTxSearchParams};

pub const ETH_PROTOCOL_TYPE: &str = "ETH";
pub const ERC20_PROTOCOL_TYPE: &str = "ERC20";

/// https://github.com/artemii235/etomic-swap/blob/master/contracts/EtomicSwap.sol
/// Dev chain (195.201.137.5:8565) contract address: 0x83965C539899cC0F918552e5A26915de40ee8852
/// Ropsten: https://ropsten.etherscan.io/address/0x7bc1bbdd6a0a722fc9bffc49c921b685ecb84b94
/// ETH mainnet: https://etherscan.io/address/0x8500AFc0bc5214728082163326C2FF0C73f4a871
pub const SWAP_CONTRACT_ABI: &str = include_str!("eth/swap_contract_abi.json");
/// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-20.md
pub const ERC20_ABI: &str = include_str!("eth/erc20_abi.json");
/// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-721.md
const ERC721_ABI: &str = include_str!("eth/erc721_abi.json");
/// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-1155.md
const ERC1155_ABI: &str = include_str!("eth/erc1155_abi.json");
const NFT_SWAP_CONTRACT_ABI: &str = include_str!("eth/nft_swap_contract_abi.json");
const NFT_MAKER_SWAP_V2_ABI: &str = include_str!("eth/nft_maker_swap_v2_abi.json");
const MAKER_SWAP_V2_ABI: &str = include_str!("eth/maker_swap_v2_abi.json");
const TAKER_SWAP_V2_ABI: &str = include_str!("eth/taker_swap_v2_abi.json");

/// Payment states from etomic swap smart contract: https://github.com/artemii235/etomic-swap/blob/master/contracts/EtomicSwap.sol#L5
pub enum PaymentState {
    Uninitialized,
    Sent,
    Spent,
    Refunded,
}

#[allow(dead_code)]
pub(crate) enum MakerPaymentStateV2 {
    Uninitialized,
    PaymentSent,
    TakerSpent,
    MakerRefunded,
}

#[allow(dead_code)]
pub(crate) enum TakerPaymentStateV2 {
    Uninitialized,
    PaymentSent,
    TakerApproved,
    MakerSpent,
    TakerRefunded,
}

/// It can change 12.5% max each block according to https://www.blocknative.com/blog/eip-1559-fees
const BASE_BLOCK_FEE_DIFF_PCT: u64 = 13;
const DEFAULT_LOGS_BLOCK_RANGE: u64 = 1000;

const DEFAULT_REQUIRED_CONFIRMATIONS: u8 = 1;

pub(crate) const ETH_DECIMALS: u8 = 18;

pub(crate) const ETH_GWEI_DECIMALS: u8 = 9;

/// Take into account that the dynamic fee may increase by 3% during the swap.
const GAS_PRICE_APPROXIMATION_PERCENT_ON_START_SWAP: u64 = 3;
/// Take into account that the dynamic fee may increase until the locktime is expired
const GAS_PRICE_APPROXIMATION_PERCENT_ON_WATCHER_PREIMAGE: u64 = 3;
/// Take into account that the dynamic fee may increase at each of the following stages:
/// - it may increase by 2% until a swap is started;
/// - it may increase by 3% during the swap.
const GAS_PRICE_APPROXIMATION_PERCENT_ON_ORDER_ISSUE: u64 = 5;
/// Take into account that the dynamic fee may increase at each of the following stages:
/// - it may increase by 2% until an order is issued;
/// - it may increase by 2% until a swap is started;
/// - it may increase by 3% during the swap.
const GAS_PRICE_APPROXIMATION_PERCENT_ON_TRADE_PREIMAGE: u64 = 7;

/// Heuristic default gas limits for withdraw and swap operations (including extra margin value for possible changes in opcodes cost)
pub mod gas_limit {
    /// Gas limit for sending coins
    pub const ETH_SEND_COINS: u64 = 21_000;
    /// Gas limit for transfer ERC20 tokens
    /// TODO: maybe this is too much and 150K is okay
    pub const ETH_SEND_ERC20: u64 = 210_000;
    /// Gas limit for swap payment tx with coins
    /// real values are approx 48,6K by etherscan
    pub const ETH_PAYMENT: u64 = 65_000;
    /// Gas limit for swap payment tx with ERC20 tokens
    /// real values are 98,9K for ERC20 and 135K for ERC-1967 proxied ERC20 contracts (use 'gas_limit' override in coins to tune)
    pub const ERC20_PAYMENT: u64 = 150_000;
    /// Gas limit for swap receiver spend tx with coins
    /// real values are 40,7K
    pub const ETH_RECEIVER_SPEND: u64 = 65_000;
    /// Gas limit for swap receiver spend tx with ERC20 tokens
    /// real values are 72,8K
    pub const ERC20_RECEIVER_SPEND: u64 = 150_000;
    /// Gas limit for swap refund tx with coins
    pub const ETH_SENDER_REFUND: u64 = 100_000;
    /// Gas limit for swap refund tx with ERC20 tokens
    pub const ERC20_SENDER_REFUND: u64 = 150_000;
    /// Gas limit for other operations
    pub const ETH_MAX_TRADE_GAS: u64 = 150_000;
}

/// Default gas limits for EthGasLimitV2
pub mod gas_limit_v2 {
    /// Gas limits for maker operations in EtomicSwapMakerV2 contract
    pub mod maker {
        pub const ETH_PAYMENT: u64 = 65_000;
        pub const ERC20_PAYMENT: u64 = 150_000;
        pub const ETH_TAKER_SPEND: u64 = 100_000;
        pub const ERC20_TAKER_SPEND: u64 = 150_000;
        pub const ETH_MAKER_REFUND_TIMELOCK: u64 = 90_000;
        pub const ERC20_MAKER_REFUND_TIMELOCK: u64 = 100_000;
        pub const ETH_MAKER_REFUND_SECRET: u64 = 90_000;
        pub const ERC20_MAKER_REFUND_SECRET: u64 = 100_000;
    }

    /// Gas limits for taker operations in EtomicSwapTakerV2 contract
    pub mod taker {
        pub const ETH_PAYMENT: u64 = 65_000;
        pub const ERC20_PAYMENT: u64 = 150_000;
        pub const ETH_MAKER_SPEND: u64 = 100_000;
        pub const ERC20_MAKER_SPEND: u64 = 115_000;
        pub const ETH_TAKER_REFUND_TIMELOCK: u64 = 90_000;
        pub const ERC20_TAKER_REFUND_TIMELOCK: u64 = 100_000;
        pub const ETH_TAKER_REFUND_SECRET: u64 = 90_000;
        pub const ERC20_TAKER_REFUND_SECRET: u64 = 100_000;
        pub const APPROVE_PAYMENT: u64 = 50_000;
    }

    pub mod nft_maker {
        pub const ERC721_PAYMENT: u64 = 130_000;
        pub const ERC1155_PAYMENT: u64 = 130_000;
        pub const ERC721_TAKER_SPEND: u64 = 100_000;
        pub const ERC1155_TAKER_SPEND: u64 = 100_000;
        pub const ERC721_MAKER_REFUND_TIMELOCK: u64 = 100_000;
        pub const ERC1155_MAKER_REFUND_TIMELOCK: u64 = 100_000;
        pub const ERC721_MAKER_REFUND_SECRET: u64 = 100_000;
        pub const ERC1155_MAKER_REFUND_SECRET: u64 = 100_000;
    }
}

/// Coin conf param to override default gas limits
#[derive(Deserialize)]
#[serde(default)]
pub struct EthGasLimit {
    /// Gas limit for sending coins
    pub eth_send_coins: u64,
    /// Gas limit for sending ERC20 tokens
    pub eth_send_erc20: u64,
    /// Gas limit for swap payment tx with coins
    pub eth_payment: u64,
    /// Gas limit for swap payment tx with ERC20 tokens
    pub erc20_payment: u64,
    /// Gas limit for swap receiver spend tx with coins
    pub eth_receiver_spend: u64,
    /// Gas limit for swap receiver spend tx with ERC20 tokens
    pub erc20_receiver_spend: u64,
    /// Gas limit for swap refund tx with coins
    pub eth_sender_refund: u64,
    /// Gas limit for swap refund tx with ERC20 tokens
    pub erc20_sender_refund: u64,
    /// Gas limit for other operations
    pub eth_max_trade_gas: u64,
}

impl Default for EthGasLimit {
    fn default() -> Self {
        EthGasLimit {
            eth_send_coins: gas_limit::ETH_SEND_COINS,
            eth_send_erc20: gas_limit::ETH_SEND_ERC20,
            eth_payment: gas_limit::ETH_PAYMENT,
            erc20_payment: gas_limit::ERC20_PAYMENT,
            eth_receiver_spend: gas_limit::ETH_RECEIVER_SPEND,
            erc20_receiver_spend: gas_limit::ERC20_RECEIVER_SPEND,
            eth_sender_refund: gas_limit::ETH_SENDER_REFUND,
            erc20_sender_refund: gas_limit::ERC20_SENDER_REFUND,
            eth_max_trade_gas: gas_limit::ETH_MAX_TRADE_GAS,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct EthGasLimitV2 {
    pub maker: MakerGasLimitV2,
    pub taker: TakerGasLimitV2,
    pub nft_maker: NftMakerGasLimitV2,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct MakerGasLimitV2 {
    pub eth_payment: u64,
    pub erc20_payment: u64,
    pub eth_taker_spend: u64,
    pub erc20_taker_spend: u64,
    pub eth_maker_refund_timelock: u64,
    pub erc20_maker_refund_timelock: u64,
    pub eth_maker_refund_secret: u64,
    pub erc20_maker_refund_secret: u64,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct TakerGasLimitV2 {
    pub eth_payment: u64,
    pub erc20_payment: u64,
    pub eth_maker_spend: u64,
    pub erc20_maker_spend: u64,
    pub eth_taker_refund_timelock: u64,
    pub erc20_taker_refund_timelock: u64,
    pub eth_taker_refund_secret: u64,
    pub erc20_taker_refund_secret: u64,
    pub approve_payment: u64,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct NftMakerGasLimitV2 {
    pub erc721_payment: u64,
    pub erc1155_payment: u64,
    pub erc721_taker_spend: u64,
    pub erc1155_taker_spend: u64,
    pub erc721_maker_refund_timelock: u64,
    pub erc1155_maker_refund_timelock: u64,
    pub erc721_maker_refund_secret: u64,
    pub erc1155_maker_refund_secret: u64,
}

impl EthGasLimitV2 {
    fn gas_limit(
        &self,
        coin_type: &EthCoinType,
        payment_type: EthPaymentType,
        method: PaymentMethod,
    ) -> Result<u64, String> {
        match coin_type {
            EthCoinType::Eth => {
                let gas_limit = match payment_type {
                    EthPaymentType::MakerPayments => match method {
                        PaymentMethod::Send => self.maker.eth_payment,
                        PaymentMethod::Spend => self.maker.eth_taker_spend,
                        PaymentMethod::RefundTimelock => self.maker.eth_maker_refund_timelock,
                        PaymentMethod::RefundSecret => self.maker.eth_maker_refund_secret,
                    },
                    EthPaymentType::TakerPayments => match method {
                        PaymentMethod::Send => self.taker.eth_payment,
                        PaymentMethod::Spend => self.taker.eth_maker_spend,
                        PaymentMethod::RefundTimelock => self.taker.eth_taker_refund_timelock,
                        PaymentMethod::RefundSecret => self.taker.eth_taker_refund_secret,
                    },
                };
                Ok(gas_limit)
            },
            EthCoinType::Erc20 { .. } => {
                let gas_limit = match payment_type {
                    EthPaymentType::MakerPayments => match method {
                        PaymentMethod::Send => self.maker.erc20_payment,
                        PaymentMethod::Spend => self.maker.erc20_taker_spend,
                        PaymentMethod::RefundTimelock => self.maker.erc20_maker_refund_timelock,
                        PaymentMethod::RefundSecret => self.maker.erc20_maker_refund_secret,
                    },
                    EthPaymentType::TakerPayments => match method {
                        PaymentMethod::Send => self.taker.erc20_payment,
                        PaymentMethod::Spend => self.taker.erc20_maker_spend,
                        PaymentMethod::RefundTimelock => self.taker.erc20_taker_refund_timelock,
                        PaymentMethod::RefundSecret => self.taker.erc20_taker_refund_secret,
                    },
                };
                Ok(gas_limit)
            },
            EthCoinType::Nft { .. } => Err("NFT protocol is not supported for ETH and ERC20 Swaps".to_string()),
        }
    }

    fn nft_gas_limit(&self, contract_type: &ContractType, method: PaymentMethod) -> u64 {
        match contract_type {
            ContractType::Erc1155 => match method {
                PaymentMethod::Send => self.nft_maker.erc1155_payment,
                PaymentMethod::Spend => self.nft_maker.erc1155_taker_spend,
                PaymentMethod::RefundTimelock => self.nft_maker.erc1155_maker_refund_timelock,
                PaymentMethod::RefundSecret => self.nft_maker.erc1155_maker_refund_secret,
            },
            ContractType::Erc721 => match method {
                PaymentMethod::Send => self.nft_maker.erc721_payment,
                PaymentMethod::Spend => self.nft_maker.erc721_taker_spend,
                PaymentMethod::RefundTimelock => self.nft_maker.erc721_maker_refund_timelock,
                PaymentMethod::RefundSecret => self.nft_maker.erc721_maker_refund_secret,
            },
        }
    }
}

impl Default for MakerGasLimitV2 {
    fn default() -> Self {
        MakerGasLimitV2 {
            eth_payment: gas_limit_v2::maker::ETH_PAYMENT,
            erc20_payment: gas_limit_v2::maker::ERC20_PAYMENT,
            eth_taker_spend: gas_limit_v2::maker::ETH_TAKER_SPEND,
            erc20_taker_spend: gas_limit_v2::maker::ERC20_TAKER_SPEND,
            eth_maker_refund_timelock: gas_limit_v2::maker::ETH_MAKER_REFUND_TIMELOCK,
            erc20_maker_refund_timelock: gas_limit_v2::maker::ERC20_MAKER_REFUND_TIMELOCK,
            eth_maker_refund_secret: gas_limit_v2::maker::ETH_MAKER_REFUND_SECRET,
            erc20_maker_refund_secret: gas_limit_v2::maker::ERC20_MAKER_REFUND_SECRET,
        }
    }
}

impl Default for TakerGasLimitV2 {
    fn default() -> Self {
        TakerGasLimitV2 {
            eth_payment: gas_limit_v2::taker::ETH_PAYMENT,
            erc20_payment: gas_limit_v2::taker::ERC20_PAYMENT,
            eth_maker_spend: gas_limit_v2::taker::ETH_MAKER_SPEND,
            erc20_maker_spend: gas_limit_v2::taker::ERC20_MAKER_SPEND,
            eth_taker_refund_timelock: gas_limit_v2::taker::ETH_TAKER_REFUND_TIMELOCK,
            erc20_taker_refund_timelock: gas_limit_v2::taker::ERC20_TAKER_REFUND_TIMELOCK,
            eth_taker_refund_secret: gas_limit_v2::taker::ETH_TAKER_REFUND_SECRET,
            erc20_taker_refund_secret: gas_limit_v2::taker::ERC20_TAKER_REFUND_SECRET,
            approve_payment: gas_limit_v2::taker::APPROVE_PAYMENT,
        }
    }
}

impl Default for NftMakerGasLimitV2 {
    fn default() -> Self {
        NftMakerGasLimitV2 {
            erc721_payment: gas_limit_v2::nft_maker::ERC721_PAYMENT,
            erc1155_payment: gas_limit_v2::nft_maker::ERC1155_PAYMENT,
            erc721_taker_spend: gas_limit_v2::nft_maker::ERC721_TAKER_SPEND,
            erc1155_taker_spend: gas_limit_v2::nft_maker::ERC1155_TAKER_SPEND,
            erc721_maker_refund_timelock: gas_limit_v2::nft_maker::ERC721_MAKER_REFUND_TIMELOCK,
            erc1155_maker_refund_timelock: gas_limit_v2::nft_maker::ERC1155_MAKER_REFUND_TIMELOCK,
            erc721_maker_refund_secret: gas_limit_v2::nft_maker::ERC721_MAKER_REFUND_SECRET,
            erc1155_maker_refund_secret: gas_limit_v2::nft_maker::ERC1155_MAKER_REFUND_SECRET,
        }
    }
}

trait ExtractGasLimit: Default + for<'de> Deserialize<'de> {
    fn key() -> &'static str;
}

impl ExtractGasLimit for EthGasLimit {
    fn key() -> &'static str { "gas_limit" }
}

impl ExtractGasLimit for EthGasLimitV2 {
    fn key() -> &'static str { "gas_limit_v2" }
}

/// Max transaction type according to EIP-2718
const ETH_MAX_TX_TYPE: u64 = 0x7f;

lazy_static! {
    pub static ref SWAP_CONTRACT: Contract = Contract::load(SWAP_CONTRACT_ABI.as_bytes()).unwrap();
    pub static ref MAKER_SWAP_V2: Contract = Contract::load(MAKER_SWAP_V2_ABI.as_bytes()).unwrap();
    pub static ref TAKER_SWAP_V2: Contract = Contract::load(TAKER_SWAP_V2_ABI.as_bytes()).unwrap();
    pub static ref ERC20_CONTRACT: Contract = Contract::load(ERC20_ABI.as_bytes()).unwrap();
    pub static ref ERC721_CONTRACT: Contract = Contract::load(ERC721_ABI.as_bytes()).unwrap();
    pub static ref ERC1155_CONTRACT: Contract = Contract::load(ERC1155_ABI.as_bytes()).unwrap();
    pub static ref NFT_SWAP_CONTRACT: Contract = Contract::load(NFT_SWAP_CONTRACT_ABI.as_bytes()).unwrap();
    pub static ref NFT_MAKER_SWAP_V2: Contract = Contract::load(NFT_MAKER_SWAP_V2_ABI.as_bytes()).unwrap();
}

pub type EthDerivationMethod = DerivationMethod<Address, EthHDWallet>;
pub type Web3RpcFut<T> = Box<dyn Future<Item = T, Error = MmError<Web3RpcError>> + Send>;
pub type Web3RpcResult<T> = Result<T, MmError<Web3RpcError>>;
type EthPrivKeyPolicy = PrivKeyPolicy<KeyPair>;

#[derive(Clone, Debug)]
pub(crate) struct LegacyGasPrice {
    pub(crate) gas_price: U256,
}

#[derive(Clone, Debug)]
pub(crate) struct Eip1559FeePerGas {
    pub(crate) max_fee_per_gas: U256,
    pub(crate) max_priority_fee_per_gas: U256,
}

/// Internal structure describing how transaction pays for gas unit:
/// either legacy gas price or EIP-1559 fee per gas
#[derive(Clone, Debug)]
pub(crate) enum PayForGasOption {
    Legacy(LegacyGasPrice),
    Eip1559(Eip1559FeePerGas),
}

impl PayForGasOption {
    fn get_gas_price(&self) -> Option<U256> {
        match self {
            PayForGasOption::Legacy(LegacyGasPrice { gas_price }) => Some(*gas_price),
            PayForGasOption::Eip1559(..) => None,
        }
    }

    fn get_fee_per_gas(&self) -> (Option<U256>, Option<U256>) {
        match self {
            PayForGasOption::Eip1559(Eip1559FeePerGas {
                max_fee_per_gas,
                max_priority_fee_per_gas,
            }) => (Some(*max_fee_per_gas), Some(*max_priority_fee_per_gas)),
            PayForGasOption::Legacy(..) => (None, None),
        }
    }
}

impl TryFrom<PayForGasParams> for PayForGasOption {
    type Error = MmError<NumConversError>;

    fn try_from(param: PayForGasParams) -> Result<Self, Self::Error> {
        match param {
            PayForGasParams::Legacy(legacy) => Ok(Self::Legacy(LegacyGasPrice {
                gas_price: wei_from_gwei_decimal(&legacy.gas_price)?,
            })),
            PayForGasParams::Eip1559(eip1559) => Ok(Self::Eip1559(Eip1559FeePerGas {
                max_fee_per_gas: wei_from_gwei_decimal(&eip1559.max_fee_per_gas)?,
                max_priority_fee_per_gas: wei_from_gwei_decimal(&eip1559.max_priority_fee_per_gas)?,
            })),
        }
    }
}

type GasDetails = (U256, PayForGasOption);

#[derive(Debug, Display, EnumFromStringify, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum Web3RpcError {
    #[display(fmt = "Transport: {}", _0)]
    Transport(String),
    #[display(fmt = "Invalid response: {}", _0)]
    InvalidResponse(String),
    #[display(fmt = "Timeout: {}", _0)]
    Timeout(String),
    #[from_stringify("serde_json::Error")]
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
    #[display(fmt = "Invalid gas api provider config: {}", _0)]
    InvalidGasApiConfig(String),
    #[display(fmt = "Nft Protocol is not supported yet!")]
    NftProtocolNotSupported,
    #[display(fmt = "Number conversion: {}", _0)]
    NumConversError(String),
}

impl From<web3::Error> for Web3RpcError {
    fn from(e: web3::Error) -> Self {
        let error_str = e.to_string();
        match e {
            web3::Error::InvalidResponse(_) | web3::Error::Decoder(_) | web3::Error::Rpc(_) => {
                Web3RpcError::InvalidResponse(error_str)
            },
            web3::Error::Unreachable | web3::Error::Transport(_) | web3::Error::Io(_) => {
                Web3RpcError::Transport(error_str)
            },
            _ => Web3RpcError::Internal(error_str),
        }
    }
}

impl From<Web3RpcError> for RawTransactionError {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(tr) | Web3RpcError::InvalidResponse(tr) => RawTransactionError::Transport(tr),
            Web3RpcError::Internal(internal)
            | Web3RpcError::Timeout(internal)
            | Web3RpcError::NumConversError(internal)
            | Web3RpcError::InvalidGasApiConfig(internal) => RawTransactionError::InternalError(internal),
            Web3RpcError::NftProtocolNotSupported => {
                RawTransactionError::InternalError("Nft Protocol is not supported yet!".to_string())
            },
        }
    }
}

impl From<ethabi::Error> for Web3RpcError {
    fn from(e: ethabi::Error) -> Web3RpcError {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        Web3RpcError::Internal(e.to_string())
    }
}

impl From<UnexpectedDerivationMethod> for Web3RpcError {
    fn from(e: UnexpectedDerivationMethod) -> Self { Web3RpcError::Internal(e.to_string()) }
}

#[cfg(target_arch = "wasm32")]
impl From<MetamaskError> for Web3RpcError {
    fn from(e: MetamaskError) -> Self {
        match e {
            MetamaskError::Internal(internal) => Web3RpcError::Internal(internal),
            other => Web3RpcError::Transport(other.to_string()),
        }
    }
}

impl From<NumConversError> for Web3RpcError {
    fn from(e: NumConversError) -> Self { Web3RpcError::NumConversError(e.to_string()) }
}

impl From<ethabi::Error> for WithdrawError {
    fn from(e: ethabi::Error) -> Self {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        WithdrawError::InternalError(e.to_string())
    }
}

impl From<web3::Error> for WithdrawError {
    fn from(e: web3::Error) -> Self { WithdrawError::Transport(e.to_string()) }
}

impl From<Web3RpcError> for WithdrawError {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(err) | Web3RpcError::InvalidResponse(err) => WithdrawError::Transport(err),
            Web3RpcError::Internal(internal)
            | Web3RpcError::Timeout(internal)
            | Web3RpcError::NumConversError(internal)
            | Web3RpcError::InvalidGasApiConfig(internal) => WithdrawError::InternalError(internal),
            Web3RpcError::NftProtocolNotSupported => WithdrawError::NftProtocolNotSupported,
        }
    }
}

impl From<ethcore_transaction::Error> for WithdrawError {
    fn from(e: ethcore_transaction::Error) -> Self { WithdrawError::SigningError(e.to_string()) }
}

impl From<web3::Error> for TradePreimageError {
    fn from(e: web3::Error) -> Self { TradePreimageError::Transport(e.to_string()) }
}

impl From<Web3RpcError> for TradePreimageError {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(err) | Web3RpcError::InvalidResponse(err) => TradePreimageError::Transport(err),
            Web3RpcError::Internal(internal)
            | Web3RpcError::Timeout(internal)
            | Web3RpcError::NumConversError(internal)
            | Web3RpcError::InvalidGasApiConfig(internal) => TradePreimageError::InternalError(internal),
            Web3RpcError::NftProtocolNotSupported => TradePreimageError::NftProtocolNotSupported,
        }
    }
}

impl From<ethabi::Error> for TradePreimageError {
    fn from(e: ethabi::Error) -> Self {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        TradePreimageError::InternalError(e.to_string())
    }
}

impl From<ethabi::Error> for BalanceError {
    fn from(e: ethabi::Error) -> Self {
        // Currently, we use the `ethabi` crate to work with a smart contract ABI known at compile time.
        // It's an internal error if there are any issues during working with a smart contract ABI.
        BalanceError::Internal(e.to_string())
    }
}

impl From<web3::Error> for BalanceError {
    fn from(e: web3::Error) -> Self { BalanceError::from(Web3RpcError::from(e)) }
}

impl From<Web3RpcError> for BalanceError {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(tr) | Web3RpcError::InvalidResponse(tr) => BalanceError::Transport(tr),
            Web3RpcError::Internal(internal)
            | Web3RpcError::Timeout(internal)
            | Web3RpcError::NumConversError(internal)
            | Web3RpcError::InvalidGasApiConfig(internal) => BalanceError::Internal(internal),
            Web3RpcError::NftProtocolNotSupported => {
                BalanceError::Internal("Nft Protocol is not supported yet!".to_string())
            },
        }
    }
}

impl From<TxBuilderError> for TransactionErr {
    fn from(e: TxBuilderError) -> Self { TransactionErr::Plain(e.to_string()) }
}

impl From<ethcore_transaction::Error> for TransactionErr {
    fn from(e: ethcore_transaction::Error) -> Self { TransactionErr::Plain(e.to_string()) }
}

#[derive(Debug, Deserialize, Serialize)]
struct SavedTraces {
    /// ETH traces for my_address
    traces: Vec<Trace>,
    /// Earliest processed block
    earliest_block: U64,
    /// Latest processed block
    latest_block: U64,
}

#[derive(Debug, Deserialize, Serialize)]
struct SavedErc20Events {
    /// ERC20 events for my_address
    events: Vec<Log>,
    /// Earliest processed block
    earliest_block: U64,
    /// Latest processed block
    latest_block: U64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EthCoinType {
    /// Ethereum itself or it's forks: ETC/others
    Eth,
    /// ERC20 token with smart contract address
    /// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-20.md
    Erc20 {
        platform: String,
        token_addr: Address,
    },
    Nft {
        platform: String,
    },
}

/// An alternative to `crate::PrivKeyBuildPolicy`, typical only for ETH coin.
pub enum EthPrivKeyBuildPolicy {
    IguanaPrivKey(IguanaPrivKey),
    GlobalHDAccount(GlobalHDAccountArc),
    #[cfg(target_arch = "wasm32")]
    Metamask(MetamaskArc),
    Trezor,
}

impl EthPrivKeyBuildPolicy {
    /// Detects the `EthPrivKeyBuildPolicy` with which the given `MmArc` is initialized.
    pub fn detect_priv_key_policy(ctx: &MmArc) -> MmResult<EthPrivKeyBuildPolicy, CryptoCtxError> {
        let crypto_ctx = CryptoCtx::from_ctx(ctx)?;

        match crypto_ctx.key_pair_policy() {
            KeyPairPolicy::Iguana => {
                // Use an internal private key as the coin secret.
                let priv_key = crypto_ctx.mm2_internal_privkey_secret();
                Ok(EthPrivKeyBuildPolicy::IguanaPrivKey(priv_key))
            },
            KeyPairPolicy::GlobalHDAccount(global_hd) => Ok(EthPrivKeyBuildPolicy::GlobalHDAccount(global_hd.clone())),
        }
    }
}

impl From<PrivKeyBuildPolicy> for EthPrivKeyBuildPolicy {
    fn from(policy: PrivKeyBuildPolicy) -> Self {
        match policy {
            PrivKeyBuildPolicy::IguanaPrivKey(iguana) => EthPrivKeyBuildPolicy::IguanaPrivKey(iguana),
            PrivKeyBuildPolicy::GlobalHDAccount(global_hd) => EthPrivKeyBuildPolicy::GlobalHDAccount(global_hd),
            PrivKeyBuildPolicy::Trezor => EthPrivKeyBuildPolicy::Trezor,
        }
    }
}

/// pImpl idiom.
pub struct EthCoinImpl {
    ticker: String,
    pub coin_type: EthCoinType,
    pub(crate) priv_key_policy: EthPrivKeyPolicy,
    /// Either an Iguana address or a 'EthHDWallet' instance.
    /// Arc is used to use the same hd wallet from platform coin if we need to.
    /// This allows the reuse of the same derived accounts/addresses of the
    /// platform coin for tokens and vice versa.
    derivation_method: Arc<EthDerivationMethod>,
    sign_message_prefix: Option<String>,
    swap_contract_address: Address,
    swap_v2_contracts: Option<SwapV2Contracts>,
    fallback_swap_contract: Option<Address>,
    contract_supports_watchers: bool,
    web3_instances: AsyncMutex<Vec<Web3Instance>>,
    decimals: u8,
    history_sync_state: Mutex<HistorySyncState>,
    required_confirmations: AtomicU64,
    swap_txfee_policy: Mutex<SwapTxFeePolicy>,
    max_eth_tx_type: Option<u64>,
    /// Coin needs access to the context in order to reuse the logging and shutdown facilities.
    /// Using a weak reference by default in order to avoid circular references and leaks.
    pub ctx: MmWeak,
    chain_id: u64,
    /// The name of the coin with which Trezor wallet associates this asset.
    trezor_coin: Option<String>,
    /// the block range used for eth_getLogs
    logs_block_range: u64,
    /// A mapping of Ethereum addresses to their respective nonce locks.
    /// This is used to ensure that only one transaction is sent at a time per address.
    /// Each address is associated with an `AsyncMutex` which is locked when a transaction is being created and sent,
    /// and unlocked once the transaction is confirmed. This prevents nonce conflicts when multiple transactions
    /// are initiated concurrently from the same address.
    address_nonce_locks: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    erc20_tokens_infos: Arc<Mutex<HashMap<String, Erc20TokenDetails>>>,
    /// Stores information about NFTs owned by the user. Each entry in the HashMap is uniquely identified by a composite key
    /// consisting of the token address and token ID, separated by a comma. This field is essential for tracking the NFT assets
    /// information (chain & contract type, amount etc.), where ownership and amount, in ERC1155 case, might change over time.
    pub nfts_infos: Arc<AsyncMutex<HashMap<String, NftInfo>>>,
    /// Config provided gas limits for swap and send transactions
    pub(crate) gas_limit: EthGasLimit,
    /// Config provided gas limits v2 for swap v2 transactions
    pub(crate) gas_limit_v2: EthGasLimitV2,
    /// This spawner is used to spawn coin's related futures that should be aborted on coin deactivation
    /// and on [`MmArc::stop`].
    pub abortable_system: AbortableQueue,
}

#[derive(Clone, Debug)]
pub struct Web3Instance {
    web3: Web3<Web3Transport>,
    is_parity: bool,
}

/// Information about a token that follows the ERC20 protocol on an EVM-based network.
#[derive(Clone, Debug)]
pub struct Erc20TokenDetails {
    /// The contract address of the token on the EVM-based network.
    pub token_address: Address,
    /// The number of decimal places the token uses.
    /// This represents the smallest unit that the token can be divided into.
    pub decimals: u8,
}

#[derive(Copy, Clone, Deserialize)]
pub struct SwapV2Contracts {
    pub maker_swap_v2_contract: Address,
    pub taker_swap_v2_contract: Address,
    pub nft_maker_swap_v2_contract: Address,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "format")]
pub enum EthAddressFormat {
    /// Single-case address (lowercase)
    #[serde(rename = "singlecase")]
    SingleCase,
    /// Mixed-case address.
    /// https://eips.ethereum.org/EIPS/eip-55
    #[serde(rename = "mixedcase")]
    MixedCase,
}

/// get tx type from pay_for_gas_option
/// currently only type2 and legacy supported
/// if for Eth Classic we also want support for type 1 then use a fn
#[macro_export]
macro_rules! tx_type_from_pay_for_gas_option {
    ($pay_for_gas_option: expr) => {
        if matches!($pay_for_gas_option, PayForGasOption::Eip1559(..)) {
            ethcore_transaction::TxType::Type2
        } else {
            ethcore_transaction::TxType::Legacy
        }
    };
}

impl EthCoinImpl {
    #[cfg(not(target_arch = "wasm32"))]
    fn eth_traces_path(&self, ctx: &MmArc, my_address: Address) -> PathBuf {
        ctx.dbdir()
            .join("TRANSACTIONS")
            .join(format!("{}_{:#02x}_trace.json", self.ticker, my_address))
    }

    /// Load saved ETH traces from local DB
    #[cfg(not(target_arch = "wasm32"))]
    fn load_saved_traces(&self, ctx: &MmArc, my_address: Address) -> Option<SavedTraces> {
        let content = gstuff::slurp(&self.eth_traces_path(ctx, my_address));
        if content.is_empty() {
            None
        } else {
            match json::from_slice(&content) {
                Ok(t) => Some(t),
                Err(_) => None,
            }
        }
    }

    /// Load saved ETH traces from local DB
    #[cfg(target_arch = "wasm32")]
    fn load_saved_traces(&self, _ctx: &MmArc, _my_address: Address) -> Option<SavedTraces> {
        common::panic_w("'load_saved_traces' is not implemented in WASM");
        unreachable!()
    }

    /// Store ETH traces to local DB
    #[cfg(not(target_arch = "wasm32"))]
    fn store_eth_traces(&self, ctx: &MmArc, my_address: Address, traces: &SavedTraces) {
        let content = json::to_vec(traces).unwrap();
        let tmp_file = format!("{}.tmp", self.eth_traces_path(ctx, my_address).display());
        std::fs::write(&tmp_file, content).unwrap();
        std::fs::rename(tmp_file, self.eth_traces_path(ctx, my_address)).unwrap();
    }

    /// Store ETH traces to local DB
    #[cfg(target_arch = "wasm32")]
    fn store_eth_traces(&self, _ctx: &MmArc, _my_address: Address, _traces: &SavedTraces) {
        common::panic_w("'store_eth_traces' is not implemented in WASM");
        unreachable!()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn erc20_events_path(&self, ctx: &MmArc, my_address: Address) -> PathBuf {
        ctx.dbdir()
            .join("TRANSACTIONS")
            .join(format!("{}_{:#02x}_events.json", self.ticker, my_address))
    }

    /// Store ERC20 events to local DB
    #[cfg(not(target_arch = "wasm32"))]
    fn store_erc20_events(&self, ctx: &MmArc, my_address: Address, events: &SavedErc20Events) {
        let content = json::to_vec(events).unwrap();
        let tmp_file = format!("{}.tmp", self.erc20_events_path(ctx, my_address).display());
        std::fs::write(&tmp_file, content).unwrap();
        std::fs::rename(tmp_file, self.erc20_events_path(ctx, my_address)).unwrap();
    }

    /// Store ERC20 events to local DB
    #[cfg(target_arch = "wasm32")]
    fn store_erc20_events(&self, _ctx: &MmArc, _my_address: Address, _events: &SavedErc20Events) {
        common::panic_w("'store_erc20_events' is not implemented in WASM");
        unreachable!()
    }

    /// Load saved ERC20 events from local DB
    #[cfg(not(target_arch = "wasm32"))]
    fn load_saved_erc20_events(&self, ctx: &MmArc, my_address: Address) -> Option<SavedErc20Events> {
        let content = gstuff::slurp(&self.erc20_events_path(ctx, my_address));
        if content.is_empty() {
            None
        } else {
            match json::from_slice(&content) {
                Ok(t) => Some(t),
                Err(_) => None,
            }
        }
    }

    /// Load saved ERC20 events from local DB
    #[cfg(target_arch = "wasm32")]
    fn load_saved_erc20_events(&self, _ctx: &MmArc, _my_address: Address) -> Option<SavedErc20Events> {
        common::panic_w("'load_saved_erc20_events' is not implemented in WASM");
        unreachable!()
    }

    /// The id used to differentiate payments on Etomic swap smart contract
    pub(crate) fn etomic_swap_id(&self, time_lock: u32, secret_hash: &[u8]) -> Vec<u8> {
        let timelock_bytes = time_lock.to_le_bytes();
        self.generate_etomic_swap_id(&timelock_bytes, secret_hash)
    }

    /// The id used to differentiate payments on Etomic swap v2 smart contracts
    pub(crate) fn etomic_swap_id_v2(&self, time_lock: u64, secret_hash: &[u8]) -> Vec<u8> {
        let timelock_bytes = time_lock.to_le_bytes();
        self.generate_etomic_swap_id(&timelock_bytes, secret_hash)
    }

    fn generate_etomic_swap_id(&self, time_lock_bytes: &[u8], secret_hash: &[u8]) -> Vec<u8> {
        let mut input = Vec::with_capacity(time_lock_bytes.len() + secret_hash.len());
        input.extend_from_slice(time_lock_bytes);
        input.extend_from_slice(secret_hash);
        sha256(&input).to_vec()
    }

    /// Try to parse address from string.
    pub fn address_from_str(&self, address: &str) -> Result<Address, String> {
        Ok(try_s!(valid_addr_from_str(address)))
    }

    pub fn erc20_token_address(&self) -> Option<Address> {
        match self.coin_type {
            EthCoinType::Erc20 { token_addr, .. } => Some(token_addr),
            EthCoinType::Eth | EthCoinType::Nft { .. } => None,
        }
    }

    pub fn add_erc_token_info(&self, ticker: String, info: Erc20TokenDetails) {
        self.erc20_tokens_infos.lock().unwrap().insert(ticker, info);
    }

    /// # Warning
    /// Be very careful using this function since it returns dereferenced clone
    /// of value behind the MutexGuard and makes it non-thread-safe.
    pub fn get_erc_tokens_infos(&self) -> HashMap<String, Erc20TokenDetails> {
        let guard = self.erc20_tokens_infos.lock().unwrap();
        (*guard).clone()
    }

    #[inline(always)]
    pub fn chain_id(&self) -> u64 { self.chain_id }
}

async fn get_raw_transaction_impl(coin: EthCoin, req: RawTransactionRequest) -> RawTransactionResult {
    let tx = match req.tx_hash.strip_prefix("0x") {
        Some(tx) => tx,
        None => &req.tx_hash,
    };
    let hash = H256::from_str(tx).map_to_mm(|e| RawTransactionError::InvalidHashError(e.to_string()))?;
    get_tx_hex_by_hash_impl(coin, hash).await
}

async fn get_tx_hex_by_hash_impl(coin: EthCoin, tx_hash: H256) -> RawTransactionResult {
    let web3_tx = coin
        .transaction(TransactionId::Hash(tx_hash))
        .await?
        .or_mm_err(|| RawTransactionError::HashNotExist(tx_hash.to_string()))?;
    let raw = signed_tx_from_web3_tx(web3_tx).map_to_mm(RawTransactionError::InternalError)?;
    Ok(RawTransactionRes {
        tx_hex: BytesJson(rlp::encode(&raw).to_vec()),
    })
}

async fn withdraw_impl(coin: EthCoin, req: WithdrawRequest) -> WithdrawResult {
    StandardEthWithdraw::new(coin.clone(), req)?.build().await
}

#[async_trait]
impl InitWithdrawCoin for EthCoin {
    async fn init_withdraw(
        &self,
        ctx: MmArc,
        req: WithdrawRequest,
        task_handle: WithdrawTaskHandleShared,
    ) -> Result<TransactionDetails, MmError<WithdrawError>> {
        InitEthWithdraw::new(ctx, self.clone(), req, task_handle)?.build().await
    }
}

/// `withdraw_erc1155` function returns details of `ERC-1155` transaction including tx hex,
/// which should be sent to`send_raw_transaction` RPC to broadcast the transaction.
pub async fn withdraw_erc1155(ctx: MmArc, withdraw_type: WithdrawErc1155) -> WithdrawNftResult {
    let coin = lp_coinfind_or_err(&ctx, withdraw_type.chain.to_ticker()).await?;
    let (to_addr, token_addr, eth_coin) =
        get_valid_nft_addr_to_withdraw(coin, &withdraw_type.to, &withdraw_type.token_address)?;

    let token_id_str = &withdraw_type.token_id.to_string();
    let wallet_erc1155_amount = eth_coin.erc1155_balance(token_addr, token_id_str).await?;

    let amount_uint = if withdraw_type.max {
        wallet_erc1155_amount.clone()
    } else {
        withdraw_type.amount.unwrap_or_else(|| BigUint::from(1u32))
    };

    if amount_uint > wallet_erc1155_amount {
        return MmError::err(WithdrawError::NotEnoughNftsAmount {
            token_address: withdraw_type.token_address,
            token_id: withdraw_type.token_id.to_string(),
            available: wallet_erc1155_amount,
            required: amount_uint,
        });
    }

    let my_address = eth_coin.derivation_method.single_addr_or_err().await?;
    let (eth_value, data, call_addr, fee_coin) = match eth_coin.coin_type {
        EthCoinType::Eth => {
            let function = ERC1155_CONTRACT.function("safeTransferFrom")?;
            let token_id_u256 =
                U256::from_dec_str(token_id_str).map_to_mm(|e| NumConversError::new(format!("{:?}", e)))?;
            let amount_u256 =
                U256::from_dec_str(&amount_uint.to_string()).map_to_mm(|e| NumConversError::new(format!("{:?}", e)))?;
            let data = function.encode_input(&[
                Token::Address(my_address),
                Token::Address(to_addr),
                Token::Uint(token_id_u256),
                Token::Uint(amount_u256),
                Token::Bytes("0x".into()),
            ])?;
            (0.into(), data, token_addr, eth_coin.ticker())
        },
        EthCoinType::Erc20 { .. } => {
            return MmError::err(WithdrawError::InternalError(
                "Erc20 coin type doesnt support withdraw nft".to_owned(),
            ))
        },
        EthCoinType::Nft { .. } => return MmError::err(WithdrawError::NftProtocolNotSupported),
    };
    let (gas, pay_for_gas_option) = get_eth_gas_details_from_withdraw_fee(
        &eth_coin,
        withdraw_type.fee,
        eth_value,
        data.clone().into(),
        my_address,
        call_addr,
        false,
    )
    .await?;
    let address_lock = eth_coin.get_address_lock(my_address.to_string()).await;
    let _nonce_lock = address_lock.lock().await;
    let (nonce, _) = eth_coin
        .clone()
        .get_addr_nonce(my_address)
        .compat()
        .timeout_secs(30.)
        .await?
        .map_to_mm(WithdrawError::Transport)?;

    let tx_type = tx_type_from_pay_for_gas_option!(pay_for_gas_option);
    if !eth_coin.is_tx_type_supported(&tx_type) {
        return MmError::err(WithdrawError::TxTypeNotSupported);
    }
    let tx_builder = UnSignedEthTxBuilder::new(tx_type, nonce, gas, Action::Call(call_addr), eth_value, data);
    let tx_builder = tx_builder_with_pay_for_gas_option(&eth_coin, tx_builder, &pay_for_gas_option)?;
    let tx = tx_builder
        .build()
        .map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    let secret = eth_coin.priv_key_policy.activated_key_or_err()?.secret();
    let signed = tx.sign(secret, Some(eth_coin.chain_id))?;
    let signed_bytes = rlp::encode(&signed);
    let fee_details = EthTxFeeDetails::new(gas, pay_for_gas_option, fee_coin)?;

    Ok(TransactionNftDetails {
        tx_hex: BytesJson::from(signed_bytes.to_vec()), // TODO: should we return tx_hex 0x-prefixed (everywhere)?
        tx_hash: format!("{:02x}", signed.tx_hash_as_bytes()), // TODO: add 0x hash (use unified hash format for eth wherever it is returned)
        from: vec![eth_coin.my_address()?],
        to: vec![withdraw_type.to],
        contract_type: ContractType::Erc1155,
        token_address: withdraw_type.token_address,
        token_id: withdraw_type.token_id,
        amount: amount_uint,
        fee_details: Some(fee_details.into()),
        coin: eth_coin.ticker.clone(),
        block_height: 0,
        timestamp: now_sec(),
        internal_id: 0,
        transaction_type: TransactionType::NftTransfer,
    })
}

/// `withdraw_erc721` function returns details of `ERC-721` transaction including tx hex,
/// which should be sent to`send_raw_transaction` RPC to broadcast the transaction.
pub async fn withdraw_erc721(ctx: MmArc, withdraw_type: WithdrawErc721) -> WithdrawNftResult {
    let coin = lp_coinfind_or_err(&ctx, withdraw_type.chain.to_ticker()).await?;
    let (to_addr, token_addr, eth_coin) =
        get_valid_nft_addr_to_withdraw(coin, &withdraw_type.to, &withdraw_type.token_address)?;

    let token_id_str = &withdraw_type.token_id.to_string();
    let token_owner = eth_coin.erc721_owner(token_addr, token_id_str).await?;
    let my_address = eth_coin.derivation_method.single_addr_or_err().await?;
    if token_owner != my_address {
        return MmError::err(WithdrawError::MyAddressNotNftOwner {
            my_address: my_address.display_address(),
            token_owner: token_owner.display_address(),
        });
    }

    let my_address = eth_coin.derivation_method.single_addr_or_err().await?;
    let (eth_value, data, call_addr, fee_coin) = match eth_coin.coin_type {
        EthCoinType::Eth => {
            let function = ERC721_CONTRACT.function("safeTransferFrom")?;
            let token_id_u256 = U256::from_dec_str(&withdraw_type.token_id.to_string())
                .map_to_mm(|e| NumConversError::new(format!("{:?}", e)))?;
            let data = function.encode_input(&[
                Token::Address(my_address),
                Token::Address(to_addr),
                Token::Uint(token_id_u256),
            ])?;
            (0.into(), data, token_addr, eth_coin.ticker())
        },
        EthCoinType::Erc20 { .. } => {
            return MmError::err(WithdrawError::InternalError(
                "Erc20 coin type doesnt support withdraw nft".to_owned(),
            ))
        },
        // TODO: start to use NFT GLOBAL TOKEN for withdraw
        EthCoinType::Nft { .. } => return MmError::err(WithdrawError::NftProtocolNotSupported),
    };
    let (gas, pay_for_gas_option) = get_eth_gas_details_from_withdraw_fee(
        &eth_coin,
        withdraw_type.fee,
        eth_value,
        data.clone().into(),
        my_address,
        call_addr,
        false,
    )
    .await?;

    let address_lock = eth_coin.get_address_lock(my_address.to_string()).await;
    let _nonce_lock = address_lock.lock().await;
    let (nonce, _) = eth_coin
        .clone()
        .get_addr_nonce(my_address)
        .compat()
        .timeout_secs(30.)
        .await?
        .map_to_mm(WithdrawError::Transport)?;

    let tx_type = tx_type_from_pay_for_gas_option!(pay_for_gas_option);
    if !eth_coin.is_tx_type_supported(&tx_type) {
        return MmError::err(WithdrawError::TxTypeNotSupported);
    }
    let tx_builder = UnSignedEthTxBuilder::new(tx_type, nonce, gas, Action::Call(call_addr), eth_value, data);
    let tx_builder = tx_builder_with_pay_for_gas_option(&eth_coin, tx_builder, &pay_for_gas_option)?;
    let tx = tx_builder
        .build()
        .map_to_mm(|e| WithdrawError::InternalError(e.to_string()))?;
    let secret = eth_coin.priv_key_policy.activated_key_or_err()?.secret();
    let signed = tx.sign(secret, Some(eth_coin.chain_id))?;
    let signed_bytes = rlp::encode(&signed);
    let fee_details = EthTxFeeDetails::new(gas, pay_for_gas_option, fee_coin)?;

    Ok(TransactionNftDetails {
        tx_hex: BytesJson::from(signed_bytes.to_vec()),
        tx_hash: format!("{:02x}", signed.tx_hash_as_bytes()), // TODO: add 0x hash (use unified hash format for eth wherever it is returned)
        from: vec![eth_coin.my_address()?],
        to: vec![withdraw_type.to],
        contract_type: ContractType::Erc721,
        token_address: withdraw_type.token_address,
        token_id: withdraw_type.token_id,
        amount: BigUint::from(1u8),
        fee_details: Some(fee_details.into()),
        coin: eth_coin.ticker.clone(),
        block_height: 0,
        timestamp: now_sec(),
        internal_id: 0,
        transaction_type: TransactionType::NftTransfer,
    })
}

#[derive(Clone)]
pub struct EthCoin(Arc<EthCoinImpl>);
impl Deref for EthCoin {
    type Target = EthCoinImpl;
    fn deref(&self) -> &EthCoinImpl { &self.0 }
}

#[async_trait]
impl SwapOps for EthCoin {
    async fn send_taker_fee(&self, dex_fee: DexFee, _uuid: &[u8], _expire_at: u64) -> TransactionResult {
        let address = try_tx_s!(addr_from_raw_pubkey(self.dex_pubkey()));
        self.send_to_address(
            address,
            try_tx_s!(wei_from_big_decimal(&dex_fee.fee_amount().into(), self.decimals)),
        )
        .map(TransactionEnum::from)
        .compat()
        .await
    }

    async fn send_maker_payment(&self, maker_payment_args: SendPaymentArgs<'_>) -> TransactionResult {
        self.send_hash_time_locked_payment(maker_payment_args)
            .compat()
            .await
            .map(TransactionEnum::from)
    }

    async fn send_taker_payment(&self, taker_payment_args: SendPaymentArgs<'_>) -> TransactionResult {
        self.send_hash_time_locked_payment(taker_payment_args)
            .map(TransactionEnum::from)
            .compat()
            .await
    }

    async fn send_maker_spends_taker_payment(
        &self,
        maker_spends_payment_args: SpendPaymentArgs<'_>,
    ) -> TransactionResult {
        self.spend_hash_time_locked_payment(maker_spends_payment_args)
            .await
            .map(TransactionEnum::from)
    }

    async fn send_taker_spends_maker_payment(
        &self,
        taker_spends_payment_args: SpendPaymentArgs<'_>,
    ) -> TransactionResult {
        self.spend_hash_time_locked_payment(taker_spends_payment_args)
            .await
            .map(TransactionEnum::from)
    }

    async fn send_taker_refunds_payment(&self, taker_refunds_payment_args: RefundPaymentArgs<'_>) -> TransactionResult {
        self.refund_hash_time_locked_payment(taker_refunds_payment_args)
            .await
            .map(TransactionEnum::from)
    }

    async fn send_maker_refunds_payment(&self, maker_refunds_payment_args: RefundPaymentArgs<'_>) -> TransactionResult {
        self.refund_hash_time_locked_payment(maker_refunds_payment_args)
            .await
            .map(TransactionEnum::from)
    }

    async fn validate_fee(&self, validate_fee_args: ValidateFeeArgs<'_>) -> ValidatePaymentResult<()> {
        let tx = match validate_fee_args.fee_tx {
            TransactionEnum::SignedEthTx(t) => t.clone(),
            fee_tx => {
                return MmError::err(ValidatePaymentError::InternalError(format!(
                    "Invalid fee tx type. fee tx: {:?}",
                    fee_tx
                )))
            },
        };
        validate_fee_impl(self.clone(), EthValidateFeeArgs {
            fee_tx_hash: &tx.tx_hash(),
            expected_sender: validate_fee_args.expected_sender,
            amount: &validate_fee_args.dex_fee.fee_amount().into(),
            min_block_number: validate_fee_args.min_block_number,
            uuid: validate_fee_args.uuid,
        })
        .compat()
        .await
    }

    #[inline]
    async fn validate_maker_payment(&self, input: ValidatePaymentInput) -> ValidatePaymentResult<()> {
        self.validate_payment(input).compat().await
    }

    #[inline]
    async fn validate_taker_payment(&self, input: ValidatePaymentInput) -> ValidatePaymentResult<()> {
        self.validate_payment(input).compat().await
    }

    async fn check_if_my_payment_sent(
        &self,
        if_my_payment_sent_args: CheckIfMyPaymentSentArgs<'_>,
    ) -> Result<Option<TransactionEnum>, String> {
        let time_lock = if_my_payment_sent_args
            .time_lock
            .try_into()
            .map_err(|e: TryFromIntError| e.to_string())?;
        let id = self.etomic_swap_id(time_lock, if_my_payment_sent_args.secret_hash);
        let swap_contract_address = if_my_payment_sent_args.swap_contract_address.try_to_address()?;
        let from_block = if_my_payment_sent_args.search_from_block;
        let status = self
            .payment_status(swap_contract_address, Token::FixedBytes(id.clone()))
            .compat()
            .await?;

        if status == U256::from(PaymentState::Uninitialized as u8) {
            return Ok(None);
        };

        let mut current_block = self.current_block().compat().await?;
        if current_block < from_block {
            current_block = from_block;
        }

        let mut from_block = from_block;

        loop {
            let to_block = current_block.min(from_block + self.logs_block_range);

            let events = self
                .payment_sent_events(swap_contract_address, from_block, to_block)
                .compat()
                .await?;

            let found = events.iter().find(|event| &event.data.0[..32] == id.as_slice());

            match found {
                Some(event) => {
                    let transaction = try_s!(
                        self.transaction(TransactionId::Hash(event.transaction_hash.unwrap()))
                            .await
                    );
                    match transaction {
                        Some(t) => break Ok(Some(try_s!(signed_tx_from_web3_tx(t)).into())),
                        None => break Ok(None),
                    }
                },
                None => {
                    if to_block >= current_block {
                        break Ok(None);
                    }
                    from_block = to_block;
                },
            }
        }
    }

    async fn search_for_swap_tx_spend_my(
        &self,
        input: SearchForSwapTxSpendInput<'_>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let swap_contract_address = try_s!(input.swap_contract_address.try_to_address());
        self.search_for_swap_tx_spend(
            input.tx,
            swap_contract_address,
            input.secret_hash,
            input.search_from_block,
            input.watcher_reward,
        )
        .await
    }

    async fn search_for_swap_tx_spend_other(
        &self,
        input: SearchForSwapTxSpendInput<'_>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let swap_contract_address = try_s!(input.swap_contract_address.try_to_address());
        self.search_for_swap_tx_spend(
            input.tx,
            swap_contract_address,
            input.secret_hash,
            input.search_from_block,
            input.watcher_reward,
        )
        .await
    }

    async fn extract_secret(
        &self,
        _secret_hash: &[u8],
        spend_tx: &[u8],
        watcher_reward: bool,
    ) -> Result<[u8; 32], String> {
        let unverified: UnverifiedTransactionWrapper = try_s!(rlp::decode(spend_tx));
        let function_name = get_function_name("receiverSpend", watcher_reward);
        let function = try_s!(SWAP_CONTRACT.function(&function_name));

        // Validate contract call; expected to be receiverSpend.
        // https://www.4byte.directory/signatures/?bytes4_signature=02ed292b.
        let expected_signature = function.short_signature();
        let actual_signature = &unverified.unsigned().data()[0..4];
        if actual_signature != expected_signature {
            return ERR!(
                "Expected 'receiverSpend' contract call signature: {:?}, found {:?}",
                expected_signature,
                actual_signature
            );
        };

        let tokens = try_s!(decode_contract_call(function, unverified.unsigned().data()));
        if tokens.len() < 3 {
            return ERR!("Invalid arguments in 'receiverSpend' call: {:?}", tokens);
        }
        match &tokens[2] {
            Token::FixedBytes(secret) => Ok(try_s!(secret.as_slice().try_into())),
            _ => ERR!(
                "Expected secret to be fixed bytes, decoded function data is {:?}",
                tokens
            ),
        }
    }

    fn negotiate_swap_contract_addr(
        &self,
        other_side_address: Option<&[u8]>,
    ) -> Result<Option<BytesJson>, MmError<NegotiateSwapContractAddrErr>> {
        match other_side_address {
            Some(bytes) => {
                if bytes.len() != 20 {
                    return MmError::err(NegotiateSwapContractAddrErr::InvalidOtherAddrLen(bytes.into()));
                }
                let other_addr = Address::from_slice(bytes);

                if other_addr == self.swap_contract_address {
                    return Ok(Some(self.swap_contract_address.0.to_vec().into()));
                }

                if Some(other_addr) == self.fallback_swap_contract {
                    return Ok(self.fallback_swap_contract.map(|addr| addr.0.to_vec().into()));
                }
                MmError::err(NegotiateSwapContractAddrErr::UnexpectedOtherAddr(bytes.into()))
            },
            None => self
                .fallback_swap_contract
                .map(|addr| Some(addr.0.to_vec().into()))
                .ok_or_else(|| MmError::new(NegotiateSwapContractAddrErr::NoOtherAddrAndNoFallback)),
        }
    }

    #[inline]
    fn derive_htlc_key_pair(&self, _swap_unique_data: &[u8]) -> keys::KeyPair {
        match self.priv_key_policy {
            EthPrivKeyPolicy::Iguana(ref key_pair)
            | EthPrivKeyPolicy::HDWallet {
                activated_key: ref key_pair,
                ..
            } => key_pair_from_secret(key_pair.secret().as_fixed_bytes()).expect("valid key"),
            EthPrivKeyPolicy::Trezor => todo!(),
            #[cfg(target_arch = "wasm32")]
            EthPrivKeyPolicy::Metamask(_) => todo!(),
        }
    }

    #[inline]
    fn derive_htlc_pubkey(&self, _swap_unique_data: &[u8]) -> [u8; 33] {
        match self.priv_key_policy {
            EthPrivKeyPolicy::Iguana(ref key_pair)
            | EthPrivKeyPolicy::HDWallet {
                activated_key: ref key_pair,
                ..
            } => key_pair_from_secret(&key_pair.secret().to_fixed_bytes())
                .expect("valid key")
                .public_slice()
                .try_into()
                .expect("valid key length!"),
            EthPrivKeyPolicy::Trezor => todo!(),
            #[cfg(target_arch = "wasm32")]
            EthPrivKeyPolicy::Metamask(ref metamask_policy) => metamask_policy.public_key.0,
        }
    }

    fn validate_other_pubkey(&self, raw_pubkey: &[u8]) -> MmResult<(), ValidateOtherPubKeyErr> {
        if let Err(e) = PublicKey::from_slice(raw_pubkey) {
            return MmError::err(ValidateOtherPubKeyErr::InvalidPubKey(e.to_string()));
        };
        Ok(())
    }

    async fn maker_payment_instructions(
        &self,
        args: PaymentInstructionArgs<'_>,
    ) -> Result<Option<Vec<u8>>, MmError<PaymentInstructionsErr>> {
        let watcher_reward = if args.watcher_reward {
            Some(
                self.get_watcher_reward_amount(args.wait_until)
                    .await
                    .map_err(|err| PaymentInstructionsErr::WatcherRewardErr(err.get_inner().to_string()))?
                    .to_string()
                    .into_bytes(),
            )
        } else {
            None
        };
        Ok(watcher_reward)
    }

    async fn taker_payment_instructions(
        &self,
        _args: PaymentInstructionArgs<'_>,
    ) -> Result<Option<Vec<u8>>, MmError<PaymentInstructionsErr>> {
        Ok(None)
    }

    fn validate_maker_payment_instructions(
        &self,
        instructions: &[u8],
        _args: PaymentInstructionArgs,
    ) -> Result<PaymentInstructions, MmError<ValidateInstructionsErr>> {
        let watcher_reward = BigDecimal::from_str(
            &String::from_utf8(instructions.to_vec())
                .map_err(|err| ValidateInstructionsErr::DeserializationErr(err.to_string()))?,
        )
        .map_err(|err| ValidateInstructionsErr::DeserializationErr(err.to_string()))?;

        // TODO: Reward can be validated here
        Ok(PaymentInstructions::WatcherReward(watcher_reward))
    }

    fn validate_taker_payment_instructions(
        &self,
        _instructions: &[u8],
        _args: PaymentInstructionArgs,
    ) -> Result<PaymentInstructions, MmError<ValidateInstructionsErr>> {
        MmError::err(ValidateInstructionsErr::UnsupportedCoin(self.ticker().to_string()))
    }

    fn is_supported_by_watchers(&self) -> bool {
        std::env::var("USE_WATCHER_REWARD").is_ok()
        //self.contract_supports_watchers
    }
}

#[async_trait]
impl WatcherOps for EthCoin {
    fn send_maker_payment_spend_preimage(&self, input: SendMakerPaymentSpendPreimageInput) -> TransactionFut {
        Box::new(
            self.watcher_spends_hash_time_locked_payment(input)
                .map(TransactionEnum::from),
        )
    }

    fn create_maker_payment_spend_preimage(
        &self,
        maker_payment_tx: &[u8],
        _time_lock: u64,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _swap_unique_data: &[u8],
    ) -> TransactionFut {
        let tx: UnverifiedTransactionWrapper = try_tx_fus!(rlp::decode(maker_payment_tx));
        let signed = try_tx_fus!(SignedEthTx::new(tx));
        let fut = async move { Ok(TransactionEnum::from(signed)) };

        Box::new(fut.boxed().compat())
    }

    fn create_taker_payment_refund_preimage(
        &self,
        taker_payment_tx: &[u8],
        _time_lock: u64,
        _maker_pub: &[u8],
        _secret_hash: &[u8],
        _swap_contract_address: &Option<BytesJson>,
        _swap_unique_data: &[u8],
    ) -> TransactionFut {
        let tx: UnverifiedTransactionWrapper = try_tx_fus!(rlp::decode(taker_payment_tx));
        let signed = try_tx_fus!(SignedEthTx::new(tx));
        let fut = async move { Ok(TransactionEnum::from(signed)) };

        Box::new(fut.boxed().compat())
    }

    fn send_taker_payment_refund_preimage(&self, args: RefundPaymentArgs) -> TransactionFut {
        Box::new(
            self.watcher_refunds_hash_time_locked_payment(args)
                .map(TransactionEnum::from),
        )
    }

    fn watcher_validate_taker_fee(&self, validate_fee_args: WatcherValidateTakerFeeInput) -> ValidatePaymentFut<()> {
        validate_fee_impl(self.clone(), EthValidateFeeArgs {
            fee_tx_hash: &H256::from_slice(validate_fee_args.taker_fee_hash.as_slice()),
            expected_sender: &validate_fee_args.sender_pubkey,
            amount: &BigDecimal::from(0),
            min_block_number: validate_fee_args.min_block_number,
            uuid: &[],
        })

        // TODO: Add validations specific for watchers
        // 1.Validate if taker fee is old
    }

    fn taker_validates_payment_spend_or_refund(&self, input: ValidateWatcherSpendInput) -> ValidatePaymentFut<()> {
        let watcher_reward = try_f!(input
            .watcher_reward
            .clone()
            .ok_or_else(|| ValidatePaymentError::WatcherRewardError("Watcher reward not found".to_string())));
        let expected_reward_amount = try_f!(wei_from_big_decimal(&watcher_reward.amount, ETH_DECIMALS));

        let expected_swap_contract_address = try_f!(input
            .swap_contract_address
            .try_to_address()
            .map_to_mm(ValidatePaymentError::InvalidParameter));

        let unsigned: UnverifiedTransactionWrapper = try_f!(rlp::decode(&input.payment_tx));
        let tx =
            try_f!(SignedEthTx::new(unsigned)
                .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string())));

        let selfi = self.clone();
        let time_lock = try_f!(input
            .time_lock
            .try_into()
            .map_to_mm(ValidatePaymentError::TimelockOverflow));
        let swap_id = selfi.etomic_swap_id(time_lock, &input.secret_hash);
        let decimals = self.decimals;
        let secret_hash = if input.secret_hash.len() == 32 {
            ripemd160(&input.secret_hash).to_vec()
        } else {
            input.secret_hash.to_vec()
        };
        let maker_addr =
            try_f!(addr_from_raw_pubkey(&input.maker_pub).map_to_mm(ValidatePaymentError::InvalidParameter));

        let trade_amount = try_f!(wei_from_big_decimal(&(input.amount), decimals));
        let fut = async move {
            match tx.unsigned().action() {
                Call(contract_address) => {
                    if *contract_address != expected_swap_contract_address {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Transaction {:?} was sent to wrong address, expected {:?}",
                            contract_address, expected_swap_contract_address,
                        )));
                    }
                },
                Create => {
                    return MmError::err(ValidatePaymentError::WrongPaymentTx(
                        "Tx action must be Call, found Create instead".to_string(),
                    ));
                },
            };

            let actual_status = selfi
                .payment_status(expected_swap_contract_address, Token::FixedBytes(swap_id.clone()))
                .compat()
                .await
                .map_to_mm(ValidatePaymentError::Transport)?;
            let expected_status = match input.spend_type {
                WatcherSpendType::MakerPaymentSpend => U256::from(PaymentState::Spent as u8),
                WatcherSpendType::TakerPaymentRefund => U256::from(PaymentState::Refunded as u8),
            };
            if actual_status != expected_status {
                return MmError::err(ValidatePaymentError::UnexpectedPaymentState(format!(
                    "Payment state is not {}, got {}",
                    expected_status, actual_status
                )));
            }

            let function_name = match input.spend_type {
                WatcherSpendType::MakerPaymentSpend => get_function_name("receiverSpend", true),
                WatcherSpendType::TakerPaymentRefund => get_function_name("senderRefund", true),
            };
            let function = SWAP_CONTRACT
                .function(&function_name)
                .map_to_mm(|err| ValidatePaymentError::InternalError(err.to_string()))?;

            let decoded = decode_contract_call(function, tx.unsigned().data())
                .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string()))?;

            let swap_id_input = get_function_input_data(&decoded, function, 0)
                .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
            if swap_id_input != Token::FixedBytes(swap_id.clone()) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction invalid swap_id arg {:?}, expected {:?}",
                    swap_id_input,
                    Token::FixedBytes(swap_id.clone())
                )));
            }

            let hash_input = match input.spend_type {
                WatcherSpendType::MakerPaymentSpend => {
                    let secret_input = get_function_input_data(&decoded, function, 2)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?
                        .into_fixed_bytes()
                        .ok_or_else(|| {
                            ValidatePaymentError::WrongPaymentTx("Invalid type for secret hash argument".to_string())
                        })?;
                    dhash160(&secret_input).to_vec()
                },
                WatcherSpendType::TakerPaymentRefund => get_function_input_data(&decoded, function, 2)
                    .map_to_mm(ValidatePaymentError::TxDeserializationError)?
                    .into_fixed_bytes()
                    .ok_or_else(|| {
                        ValidatePaymentError::WrongPaymentTx("Invalid type for secret argument".to_string())
                    })?,
            };
            if hash_input != secret_hash {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction secret or secret_hash arg {:?} is invalid, expected {:?}",
                    hash_input,
                    Token::FixedBytes(secret_hash),
                )));
            }

            let my_address = selfi.derivation_method.single_addr_or_err().await?;
            let sender_input = get_function_input_data(&decoded, function, 4)
                .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
            let expected_sender = match input.spend_type {
                WatcherSpendType::MakerPaymentSpend => maker_addr,
                WatcherSpendType::TakerPaymentRefund => my_address,
            };
            if sender_input != Token::Address(expected_sender) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction sender arg {:?} is invalid, expected {:?}",
                    sender_input,
                    Token::Address(expected_sender)
                )));
            }

            let receiver_input = get_function_input_data(&decoded, function, 5)
                .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
            let expected_receiver = match input.spend_type {
                WatcherSpendType::MakerPaymentSpend => my_address,
                WatcherSpendType::TakerPaymentRefund => maker_addr,
            };
            if receiver_input != Token::Address(expected_receiver) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction receiver arg {:?} is invalid, expected {:?}",
                    receiver_input,
                    Token::Address(expected_receiver)
                )));
            }

            let reward_target_input = get_function_input_data(&decoded, function, 6)
                .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
            if reward_target_input != Token::Uint(U256::from(watcher_reward.reward_target as u8)) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction reward target arg {:?} is invalid, expected {:?}",
                    reward_target_input,
                    Token::Uint(U256::from(watcher_reward.reward_target as u8))
                )));
            }

            let contract_reward_input = get_function_input_data(&decoded, function, 7)
                .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
            if contract_reward_input != Token::Bool(watcher_reward.send_contract_reward_on_spend) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction sends contract reward on spend arg {:?} is invalid, expected {:?}",
                    contract_reward_input,
                    Token::Bool(watcher_reward.send_contract_reward_on_spend)
                )));
            }

            let reward_amount_input = get_function_input_data(&decoded, function, 8)
                .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
            if reward_amount_input != Token::Uint(expected_reward_amount) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction watcher reward amount arg {:?} is invalid, expected {:?}",
                    reward_amount_input,
                    Token::Uint(expected_reward_amount)
                )));
            }

            if tx.unsigned().value() != U256::zero() {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Transaction value arg {:?} is invalid, expected 0",
                    tx.unsigned().value()
                )));
            }

            match &selfi.coin_type {
                EthCoinType::Eth => {
                    let amount_input = get_function_input_data(&decoded, function, 1)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    let total_amount = match input.spend_type {
                        WatcherSpendType::MakerPaymentSpend => {
                            if !matches!(watcher_reward.reward_target, RewardTarget::None)
                                || watcher_reward.send_contract_reward_on_spend
                            {
                                trade_amount + expected_reward_amount
                            } else {
                                trade_amount
                            }
                        },
                        WatcherSpendType::TakerPaymentRefund => trade_amount + expected_reward_amount,
                    };
                    if amount_input != Token::Uint(total_amount) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Transaction amount arg {:?} is invalid, expected {:?}",
                            amount_input,
                            Token::Uint(total_amount),
                        )));
                    }

                    let token_address_input = get_function_input_data(&decoded, function, 3)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if token_address_input != Token::Address(Address::default()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Transaction token address arg {:?} is invalid, expected {:?}",
                            token_address_input,
                            Token::Address(Address::default()),
                        )));
                    }
                },
                EthCoinType::Erc20 {
                    platform: _,
                    token_addr,
                } => {
                    let amount_input = get_function_input_data(&decoded, function, 1)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if amount_input != Token::Uint(trade_amount) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Transaction amount arg {:?} is invalid, expected {:?}",
                            amount_input,
                            Token::Uint(trade_amount),
                        )));
                    }

                    let token_address_input = get_function_input_data(&decoded, function, 3)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if token_address_input != Token::Address(*token_addr) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Transaction token address arg {:?} is invalid, expected {:?}",
                            token_address_input,
                            Token::Address(*token_addr),
                        )));
                    }
                },
                EthCoinType::Nft { .. } => {
                    return MmError::err(ValidatePaymentError::ProtocolNotSupported(
                        "Nft protocol is not supported by watchers yet".to_string(),
                    ))
                },
            }

            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    fn watcher_validate_taker_payment(&self, input: WatcherValidatePaymentInput) -> ValidatePaymentFut<()> {
        let unsigned: UnverifiedTransactionWrapper = try_f!(rlp::decode(&input.payment_tx));
        let tx =
            try_f!(SignedEthTx::new(unsigned)
                .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string())));
        let sender = try_f!(addr_from_raw_pubkey(&input.taker_pub).map_to_mm(ValidatePaymentError::InvalidParameter));
        let receiver = try_f!(addr_from_raw_pubkey(&input.maker_pub).map_to_mm(ValidatePaymentError::InvalidParameter));
        let time_lock = try_f!(input
            .time_lock
            .try_into()
            .map_to_mm(ValidatePaymentError::TimelockOverflow));

        let selfi = self.clone();
        let swap_id = selfi.etomic_swap_id(time_lock, &input.secret_hash);
        let secret_hash = if input.secret_hash.len() == 32 {
            ripemd160(&input.secret_hash).to_vec()
        } else {
            input.secret_hash.to_vec()
        };
        let expected_swap_contract_address = self.swap_contract_address;
        let fallback_swap_contract = self.fallback_swap_contract;

        let fut = async move {
            let tx_from_rpc = selfi.transaction(TransactionId::Hash(tx.tx_hash())).await?;

            let tx_from_rpc = tx_from_rpc.as_ref().ok_or_else(|| {
                ValidatePaymentError::TxDoesNotExist(format!("Didn't find provided tx {:?} on ETH node", tx))
            })?;

            if tx_from_rpc.from != Some(sender) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "{INVALID_SENDER_ERR_LOG}: Payment tx {tx_from_rpc:?} was sent from wrong address, expected {sender:?}"
                )));
            }

            let swap_contract_address = tx_from_rpc.to.ok_or_else(|| {
                ValidatePaymentError::TxDeserializationError(format!(
                    "Swap contract address not found in payment Tx {tx_from_rpc:?}"
                ))
            })?;

            if swap_contract_address != expected_swap_contract_address
                && Some(swap_contract_address) != fallback_swap_contract
            {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "{INVALID_CONTRACT_ADDRESS_ERR_LOG}: Payment tx {tx_from_rpc:?} was sent to wrong address, expected either {expected_swap_contract_address:?} or the fallback {fallback_swap_contract:?}"
                )));
            }

            let status = selfi
                .payment_status(swap_contract_address, Token::FixedBytes(swap_id.clone()))
                .compat()
                .await
                .map_to_mm(ValidatePaymentError::Transport)?;
            if status != U256::from(PaymentState::Sent as u8) && status != U256::from(PaymentState::Spent as u8) {
                return MmError::err(ValidatePaymentError::UnexpectedPaymentState(format!(
                    "{INVALID_PAYMENT_STATE_ERR_LOG}: Payment state is not PAYMENT_STATE_SENT or PAYMENT_STATE_SPENT, got {status}"
                )));
            }

            let watcher_reward = selfi
                .get_taker_watcher_reward(&input.maker_coin, None, None, None, input.wait_until)
                .await
                .map_err(|err| ValidatePaymentError::WatcherRewardError(err.into_inner().to_string()))?;
            let expected_reward_amount = wei_from_big_decimal(&watcher_reward.amount, ETH_DECIMALS)?;

            match &selfi.coin_type {
                EthCoinType::Eth => {
                    let function_name = get_function_name("ethPayment", true);
                    let function = SWAP_CONTRACT
                        .function(&function_name)
                        .map_to_mm(|err| ValidatePaymentError::InternalError(err.to_string()))?;
                    let decoded = decode_contract_call(function, &tx_from_rpc.input.0)
                        .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string()))?;

                    let swap_id_input = get_function_input_data(&decoded, function, 0)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if swap_id_input != Token::FixedBytes(swap_id.clone()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "{INVALID_SWAP_ID_ERR_LOG}: Invalid 'swap_id' {decoded:?}, expected {swap_id:?}"
                        )));
                    }

                    let receiver_input = get_function_input_data(&decoded, function, 1)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if receiver_input != Token::Address(receiver) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "{INVALID_RECEIVER_ERR_LOG}: Payment tx receiver arg {receiver_input:?} is invalid, expected {:?}", Token::Address(receiver)
                        )));
                    }

                    let secret_hash_input = get_function_input_data(&decoded, function, 2)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if secret_hash_input != Token::FixedBytes(secret_hash.to_vec()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx secret_hash arg {:?} is invalid, expected {:?}",
                            secret_hash_input,
                            Token::FixedBytes(secret_hash.to_vec()),
                        )));
                    }

                    let time_lock_input = get_function_input_data(&decoded, function, 3)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if time_lock_input != Token::Uint(U256::from(input.time_lock)) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx time_lock arg {:?} is invalid, expected {:?}",
                            time_lock_input,
                            Token::Uint(U256::from(input.time_lock)),
                        )));
                    }

                    let reward_target_input = get_function_input_data(&decoded, function, 4)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    let expected_reward_target = watcher_reward.reward_target as u8;
                    if reward_target_input != Token::Uint(U256::from(expected_reward_target)) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx reward target arg {:?} is invalid, expected {:?}",
                            reward_target_input, expected_reward_target
                        )));
                    }

                    let sends_contract_reward_input = get_function_input_data(&decoded, function, 5)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if sends_contract_reward_input != Token::Bool(watcher_reward.send_contract_reward_on_spend) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx sends_contract_reward_on_spend arg {:?} is invalid, expected {:?}",
                            sends_contract_reward_input, watcher_reward.send_contract_reward_on_spend
                        )));
                    }

                    let reward_amount_input = get_function_input_data(&decoded, function, 6)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?
                        .into_uint()
                        .ok_or_else(|| {
                            ValidatePaymentError::WrongPaymentTx("Invalid type for reward amount argument".to_string())
                        })?;

                    validate_watcher_reward(expected_reward_amount.as_u64(), reward_amount_input.as_u64(), false)?;

                    // TODO: Validate the value
                },
                EthCoinType::Erc20 {
                    platform: _,
                    token_addr,
                } => {
                    let function_name = get_function_name("erc20Payment", true);
                    let function = SWAP_CONTRACT
                        .function(&function_name)
                        .map_to_mm(|err| ValidatePaymentError::InternalError(err.to_string()))?;
                    let decoded = decode_contract_call(function, &tx_from_rpc.input.0)
                        .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string()))?;

                    let swap_id_input = get_function_input_data(&decoded, function, 0)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if swap_id_input != Token::FixedBytes(swap_id.clone()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "{INVALID_SWAP_ID_ERR_LOG}: Invalid 'swap_id' {decoded:?}, expected {swap_id:?}"
                        )));
                    }

                    let token_addr_input = get_function_input_data(&decoded, function, 2)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if token_addr_input != Token::Address(*token_addr) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx token_addr arg {:?} is invalid, expected {:?}",
                            token_addr_input,
                            Token::Address(*token_addr)
                        )));
                    }

                    let receiver_addr_input = get_function_input_data(&decoded, function, 3)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if receiver_addr_input != Token::Address(receiver) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "{INVALID_RECEIVER_ERR_LOG}: Payment tx receiver arg {receiver_addr_input:?} is invalid, expected {:?}", Token::Address(receiver),
                        )));
                    }

                    let secret_hash_input = get_function_input_data(&decoded, function, 4)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if secret_hash_input != Token::FixedBytes(secret_hash.to_vec()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx secret_hash arg {:?} is invalid, expected {:?}",
                            secret_hash_input,
                            Token::FixedBytes(secret_hash.to_vec()),
                        )));
                    }

                    let time_lock_input = get_function_input_data(&decoded, function, 5)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if time_lock_input != Token::Uint(U256::from(input.time_lock)) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx time_lock arg {:?} is invalid, expected {:?}",
                            time_lock_input,
                            Token::Uint(U256::from(input.time_lock)),
                        )));
                    }

                    let reward_target_input = get_function_input_data(&decoded, function, 6)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    let expected_reward_target = watcher_reward.reward_target as u8;
                    if reward_target_input != Token::Uint(U256::from(expected_reward_target)) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx reward target arg {:?} is invalid, expected {:?}",
                            reward_target_input, expected_reward_target
                        )));
                    }

                    let sends_contract_reward_input = get_function_input_data(&decoded, function, 7)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?;
                    if sends_contract_reward_input != Token::Bool(watcher_reward.send_contract_reward_on_spend) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx sends_contract_reward_on_spend arg {:?} is invalid, expected {:?}",
                            sends_contract_reward_input, watcher_reward.send_contract_reward_on_spend
                        )));
                    }

                    let reward_amount_input = get_function_input_data(&decoded, function, 8)
                        .map_to_mm(ValidatePaymentError::TxDeserializationError)?
                        .into_uint()
                        .ok_or_else(|| {
                            ValidatePaymentError::WrongPaymentTx("Invalid type for reward amount argument".to_string())
                        })?;

                    validate_watcher_reward(expected_reward_amount.as_u64(), reward_amount_input.as_u64(), false)?;

                    if tx_from_rpc.value != reward_amount_input {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx value arg {:?} is invalid, expected {:?}",
                            tx_from_rpc.value, reward_amount_input
                        )));
                    }
                },
                EthCoinType::Nft { .. } => {
                    return MmError::err(ValidatePaymentError::ProtocolNotSupported(
                        "Nft protocol is not supported by watchers yet".to_string(),
                    ))
                },
            }

            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    async fn watcher_search_for_swap_tx_spend(
        &self,
        input: WatcherSearchForSwapTxSpendInput<'_>,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let unverified: UnverifiedTransactionWrapper = try_s!(rlp::decode(input.tx));
        let tx = try_s!(SignedEthTx::new(unverified));
        let swap_contract_address = match tx.unsigned().action() {
            Call(address) => *address,
            Create => return Err(ERRL!("Invalid payment action: the payment action cannot be create")),
        };

        self.search_for_swap_tx_spend(
            input.tx,
            swap_contract_address,
            input.secret_hash,
            input.search_from_block,
            true,
        )
        .await
    }

    async fn get_taker_watcher_reward(
        &self,
        other_coin: &MmCoinEnum,
        _coin_amount: Option<BigDecimal>,
        _other_coin_amount: Option<BigDecimal>,
        reward_amount: Option<BigDecimal>,
        wait_until: u64,
    ) -> Result<WatcherReward, MmError<WatcherRewardError>> {
        let reward_target = if other_coin.is_eth() {
            RewardTarget::Contract
        } else {
            RewardTarget::PaymentSender
        };

        let amount = match reward_amount {
            Some(amount) => amount,
            None => self.get_watcher_reward_amount(wait_until).await?,
        };

        let send_contract_reward_on_spend = false;

        Ok(WatcherReward {
            amount,
            is_exact_amount: false,
            reward_target,
            send_contract_reward_on_spend,
        })
    }

    async fn get_maker_watcher_reward(
        &self,
        other_coin: &MmCoinEnum,
        reward_amount: Option<BigDecimal>,
        wait_until: u64,
    ) -> Result<Option<WatcherReward>, MmError<WatcherRewardError>> {
        let reward_target = if other_coin.is_eth() {
            RewardTarget::None
        } else {
            RewardTarget::PaymentSpender
        };

        let is_exact_amount = reward_amount.is_some();
        let amount = match reward_amount {
            Some(amount) => amount,
            None => {
                let gas_cost_eth = self.get_watcher_reward_amount(wait_until).await?;

                match &self.coin_type {
                    EthCoinType::Eth => gas_cost_eth,
                    EthCoinType::Erc20 { .. } => {
                        if other_coin.is_eth() {
                            gas_cost_eth
                        } else {
                            get_base_price_in_rel(Some(self.ticker().to_string()), Some("ETH".to_string()))
                                .await
                                .and_then(|price_in_eth| gas_cost_eth.checked_div(price_in_eth))
                                .ok_or_else(|| {
                                    WatcherRewardError::RPCError(format!(
                                        "Price of coin {} in ETH could not be found",
                                        self.ticker()
                                    ))
                                })?
                        }
                    },
                    EthCoinType::Nft { .. } => {
                        return MmError::err(WatcherRewardError::InternalError(
                            "Nft Protocol is not supported yet!".to_string(),
                        ))
                    },
                }
            },
        };

        let send_contract_reward_on_spend = other_coin.is_eth();

        Ok(Some(WatcherReward {
            amount,
            is_exact_amount,
            reward_target,
            send_contract_reward_on_spend,
        }))
    }
}

#[async_trait]
#[cfg_attr(test, mockable)]
impl MarketCoinOps for EthCoin {
    fn ticker(&self) -> &str { &self.ticker[..] }

    fn my_address(&self) -> MmResult<String, MyAddressError> {
        match self.derivation_method() {
            DerivationMethod::SingleAddress(my_address) => Ok(my_address.display_address()),
            DerivationMethod::HDWallet(_) => MmError::err(MyAddressError::UnexpectedDerivationMethod(
                "'my_address' is deprecated for HD wallets".to_string(),
            )),
        }
    }

    async fn get_public_key(&self) -> Result<String, MmError<UnexpectedDerivationMethod>> {
        match self.priv_key_policy {
            EthPrivKeyPolicy::Iguana(ref key_pair)
            | EthPrivKeyPolicy::HDWallet {
                activated_key: ref key_pair,
                ..
            } => {
                let uncompressed_without_prefix = hex::encode(key_pair.public());
                Ok(format!("04{}", uncompressed_without_prefix))
            },
            EthPrivKeyPolicy::Trezor => {
                let public_key = self
                    .deref()
                    .derivation_method
                    .hd_wallet()
                    .ok_or(UnexpectedDerivationMethod::ExpectedHDWallet)?
                    .get_enabled_address()
                    .await
                    .ok_or_else(|| UnexpectedDerivationMethod::InternalError("no enabled address".to_owned()))?
                    .pubkey();
                let uncompressed_without_prefix = hex::encode(public_key);
                Ok(format!("04{}", uncompressed_without_prefix))
            },
            #[cfg(target_arch = "wasm32")]
            EthPrivKeyPolicy::Metamask(ref metamask_policy) => {
                Ok(format!("{:02x}", metamask_policy.public_key_uncompressed))
            },
        }
    }

    /// Hash message for signature using Ethereum's message signing format.
    /// keccak256(PREFIX_LENGTH + PREFIX + MESSAGE_LENGTH + MESSAGE)
    fn sign_message_hash(&self, message: &str) -> Option<[u8; 32]> {
        let message_prefix = self.sign_message_prefix.as_ref()?;

        let mut stream = Stream::new();
        let prefix_len = CompactInteger::from(message_prefix.len());
        prefix_len.serialize(&mut stream);
        stream.append_slice(message_prefix.as_bytes());
        stream.append_slice(message.len().to_string().as_bytes());
        stream.append_slice(message.as_bytes());
        Some(keccak256(&stream.out()).take())
    }

    fn sign_message(&self, message: &str) -> SignatureResult<String> {
        let message_hash = self.sign_message_hash(message).ok_or(SignatureError::PrefixNotFound)?;
        let privkey = &self.priv_key_policy.activated_key_or_err()?.secret();
        let signature = sign(privkey, &H256::from(message_hash))?;
        Ok(format!("0x{}", signature))
    }

    fn verify_message(&self, signature: &str, message: &str, address: &str) -> VerificationResult<bool> {
        let message_hash = self
            .sign_message_hash(message)
            .ok_or(VerificationError::PrefixNotFound)?;
        let address = self
            .address_from_str(address)
            .map_err(VerificationError::AddressDecodingError)?;
        let signature = Signature::from_str(signature.strip_prefix("0x").unwrap_or(signature))?;
        let is_verified = verify_address(&address, &signature, &H256::from(message_hash))?;
        Ok(is_verified)
    }

    fn my_balance(&self) -> BalanceFut<CoinBalance> {
        let decimals = self.decimals;
        let fut = self
            .get_balance()
            .and_then(move |result| Ok(u256_to_big_decimal(result, decimals)?))
            .map(|spendable| CoinBalance {
                spendable,
                unspendable: BigDecimal::from(0),
            });
        Box::new(fut)
    }

    fn base_coin_balance(&self) -> BalanceFut<BigDecimal> {
        Box::new(
            self.eth_balance()
                .and_then(move |result| Ok(u256_to_big_decimal(result, ETH_DECIMALS)?)),
        )
    }

    fn platform_ticker(&self) -> &str {
        match &self.coin_type {
            EthCoinType::Eth => self.ticker(),
            EthCoinType::Erc20 { platform, .. } | EthCoinType::Nft { platform } => platform,
        }
    }

    fn send_raw_tx(&self, mut tx: &str) -> Box<dyn Future<Item = String, Error = String> + Send> {
        if tx.starts_with("0x") {
            tx = &tx[2..];
        }
        let bytes = try_fus!(hex::decode(tx));

        let coin = self.clone();

        let fut = async move {
            coin.send_raw_transaction(bytes.into())
                .await
                .map(|res| format!("{:02x}", res)) // TODO: add 0x hash (use unified hash format for eth wherever it is returned)
                .map_err(|e| ERRL!("{}", e))
        };

        Box::new(fut.boxed().compat())
    }

    fn send_raw_tx_bytes(&self, tx: &[u8]) -> Box<dyn Future<Item = String, Error = String> + Send> {
        let coin = self.clone();

        let tx = tx.to_owned();
        let fut = async move {
            coin.send_raw_transaction(tx.into())
                .await
                .map(|res| format!("{:02x}", res))
                .map_err(|e| ERRL!("{}", e))
        };

        Box::new(fut.boxed().compat())
    }

    async fn sign_raw_tx(&self, args: &SignRawTransactionRequest) -> RawTransactionResult {
        if let SignRawTransactionEnum::ETH(eth_args) = &args.tx {
            sign_raw_eth_tx(self, eth_args).await
        } else {
            MmError::err(RawTransactionError::InvalidParam("eth type expected".to_string()))
        }
    }

    fn wait_for_confirmations(&self, input: ConfirmPaymentInput) -> Box<dyn Future<Item = (), Error = String> + Send> {
        macro_rules! update_status_with_error {
            ($status: ident, $error: ident) => {
                match $error.get_inner() {
                    Web3RpcError::Timeout(_) => $status.append(" Timed out."),
                    _ => $status.append(" Failed."),
                }
            };
        }

        let ctx = try_fus!(MmArc::from_weak(&self.ctx).ok_or("No context"));
        let mut status = ctx.log.status_handle();
        status.status(&[&self.ticker], "Waiting for confirmations…");
        status.deadline(input.wait_until * 1000);

        let unsigned: UnverifiedTransactionWrapper = try_fus!(rlp::decode(&input.payment_tx));
        let tx = try_fus!(SignedEthTx::new(unsigned));
        let tx_hash = tx.tx_hash();

        let required_confirms = U64::from(input.confirmations);
        let check_every = input.check_every as f64;
        let selfi = self.clone();
        let fut = async move {
            loop {
                // Wait for one confirmation and return the transaction confirmation block number
                let confirmed_at = match selfi
                    .transaction_confirmed_at(tx_hash, input.wait_until, check_every)
                    .compat()
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        update_status_with_error!(status, e);
                        return Err(e.to_string());
                    },
                };

                // checking that confirmed_at is greater than zero to prevent overflow.
                // untrusted RPC nodes might send a zero value to cause overflow if we didn't do this check.
                // required_confirms should always be more than 0 anyways but we should keep this check nonetheless.
                if confirmed_at <= U64::from(0) {
                    error!(
                        "confirmed_at: {}, for payment tx: {:02x}, for coin:{} should be greater than zero!",
                        confirmed_at,
                        tx_hash,
                        selfi.ticker()
                    );
                    Timer::sleep(check_every).await;
                    continue;
                }

                // Wait for a block that achieves the required confirmations
                let confirmation_block_number = confirmed_at + required_confirms - 1;
                if let Err(e) = selfi
                    .wait_for_block(confirmation_block_number, input.wait_until, check_every)
                    .compat()
                    .await
                {
                    update_status_with_error!(status, e);
                    return Err(e.to_string());
                }

                // Make sure that there was no chain reorganization that led to transaction confirmation block to be changed
                match selfi
                    .transaction_confirmed_at(tx_hash, input.wait_until, check_every)
                    .compat()
                    .await
                {
                    Ok(conf) => {
                        if conf == confirmed_at {
                            status.append(" Confirmed.");
                            break Ok(());
                        }
                    },
                    Err(e) => {
                        update_status_with_error!(status, e);
                        return Err(e.to_string());
                    },
                }

                Timer::sleep(check_every).await;
            }
        };

        Box::new(fut.boxed().compat())
    }

    async fn wait_for_htlc_tx_spend(&self, args: WaitForHTLCTxSpendArgs<'_>) -> TransactionResult {
        let unverified: UnverifiedTransactionWrapper = try_tx_s!(rlp::decode(args.tx_bytes));
        let tx = try_tx_s!(SignedEthTx::new(unverified));

        let swap_contract_address = match args.swap_contract_address {
            Some(addr) => try_tx_s!(addr.try_to_address()),
            None => match tx.unsigned().action() {
                Call(address) => *address,
                Create => {
                    return Err(TransactionErr::Plain(ERRL!(
                        "Invalid payment action: the payment action cannot be create"
                    )))
                },
            },
        };

        let func_name = match self.coin_type {
            EthCoinType::Eth => get_function_name("ethPayment", args.watcher_reward),
            EthCoinType::Erc20 { .. } => get_function_name("erc20Payment", args.watcher_reward),
            EthCoinType::Nft { .. } => {
                return Err(TransactionErr::ProtocolNotSupported(ERRL!(
                    "Nft Protocol is not supported yet!"
                )))
            },
        };

        let id = try_tx_s!(extract_id_from_tx_data(tx.unsigned().data(), &SWAP_CONTRACT, &func_name).await);

        let find_params = SpendTxSearchParams {
            swap_contract_address,
            event_name: "ReceiverSpent",
            abi_contract: &SWAP_CONTRACT,
            swap_id: &try_tx_s!(id.as_slice().try_into()),
            from_block: args.from_block,
            wait_until: args.wait_until,
            check_every: args.check_every,
        };
        let tx_hash = self
            .find_transaction_hash_by_event(find_params)
            .await
            .map_err(|e| TransactionErr::Plain(e.get_inner().to_string()))?;

        let spend_tx = self
            .wait_for_transaction(tx_hash, args.wait_until, args.check_every)
            .await
            .map_err(|e| TransactionErr::Plain(e.get_inner().to_string()))?;
        Ok(TransactionEnum::from(spend_tx))
    }

    fn tx_enum_from_bytes(&self, bytes: &[u8]) -> Result<TransactionEnum, MmError<TxMarshalingErr>> {
        signed_eth_tx_from_bytes(bytes)
            .map(TransactionEnum::from)
            .map_to_mm(TxMarshalingErr::InvalidInput)
    }

    fn current_block(&self) -> Box<dyn Future<Item = u64, Error = String> + Send> {
        let coin = self.clone();

        let fut = async move {
            coin.block_number()
                .await
                .map(|res| res.as_u64())
                .map_err(|e| ERRL!("{}", e))
        };

        Box::new(fut.boxed().compat())
    }

    fn display_priv_key(&self) -> Result<String, String> {
        match self.priv_key_policy {
            EthPrivKeyPolicy::Iguana(ref key_pair)
            | EthPrivKeyPolicy::HDWallet {
                activated_key: ref key_pair,
                ..
            } => Ok(format!("{:#02x}", key_pair.secret())),
            EthPrivKeyPolicy::Trezor => ERR!("'display_priv_key' is not supported for Hardware Wallets"),
            #[cfg(target_arch = "wasm32")]
            EthPrivKeyPolicy::Metamask(_) => ERR!("'display_priv_key' is not supported for MetaMask"),
        }
    }

    #[inline]
    fn min_tx_amount(&self) -> BigDecimal { BigDecimal::from(0) }

    #[inline]
    fn min_trading_vol(&self) -> MmNumber {
        let pow = self.decimals as u32;
        MmNumber::from(1) / MmNumber::from(10u64.pow(pow))
    }

    #[inline]
    fn should_burn_dex_fee(&self) -> bool { false }

    fn is_trezor(&self) -> bool { self.priv_key_policy.is_trezor() }
}

pub fn signed_eth_tx_from_bytes(bytes: &[u8]) -> Result<SignedEthTx, String> {
    let tx: UnverifiedTransactionWrapper = try_s!(rlp::decode(bytes));
    let signed = try_s!(SignedEthTx::new(tx));
    Ok(signed)
}

type AddressNonceLocks = Mutex<HashMap<String, HashMap<String, Arc<AsyncMutex<()>>>>>;

// We can use a nonce lock shared between tokens using the same platform coin and the platform itself.
// For example, ETH/USDT-ERC20 should use the same lock, but it will be different for BNB/USDT-BEP20.
// This lock is used to ensure that only one transaction is sent at a time per address.
lazy_static! {
    static ref NONCE_LOCK: AddressNonceLocks = Mutex::new(HashMap::new());
}

type EthTxFut = Box<dyn Future<Item = SignedEthTx, Error = TransactionErr> + Send + 'static>;

/// Signs an Eth transaction using `key_pair`.
///
/// This method polls for the latest nonce from the RPC nodes and uses it for the transaction to be signed.
/// A `nonce_lock` is returned so that the caller doesn't release it until the transaction is sent and the
/// address nonce is updated on RPC nodes.
#[allow(clippy::too_many_arguments)]
async fn sign_transaction_with_keypair<'a>(
    coin: &'a EthCoin,
    key_pair: &KeyPair,
    value: U256,
    action: Action,
    data: Vec<u8>,
    gas: U256,
    pay_for_gas_option: &PayForGasOption,
    from_address: Address,
) -> Result<(SignedEthTx, Vec<Web3Instance>), TransactionErr> {
    info!(target: "sign", "get_addr_nonce…");
    let (nonce, web3_instances_with_latest_nonce) = try_tx_s!(coin.clone().get_addr_nonce(from_address).compat().await);
    let tx_type = tx_type_from_pay_for_gas_option!(pay_for_gas_option);
    if !coin.is_tx_type_supported(&tx_type) {
        return Err(TransactionErr::Plain("Eth transaction type not supported".into()));
    }
    let tx_builder = UnSignedEthTxBuilder::new(tx_type, nonce, gas, action, value, data);
    let tx_builder = tx_builder_with_pay_for_gas_option(coin, tx_builder, pay_for_gas_option)
        .map_err(|e| TransactionErr::Plain(e.get_inner().to_string()))?;
    let tx = tx_builder.build()?;

    Ok((
        tx.sign(key_pair.secret(), Some(coin.chain_id))?,
        web3_instances_with_latest_nonce,
    ))
}

/// Sign and send eth transaction with provided keypair,
/// This fn is primarily for swap transactions so it uses swap tx fee policy
async fn sign_and_send_transaction_with_keypair(
    coin: &EthCoin,
    key_pair: &KeyPair,
    address: Address,
    value: U256,
    action: Action,
    data: Vec<u8>,
    gas: U256,
) -> Result<SignedEthTx, TransactionErr> {
    info!(target: "sign-and-send", "get_gas_price…");
    let pay_for_gas_option = try_tx_s!(
        coin.get_swap_pay_for_gas_option(coin.get_swap_transaction_fee_policy())
            .await
    );
    let address_lock = coin.get_address_lock(address.to_string()).await;
    let _nonce_lock = address_lock.lock().await;
    let (signed, web3_instances_with_latest_nonce) =
        sign_transaction_with_keypair(coin, key_pair, value, action, data, gas, &pay_for_gas_option, address).await?;
    let bytes = Bytes(rlp::encode(&signed).to_vec());
    info!(target: "sign-and-send", "send_raw_transaction…");

    let futures = web3_instances_with_latest_nonce
        .into_iter()
        .map(|web3_instance| web3_instance.web3.eth().send_raw_transaction(bytes.clone()));
    try_tx_s!(select_ok(futures).await.map_err(|e| ERRL!("{}", e)), signed);

    info!(target: "sign-and-send", "wait_for_tx_appears_on_rpc…");
    coin.wait_for_addr_nonce_increase(address, signed.unsigned().nonce())
        .await;
    Ok(signed)
}

/// Sign and send eth transaction with metamask API,
/// This fn is primarily for swap transactions so it uses swap tx fee policy
#[cfg(target_arch = "wasm32")]
async fn sign_and_send_transaction_with_metamask(
    coin: EthCoin,
    value: U256,
    action: Action,
    data: Vec<u8>,
    gas: U256,
) -> Result<SignedEthTx, TransactionErr> {
    let to = match action {
        Action::Create => None,
        Action::Call(to) => Some(to),
    };

    let pay_for_gas_option = try_tx_s!(
        coin.get_swap_pay_for_gas_option(coin.get_swap_transaction_fee_policy())
            .await
    );
    let my_address = try_tx_s!(coin.derivation_method.single_addr_or_err().await);
    let gas_price = pay_for_gas_option.get_gas_price();
    let (max_fee_per_gas, max_priority_fee_per_gas) = pay_for_gas_option.get_fee_per_gas();
    let tx_to_send = TransactionRequest {
        from: my_address,
        to,
        gas: Some(gas),
        gas_price,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        value: Some(value),
        data: Some(data.clone().into()),
        nonce: None,
        ..TransactionRequest::default()
    };

    // It's important to return the transaction hex for the swap,
    // so wait up to 60 seconds for the transaction to appear on the RPC node.
    let wait_rpc_timeout = 60_000;
    let check_every = 1.;

    // Please note that this method may take a long time
    // due to `wallet_switchEthereumChain` and `eth_sendTransaction` requests.
    let tx_hash = try_tx_s!(coin.send_transaction(tx_to_send).await);

    let maybe_signed_tx = try_tx_s!(
        coin.wait_for_tx_appears_on_rpc(tx_hash, wait_rpc_timeout, check_every)
            .await
    );
    match maybe_signed_tx {
        Some(signed_tx) => Ok(signed_tx),
        None => TX_PLAIN_ERR!(
            "Waited too long until the transaction {:?} appear on the RPC node",
            tx_hash
        ),
    }
}

/// Sign eth transaction
async fn sign_raw_eth_tx(coin: &EthCoin, args: &SignEthTransactionParams) -> RawTransactionResult {
    let value = wei_from_big_decimal(args.value.as_ref().unwrap_or(&BigDecimal::from(0)), coin.decimals)?;
    let action = if let Some(to) = &args.to {
        Call(Address::from_str(to).map_to_mm(|err| RawTransactionError::InvalidParam(err.to_string()))?)
    } else {
        Create
    };
    let data = hex::decode(args.data.as_ref().unwrap_or(&String::from("")))?;
    match coin.priv_key_policy {
        // TODO: use zeroise for privkey
        EthPrivKeyPolicy::Iguana(ref key_pair)
        | EthPrivKeyPolicy::HDWallet {
            activated_key: ref key_pair,
            ..
        } => {
            let my_address = coin
                .derivation_method
                .single_addr_or_err()
                .await
                .mm_err(|e| RawTransactionError::InternalError(e.to_string()))?;
            let address_lock = coin.get_address_lock(my_address.to_string()).await;
            let _nonce_lock = address_lock.lock().await;
            let pay_for_gas_option = if let Some(ref pay_for_gas) = args.pay_for_gas {
                pay_for_gas.clone().try_into()?
            } else {
                // use legacy gas_price() if not set
                info!(target: "sign-and-send", "get_gas_price…");
                let gas_price = coin.get_gas_price().await?;
                PayForGasOption::Legacy(LegacyGasPrice { gas_price })
            };
            sign_transaction_with_keypair(
                coin,
                key_pair,
                value,
                action,
                data,
                args.gas_limit,
                &pay_for_gas_option,
                my_address,
            )
            .await
            .map(|(signed_tx, _)| RawTransactionRes {
                tx_hex: signed_tx.tx_hex().into(),
            })
            .map_to_mm(|err| RawTransactionError::TransactionError(err.get_plain_text_format()))
        },
        #[cfg(target_arch = "wasm32")]
        EthPrivKeyPolicy::Metamask(_) => MmError::err(RawTransactionError::InvalidParam(
            "sign raw eth tx not implemented for Metamask".into(),
        )),
        EthPrivKeyPolicy::Trezor => MmError::err(RawTransactionError::InvalidParam(
            "sign raw eth tx not implemented for Trezor".into(),
        )),
    }
}

#[async_trait]
impl RpcCommonOps for EthCoin {
    type RpcClient = Web3Instance;
    type Error = Web3RpcError;

    async fn get_live_client(&self) -> Result<Self::RpcClient, Self::Error> {
        let mut clients = self.web3_instances.lock().await;

        // try to find first live client
        for (i, client) in clients.clone().into_iter().enumerate() {
            if let Web3Transport::Websocket(socket_transport) = &client.web3.transport() {
                socket_transport.maybe_spawn_connection_loop(self.clone());
            };

            if !client.web3.transport().is_last_request_failed() {
                // Bring the live client to the front of rpc_clients
                clients.rotate_left(i);
                return Ok(client);
            }

            match client
                .web3
                .web3()
                .client_version()
                .timeout(ETH_RPC_REQUEST_TIMEOUT)
                .await
            {
                Ok(Ok(_)) => {
                    // Bring the live client to the front of rpc_clients
                    clients.rotate_left(i);
                    return Ok(client);
                },
                Ok(Err(rpc_error)) => {
                    debug!("Could not get client version on: {:?}. Error: {}", &client, rpc_error);

                    if let Web3Transport::Websocket(socket_transport) = client.web3.transport() {
                        socket_transport.stop_connection_loop().await;
                    };
                },
                Err(timeout_error) => {
                    debug!(
                        "Client version timeout exceed on: {:?}. Error: {}",
                        &client, timeout_error
                    );

                    if let Web3Transport::Websocket(socket_transport) = client.web3.transport() {
                        socket_transport.stop_connection_loop().await;
                    };
                },
            };
        }

        return Err(Web3RpcError::Transport(
            "All the current rpc nodes are unavailable.".to_string(),
        ));
    }
}

impl EthCoin {
    pub(crate) async fn web3(&self) -> Result<Web3<Web3Transport>, Web3RpcError> {
        self.get_live_client().await.map(|t| t.web3)
    }

    /// Gets `SenderRefunded` events from etomic swap smart contract since `from_block`
    fn refund_events(
        &self,
        swap_contract_address: Address,
        from_block: u64,
        to_block: u64,
    ) -> Box<dyn Future<Item = Vec<Log>, Error = String> + Send> {
        let contract_event = try_fus!(SWAP_CONTRACT.event("SenderRefunded"));
        let filter = FilterBuilder::default()
            .topics(Some(vec![contract_event.signature()]), None, None, None)
            .from_block(BlockNumber::Number(from_block.into()))
            .to_block(BlockNumber::Number(to_block.into()))
            .address(vec![swap_contract_address])
            .build();

        let coin = self.clone();

        let fut = async move { coin.logs(filter).await.map_err(|e| ERRL!("{}", e)) };

        Box::new(fut.boxed().compat())
    }

    /// Gets ETH traces from ETH node between addresses in `from_block` and `to_block`
    async fn eth_traces(
        &self,
        from_addr: Vec<Address>,
        to_addr: Vec<Address>,
        from_block: BlockNumber,
        to_block: BlockNumber,
        limit: Option<usize>,
    ) -> Web3RpcResult<Vec<Trace>> {
        let mut filter = TraceFilterBuilder::default()
            .from_address(from_addr)
            .to_address(to_addr)
            .from_block(from_block)
            .to_block(to_block);
        if let Some(l) = limit {
            filter = filter.count(l);
        }
        drop_mutability!(filter);

        self.trace_filter(filter.build()).await.map_to_mm(Web3RpcError::from)
    }

    /// Gets Transfer events from ERC20 smart contract `addr` between `from_block` and `to_block`
    async fn erc20_transfer_events(
        &self,
        contract: Address,
        from_addr: Option<Address>,
        to_addr: Option<Address>,
        from_block: BlockNumber,
        to_block: BlockNumber,
        limit: Option<usize>,
    ) -> Web3RpcResult<Vec<Log>> {
        let contract_event = ERC20_CONTRACT.event("Transfer")?;
        let topic0 = Some(vec![contract_event.signature()]);
        let topic1 = from_addr.map(|addr| vec![addr.into()]);
        let topic2 = to_addr.map(|addr| vec![addr.into()]);

        let mut filter = FilterBuilder::default()
            .topics(topic0, topic1, topic2, None)
            .from_block(from_block)
            .to_block(to_block)
            .address(vec![contract]);
        if let Some(l) = limit {
            filter = filter.limit(l);
        }
        drop_mutability!(filter);

        self.logs(filter.build()).await.map_to_mm(Web3RpcError::from)
    }

    /// Downloads and saves ETH transaction history of my_address, relies on Parity trace_filter API
    /// https://wiki.parity.io/JSONRPC-trace-module#trace_filter, this requires tracing to be enabled
    /// in node config. Other ETH clients (Geth, etc.) are `not` supported (yet).
    #[allow(clippy::cognitive_complexity)]
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    async fn process_eth_history(&self, ctx: &MmArc) {
        // Artem Pikulin: by playing a bit with Parity mainnet node I've discovered that trace_filter API responds after reasonable time for 1000 blocks.
        // I've tried to increase the amount to 10000, but request times out somewhere near 2500000 block.
        // Also the Parity RPC server seem to get stuck while request in running (other requests performance is also lowered).
        let delta = U64::from(1000);

        let my_address = match self.derivation_method.single_addr_or_err().await {
            Ok(addr) => addr,
            Err(e) => {
                ctx.log.log(
                    "",
                    &[&"tx_history", &self.ticker],
                    &ERRL!("Error on getting my address: {}", e),
                );
                return;
            },
        };
        let mut success_iteration = 0i32;
        loop {
            if ctx.is_stopping() {
                break;
            };
            {
                let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
                let coins = coins_ctx.coins.lock().await;
                if !coins.contains_key(&self.ticker) {
                    ctx.log.log("", &[&"tx_history", &self.ticker], "Loop stopped");
                    break;
                };
            }

            let current_block = match self.block_number().await {
                Ok(block) => block,
                Err(e) => {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on eth_block_number, retrying", e),
                    );
                    Timer::sleep(10.).await;
                    continue;
                },
            };

            let mut saved_traces = match self.load_saved_traces(ctx, my_address) {
                Some(traces) => traces,
                None => SavedTraces {
                    traces: vec![],
                    earliest_block: current_block,
                    latest_block: current_block,
                },
            };
            *self.history_sync_state.lock().unwrap() = HistorySyncState::InProgress(json!({
                "blocks_left": saved_traces.earliest_block.as_u64(),
            }));

            let mut existing_history = match self.load_history_from_file(ctx).compat().await {
                Ok(history) => history,
                Err(e) => {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on 'load_history_from_file', stop the history loop", e),
                    );
                    return;
                },
            };

            // AP: AFAIK ETH RPC doesn't support conditional filters like `get this OR this` so we have
            // to run several queries to get trace events including our address as sender `or` receiver
            // TODO refactor this to batch requests instead of single request per query
            if saved_traces.earliest_block > 0.into() {
                let before_earliest = if saved_traces.earliest_block >= delta {
                    saved_traces.earliest_block - delta
                } else {
                    0.into()
                };

                let from_traces_before_earliest = match self
                    .eth_traces(
                        vec![my_address],
                        vec![],
                        BlockNumber::Number(before_earliest),
                        BlockNumber::Number(saved_traces.earliest_block),
                        None,
                    )
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_traces_before_earliest = match self
                    .eth_traces(
                        vec![],
                        vec![my_address],
                        BlockNumber::Number(before_earliest),
                        BlockNumber::Number(saved_traces.earliest_block),
                        None,
                    )
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_traces_before_earliest.len() + to_traces_before_earliest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "eth_traces");

                saved_traces.traces.extend(from_traces_before_earliest);
                saved_traces.traces.extend(to_traces_before_earliest);
                saved_traces.earliest_block = if before_earliest > 0.into() {
                    // need to exclude the before earliest block from next iteration
                    before_earliest - 1
                } else {
                    0.into()
                };
                self.store_eth_traces(ctx, my_address, &saved_traces);
            }

            if current_block > saved_traces.latest_block {
                let from_traces_after_latest = match self
                    .eth_traces(
                        vec![my_address],
                        vec![],
                        BlockNumber::Number(saved_traces.latest_block + 1),
                        BlockNumber::Number(current_block),
                        None,
                    )
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_traces_after_latest = match self
                    .eth_traces(
                        vec![],
                        vec![my_address],
                        BlockNumber::Number(saved_traces.latest_block + 1),
                        BlockNumber::Number(current_block),
                        None,
                    )
                    .await
                {
                    Ok(traces) => traces,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on eth_traces, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_traces_after_latest.len() + to_traces_after_latest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "eth_traces");

                saved_traces.traces.extend(from_traces_after_latest);
                saved_traces.traces.extend(to_traces_after_latest);
                saved_traces.latest_block = current_block;

                self.store_eth_traces(ctx, my_address, &saved_traces);
            }
            saved_traces.traces.sort_by(|a, b| b.block_number.cmp(&a.block_number));
            for trace in saved_traces.traces {
                let hash = sha256(&json::to_vec(&trace).unwrap());
                let internal_id = BytesJson::from(hash.to_vec());
                let processed = existing_history.iter().find(|tx| tx.internal_id == internal_id);
                if processed.is_some() {
                    continue;
                }

                // TODO Only standard Call traces are supported, contract creations, suicides and block rewards will be supported later
                let call_data = match trace.action {
                    TraceAction::Call(d) => d,
                    _ => continue,
                };

                mm_counter!(ctx.metrics, "tx.history.request.count", 1, "coin" => self.ticker.clone(), "method" => "tx_detail_by_hash");

                let web3_tx = match self
                    .transaction(TransactionId::Hash(trace.transaction_hash.unwrap()))
                    .await
                {
                    Ok(tx) => tx,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!(
                                "Error {} on getting transaction {:?}",
                                e,
                                trace.transaction_hash.unwrap()
                            ),
                        );
                        continue;
                    },
                };
                let web3_tx = match web3_tx {
                    Some(t) => t,
                    None => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("No such transaction {:?}", trace.transaction_hash.unwrap()),
                        );
                        continue;
                    },
                };

                mm_counter!(ctx.metrics, "tx.history.response.count", 1, "coin" => self.ticker.clone(), "method" => "tx_detail_by_hash");

                let receipt = match self.transaction_receipt(trace.transaction_hash.unwrap()).await {
                    Ok(r) => r,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!(
                                "Error {} on getting transaction {:?} receipt",
                                e,
                                trace.transaction_hash.unwrap()
                            ),
                        );
                        continue;
                    },
                };
                let fee_coin = match &self.coin_type {
                    EthCoinType::Eth => self.ticker(),
                    EthCoinType::Erc20 { platform, .. } => platform.as_str(),
                    EthCoinType::Nft { .. } => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error on getting fee coin: Nft Protocol is not supported yet!"),
                        );
                        continue;
                    },
                };
                let fee_details: Option<EthTxFeeDetails> = match receipt {
                    Some(r) => {
                        let gas_used = r.gas_used.unwrap_or_default();
                        let gas_price = web3_tx.gas_price.unwrap_or_default();
                        // TODO: create and use EthTxFeeDetails::from(web3_tx)
                        // It's relatively safe to unwrap `EthTxFeeDetails::new` as it may fail due to `u256_to_big_decimal` only.
                        // Also TX history is not used by any GUI and has significant disadvantages.
                        Some(
                            EthTxFeeDetails::new(
                                gas_used,
                                PayForGasOption::Legacy(LegacyGasPrice { gas_price }),
                                fee_coin,
                            )
                            .unwrap(),
                        )
                    },
                    None => None,
                };

                let total_amount: BigDecimal = u256_to_big_decimal(call_data.value, ETH_DECIMALS).unwrap();
                let mut received_by_me = 0.into();
                let mut spent_by_me = 0.into();

                if call_data.from == my_address {
                    // ETH transfer is actually happening only if no error occurred
                    if trace.error.is_none() {
                        spent_by_me = total_amount.clone();
                    }
                    if let Some(ref fee) = fee_details {
                        spent_by_me += &fee.total_fee;
                    }
                }

                if call_data.to == my_address {
                    // ETH transfer is actually happening only if no error occurred
                    if trace.error.is_none() {
                        received_by_me = total_amount.clone();
                    }
                }

                let raw = signed_tx_from_web3_tx(web3_tx).unwrap();
                let block = match self
                    .block(BlockId::Number(BlockNumber::Number(trace.block_number.into())))
                    .await
                {
                    Ok(b) => b.unwrap(),
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on getting block {} data", e, trace.block_number),
                        );
                        continue;
                    },
                };

                let details = TransactionDetails {
                    my_balance_change: &received_by_me - &spent_by_me,
                    spent_by_me,
                    received_by_me,
                    total_amount,
                    to: vec![call_data.to.display_address()],
                    from: vec![call_data.from.display_address()],
                    coin: self.ticker.clone(),
                    fee_details: fee_details.map(|d| d.into()),
                    block_height: trace.block_number,
                    tx: TransactionData::new_signed(
                        BytesJson(rlp::encode(&raw).to_vec()),
                        format!("{:02x}", BytesJson(raw.tx_hash_as_bytes().to_vec())),
                    ),
                    internal_id,
                    timestamp: block.timestamp.into_or_max(),
                    kmd_rewards: None,
                    transaction_type: Default::default(),
                    memo: None,
                };

                existing_history.push(details);

                if let Err(e) = self.save_history_to_file(ctx, existing_history.clone()).compat().await {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on 'save_history_to_file', stop the history loop", e),
                    );
                    return;
                }
            }
            if saved_traces.earliest_block == 0.into() {
                if success_iteration == 0 {
                    ctx.log.log(
                        "😅",
                        &[&"tx_history", &("coin", self.ticker.clone().as_str())],
                        "history has been loaded successfully",
                    );
                }

                success_iteration += 1;
                *self.history_sync_state.lock().unwrap() = HistorySyncState::Finished;
                Timer::sleep(15.).await;
            } else {
                Timer::sleep(2.).await;
            }
        }
    }

    /// Downloads and saves ERC20 transaction history of my_address
    #[allow(clippy::cognitive_complexity)]
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    async fn process_erc20_history(&self, token_addr: H160, ctx: &MmArc) {
        let delta = U64::from(10000);

        let my_address = match self.derivation_method.single_addr_or_err().await {
            Ok(addr) => addr,
            Err(e) => {
                ctx.log.log(
                    "",
                    &[&"tx_history", &self.ticker],
                    &ERRL!("Error on getting my address: {}", e),
                );
                return;
            },
        };
        let mut success_iteration = 0i32;
        loop {
            if ctx.is_stopping() {
                break;
            };
            {
                let coins_ctx = CoinsContext::from_ctx(ctx).unwrap();
                let coins = coins_ctx.coins.lock().await;
                if !coins.contains_key(&self.ticker) {
                    ctx.log.log("", &[&"tx_history", &self.ticker], "Loop stopped");
                    break;
                };
            }

            let current_block = match self.block_number().await {
                Ok(block) => block,
                Err(e) => {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on eth_block_number, retrying", e),
                    );
                    Timer::sleep(10.).await;
                    continue;
                },
            };

            let mut saved_events = match self.load_saved_erc20_events(ctx, my_address) {
                Some(events) => events,
                None => SavedErc20Events {
                    events: vec![],
                    earliest_block: current_block,
                    latest_block: current_block,
                },
            };
            *self.history_sync_state.lock().unwrap() = HistorySyncState::InProgress(json!({
                "blocks_left": saved_events.earliest_block,
            }));

            // AP: AFAIK ETH RPC doesn't support conditional filters like `get this OR this` so we have
            // to run several queries to get transfer events including our address as sender `or` receiver
            // TODO refactor this to batch requests instead of single request per query
            if saved_events.earliest_block > 0.into() {
                let before_earliest = if saved_events.earliest_block >= delta {
                    saved_events.earliest_block - delta
                } else {
                    0.into()
                };

                let from_events_before_earliest = match self
                    .erc20_transfer_events(
                        token_addr,
                        Some(my_address),
                        None,
                        BlockNumber::Number(before_earliest),
                        BlockNumber::Number(saved_events.earliest_block - 1),
                        None,
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_events_before_earliest = match self
                    .erc20_transfer_events(
                        token_addr,
                        None,
                        Some(my_address),
                        BlockNumber::Number(before_earliest),
                        BlockNumber::Number(saved_events.earliest_block - 1),
                        None,
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_events_before_earliest.len() + to_events_before_earliest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "erc20_transfer_events");

                saved_events.events.extend(from_events_before_earliest);
                saved_events.events.extend(to_events_before_earliest);
                saved_events.earliest_block = if before_earliest > 0.into() {
                    before_earliest - 1
                } else {
                    0.into()
                };
                self.store_erc20_events(ctx, my_address, &saved_events);
            }

            if current_block > saved_events.latest_block {
                let from_events_after_latest = match self
                    .erc20_transfer_events(
                        token_addr,
                        Some(my_address),
                        None,
                        BlockNumber::Number(saved_events.latest_block + 1),
                        BlockNumber::Number(current_block),
                        None,
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let to_events_after_latest = match self
                    .erc20_transfer_events(
                        token_addr,
                        None,
                        Some(my_address),
                        BlockNumber::Number(saved_events.latest_block + 1),
                        BlockNumber::Number(current_block),
                        None,
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on erc20_transfer_events, retrying", e),
                        );
                        Timer::sleep(10.).await;
                        continue;
                    },
                };

                let total_length = from_events_after_latest.len() + to_events_after_latest.len();
                mm_counter!(ctx.metrics, "tx.history.response.total_length", total_length as u64,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "erc20_transfer_events");

                saved_events.events.extend(from_events_after_latest);
                saved_events.events.extend(to_events_after_latest);
                saved_events.latest_block = current_block;
                self.store_erc20_events(ctx, my_address, &saved_events);
            }

            let all_events: HashMap<_, _> = saved_events
                .events
                .iter()
                .filter(|e| e.block_number.is_some() && e.transaction_hash.is_some() && !e.is_removed())
                .map(|e| (e.transaction_hash.unwrap(), e))
                .collect();
            let mut all_events: Vec<_> = all_events.into_values().collect();
            all_events.sort_by(|a, b| b.block_number.unwrap().cmp(&a.block_number.unwrap()));

            for event in all_events {
                let mut existing_history = match self.load_history_from_file(ctx).compat().await {
                    Ok(history) => history,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on 'load_history_from_file', stop the history loop", e),
                        );
                        return;
                    },
                };
                let internal_id = BytesJson::from(sha256(&json::to_vec(&event).unwrap()).to_vec());
                if existing_history.iter().any(|item| item.internal_id == internal_id) {
                    // the transaction already imported
                    continue;
                };

                let amount = U256::from(event.data.0.as_slice());
                let total_amount = u256_to_big_decimal(amount, self.decimals).unwrap();
                let mut received_by_me = 0.into();
                let mut spent_by_me = 0.into();

                let from_addr = H160::from(event.topics[1]);
                let to_addr = H160::from(event.topics[2]);

                if from_addr == my_address {
                    spent_by_me = total_amount.clone();
                }

                if to_addr == my_address {
                    received_by_me = total_amount.clone();
                }

                mm_counter!(ctx.metrics, "tx.history.request.count", 1,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "tx_detail_by_hash");

                let web3_tx = match self
                    .transaction(TransactionId::Hash(event.transaction_hash.unwrap()))
                    .await
                {
                    Ok(tx) => tx,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!(
                                "Error {} on getting transaction {:?}",
                                e,
                                event.transaction_hash.unwrap()
                            ),
                        );
                        continue;
                    },
                };

                mm_counter!(ctx.metrics, "tx.history.response.count", 1,
                    "coin" => self.ticker.clone(), "client" => "ethereum", "method" => "tx_detail_by_hash");

                let web3_tx = match web3_tx {
                    Some(t) => t,
                    None => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("No such transaction {:?}", event.transaction_hash.unwrap()),
                        );
                        continue;
                    },
                };

                let receipt = match self.transaction_receipt(event.transaction_hash.unwrap()).await {
                    Ok(r) => r,
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!(
                                "Error {} on getting transaction {:?} receipt",
                                e,
                                event.transaction_hash.unwrap()
                            ),
                        );
                        continue;
                    },
                };
                let fee_coin = match &self.coin_type {
                    EthCoinType::Eth => self.ticker(),
                    EthCoinType::Erc20 { platform, .. } => platform.as_str(),
                    EthCoinType::Nft { .. } => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error on getting fee coin: Nft Protocol is not supported yet!"),
                        );
                        continue;
                    },
                };
                let fee_details = match receipt {
                    Some(r) => {
                        let gas_used = r.gas_used.unwrap_or_default();
                        let gas_price = web3_tx.gas_price.unwrap_or_default();
                        // It's relatively safe to unwrap `EthTxFeeDetails::new` as it may fail
                        // due to `u256_to_big_decimal` only.
                        // Also TX history is not used by any GUI and has significant disadvantages.
                        Some(
                            EthTxFeeDetails::new(
                                gas_used,
                                PayForGasOption::Legacy(LegacyGasPrice { gas_price }),
                                fee_coin,
                            )
                            .unwrap(),
                        )
                    },
                    None => None,
                };
                let block_number = event.block_number.unwrap();
                let block = match self.block(BlockId::Number(BlockNumber::Number(block_number))).await {
                    Ok(Some(b)) => b,
                    Ok(None) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Block {} is None", block_number),
                        );
                        continue;
                    },
                    Err(e) => {
                        ctx.log.log(
                            "",
                            &[&"tx_history", &self.ticker],
                            &ERRL!("Error {} on getting block {} data", e, block_number),
                        );
                        continue;
                    },
                };

                let raw = signed_tx_from_web3_tx(web3_tx).unwrap();
                let details = TransactionDetails {
                    my_balance_change: &received_by_me - &spent_by_me,
                    spent_by_me,
                    received_by_me,
                    total_amount,
                    to: vec![to_addr.display_address()],
                    from: vec![from_addr.display_address()],
                    coin: self.ticker.clone(),
                    fee_details: fee_details.map(|d| d.into()),
                    block_height: block_number.as_u64(),
                    tx: TransactionData::new_signed(
                        BytesJson(rlp::encode(&raw).to_vec()),
                        format!("{:02x}", BytesJson(raw.tx_hash_as_bytes().to_vec())),
                    ),
                    internal_id: BytesJson(internal_id.to_vec()),
                    timestamp: block.timestamp.into_or_max(),
                    kmd_rewards: None,
                    transaction_type: Default::default(),
                    memo: None,
                };

                existing_history.push(details);

                if let Err(e) = self.save_history_to_file(ctx, existing_history).compat().await {
                    ctx.log.log(
                        "",
                        &[&"tx_history", &self.ticker],
                        &ERRL!("Error {} on 'save_history_to_file', stop the history loop", e),
                    );
                    return;
                }
            }
            if saved_events.earliest_block == 0.into() {
                if success_iteration == 0 {
                    ctx.log.log(
                        "😅",
                        &[&"tx_history", &("coin", self.ticker.clone().as_str())],
                        "history has been loaded successfully",
                    );
                }

                success_iteration += 1;
                *self.history_sync_state.lock().unwrap() = HistorySyncState::Finished;
                Timer::sleep(15.).await;
            } else {
                Timer::sleep(2.).await;
            }
        }
    }

    /// Returns tx type as number if this type supported by this coin
    fn is_tx_type_supported(&self, tx_type: &TxType) -> bool {
        let tx_type_as_num = match tx_type {
            TxType::Legacy => 0_u64,
            TxType::Type1 => 1_u64,
            TxType::Type2 => 2_u64,
            TxType::Invalid => return false,
        };
        let max_tx_type = self.max_eth_tx_type.unwrap_or(0_u64);
        tx_type_as_num <= max_tx_type
    }

    /// Retrieves the lock associated with a given address.
    ///
    /// This function is used to ensure that only one transaction is sent at a time per address.
    /// If the address does not have an associated lock, a new one is created and stored.
    async fn get_address_lock(&self, address: String) -> Arc<AsyncMutex<()>> {
        let address_lock = {
            let mut lock = self.address_nonce_locks.lock().await;
            lock.entry(address)
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        address_lock
    }
}

#[cfg_attr(test, mockable)]
impl EthCoin {
    /// Sign and send eth transaction.
    /// This function is primarily for swap transactions so internally it relies on the swap tx fee policy
    pub fn sign_and_send_transaction(&self, value: U256, action: Action, data: Vec<u8>, gas: U256) -> EthTxFut {
        let coin = self.clone();
        let fut = async move {
            match coin.priv_key_policy {
                EthPrivKeyPolicy::Iguana(ref key_pair)
                | EthPrivKeyPolicy::HDWallet {
                    activated_key: ref key_pair,
                    ..
                } => {
                    let address = coin
                        .derivation_method
                        .single_addr_or_err()
                        .await
                        .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)))?;
                    sign_and_send_transaction_with_keypair(&coin, key_pair, address, value, action, data, gas).await
                },
                EthPrivKeyPolicy::Trezor => Err(TransactionErr::Plain(ERRL!("Trezor is not supported for swaps yet!"))),
                #[cfg(target_arch = "wasm32")]
                EthPrivKeyPolicy::Metamask(_) => {
                    sign_and_send_transaction_with_metamask(coin, value, action, data, gas).await
                },
            }
        };
        Box::new(fut.boxed().compat())
    }

    pub fn send_to_address(&self, address: Address, value: U256) -> EthTxFut {
        match &self.coin_type {
            EthCoinType::Eth => self.sign_and_send_transaction(
                value,
                Action::Call(address),
                vec![],
                U256::from(self.gas_limit.eth_send_coins),
            ),
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let abi = try_tx_fus!(Contract::load(ERC20_ABI.as_bytes()));
                let function = try_tx_fus!(abi.function("transfer"));
                let data = try_tx_fus!(function.encode_input(&[Token::Address(address), Token::Uint(value)]));
                self.sign_and_send_transaction(
                    0.into(),
                    Action::Call(*token_addr),
                    data,
                    U256::from(self.gas_limit.eth_send_erc20),
                )
            },
            EthCoinType::Nft { .. } => Box::new(futures01::future::err(TransactionErr::ProtocolNotSupported(ERRL!(
                "Nft Protocol is not supported yet!"
            )))),
        }
    }

    fn send_hash_time_locked_payment(&self, args: SendPaymentArgs<'_>) -> EthTxFut {
        let receiver_addr = try_tx_fus!(addr_from_raw_pubkey(args.other_pubkey));
        let swap_contract_address = try_tx_fus!(args.swap_contract_address.try_to_address());
        let id = self.etomic_swap_id(try_tx_fus!(args.time_lock.try_into()), args.secret_hash);
        let trade_amount = try_tx_fus!(wei_from_big_decimal(&args.amount, self.decimals));

        let time_lock = U256::from(args.time_lock);

        let secret_hash = if args.secret_hash.len() == 32 {
            ripemd160(args.secret_hash).to_vec()
        } else {
            args.secret_hash.to_vec()
        };

        match &self.coin_type {
            EthCoinType::Eth => {
                let function_name = get_function_name("ethPayment", args.watcher_reward.is_some());
                let function = try_tx_fus!(SWAP_CONTRACT.function(&function_name));

                let mut value = trade_amount;
                let data = match &args.watcher_reward {
                    Some(reward) => {
                        let reward_amount = try_tx_fus!(wei_from_big_decimal(&reward.amount, self.decimals));
                        if !matches!(reward.reward_target, RewardTarget::None) || reward.send_contract_reward_on_spend {
                            value += reward_amount;
                        }

                        try_tx_fus!(function.encode_input(&[
                            Token::FixedBytes(id),
                            Token::Address(receiver_addr),
                            Token::FixedBytes(secret_hash),
                            Token::Uint(time_lock),
                            Token::Uint(U256::from(reward.reward_target as u8)),
                            Token::Bool(reward.send_contract_reward_on_spend),
                            Token::Uint(reward_amount)
                        ]))
                    },
                    None => try_tx_fus!(function.encode_input(&[
                        Token::FixedBytes(id),
                        Token::Address(receiver_addr),
                        Token::FixedBytes(secret_hash),
                        Token::Uint(time_lock),
                    ])),
                };
                let gas = U256::from(self.gas_limit.eth_payment);
                self.sign_and_send_transaction(value, Action::Call(swap_contract_address), data, gas)
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let allowance_fut = self
                    .allowance(swap_contract_address)
                    .map_err(|e| TransactionErr::Plain(ERRL!("{}", e)));

                let function_name = get_function_name("erc20Payment", args.watcher_reward.is_some());
                let function = try_tx_fus!(SWAP_CONTRACT.function(&function_name));

                let mut value = U256::from(0);
                let mut amount = trade_amount;

                debug!("Using watcher reward {:?} for swap payment", args.watcher_reward);

                let data = match args.watcher_reward {
                    Some(reward) => {
                        let reward_amount = match reward.reward_target {
                            RewardTarget::Contract | RewardTarget::PaymentSender => {
                                let eth_reward_amount = try_tx_fus!(wei_from_big_decimal(&reward.amount, ETH_DECIMALS));
                                value += eth_reward_amount;
                                eth_reward_amount
                            },
                            RewardTarget::PaymentSpender => {
                                let token_reward_amount =
                                    try_tx_fus!(wei_from_big_decimal(&reward.amount, self.decimals));
                                amount += token_reward_amount;
                                token_reward_amount
                            },
                            _ => {
                                // TODO tests passed without this change, need to research on how it worked
                                if reward.send_contract_reward_on_spend {
                                    let eth_reward_amount =
                                        try_tx_fus!(wei_from_big_decimal(&reward.amount, ETH_DECIMALS));
                                    value += eth_reward_amount;
                                    eth_reward_amount
                                } else {
                                    0.into()
                                }
                            },
                        };

                        try_tx_fus!(function.encode_input(&[
                            Token::FixedBytes(id),
                            Token::Uint(amount),
                            Token::Address(*token_addr),
                            Token::Address(receiver_addr),
                            Token::FixedBytes(secret_hash),
                            Token::Uint(time_lock),
                            Token::Uint(U256::from(reward.reward_target as u8)),
                            Token::Bool(reward.send_contract_reward_on_spend),
                            Token::Uint(reward_amount),
                        ]))
                    },
                    None => {
                        try_tx_fus!(function.encode_input(&[
                            Token::FixedBytes(id),
                            Token::Uint(trade_amount),
                            Token::Address(*token_addr),
                            Token::Address(receiver_addr),
                            Token::FixedBytes(secret_hash),
                            Token::Uint(time_lock)
                        ]))
                    },
                };

                let wait_for_required_allowance_until = args.wait_for_confirmation_until;
                let gas = U256::from(self.gas_limit.erc20_payment);

                let arc = self.clone();
                Box::new(allowance_fut.and_then(move |allowed| -> EthTxFut {
                    if allowed < amount {
                        Box::new(
                            arc.approve(swap_contract_address, U256::max_value())
                                .and_then(move |approved| {
                                    // make sure the approve tx is confirmed by making sure that the allowed value has been updated
                                    // this call is cheaper than waiting for confirmation calls
                                    arc.wait_for_required_allowance(
                                        swap_contract_address,
                                        amount,
                                        wait_for_required_allowance_until,
                                    )
                                    .map_err(move |e| {
                                        TransactionErr::Plain(ERRL!(
                                            "Allowed value was not updated in time after sending approve transaction {:02x}: {}",
                                            approved.tx_hash_as_bytes(),
                                            e
                                        ))
                                    })
                                    .and_then(move |_| {
                                        arc.sign_and_send_transaction(
                                            value,
                                            Call(swap_contract_address),
                                            data,
                                            gas,
                                        )
                                    })
                                }),
                        )
                    } else {
                        Box::new(arc.sign_and_send_transaction(
                            value,
                            Call(swap_contract_address),
                            data,
                            gas,
                        ))
                    }
                }))
            },
            EthCoinType::Nft { .. } => Box::new(futures01::future::err(TransactionErr::ProtocolNotSupported(ERRL!(
                "Nft Protocol is not supported yet!"
            )))),
        }
    }

    fn watcher_spends_hash_time_locked_payment(&self, input: SendMakerPaymentSpendPreimageInput) -> EthTxFut {
        let tx: UnverifiedTransactionWrapper = try_tx_fus!(rlp::decode(input.preimage));
        let payment = try_tx_fus!(SignedEthTx::new(tx));

        let function_name = get_function_name("receiverSpend", input.watcher_reward);
        let spend_func = try_tx_fus!(SWAP_CONTRACT.function(&function_name));
        let clone = self.clone();
        let secret_vec = input.secret.to_vec();
        let taker_addr = addr_from_raw_pubkey(input.taker_pub).unwrap();
        let swap_contract_address = match payment.unsigned().action() {
            Call(address) => *address,
            Create => {
                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                    "Invalid payment action: the payment action cannot be create"
                ))))
            },
        };

        let watcher_reward = input.watcher_reward;
        match self.coin_type {
            EthCoinType::Eth => {
                let function_name = get_function_name("ethPayment", watcher_reward);
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function(&function_name));
                let decoded = try_tx_fus!(decode_contract_call(payment_func, payment.unsigned().data()));
                let swap_id_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 0));

                let state_f = self.payment_status(swap_contract_address, swap_id_input.clone());
                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != U256::from(PaymentState::Sent as u8) {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }

                            let value = payment.unsigned().value();
                            let reward_target = try_tx_fus!(get_function_input_data(&decoded, payment_func, 4));
                            let sends_contract_reward = try_tx_fus!(get_function_input_data(&decoded, payment_func, 5));
                            let watcher_reward_amount = try_tx_fus!(get_function_input_data(&decoded, payment_func, 6));

                            let data = try_tx_fus!(spend_func.encode_input(&[
                                swap_id_input,
                                Token::Uint(value),
                                Token::FixedBytes(secret_vec.clone()),
                                Token::Address(Address::default()),
                                Token::Address(payment.sender()),
                                Token::Address(taker_addr),
                                reward_target,
                                sends_contract_reward,
                                watcher_reward_amount,
                            ]));

                            clone.sign_and_send_transaction(
                                0.into(),
                                Call(swap_contract_address),
                                data,
                                U256::from(clone.gas_limit.eth_receiver_spend),
                            )
                        }),
                )
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let function_name = get_function_name("erc20Payment", watcher_reward);
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function(&function_name));

                let decoded = try_tx_fus!(decode_contract_call(payment_func, payment.unsigned().data()));
                let swap_id_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 0));
                let amount_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 1));

                let reward_target = try_tx_fus!(get_function_input_data(&decoded, payment_func, 6));
                let sends_contract_reward = try_tx_fus!(get_function_input_data(&decoded, payment_func, 7));
                let reward_amount = try_tx_fus!(get_function_input_data(&decoded, payment_func, 8));

                let state_f = self.payment_status(swap_contract_address, swap_id_input.clone());

                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != U256::from(PaymentState::Sent as u8) {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }
                            let data = try_tx_fus!(spend_func.encode_input(&[
                                swap_id_input.clone(),
                                amount_input,
                                Token::FixedBytes(secret_vec.clone()),
                                Token::Address(token_addr),
                                Token::Address(payment.sender()),
                                Token::Address(taker_addr),
                                reward_target,
                                sends_contract_reward,
                                reward_amount
                            ]));
                            clone.sign_and_send_transaction(
                                0.into(),
                                Call(swap_contract_address),
                                data,
                                U256::from(clone.gas_limit.erc20_receiver_spend),
                            )
                        }),
                )
            },
            EthCoinType::Nft { .. } => Box::new(futures01::future::err(TransactionErr::ProtocolNotSupported(ERRL!(
                "Nft Protocol is not supported yet!"
            )))),
        }
    }

    fn watcher_refunds_hash_time_locked_payment(&self, args: RefundPaymentArgs) -> EthTxFut {
        let tx: UnverifiedTransactionWrapper = try_tx_fus!(rlp::decode(args.payment_tx));
        let payment = try_tx_fus!(SignedEthTx::new(tx));

        let function_name = get_function_name("senderRefund", true);
        let refund_func = try_tx_fus!(SWAP_CONTRACT.function(&function_name));

        let clone = self.clone();
        let taker_addr = addr_from_raw_pubkey(args.other_pubkey).unwrap();
        let swap_contract_address = match payment.unsigned().action() {
            Call(address) => *address,
            Create => {
                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                    "Invalid payment action: the payment action cannot be create"
                ))))
            },
        };

        match self.coin_type {
            EthCoinType::Eth => {
                let function_name = get_function_name("ethPayment", true);
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function(&function_name));
                let decoded = try_tx_fus!(decode_contract_call(payment_func, payment.unsigned().data()));
                let swap_id_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 0));
                let receiver_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 1));
                let hash_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 2));

                let state_f = self.payment_status(swap_contract_address, swap_id_input.clone());
                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != U256::from(PaymentState::Sent as u8) {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }

                            let value = payment.unsigned().value();
                            let reward_target = try_tx_fus!(get_function_input_data(&decoded, payment_func, 4));
                            let sends_contract_reward = try_tx_fus!(get_function_input_data(&decoded, payment_func, 5));
                            let reward_amount = try_tx_fus!(get_function_input_data(&decoded, payment_func, 6));

                            let data = try_tx_fus!(refund_func.encode_input(&[
                                swap_id_input.clone(),
                                Token::Uint(value),
                                hash_input.clone(),
                                Token::Address(Address::default()),
                                Token::Address(taker_addr),
                                receiver_input.clone(),
                                reward_target,
                                sends_contract_reward,
                                reward_amount
                            ]));

                            clone.sign_and_send_transaction(
                                0.into(),
                                Call(swap_contract_address),
                                data,
                                U256::from(clone.gas_limit.eth_sender_refund),
                            )
                        }),
                )
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let function_name = get_function_name("erc20Payment", true);
                let payment_func = try_tx_fus!(SWAP_CONTRACT.function(&function_name));

                let decoded = try_tx_fus!(decode_contract_call(payment_func, payment.unsigned().data()));
                let swap_id_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 0));
                let amount_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 1));
                let receiver_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 3));
                let hash_input = try_tx_fus!(get_function_input_data(&decoded, payment_func, 4));

                let reward_target = try_tx_fus!(get_function_input_data(&decoded, payment_func, 6));
                let sends_contract_reward = try_tx_fus!(get_function_input_data(&decoded, payment_func, 7));
                let reward_amount = try_tx_fus!(get_function_input_data(&decoded, payment_func, 8));

                let state_f = self.payment_status(swap_contract_address, swap_id_input.clone());
                Box::new(
                    state_f
                        .map_err(TransactionErr::Plain)
                        .and_then(move |state| -> EthTxFut {
                            if state != U256::from(PaymentState::Sent as u8) {
                                return Box::new(futures01::future::err(TransactionErr::Plain(ERRL!(
                                    "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                                    payment,
                                    state
                                ))));
                            }

                            let data = try_tx_fus!(refund_func.encode_input(&[
                                swap_id_input.clone(),
                                amount_input.clone(),
                                hash_input.clone(),
                                Token::Address(token_addr),
                                Token::Address(taker_addr),
                                receiver_input.clone(),
                                reward_target,
                                sends_contract_reward,
                                reward_amount
                            ]));

                            clone.sign_and_send_transaction(
                                0.into(),
                                Call(swap_contract_address),
                                data,
                                U256::from(clone.gas_limit.erc20_sender_refund),
                            )
                        }),
                )
            },
            EthCoinType::Nft { .. } => Box::new(futures01::future::err(TransactionErr::ProtocolNotSupported(ERRL!(
                "Nft Protocol is not supported yet!"
            )))),
        }
    }

    async fn spend_hash_time_locked_payment<'a>(
        &self,
        args: SpendPaymentArgs<'a>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let tx: UnverifiedTransactionWrapper = try_tx_s!(rlp::decode(args.other_payment_tx));
        let payment = try_tx_s!(SignedEthTx::new(tx));
        let my_address = try_tx_s!(self.derivation_method.single_addr_or_err().await);
        let swap_contract_address = try_tx_s!(args.swap_contract_address.try_to_address());

        let function_name = get_function_name("receiverSpend", args.watcher_reward);
        let spend_func = try_tx_s!(SWAP_CONTRACT.function(&function_name));

        let secret_vec = args.secret.to_vec();
        let watcher_reward = args.watcher_reward;

        match self.coin_type {
            EthCoinType::Eth => {
                let function_name = get_function_name("ethPayment", watcher_reward);
                let payment_func = try_tx_s!(SWAP_CONTRACT.function(&function_name));
                let decoded = try_tx_s!(decode_contract_call(payment_func, payment.unsigned().data()));

                let state = try_tx_s!(
                    self.payment_status(swap_contract_address, decoded[0].clone())
                        .compat()
                        .await
                );
                if state != U256::from(PaymentState::Sent as u8) {
                    return Err(TransactionErr::Plain(ERRL!(
                        "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                        payment,
                        state
                    )));
                }

                let data = if watcher_reward {
                    try_tx_s!(spend_func.encode_input(&[
                        decoded[0].clone(),
                        Token::Uint(payment.unsigned().value()),
                        Token::FixedBytes(secret_vec),
                        Token::Address(Address::default()),
                        Token::Address(payment.sender()),
                        Token::Address(my_address),
                        decoded[4].clone(),
                        decoded[5].clone(),
                        decoded[6].clone(),
                    ]))
                } else {
                    try_tx_s!(spend_func.encode_input(&[
                        decoded[0].clone(),
                        Token::Uint(payment.unsigned().value()),
                        Token::FixedBytes(secret_vec),
                        Token::Address(Address::default()),
                        Token::Address(payment.sender()),
                    ]))
                };

                self.sign_and_send_transaction(
                    0.into(),
                    Call(swap_contract_address),
                    data,
                    U256::from(self.gas_limit.eth_receiver_spend),
                )
                .compat()
                .await
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let function_name = get_function_name("erc20Payment", watcher_reward);
                let payment_func = try_tx_s!(SWAP_CONTRACT.function(&function_name));

                let decoded = try_tx_s!(decode_contract_call(payment_func, payment.unsigned().data()));
                let state = try_tx_s!(
                    self.payment_status(swap_contract_address, decoded[0].clone())
                        .compat()
                        .await
                );
                if state != U256::from(PaymentState::Sent as u8) {
                    return Err(TransactionErr::Plain(ERRL!(
                        "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                        payment,
                        state
                    )));
                }

                let data = if watcher_reward {
                    try_tx_s!(spend_func.encode_input(&[
                        decoded[0].clone(),
                        decoded[1].clone(),
                        Token::FixedBytes(secret_vec),
                        Token::Address(token_addr),
                        Token::Address(payment.sender()),
                        Token::Address(my_address),
                        decoded[6].clone(),
                        decoded[7].clone(),
                        decoded[8].clone(),
                    ]))
                } else {
                    try_tx_s!(spend_func.encode_input(&[
                        decoded[0].clone(),
                        decoded[1].clone(),
                        Token::FixedBytes(secret_vec),
                        Token::Address(token_addr),
                        Token::Address(payment.sender()),
                    ]))
                };

                self.sign_and_send_transaction(
                    0.into(),
                    Call(swap_contract_address),
                    data,
                    U256::from(self.gas_limit.erc20_receiver_spend),
                )
                .compat()
                .await
            },
            EthCoinType::Nft { .. } => Err(TransactionErr::ProtocolNotSupported(ERRL!(
                "Nft Protocol is not supported!"
            ))),
        }
    }

    async fn refund_hash_time_locked_payment<'a>(
        &self,
        args: RefundPaymentArgs<'a>,
    ) -> Result<SignedEthTx, TransactionErr> {
        let tx: UnverifiedTransactionWrapper = try_tx_s!(rlp::decode(args.payment_tx));
        let payment = try_tx_s!(SignedEthTx::new(tx));
        let my_address = try_tx_s!(self.derivation_method.single_addr_or_err().await);
        let swap_contract_address = try_tx_s!(args.swap_contract_address.try_to_address());

        let function_name = get_function_name("senderRefund", args.watcher_reward);
        let refund_func = try_tx_s!(SWAP_CONTRACT.function(&function_name));
        let watcher_reward = args.watcher_reward;

        match self.coin_type {
            EthCoinType::Eth => {
                let function_name = get_function_name("ethPayment", watcher_reward);
                let payment_func = try_tx_s!(SWAP_CONTRACT.function(&function_name));

                let decoded = try_tx_s!(decode_contract_call(payment_func, payment.unsigned().data()));

                let state = try_tx_s!(
                    self.payment_status(swap_contract_address, decoded[0].clone())
                        .compat()
                        .await
                );
                if state != U256::from(PaymentState::Sent as u8) {
                    return Err(TransactionErr::Plain(ERRL!(
                        "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                        payment,
                        state
                    )));
                }

                let value = payment.unsigned().value();
                let data = if watcher_reward {
                    try_tx_s!(refund_func.encode_input(&[
                        decoded[0].clone(),
                        Token::Uint(value),
                        decoded[2].clone(),
                        Token::Address(Address::default()),
                        Token::Address(my_address),
                        decoded[1].clone(),
                        decoded[4].clone(),
                        decoded[5].clone(),
                        decoded[6].clone(),
                    ]))
                } else {
                    try_tx_s!(refund_func.encode_input(&[
                        decoded[0].clone(),
                        Token::Uint(value),
                        decoded[2].clone(),
                        Token::Address(Address::default()),
                        decoded[1].clone(),
                    ]))
                };

                self.sign_and_send_transaction(
                    0.into(),
                    Call(swap_contract_address),
                    data,
                    U256::from(self.gas_limit.eth_sender_refund),
                )
                .compat()
                .await
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                let function_name = get_function_name("erc20Payment", watcher_reward);
                let payment_func = try_tx_s!(SWAP_CONTRACT.function(&function_name));

                let decoded = try_tx_s!(decode_contract_call(payment_func, payment.unsigned().data()));
                let state = try_tx_s!(
                    self.payment_status(swap_contract_address, decoded[0].clone())
                        .compat()
                        .await
                );
                if state != U256::from(PaymentState::Sent as u8) {
                    return Err(TransactionErr::Plain(ERRL!(
                        "Payment {:?} state is not PAYMENT_STATE_SENT, got {}",
                        payment,
                        state
                    )));
                }

                let data = if watcher_reward {
                    try_tx_s!(refund_func.encode_input(&[
                        decoded[0].clone(),
                        decoded[1].clone(),
                        decoded[4].clone(),
                        Token::Address(token_addr),
                        Token::Address(my_address),
                        decoded[3].clone(),
                        decoded[6].clone(),
                        decoded[7].clone(),
                        decoded[8].clone(),
                    ]))
                } else {
                    try_tx_s!(refund_func.encode_input(&[
                        decoded[0].clone(),
                        decoded[1].clone(),
                        decoded[4].clone(),
                        Token::Address(token_addr),
                        decoded[3].clone(),
                    ]))
                };

                self.sign_and_send_transaction(
                    0.into(),
                    Call(swap_contract_address),
                    data,
                    U256::from(self.gas_limit.erc20_sender_refund),
                )
                .compat()
                .await
            },
            EthCoinType::Nft { .. } => Err(TransactionErr::ProtocolNotSupported(ERRL!(
                "Nft Protocol is not supported yet!"
            ))),
        }
    }

    fn address_balance(&self, address: Address) -> BalanceFut<U256> {
        let coin = self.clone();
        let fut = async move {
            match coin.coin_type {
                EthCoinType::Eth => Ok(coin.balance(address, Some(BlockNumber::Latest)).await?),
                EthCoinType::Erc20 { ref token_addr, .. } => {
                    let function = ERC20_CONTRACT.function("balanceOf")?;
                    let data = function.encode_input(&[Token::Address(address)])?;

                    let res = coin
                        .call_request(address, *token_addr, None, Some(data.into()), BlockNumber::Latest)
                        .await?;
                    let decoded = function.decode_output(&res.0)?;
                    match decoded[0] {
                        Token::Uint(number) => Ok(number),
                        _ => {
                            let error = format!("Expected U256 as balanceOf result but got {:?}", decoded);
                            MmError::err(BalanceError::InvalidResponse(error))
                        },
                    }
                },
                EthCoinType::Nft { .. } => {
                    MmError::err(BalanceError::Internal("Nft Protocol is not supported yet!".to_string()))
                },
            }
        };
        Box::new(fut.boxed().compat())
    }

    fn get_balance(&self) -> BalanceFut<U256> {
        let coin = self.clone();
        let fut = async move {
            let my_address = coin.derivation_method.single_addr_or_err().await?;
            coin.address_balance(my_address).compat().await
        };
        Box::new(fut.boxed().compat())
    }

    pub async fn get_tokens_balance_list_for_address(
        &self,
        address: Address,
    ) -> Result<CoinBalanceMap, MmError<BalanceError>> {
        let coin = || self;

        let tokens = self.get_erc_tokens_infos();
        let mut requests = Vec::with_capacity(tokens.len());

        for (token_ticker, info) in tokens {
            let fut = async move {
                let balance_as_u256 = coin()
                    .get_token_balance_for_address(address, info.token_address)
                    .await?;
                let balance_as_big_decimal = u256_to_big_decimal(balance_as_u256, info.decimals)?;
                let balance = CoinBalance::new(balance_as_big_decimal);
                Ok((token_ticker, balance))
            };
            requests.push(fut);
        }

        try_join_all(requests).await.map(|res| res.into_iter().collect())
    }

    pub async fn get_tokens_balance_list(&self) -> Result<CoinBalanceMap, MmError<BalanceError>> {
        let my_address = self.derivation_method.single_addr_or_err().await?;
        self.get_tokens_balance_list_for_address(my_address).await
    }

    async fn get_token_balance_for_address(
        &self,
        address: Address,
        token_address: Address,
    ) -> Result<U256, MmError<BalanceError>> {
        let function = ERC20_CONTRACT.function("balanceOf")?;
        let data = function.encode_input(&[Token::Address(address)])?;
        let res = self
            .call_request(address, token_address, None, Some(data.into()), BlockNumber::Latest)
            .await?;
        let decoded = function.decode_output(&res.0)?;

        match decoded[0] {
            Token::Uint(number) => Ok(number),
            _ => {
                let error = format!("Expected U256 as balanceOf result but got {:?}", decoded);
                MmError::err(BalanceError::InvalidResponse(error))
            },
        }
    }

    async fn get_token_balance(&self, token_address: Address) -> Result<U256, MmError<BalanceError>> {
        let my_address = self.derivation_method.single_addr_or_err().await?;
        self.get_token_balance_for_address(my_address, token_address).await
    }

    async fn erc1155_balance(&self, token_addr: Address, token_id: &str) -> MmResult<BigUint, BalanceError> {
        let wallet_amount_uint = match self.coin_type {
            EthCoinType::Eth | EthCoinType::Nft { .. } => {
                let function = ERC1155_CONTRACT.function("balanceOf")?;
                let token_id_u256 =
                    U256::from_dec_str(token_id).map_to_mm(|e| NumConversError::new(format!("{:?}", e)))?;
                let my_address = self.derivation_method.single_addr_or_err().await?;
                let data = function.encode_input(&[Token::Address(my_address), Token::Uint(token_id_u256)])?;
                let result = self
                    .call_request(my_address, token_addr, None, Some(data.into()), BlockNumber::Latest)
                    .await?;
                let decoded = function.decode_output(&result.0)?;
                match decoded[0] {
                    Token::Uint(number) => number,
                    _ => {
                        let error = format!("Expected U256 as balanceOf result but got {:?}", decoded);
                        return MmError::err(BalanceError::InvalidResponse(error));
                    },
                }
            },
            EthCoinType::Erc20 { .. } => {
                return MmError::err(BalanceError::Internal(
                    "Erc20 coin type doesnt support Erc1155 standard".to_owned(),
                ))
            },
        };
        // The "balanceOf" function in ERC1155 standard returns the exact count of tokens held by address without any decimals or scaling factors
        let wallet_amount = wallet_amount_uint.to_string().parse::<BigUint>()?;
        Ok(wallet_amount)
    }

    async fn erc721_owner(&self, token_addr: Address, token_id: &str) -> MmResult<Address, GetNftInfoError> {
        let owner_address = match self.coin_type {
            EthCoinType::Eth | EthCoinType::Nft { .. } => {
                let function = ERC721_CONTRACT.function("ownerOf")?;
                let token_id_u256 =
                    U256::from_dec_str(token_id).map_to_mm(|e| NumConversError::new(format!("{:?}", e)))?;
                let data = function.encode_input(&[Token::Uint(token_id_u256)])?;
                let my_address = self.derivation_method.single_addr_or_err().await?;
                let result = self
                    .call_request(my_address, token_addr, None, Some(data.into()), BlockNumber::Latest)
                    .await?;
                let decoded = function.decode_output(&result.0)?;
                match decoded[0] {
                    Token::Address(owner) => owner,
                    _ => {
                        let error = format!("Expected Address as ownerOf result but got {:?}", decoded);
                        return MmError::err(GetNftInfoError::InvalidResponse(error));
                    },
                }
            },
            EthCoinType::Erc20 { .. } => {
                return MmError::err(GetNftInfoError::Internal(
                    "Erc20 coin type doesnt support Erc721 standard".to_owned(),
                ))
            },
        };
        Ok(owner_address)
    }

    fn estimate_gas_wrapper(&self, req: CallRequest) -> Box<dyn Future<Item = U256, Error = web3::Error> + Send> {
        let coin = self.clone();

        // always using None block number as old Geth version accept only single argument in this RPC
        let fut = async move { coin.estimate_gas(req, None).await };

        Box::new(fut.boxed().compat())
    }

    /// Estimates how much gas is necessary to allow the contract call to complete.
    /// `contract_addr` can be a ERC20 token address or any other contract address.
    ///
    /// # Important
    ///
    /// Don't use this method to estimate gas for a withdrawal of `ETH` coin.
    /// For more details, see `withdraw_impl`.
    ///
    /// Also, note that the contract call has to be initiated by my wallet address,
    /// because [`CallRequest::from`] is set to [`EthCoinImpl::my_address`].
    async fn estimate_gas_for_contract_call(&self, contract_addr: Address, call_data: Bytes) -> Web3RpcResult<U256> {
        let coin = self.clone();
        let my_address = coin.derivation_method.single_addr_or_err().await?;
        let fee_policy_for_estimate = get_swap_fee_policy_for_estimate(self.get_swap_transaction_fee_policy());
        let pay_for_gas_option = coin.get_swap_pay_for_gas_option(fee_policy_for_estimate).await?;
        let eth_value = U256::zero();
        let estimate_gas_req = CallRequest {
            value: Some(eth_value),
            data: Some(call_data),
            from: Some(my_address),
            to: Some(contract_addr),
            ..CallRequest::default()
        };
        // gas price must be supplied because some smart contracts base their
        // logic on gas price, e.g. TUSD: https://github.com/KomodoPlatform/atomicDEX-API/issues/643
        let estimate_gas_req = call_request_with_pay_for_gas_option(estimate_gas_req, pay_for_gas_option);
        coin.estimate_gas_wrapper(estimate_gas_req)
            .compat()
            .await
            .map_to_mm(Web3RpcError::from)
    }

    fn eth_balance(&self) -> BalanceFut<U256> {
        let coin = self.clone();
        let fut = async move {
            let my_address = coin.derivation_method.single_addr_or_err().await?;
            coin.balance(my_address, Some(BlockNumber::Latest))
                .await
                .map_to_mm(BalanceError::from)
        };
        Box::new(fut.boxed().compat())
    }

    pub(crate) async fn call_request(
        &self,
        from: Address,
        to: Address,
        value: Option<U256>,
        data: Option<Bytes>,
        block_number: BlockNumber,
    ) -> Result<Bytes, web3::Error> {
        let request = CallRequest {
            from: Some(from),
            to: Some(to),
            gas: None,
            gas_price: None,
            value,
            data,
            ..CallRequest::default()
        };

        self.call(request, Some(BlockId::Number(block_number))).await
    }

    pub fn allowance(&self, spender: Address) -> Web3RpcFut<U256> {
        let coin = self.clone();
        let fut = async move {
            match coin.coin_type {
                EthCoinType::Eth => MmError::err(Web3RpcError::Internal(
                    "'allowance' must not be called for ETH coin".to_owned(),
                )),
                EthCoinType::Erc20 { ref token_addr, .. } => {
                    let function = ERC20_CONTRACT.function("allowance")?;
                    let my_address = coin.derivation_method.single_addr_or_err().await?;
                    let data = function.encode_input(&[Token::Address(my_address), Token::Address(spender)])?;

                    let res = coin
                        .call_request(my_address, *token_addr, None, Some(data.into()), BlockNumber::Latest)
                        .await?;
                    let decoded = function.decode_output(&res.0)?;

                    match decoded[0] {
                        Token::Uint(number) => Ok(number),
                        _ => {
                            let error = format!("Expected U256 as allowance result but got {:?}", decoded);
                            MmError::err(Web3RpcError::InvalidResponse(error))
                        },
                    }
                },
                EthCoinType::Nft { .. } => MmError::err(Web3RpcError::NftProtocolNotSupported),
            }
        };
        Box::new(fut.boxed().compat())
    }

    fn wait_for_required_allowance(
        &self,
        spender: Address,
        required_allowance: U256,
        wait_until: u64,
    ) -> Web3RpcFut<()> {
        const CHECK_ALLOWANCE_EVERY: f64 = 5.;

        let selfi = self.clone();
        let fut = async move {
            loop {
                if now_sec() > wait_until {
                    return MmError::err(Web3RpcError::Timeout(ERRL!(
                        "Waited too long until {} for allowance to be updated to at least {}",
                        wait_until,
                        required_allowance
                    )));
                }

                match selfi.allowance(spender).compat().await {
                    Ok(allowed) if allowed >= required_allowance => return Ok(()),
                    Ok(_allowed) => (),
                    Err(e) => match e.get_inner() {
                        Web3RpcError::Transport(e) => error!("Error {} on trying to get the allowed amount!", e),
                        _ => return Err(e),
                    },
                }

                Timer::sleep(CHECK_ALLOWANCE_EVERY).await;
            }
        };
        Box::new(fut.boxed().compat())
    }

    pub fn approve(&self, spender: Address, amount: U256) -> EthTxFut {
        let coin = self.clone();
        let fut = async move {
            let token_addr = match coin.coin_type {
                EthCoinType::Eth => return TX_PLAIN_ERR!("'approve' is expected to be call for ERC20 coins only"),
                EthCoinType::Erc20 { token_addr, .. } => token_addr,
                EthCoinType::Nft { .. } => {
                    return Err(TransactionErr::ProtocolNotSupported(ERRL!(
                        "Nft Protocol is not supported by 'approve'!"
                    )))
                },
            };
            let function = try_tx_s!(ERC20_CONTRACT.function("approve"));
            let data = try_tx_s!(function.encode_input(&[Token::Address(spender), Token::Uint(amount)]));

            let gas_limit = try_tx_s!(
                coin.estimate_gas_for_contract_call(token_addr, Bytes::from(data.clone()))
                    .await
            );

            coin.sign_and_send_transaction(0.into(), Call(token_addr), data, gas_limit)
                .compat()
                .await
        };
        Box::new(fut.boxed().compat())
    }

    /// Gets `PaymentSent` events from etomic swap smart contract since `from_block`
    fn payment_sent_events(
        &self,
        swap_contract_address: Address,
        from_block: u64,
        to_block: u64,
    ) -> Box<dyn Future<Item = Vec<Log>, Error = String> + Send> {
        let contract_event = try_fus!(SWAP_CONTRACT.event("PaymentSent"));
        let filter = FilterBuilder::default()
            .topics(Some(vec![contract_event.signature()]), None, None, None)
            .from_block(BlockNumber::Number(from_block.into()))
            .to_block(BlockNumber::Number(to_block.into()))
            .address(vec![swap_contract_address])
            .build();

        let coin = self.clone();

        let fut = async move { coin.logs(filter).await.map_err(|e| ERRL!("{}", e)) };
        Box::new(fut.boxed().compat())
    }

    /// Returns events from `from_block` to `to_block` or current `latest` block.
    /// According to ["eth_getLogs" doc](https://docs.infura.io/api/networks/ethereum/json-rpc-methods/eth_getlogs) `toBlock` is optional, default is "latest".
    async fn events_from_block(
        &self,
        swap_contract_address: Address,
        event_name: &str,
        from_block: u64,
        to_block: Option<u64>,
        swap_contract: &Contract,
    ) -> MmResult<Vec<Log>, FindPaymentSpendError> {
        let contract_event = swap_contract.event(event_name)?;
        let mut filter_builder = FilterBuilder::default()
            .topics(Some(vec![contract_event.signature()]), None, None, None)
            .from_block(BlockNumber::Number(from_block.into()))
            .address(vec![swap_contract_address]);
        if let Some(block) = to_block {
            filter_builder = filter_builder.to_block(BlockNumber::Number(block.into()));
        }
        let filter = filter_builder.build();
        let events_logs = self
            .logs(filter)
            .await
            .map_err(|e| FindPaymentSpendError::Transport(e.to_string()))?;
        Ok(events_logs)
    }

    fn validate_payment(&self, input: ValidatePaymentInput) -> ValidatePaymentFut<()> {
        let expected_swap_contract_address = try_f!(input
            .swap_contract_address
            .try_to_address()
            .map_to_mm(ValidatePaymentError::InvalidParameter));

        let unsigned: UnverifiedTransactionWrapper = try_f!(rlp::decode(&input.payment_tx));
        let tx =
            try_f!(SignedEthTx::new(unsigned)
                .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string())));
        let sender = try_f!(addr_from_raw_pubkey(&input.other_pub).map_to_mm(ValidatePaymentError::InvalidParameter));
        let time_lock = try_f!(input
            .time_lock
            .try_into()
            .map_to_mm(ValidatePaymentError::TimelockOverflow));

        let selfi = self.clone();
        let swap_id = selfi.etomic_swap_id(time_lock, &input.secret_hash);
        let decimals = self.decimals;
        let secret_hash = if input.secret_hash.len() == 32 {
            ripemd160(&input.secret_hash).to_vec()
        } else {
            input.secret_hash.to_vec()
        };
        let trade_amount = try_f!(wei_from_big_decimal(&(input.amount), decimals));
        let fut = async move {
            let status = selfi
                .payment_status(expected_swap_contract_address, Token::FixedBytes(swap_id.clone()))
                .compat()
                .await
                .map_to_mm(ValidatePaymentError::Transport)?;
            if status != U256::from(PaymentState::Sent as u8) {
                return MmError::err(ValidatePaymentError::UnexpectedPaymentState(format!(
                    "Payment state is not PAYMENT_STATE_SENT, got {}",
                    status
                )));
            }

            let tx_from_rpc = selfi.transaction(TransactionId::Hash(tx.tx_hash())).await?;
            let tx_from_rpc = tx_from_rpc.as_ref().ok_or_else(|| {
                ValidatePaymentError::TxDoesNotExist(format!("Didn't find provided tx {:?} on ETH node", tx.tx_hash()))
            })?;

            if tx_from_rpc.from != Some(sender) {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "Payment tx {:?} was sent from wrong address, expected {:?}",
                    tx_from_rpc, sender
                )));
            }

            let my_address = selfi.derivation_method.single_addr_or_err().await?;
            match &selfi.coin_type {
                EthCoinType::Eth => {
                    let mut expected_value = trade_amount;

                    if tx_from_rpc.to != Some(expected_swap_contract_address) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx {:?} was sent to wrong address, expected {:?}",
                            tx_from_rpc, expected_swap_contract_address,
                        )));
                    }

                    let function_name = get_function_name("ethPayment", input.watcher_reward.is_some());
                    let function = SWAP_CONTRACT
                        .function(&function_name)
                        .map_to_mm(|err| ValidatePaymentError::InternalError(err.to_string()))?;

                    let decoded = decode_contract_call(function, &tx_from_rpc.input.0)
                        .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string()))?;

                    if decoded[0] != Token::FixedBytes(swap_id.clone()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Invalid 'swap_id' {:?}, expected {:?}",
                            decoded, swap_id
                        )));
                    }

                    if decoded[1] != Token::Address(my_address) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx receiver arg {:?} is invalid, expected {:?}",
                            decoded[1],
                            Token::Address(my_address)
                        )));
                    }

                    if decoded[2] != Token::FixedBytes(secret_hash.to_vec()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx secret_hash arg {:?} is invalid, expected {:?}",
                            decoded[2],
                            Token::FixedBytes(secret_hash.to_vec()),
                        )));
                    }

                    if decoded[3] != Token::Uint(U256::from(input.time_lock)) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx time_lock arg {:?} is invalid, expected {:?}",
                            decoded[3],
                            Token::Uint(U256::from(input.time_lock)),
                        )));
                    }

                    if let Some(watcher_reward) = input.watcher_reward {
                        if decoded[4] != Token::Uint(U256::from(watcher_reward.reward_target as u8)) {
                            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                                "Payment tx reward target arg {:?} is invalid, expected {:?}",
                                decoded[4], watcher_reward.reward_target as u8
                            )));
                        }

                        if decoded[5] != Token::Bool(watcher_reward.send_contract_reward_on_spend) {
                            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                                "Payment tx sends_contract_reward_on_spend arg {:?} is invalid, expected {:?}",
                                decoded[5], watcher_reward.send_contract_reward_on_spend
                            )));
                        }

                        let expected_reward_amount = wei_from_big_decimal(&watcher_reward.amount, decimals)?;
                        let actual_reward_amount = decoded[6].clone().into_uint().ok_or_else(|| {
                            ValidatePaymentError::WrongPaymentTx("Invalid type for watcher reward argument".to_string())
                        })?;

                        validate_watcher_reward(
                            expected_reward_amount.as_u64(),
                            actual_reward_amount.as_u64(),
                            watcher_reward.is_exact_amount,
                        )?;

                        match watcher_reward.reward_target {
                            RewardTarget::None | RewardTarget::PaymentReceiver => {
                                if watcher_reward.send_contract_reward_on_spend {
                                    expected_value += actual_reward_amount
                                }
                            },
                            RewardTarget::PaymentSender | RewardTarget::PaymentSpender | RewardTarget::Contract => {
                                expected_value += actual_reward_amount
                            },
                        };
                    }

                    if tx_from_rpc.value != expected_value {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx value arg {:?} is invalid, expected {:?}",
                            tx_from_rpc.value, trade_amount
                        )));
                    }
                },
                EthCoinType::Erc20 {
                    platform: _,
                    token_addr,
                } => {
                    let mut expected_value = U256::from(0);
                    let mut expected_amount = trade_amount;

                    if tx_from_rpc.to != Some(expected_swap_contract_address) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx {:?} was sent to wrong address, expected {:?}",
                            tx_from_rpc, expected_swap_contract_address,
                        )));
                    }
                    let function_name = get_function_name("erc20Payment", input.watcher_reward.is_some());
                    let function = SWAP_CONTRACT
                        .function(&function_name)
                        .map_to_mm(|err| ValidatePaymentError::InternalError(err.to_string()))?;
                    let decoded = decode_contract_call(function, &tx_from_rpc.input.0)
                        .map_to_mm(|err| ValidatePaymentError::TxDeserializationError(err.to_string()))?;

                    if decoded[0] != Token::FixedBytes(swap_id.clone()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Invalid 'swap_id' {:?}, expected {:?}",
                            decoded, swap_id
                        )));
                    }

                    if decoded[2] != Token::Address(*token_addr) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx token_addr arg {:?} is invalid, expected {:?}",
                            decoded[2],
                            Token::Address(*token_addr)
                        )));
                    }

                    if decoded[3] != Token::Address(my_address) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx receiver arg {:?} is invalid, expected {:?}",
                            decoded[3],
                            Token::Address(my_address),
                        )));
                    }

                    if decoded[4] != Token::FixedBytes(secret_hash.to_vec()) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx secret_hash arg {:?} is invalid, expected {:?}",
                            decoded[4],
                            Token::FixedBytes(secret_hash.to_vec()),
                        )));
                    }

                    if decoded[5] != Token::Uint(U256::from(input.time_lock)) {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx time_lock arg {:?} is invalid, expected {:?}",
                            decoded[5],
                            Token::Uint(U256::from(input.time_lock)),
                        )));
                    }

                    if let Some(watcher_reward) = input.watcher_reward {
                        if decoded[6] != Token::Uint(U256::from(watcher_reward.reward_target as u8)) {
                            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                                "Payment tx reward target arg {:?} is invalid, expected {:?}",
                                decoded[4], watcher_reward.reward_target as u8
                            )));
                        }

                        if decoded[7] != Token::Bool(watcher_reward.send_contract_reward_on_spend) {
                            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                                "Payment tx sends_contract_reward_on_spend arg {:?} is invalid, expected {:?}",
                                decoded[5], watcher_reward.send_contract_reward_on_spend
                            )));
                        }

                        let expected_reward_amount = match watcher_reward.reward_target {
                            RewardTarget::Contract | RewardTarget::PaymentSender => {
                                wei_from_big_decimal(&watcher_reward.amount, ETH_DECIMALS)?
                            },
                            RewardTarget::PaymentSpender => {
                                wei_from_big_decimal(&watcher_reward.amount, selfi.decimals)?
                            },
                            _ => {
                                // TODO tests passed without this change, need to research on how it worked
                                if watcher_reward.send_contract_reward_on_spend {
                                    wei_from_big_decimal(&watcher_reward.amount, ETH_DECIMALS)?
                                } else {
                                    0.into()
                                }
                            },
                        };

                        let actual_reward_amount = get_function_input_data(&decoded, function, 8)
                            .map_to_mm(ValidatePaymentError::TxDeserializationError)?
                            .into_uint()
                            .ok_or_else(|| {
                                ValidatePaymentError::WrongPaymentTx(
                                    "Invalid type for watcher reward argument".to_string(),
                                )
                            })?;

                        validate_watcher_reward(
                            expected_reward_amount.as_u64(),
                            actual_reward_amount.as_u64(),
                            watcher_reward.is_exact_amount,
                        )?;

                        match watcher_reward.reward_target {
                            RewardTarget::PaymentSender | RewardTarget::Contract => {
                                expected_value += actual_reward_amount
                            },
                            RewardTarget::PaymentSpender => expected_amount += actual_reward_amount,
                            _ => {
                                if watcher_reward.send_contract_reward_on_spend {
                                    expected_value += actual_reward_amount
                                }
                            },
                        };

                        if decoded[1] != Token::Uint(expected_amount) {
                            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                                "Payment tx amount arg {:?} is invalid, expected {:?}",
                                decoded[1], expected_amount,
                            )));
                        }
                    }

                    if tx_from_rpc.value != expected_value {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Payment tx value arg {:?} is invalid, expected {:?}",
                            tx_from_rpc.value, expected_value
                        )));
                    }
                },
                EthCoinType::Nft { .. } => {
                    return MmError::err(ValidatePaymentError::ProtocolNotSupported(
                        "Nft protocol is not supported by legacy swap".to_string(),
                    ))
                },
            }

            Ok(())
        };
        Box::new(fut.boxed().compat())
    }

    fn payment_status(
        &self,
        swap_contract_address: H160,
        token: Token,
    ) -> Box<dyn Future<Item = U256, Error = String> + Send + 'static> {
        let function = try_fus!(SWAP_CONTRACT.function("payments"));

        let data = try_fus!(function.encode_input(&[token]));

        let coin = self.clone();
        let fut = async move {
            let my_address = coin
                .derivation_method
                .single_addr_or_err()
                .await
                .map_err(|e| ERRL!("{}", e))?;
            coin.call_request(
                my_address,
                swap_contract_address,
                None,
                Some(data.into()),
                // TODO worth reviewing places where we could use BlockNumber::Pending
                BlockNumber::Latest,
            )
            .await
            .map_err(|e| ERRL!("{}", e))
        };

        Box::new(fut.boxed().compat().and_then(move |bytes| {
            let decoded_tokens = try_s!(function.decode_output(&bytes.0));
            let state = decoded_tokens
                .get(2)
                .ok_or_else(|| ERRL!("Payment status must contain 'state' as the 2nd token"))?;
            match state {
                Token::Uint(state) => Ok(*state),
                _ => ERR!("Payment status must be uint, got {:?}", state),
            }
        }))
    }

    async fn search_for_swap_tx_spend(
        &self,
        tx: &[u8],
        swap_contract_address: Address,
        _secret_hash: &[u8],
        search_from_block: u64,
        watcher_reward: bool,
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let unverified: UnverifiedTransactionWrapper = try_s!(rlp::decode(tx));
        let tx = try_s!(SignedEthTx::new(unverified));

        let func_name = match self.coin_type {
            EthCoinType::Eth => get_function_name("ethPayment", watcher_reward),
            EthCoinType::Erc20 { .. } => get_function_name("erc20Payment", watcher_reward),
            EthCoinType::Nft { .. } => return ERR!("Nft Protocol is not supported yet!"),
        };

        let payment_func = try_s!(SWAP_CONTRACT.function(&func_name));
        let decoded = try_s!(decode_contract_call(payment_func, tx.unsigned().data()));
        let id = match decoded.first() {
            Some(Token::FixedBytes(bytes)) => bytes.clone(),
            invalid_token => return ERR!("Expected Token::FixedBytes, got {:?}", invalid_token),
        };

        let mut current_block = try_s!(self.current_block().compat().await);
        if current_block < search_from_block {
            current_block = search_from_block;
        }

        let mut from_block = search_from_block;

        loop {
            let to_block = current_block.min(from_block + self.logs_block_range);

            let spend_events = try_s!(
                self.events_from_block(
                    swap_contract_address,
                    "ReceiverSpent",
                    from_block,
                    Some(to_block),
                    &SWAP_CONTRACT
                )
                .await
            );

            let found = spend_events.iter().find(|event| &event.data.0[..32] == id.as_slice());

            if let Some(event) = found {
                match event.transaction_hash {
                    Some(tx_hash) => {
                        let transaction = match try_s!(self.transaction(TransactionId::Hash(tx_hash)).await) {
                            Some(t) => t,
                            None => {
                                return ERR!("Found ReceiverSpent event, but transaction {:02x} is missing", tx_hash)
                            },
                        };

                        return Ok(Some(FoundSwapTxSpend::Spent(TransactionEnum::from(try_s!(
                            signed_tx_from_web3_tx(transaction)
                        )))));
                    },
                    None => return ERR!("Found ReceiverSpent event, but it doesn't have tx_hash"),
                }
            }

            let refund_events = try_s!(
                self.refund_events(swap_contract_address, from_block, to_block)
                    .compat()
                    .await
            );
            let found = refund_events.iter().find(|event| &event.data.0[..32] == id.as_slice());

            if let Some(event) = found {
                match event.transaction_hash {
                    Some(tx_hash) => {
                        let transaction = match try_s!(self.transaction(TransactionId::Hash(tx_hash)).await) {
                            Some(t) => t,
                            None => {
                                return ERR!("Found SenderRefunded event, but transaction {:02x} is missing", tx_hash)
                            },
                        };

                        return Ok(Some(FoundSwapTxSpend::Refunded(TransactionEnum::from(try_s!(
                            signed_tx_from_web3_tx(transaction)
                        )))));
                    },
                    None => return ERR!("Found SenderRefunded event, but it doesn't have tx_hash"),
                }
            }

            if to_block >= current_block {
                break;
            }
            from_block = to_block;
        }

        Ok(None)
    }

    pub async fn get_watcher_reward_amount(&self, wait_until: u64) -> Result<BigDecimal, MmError<WatcherRewardError>> {
        let pay_for_gas_option = repeatable!(async {
            self.get_swap_pay_for_gas_option(self.get_swap_transaction_fee_policy())
                .await
                .retry_on_err()
        })
        .until_s(wait_until)
        .repeat_every_secs(10.)
        .await
        .map_err(|_| WatcherRewardError::RPCError("Error getting the gas price".to_string()))?;

        let gas_cost_wei = calc_total_fee(U256::from(REWARD_GAS_AMOUNT), &pay_for_gas_option)
            .map_err(|e| WatcherRewardError::InternalError(e.to_string()))?;
        let gas_cost_eth = u256_to_big_decimal(gas_cost_wei, ETH_DECIMALS)
            .map_err(|e| WatcherRewardError::InternalError(e.to_string()))?;
        Ok(gas_cost_eth)
    }

    /// Get gas price
    pub async fn get_gas_price(&self) -> Web3RpcResult<U256> {
        let coin = self.clone();
        let eth_gas_price_fut = async {
            match coin.gas_price().await {
                Ok(eth_gas) => Some(eth_gas),
                Err(e) => {
                    error!("Error {} on eth_gasPrice request", e);
                    None
                },
            }
        }
        .boxed();

        let eth_fee_history_price_fut = async {
            match coin.eth_fee_history(U256::from(1u64), BlockNumber::Latest, &[]).await {
                Ok(res) => res
                    .base_fee_per_gas
                    .first()
                    .map(|val| increase_by_percent_one_gwei(*val, BASE_BLOCK_FEE_DIFF_PCT)),
                Err(e) => {
                    debug!("Error {} on eth_feeHistory request", e);
                    None
                },
            }
        }
        .boxed();

        let (eth_gas_price, eth_fee_history_price) = join(eth_gas_price_fut, eth_fee_history_price_fut).await;
        // on editions < 2021 the compiler will resolve array.into_iter() as (&array).into_iter()
        // https://doc.rust-lang.org/edition-guide/rust-2021/IntoIterator-for-arrays.html#details
        IntoIterator::into_iter([eth_gas_price, eth_fee_history_price])
            .flatten()
            .max()
            .or_mm_err(|| Web3RpcError::Internal("All requests failed".into()))
    }

    /// Get gas base fee and suggest priority tip fees for the next block (see EIP-1559)
    pub async fn get_eip1559_gas_fee(&self, use_simple: bool) -> Web3RpcResult<FeePerGasEstimated> {
        let coin = self.clone();
        let history_estimator_fut = FeePerGasSimpleEstimator::estimate_fee_by_history(&coin);
        let ctx =
            MmArc::from_weak(&coin.ctx).ok_or_else(|| MmError::new(Web3RpcError::Internal("ctx is null".into())))?;

        let gas_api_conf = ctx.conf["gas_api"].clone();
        if gas_api_conf.is_null() || use_simple {
            return history_estimator_fut
                .await
                .map_err(|e| MmError::new(Web3RpcError::Internal(e.to_string())));
        }
        let gas_api_conf: GasApiConfig = json::from_value(gas_api_conf)
            .map_err(|e| MmError::new(Web3RpcError::InvalidGasApiConfig(e.to_string())))?;
        let provider_estimator_fut = match gas_api_conf.provider {
            GasApiProvider::Infura => InfuraGasApiCaller::fetch_infura_fee_estimation(&gas_api_conf.url).boxed(),
            GasApiProvider::Blocknative => {
                BlocknativeGasApiCaller::fetch_blocknative_fee_estimation(&gas_api_conf.url).boxed()
            },
        };
        provider_estimator_fut
            .or_else(|provider_estimator_err| {
                debug!(
                    "Call to eth gas api provider failed {}, using internal fee estimator",
                    provider_estimator_err
                );
                history_estimator_fut.map_err(move |history_estimator_err| {
                    MmError::new(Web3RpcError::Internal(format!(
                        "All gas api requests failed, provider estimator error: {}, history estimator error: {}",
                        provider_estimator_err, history_estimator_err
                    )))
                })
            })
            .await
    }

    async fn get_swap_pay_for_gas_option(&self, swap_fee_policy: SwapTxFeePolicy) -> Web3RpcResult<PayForGasOption> {
        let coin = self.clone();
        match swap_fee_policy {
            SwapTxFeePolicy::Internal => {
                let gas_price = coin.get_gas_price().await?;
                Ok(PayForGasOption::Legacy(LegacyGasPrice { gas_price }))
            },
            SwapTxFeePolicy::Low | SwapTxFeePolicy::Medium | SwapTxFeePolicy::High => {
                let fee_per_gas = coin.get_eip1559_gas_fee(false).await?;
                let pay_result = match swap_fee_policy {
                    SwapTxFeePolicy::Low => PayForGasOption::Eip1559(Eip1559FeePerGas {
                        max_fee_per_gas: fee_per_gas.low.max_fee_per_gas,
                        max_priority_fee_per_gas: fee_per_gas.low.max_priority_fee_per_gas,
                    }),
                    SwapTxFeePolicy::Medium => PayForGasOption::Eip1559(Eip1559FeePerGas {
                        max_fee_per_gas: fee_per_gas.medium.max_fee_per_gas,
                        max_priority_fee_per_gas: fee_per_gas.medium.max_priority_fee_per_gas,
                    }),
                    _ => PayForGasOption::Eip1559(Eip1559FeePerGas {
                        max_fee_per_gas: fee_per_gas.high.max_fee_per_gas,
                        max_priority_fee_per_gas: fee_per_gas.high.max_priority_fee_per_gas,
                    }),
                };
                Ok(pay_result)
            },
            SwapTxFeePolicy::Unsupported => Err(MmError::new(Web3RpcError::Internal("swap fee policy not set".into()))),
        }
    }

    /// Checks every second till at least one ETH node recognizes that nonce is increased.
    /// Parity has reliable "nextNonce" method that always returns correct nonce for address.
    /// But we can't expect that all nodes will always be Parity.
    /// Some of ETH forks use Geth only so they don't have Parity nodes at all.
    ///
    /// Please note that we just keep looping in case of a transport error hoping it will go away.
    ///
    /// # Warning
    ///
    /// The function is endless, we just keep looping in case of a transport error hoping it will go away.
    async fn wait_for_addr_nonce_increase(&self, addr: Address, prev_nonce: U256) {
        repeatable!(async {
            match self.clone().get_addr_nonce(addr).compat().await {
                Ok((new_nonce, _)) if new_nonce > prev_nonce => Ready(()),
                Ok((_nonce, _)) => Retry(()),
                Err(e) => {
                    error!("Error getting {} {} nonce: {}", self.ticker(), addr, e);
                    Retry(())
                },
            }
        })
        .until_ready()
        .repeat_every_secs(1.)
        .await
        .ok();
    }

    /// Returns `None` if the transaction hasn't appeared on the RPC nodes at the specified time.
    #[cfg(target_arch = "wasm32")]
    async fn wait_for_tx_appears_on_rpc(
        &self,
        tx_hash: H256,
        wait_rpc_timeout_ms: u64,
        check_every: f64,
    ) -> Web3RpcResult<Option<SignedEthTx>> {
        let wait_until = wait_until_ms(wait_rpc_timeout_ms);
        while now_ms() < wait_until {
            let maybe_tx = self.transaction(TransactionId::Hash(tx_hash)).await?;
            if let Some(tx) = maybe_tx {
                let signed_tx = signed_tx_from_web3_tx(tx).map_to_mm(Web3RpcError::InvalidResponse)?;
                return Ok(Some(signed_tx));
            }

            Timer::sleep(check_every).await;
        }

        let timeout_s = wait_rpc_timeout_ms / 1000;
        warn!(
            "Couldn't fetch the '{tx_hash:02x}' transaction hex as it hasn't appeared on the RPC node in {timeout_s}s"
        );
        Ok(None)
    }

    fn transaction_confirmed_at(&self, payment_hash: H256, wait_until: u64, check_every: f64) -> Web3RpcFut<U64> {
        let selfi = self.clone();
        let fut = async move {
            loop {
                if now_sec() > wait_until {
                    return MmError::err(Web3RpcError::Timeout(ERRL!(
                        "Waited too long until {} for payment tx: {:02x}, for coin:{}, to be confirmed!",
                        wait_until,
                        payment_hash,
                        selfi.ticker()
                    )));
                }

                let web3_receipt = match selfi.transaction_receipt(payment_hash).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!(
                            "Error {:?} getting the {} transaction {:?}, retrying in 15 seconds",
                            e,
                            selfi.ticker(),
                            payment_hash
                        );
                        Timer::sleep(check_every).await;
                        continue;
                    },
                };

                if let Some(receipt) = web3_receipt {
                    if receipt.status != Some(1.into()) {
                        return MmError::err(Web3RpcError::Internal(ERRL!(
                            "Tx receipt {:?} status of {} tx {:?} is failed",
                            receipt,
                            selfi.ticker(),
                            payment_hash
                        )));
                    }

                    if let Some(confirmed_at) = receipt.block_number {
                        break Ok(confirmed_at);
                    }
                }

                Timer::sleep(check_every).await;
            }
        };
        Box::new(fut.boxed().compat())
    }

    fn wait_for_block(&self, block_number: U64, wait_until: u64, check_every: f64) -> Web3RpcFut<()> {
        let selfi = self.clone();
        let fut = async move {
            loop {
                if now_sec() > wait_until {
                    return MmError::err(Web3RpcError::Timeout(ERRL!(
                        "Waited too long until {} for block number: {:02x} to appear on-chain, for coin:{}",
                        wait_until,
                        block_number,
                        selfi.ticker()
                    )));
                }

                match selfi.block_number().await {
                    Ok(current_block) => {
                        if current_block >= block_number {
                            break Ok(());
                        }
                    },
                    Err(e) => {
                        error!(
                            "Error {:?} getting the {} block number retrying in 15 seconds",
                            e,
                            selfi.ticker()
                        );
                    },
                };

                Timer::sleep(check_every).await;
            }
        };
        Box::new(fut.boxed().compat())
    }

    /// Requests the nonce from all available nodes and returns the highest nonce available with the list of nodes that returned the highest nonce.
    /// Transactions will be sent using the nodes that returned the highest nonce.
    pub fn get_addr_nonce(
        self,
        addr: Address,
    ) -> Box<dyn Future<Item = (U256, Vec<Web3Instance>), Error = String> + Send> {
        const TMP_SOCKET_DURATION: Duration = Duration::from_secs(300);

        let fut = async move {
            let mut errors: u32 = 0;
            let web3_instances = self.web3_instances.lock().await.to_vec();
            loop {
                let (futures, web3_instances): (Vec<_>, Vec<_>) = web3_instances
                    .iter()
                    .map(|instance| {
                        if let Web3Transport::Websocket(socket_transport) = instance.web3.transport() {
                            socket_transport.maybe_spawn_temporary_connection_loop(
                                self.clone(),
                                Instant::now() + TMP_SOCKET_DURATION,
                            );
                        };

                        if instance.is_parity {
                            let parity: ParityNonce<_> = instance.web3.api();
                            (Either::Left(parity.parity_next_nonce(addr)), instance.clone())
                        } else {
                            (
                                Either::Right(instance.web3.eth().transaction_count(addr, Some(BlockNumber::Pending))),
                                instance.clone(),
                            )
                        }
                    })
                    .unzip();

                let nonces: Vec<_> = join_all(futures)
                    .await
                    .into_iter()
                    .zip(web3_instances.into_iter())
                    .filter_map(|(nonce_res, instance)| match nonce_res {
                        Ok(n) => Some((n, instance)),
                        Err(e) => {
                            error!("Error getting nonce for addr {:?}: {}", addr, e);
                            None
                        },
                    })
                    .collect();
                if nonces.is_empty() {
                    // all requests errored
                    errors += 1;
                    if errors > 5 {
                        return ERR!("Couldn't get nonce after 5 errored attempts, aborting");
                    }
                } else {
                    let max = nonces
                        .iter()
                        .map(|(n, _)| *n)
                        .max()
                        .expect("nonces should not be empty!");
                    break Ok((
                        max,
                        nonces
                            .into_iter()
                            .filter_map(|(n, instance)| if n == max { Some(instance) } else { None })
                            .collect(),
                    ));
                }
                Timer::sleep(1.).await
            }
        };
        Box::new(Box::pin(fut).compat())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EthTxFeeDetails {
    pub coin: String,
    pub gas: u64,
    /// Gas price in ETH per gas unit
    /// if 'max_fee_per_gas' and 'max_priority_fee_per_gas' are used we set 'gas_price' as 'max_fee_per_gas' for compatibility with GUI
    pub gas_price: BigDecimal,
    /// Max fee per gas in ETH per gas unit
    pub max_fee_per_gas: Option<BigDecimal>,
    /// Max priority fee per gas in ETH per gas unit
    pub max_priority_fee_per_gas: Option<BigDecimal>,
    pub total_fee: BigDecimal,
}

impl EthTxFeeDetails {
    pub(crate) fn new(gas: U256, pay_for_gas_option: PayForGasOption, coin: &str) -> NumConversResult<EthTxFeeDetails> {
        let total_fee = calc_total_fee(gas, &pay_for_gas_option)?;
        // Fees are always paid in ETH, can use 18 decimals by default
        let total_fee = u256_to_big_decimal(total_fee, ETH_DECIMALS)?;
        let (gas_price, max_fee_per_gas, max_priority_fee_per_gas) = match pay_for_gas_option {
            PayForGasOption::Legacy(LegacyGasPrice { gas_price }) => (gas_price, None, None),
            // Using max_fee_per_gas as estimated gas_price value for compatibility in caller not expecting eip1559 fee per gas values.
            // Normally the caller should pay attention to presence of max_fee_per_gas and max_priority_fee_per_gas in the result:
            PayForGasOption::Eip1559(Eip1559FeePerGas {
                max_fee_per_gas,
                max_priority_fee_per_gas,
            }) => (max_fee_per_gas, Some(max_fee_per_gas), Some(max_priority_fee_per_gas)),
        };
        let gas_price = u256_to_big_decimal(gas_price, ETH_DECIMALS)?;
        let (max_fee_per_gas, max_priority_fee_per_gas) = match (max_fee_per_gas, max_priority_fee_per_gas) {
            (Some(max_fee_per_gas), Some(max_priority_fee_per_gas)) => (
                Some(u256_to_big_decimal(max_fee_per_gas, ETH_DECIMALS)?),
                Some(u256_to_big_decimal(max_priority_fee_per_gas, ETH_DECIMALS)?),
            ),
            (_, _) => (None, None),
        };
        let gas_u64 = u64::try_from(gas).map_to_mm(|e| NumConversError::new(e.to_string()))?;

        Ok(EthTxFeeDetails {
            coin: coin.to_owned(),
            gas: gas_u64,
            gas_price,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            total_fee,
        })
    }
}

#[async_trait]
impl MmCoin for EthCoin {
    fn is_asset_chain(&self) -> bool { false }

    fn spawner(&self) -> WeakSpawner { self.abortable_system.weak_spawner() }

    fn get_raw_transaction(&self, req: RawTransactionRequest) -> RawTransactionFut {
        Box::new(get_raw_transaction_impl(self.clone(), req).boxed().compat())
    }

    fn get_tx_hex_by_hash(&self, tx_hash: Vec<u8>) -> RawTransactionFut {
        if tx_hash.len() != H256::len_bytes() {
            let error = format!(
                "TX hash should have exactly {} bytes, got {}",
                H256::len_bytes(),
                tx_hash.len(),
            );
            return Box::new(futures01::future::err(MmError::new(
                RawTransactionError::InvalidHashError(error),
            )));
        }

        let tx_hash = H256::from_slice(tx_hash.as_slice());
        Box::new(get_tx_hex_by_hash_impl(self.clone(), tx_hash).boxed().compat())
    }

    fn withdraw(&self, req: WithdrawRequest) -> WithdrawFut {
        Box::new(Box::pin(withdraw_impl(self.clone(), req)).compat())
    }

    fn decimals(&self) -> u8 { self.decimals }

    fn convert_to_address(&self, from: &str, to_address_format: Json) -> Result<String, String> {
        let to_address_format: EthAddressFormat =
            json::from_value(to_address_format).map_err(|e| ERRL!("Error on parse ETH address format {:?}", e))?;
        match to_address_format {
            EthAddressFormat::SingleCase => ERR!("conversion is available only to mixed-case"),
            EthAddressFormat::MixedCase => {
                let _addr = try_s!(addr_from_str(from));
                Ok(checksum_address(from))
            },
        }
    }

    fn validate_address(&self, address: &str) -> ValidateAddressResult {
        let result = self.address_from_str(address);
        ValidateAddressResult {
            is_valid: result.is_ok(),
            reason: result.err(),
        }
    }

    fn process_history_loop(&self, ctx: MmArc) -> Box<dyn Future<Item = (), Error = ()> + Send> {
        cfg_wasm32! {
            ctx.log.log(
                "🤔",
                &[&"tx_history", &self.ticker],
                &ERRL!("Transaction history is not supported for ETH/ERC20 coins"),
            );
            Box::new(futures01::future::ok(()))
        }
        cfg_native! {
            let coin = self.clone();
            let fut = async move {
                match coin.coin_type {
                    EthCoinType::Eth => coin.process_eth_history(&ctx).await,
                    EthCoinType::Erc20 { ref token_addr, .. } => coin.process_erc20_history(*token_addr, &ctx).await,
                    EthCoinType::Nft {..} => return Err(())
                }
                Ok(())
            };
            Box::new(fut.boxed().compat())
        }
    }

    fn history_sync_status(&self) -> HistorySyncState { self.history_sync_state.lock().unwrap().clone() }

    fn get_trade_fee(&self) -> Box<dyn Future<Item = TradeFee, Error = String> + Send> {
        let coin = self.clone();
        Box::new(
            async move {
                let pay_for_gas_option = coin
                    .get_swap_pay_for_gas_option(coin.get_swap_transaction_fee_policy())
                    .await
                    .map_err(|e| e.to_string())?;

                let fee = calc_total_fee(U256::from(coin.gas_limit.eth_max_trade_gas), &pay_for_gas_option)
                    .map_err(|e| e.to_string())?;
                let fee_coin = match &coin.coin_type {
                    EthCoinType::Eth => &coin.ticker,
                    EthCoinType::Erc20 { platform, .. } => platform,
                    EthCoinType::Nft { .. } => return ERR!("Nft Protocol is not supported yet!"),
                };
                Ok(TradeFee {
                    coin: fee_coin.into(),
                    amount: try_s!(u256_to_big_decimal(fee, ETH_DECIMALS)).into(),
                    paid_from_trading_vol: false,
                })
            }
            .boxed()
            .compat(),
        )
    }

    async fn get_sender_trade_fee(
        &self,
        value: TradePreimageValue,
        stage: FeeApproxStage,
        include_refund_fee: bool,
    ) -> TradePreimageResult<TradeFee> {
        let pay_for_gas_option = self
            .get_swap_pay_for_gas_option(self.get_swap_transaction_fee_policy())
            .await?;
        let pay_for_gas_option = increase_gas_price_by_stage(pay_for_gas_option, &stage);
        let gas_limit = match self.coin_type {
            EthCoinType::Eth => {
                // this gas_limit includes gas for `ethPayment` and optionally `senderRefund` contract calls
                if include_refund_fee {
                    U256::from(self.gas_limit.eth_payment) + U256::from(self.gas_limit.eth_sender_refund)
                } else {
                    U256::from(self.gas_limit.eth_payment)
                }
            },
            EthCoinType::Erc20 { token_addr, .. } => {
                let mut gas = U256::from(self.gas_limit.erc20_payment);
                let value = match value {
                    TradePreimageValue::Exact(value) | TradePreimageValue::UpperBound(value) => {
                        wei_from_big_decimal(&value, self.decimals)?
                    },
                };
                let allowed = self.allowance(self.swap_contract_address).compat().await?;
                if allowed < value {
                    // estimate gas for the `approve` contract call

                    // Pass a dummy spender. Let's use `my_address`.
                    let spender = self.derivation_method.single_addr_or_err().await?;
                    let approve_function = ERC20_CONTRACT.function("approve")?;
                    let approve_data = approve_function.encode_input(&[Token::Address(spender), Token::Uint(value)])?;
                    let approve_gas_limit = self
                        .estimate_gas_for_contract_call(token_addr, Bytes::from(approve_data))
                        .await?;

                    // this gas_limit includes gas for `approve`, `erc20Payment` contract calls
                    gas += approve_gas_limit;
                }
                // add 'senderRefund' gas if requested
                if include_refund_fee {
                    gas += U256::from(self.gas_limit.erc20_sender_refund);
                }
                gas
            },
            EthCoinType::Nft { .. } => return MmError::err(TradePreimageError::NftProtocolNotSupported),
        };

        let total_fee = calc_total_fee(gas_limit, &pay_for_gas_option)?;
        let amount = u256_to_big_decimal(total_fee, ETH_DECIMALS)?;
        let fee_coin = match &self.coin_type {
            EthCoinType::Eth => &self.ticker,
            EthCoinType::Erc20 { platform, .. } => platform,
            EthCoinType::Nft { .. } => return MmError::err(TradePreimageError::NftProtocolNotSupported),
        };
        Ok(TradeFee {
            coin: fee_coin.into(),
            amount: amount.into(),
            paid_from_trading_vol: false,
        })
    }

    fn get_receiver_trade_fee(&self, stage: FeeApproxStage) -> TradePreimageFut<TradeFee> {
        let coin = self.clone();
        let fut = async move {
            let pay_for_gas_option = coin
                .get_swap_pay_for_gas_option(coin.get_swap_transaction_fee_policy())
                .await?;
            let pay_for_gas_option = increase_gas_price_by_stage(pay_for_gas_option, &stage);
            let (fee_coin, total_fee) = match &coin.coin_type {
                EthCoinType::Eth => (
                    &coin.ticker,
                    calc_total_fee(U256::from(coin.gas_limit.eth_receiver_spend), &pay_for_gas_option)?,
                ),
                EthCoinType::Erc20 { platform, .. } => (
                    platform,
                    calc_total_fee(U256::from(coin.gas_limit.erc20_receiver_spend), &pay_for_gas_option)?,
                ),
                EthCoinType::Nft { .. } => return MmError::err(TradePreimageError::NftProtocolNotSupported),
            };
            let amount = u256_to_big_decimal(total_fee, ETH_DECIMALS)?;
            Ok(TradeFee {
                coin: fee_coin.into(),
                amount: amount.into(),
                paid_from_trading_vol: false,
            })
        };
        Box::new(fut.boxed().compat())
    }

    async fn get_fee_to_send_taker_fee(
        &self,
        dex_fee_amount: DexFee,
        stage: FeeApproxStage,
    ) -> TradePreimageResult<TradeFee> {
        let dex_fee_amount = wei_from_big_decimal(&dex_fee_amount.fee_amount().into(), self.decimals)?;
        // pass the dummy params
        let to_addr = addr_from_raw_pubkey(&DEX_FEE_ADDR_RAW_PUBKEY)
            .expect("addr_from_raw_pubkey should never fail with DEX_FEE_ADDR_RAW_PUBKEY");
        let my_address = self.derivation_method.single_addr_or_err().await?;
        let (eth_value, data, call_addr, fee_coin) = match &self.coin_type {
            EthCoinType::Eth => (dex_fee_amount, Vec::new(), &to_addr, &self.ticker),
            EthCoinType::Erc20 { platform, token_addr } => {
                let function = ERC20_CONTRACT.function("transfer")?;
                let data = function.encode_input(&[Token::Address(to_addr), Token::Uint(dex_fee_amount)])?;
                (0.into(), data, token_addr, platform)
            },
            EthCoinType::Nft { .. } => return MmError::err(TradePreimageError::NftProtocolNotSupported),
        };
        let fee_policy_for_estimate = get_swap_fee_policy_for_estimate(self.get_swap_transaction_fee_policy());
        let pay_for_gas_option = self.get_swap_pay_for_gas_option(fee_policy_for_estimate).await?;
        let pay_for_gas_option = increase_gas_price_by_stage(pay_for_gas_option, &stage);
        let estimate_gas_req = CallRequest {
            value: Some(eth_value),
            data: Some(data.clone().into()),
            from: Some(my_address),
            to: Some(*call_addr),
            gas: None,
            ..CallRequest::default()
        };
        // gas price must be supplied because some smart contracts base their
        // logic on gas price, e.g. TUSD: https://github.com/KomodoPlatform/atomicDEX-API/issues/643
        let estimate_gas_req = call_request_with_pay_for_gas_option(estimate_gas_req, pay_for_gas_option.clone());
        // Please note if the wallet's balance is insufficient to withdraw, then `estimate_gas` may fail with the `Exception` error.
        // Ideally we should determine the case when we have the insufficient balance and return `TradePreimageError::NotSufficientBalance` error.
        let gas_limit = self.estimate_gas_wrapper(estimate_gas_req).compat().await?;
        let total_fee = calc_total_fee(gas_limit, &pay_for_gas_option)?;
        let amount = u256_to_big_decimal(total_fee, ETH_DECIMALS)?;
        Ok(TradeFee {
            coin: fee_coin.into(),
            amount: amount.into(),
            paid_from_trading_vol: false,
        })
    }

    fn required_confirmations(&self) -> u64 { self.required_confirmations.load(AtomicOrdering::Relaxed) }

    fn requires_notarization(&self) -> bool { false }

    fn set_required_confirmations(&self, confirmations: u64) {
        self.required_confirmations
            .store(confirmations, AtomicOrdering::Relaxed);
    }

    fn set_requires_notarization(&self, _requires_nota: bool) {
        warn!("set_requires_notarization doesn't take any effect on ETH/ERC20 coins");
    }

    fn swap_contract_address(&self) -> Option<BytesJson> {
        Some(BytesJson::from(self.swap_contract_address.0.as_ref()))
    }

    fn fallback_swap_contract(&self) -> Option<BytesJson> {
        self.fallback_swap_contract.map(|a| BytesJson::from(a.0.as_ref()))
    }

    fn mature_confirmations(&self) -> Option<u32> { None }

    fn coin_protocol_info(&self, _amount_to_receive: Option<MmNumber>) -> Vec<u8> { Vec::new() }

    fn is_coin_protocol_supported(
        &self,
        _info: &Option<Vec<u8>>,
        _amount_to_send: Option<MmNumber>,
        _locktime: u64,
        _is_maker: bool,
    ) -> bool {
        true
    }

    fn on_disabled(&self) -> Result<(), AbortedError> { AbortableSystem::abort_all(&self.abortable_system) }

    fn on_token_deactivated(&self, ticker: &str) {
        if let Ok(tokens) = self.erc20_tokens_infos.lock().as_deref_mut() {
            tokens.remove(ticker);
        };
    }
}

pub trait TryToAddress {
    fn try_to_address(&self) -> Result<Address, String>;
}

impl TryToAddress for BytesJson {
    fn try_to_address(&self) -> Result<Address, String> { self.0.try_to_address() }
}

impl TryToAddress for [u8] {
    fn try_to_address(&self) -> Result<Address, String> { (&self).try_to_address() }
}

impl<'a> TryToAddress for &'a [u8] {
    fn try_to_address(&self) -> Result<Address, String> {
        if self.len() != Address::len_bytes() {
            return ERR!(
                "Cannot construct an Ethereum address from {} bytes slice",
                Address::len_bytes()
            );
        }

        Ok(Address::from_slice(self))
    }
}

impl<T: TryToAddress> TryToAddress for Option<T> {
    fn try_to_address(&self) -> Result<Address, String> {
        match self {
            Some(ref inner) => inner.try_to_address(),
            None => ERR!("Cannot convert None to address"),
        }
    }
}

fn validate_fee_impl(coin: EthCoin, validate_fee_args: EthValidateFeeArgs<'_>) -> ValidatePaymentFut<()> {
    let fee_tx_hash = validate_fee_args.fee_tx_hash.to_owned();
    let sender_addr = try_f!(
        addr_from_raw_pubkey(validate_fee_args.expected_sender).map_to_mm(ValidatePaymentError::InvalidParameter)
    );
    let fee_addr = try_f!(addr_from_raw_pubkey(coin.dex_pubkey()).map_to_mm(ValidatePaymentError::InvalidParameter));
    let amount = validate_fee_args.amount.clone();
    let min_block_number = validate_fee_args.min_block_number;

    let fut = async move {
        let expected_value = wei_from_big_decimal(&amount, coin.decimals)?;
        let tx_from_rpc = coin.transaction(TransactionId::Hash(fee_tx_hash)).await?;

        let tx_from_rpc = tx_from_rpc.as_ref().ok_or_else(|| {
            ValidatePaymentError::TxDoesNotExist(format!("Didn't find provided tx {:?} on ETH node", fee_tx_hash))
        })?;

        if tx_from_rpc.from != Some(sender_addr) {
            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                "{}: Fee tx {:?} was sent from wrong address, expected {:?}",
                INVALID_SENDER_ERR_LOG, tx_from_rpc, sender_addr
            )));
        }

        if let Some(block_number) = tx_from_rpc.block_number {
            if block_number <= min_block_number.into() {
                return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                    "{}: Fee tx {:?} confirmed before min_block {}",
                    EARLY_CONFIRMATION_ERR_LOG, tx_from_rpc, min_block_number
                )));
            }
        }
        match &coin.coin_type {
            EthCoinType::Eth => {
                if tx_from_rpc.to != Some(fee_addr) {
                    return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                        "{}: Fee tx {:?} was sent to wrong address, expected {:?}",
                        INVALID_RECEIVER_ERR_LOG, tx_from_rpc, fee_addr
                    )));
                }

                if tx_from_rpc.value < expected_value {
                    return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                        "Fee tx {:?} value is less than expected {:?}",
                        tx_from_rpc, expected_value
                    )));
                }
            },
            EthCoinType::Erc20 {
                platform: _,
                token_addr,
            } => {
                if tx_from_rpc.to != Some(*token_addr) {
                    return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                        "{}: ERC20 Fee tx {:?} called wrong smart contract, expected {:?}",
                        INVALID_CONTRACT_ADDRESS_ERR_LOG, tx_from_rpc, token_addr
                    )));
                }

                let function = ERC20_CONTRACT
                    .function("transfer")
                    .map_to_mm(|e| ValidatePaymentError::InternalError(e.to_string()))?;
                let decoded_input = decode_contract_call(function, &tx_from_rpc.input.0)
                    .map_to_mm(|e| ValidatePaymentError::TxDeserializationError(e.to_string()))?;
                let address_input = get_function_input_data(&decoded_input, function, 0)
                    .map_to_mm(ValidatePaymentError::TxDeserializationError)?;

                if address_input != Token::Address(fee_addr) {
                    return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                        "{}: ERC20 Fee tx was sent to wrong address {:?}, expected {:?}",
                        INVALID_RECEIVER_ERR_LOG, address_input, fee_addr
                    )));
                }

                let value_input = get_function_input_data(&decoded_input, function, 1)
                    .map_to_mm(ValidatePaymentError::TxDeserializationError)?;

                match value_input {
                    Token::Uint(value) => {
                        if value < expected_value {
                            return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                                "ERC20 Fee tx value {} is less than expected {}",
                                value, expected_value
                            )));
                        }
                    },
                    _ => {
                        return MmError::err(ValidatePaymentError::WrongPaymentTx(format!(
                            "Should have got uint token but got {:?}",
                            value_input
                        )))
                    },
                }
            },
            EthCoinType::Nft { .. } => {
                return MmError::err(ValidatePaymentError::ProtocolNotSupported(
                    "Nft protocol is not supported".to_string(),
                ))
            },
        }

        Ok(())
    };
    Box::new(fut.boxed().compat())
}

fn get_function_input_data(decoded: &[Token], func: &Function, index: usize) -> Result<Token, String> {
    decoded.get(index).cloned().ok_or(format!(
        "Missing input in function {}: No input found at index {}",
        func.name.clone(),
        index
    ))
}

fn get_function_name(name: &str, watcher_reward: bool) -> String {
    if watcher_reward {
        format!("{}{}", name, "Reward")
    } else {
        name.to_owned()
    }
}

pub fn addr_from_raw_pubkey(pubkey: &[u8]) -> Result<Address, String> {
    let pubkey = try_s!(PublicKey::from_slice(pubkey).map_err(|e| ERRL!("{:?}", e)));
    let eth_public = Public::from_slice(&pubkey.serialize_uncompressed()[1..65]);
    Ok(public_to_address(&eth_public))
}

pub fn addr_from_pubkey_str(pubkey: &str) -> Result<String, String> {
    let pubkey_bytes = try_s!(hex::decode(pubkey));
    let addr = try_s!(addr_from_raw_pubkey(&pubkey_bytes));
    Ok(format!("{:#02x}", addr))
}

fn display_u256_with_decimal_point(number: U256, decimals: u8) -> String {
    let mut string = number.to_string();
    let decimals = decimals as usize;
    if string.len() <= decimals {
        string.insert_str(0, &"0".repeat(decimals - string.len() + 1));
    }

    string.insert(string.len() - decimals, '.');
    string.trim_end_matches('0').into()
}

/// Converts 'number' to value with decimal point and shifts it left by 'decimals' places
pub fn u256_to_big_decimal(number: U256, decimals: u8) -> NumConversResult<BigDecimal> {
    let string = display_u256_with_decimal_point(number, decimals);
    Ok(string.parse::<BigDecimal>()?)
}

/// Shifts 'number' with decimal point right by 'decimals' places and converts it to U256 value
pub fn wei_from_big_decimal(amount: &BigDecimal, decimals: u8) -> NumConversResult<U256> {
    let mut amount = amount.to_string();
    let dot = amount.find(|c| c == '.');
    let decimals = decimals as usize;
    if let Some(index) = dot {
        let mut fractional = amount.split_off(index);
        // remove the dot from fractional part
        fractional.remove(0);
        if fractional.len() < decimals {
            fractional.insert_str(fractional.len(), &"0".repeat(decimals - fractional.len()));
        }
        fractional.truncate(decimals);
        amount.push_str(&fractional);
    } else {
        amount.insert_str(amount.len(), &"0".repeat(decimals));
    }
    U256::from_dec_str(&amount).map_to_mm(|e| NumConversError::new(format!("{:?}", e)))
}

#[inline(always)]
pub fn wei_from_gwei_decimal(bigdec: &BigDecimal) -> NumConversResult<U256> {
    wei_from_big_decimal(bigdec, ETH_GWEI_DECIMALS)
}

#[inline(always)]
pub fn wei_to_gwei_decimal(wei: U256) -> NumConversResult<BigDecimal> { u256_to_big_decimal(wei, ETH_GWEI_DECIMALS) }

#[inline(always)]
pub fn wei_to_eth_decimal(wei: U256) -> NumConversResult<BigDecimal> { u256_to_big_decimal(wei, ETH_DECIMALS) } // TODO: use wei_to_eth_decimal instead of u256_to_big_decimal(gas_cost_wei, ETH_DECIMALS)

impl Transaction for SignedEthTx {
    fn tx_hex(&self) -> Vec<u8> { rlp::encode(self).to_vec() }

    fn tx_hash_as_bytes(&self) -> BytesJson { self.tx_hash().as_bytes().into() }
}

fn signed_tx_from_web3_tx(transaction: Web3Transaction) -> Result<SignedEthTx, String> {
    // Local function to map the access list
    fn map_access_list(web3_access_list: &Option<Vec<web3::types::AccessListItem>>) -> ethcore_transaction::AccessList {
        match web3_access_list {
            Some(list) => ethcore_transaction::AccessList(
                list.iter()
                    .map(|item| ethcore_transaction::AccessListItem {
                        address: item.address,
                        storage_keys: item.storage_keys.clone(),
                    })
                    .collect(),
            ),
            None => ethcore_transaction::AccessList(vec![]),
        }
    }

    // Define transaction types
    let type_0: ethereum_types::U64 = 0.into();
    let type_1: ethereum_types::U64 = 1.into();
    let type_2: ethereum_types::U64 = 2.into();

    // Determine the transaction type
    let tx_type = match transaction.transaction_type {
        None => TxType::Legacy,
        Some(t) if t == type_0 => TxType::Legacy,
        Some(t) if t == type_1 => TxType::Type1,
        Some(t) if t == type_2 => TxType::Type2,
        _ => return Err(ERRL!("'Transaction::transaction_type' unsupported")),
    };

    // Determine the action based on the presence of 'to' field
    let action = match transaction.to {
        Some(addr) => Action::Call(addr),
        None => Action::Create,
    };

    // Initialize the transaction builder
    let tx_builder = UnSignedEthTxBuilder::new(
        tx_type.clone(),
        transaction.nonce,
        transaction.gas,
        action,
        transaction.value,
        transaction.input.0,
    );

    // Modify the builder based on the transaction type
    let tx_builder = match tx_type {
        TxType::Legacy => {
            let gas_price = transaction
                .gas_price
                .ok_or_else(|| ERRL!("'Transaction::gas_price' is not set"))?;
            tx_builder.with_gas_price(gas_price)
        },
        TxType::Type1 => {
            let gas_price = transaction
                .gas_price
                .ok_or_else(|| ERRL!("'Transaction::gas_price' is not set"))?;
            let chain_id = transaction
                .chain_id
                .ok_or_else(|| ERRL!("'Transaction::chain_id' is not set"))?
                .to_string()
                .parse()
                .map_err(|e: std::num::ParseIntError| e.to_string())?;
            tx_builder
                .with_gas_price(gas_price)
                .with_chain_id(chain_id)
                .with_access_list(map_access_list(&transaction.access_list))
        },
        TxType::Type2 => {
            let max_fee_per_gas = transaction
                .max_fee_per_gas
                .ok_or_else(|| ERRL!("'Transaction::max_fee_per_gas' is not set"))?;
            let max_priority_fee_per_gas = transaction
                .max_priority_fee_per_gas
                .ok_or_else(|| ERRL!("'Transaction::max_priority_fee_per_gas' is not set"))?;
            let chain_id = transaction
                .chain_id
                .ok_or_else(|| ERRL!("'Transaction::chain_id' is not set"))?
                .to_string()
                .parse()
                .map_err(|e: std::num::ParseIntError| e.to_string())?;
            tx_builder
                .with_priority_fee_per_gas(max_fee_per_gas, max_priority_fee_per_gas)
                .with_chain_id(chain_id)
                .with_access_list(map_access_list(&transaction.access_list))
        },
        TxType::Invalid => return Err(ERRL!("Internal error: 'tx_type' invalid")),
    };

    // Build the unsigned transaction
    let unsigned = tx_builder.build().map_err(|err| err.to_string())?;

    // Extract signature components
    let r = transaction.r.ok_or_else(|| ERRL!("'Transaction::r' is not set"))?;
    let s = transaction.s.ok_or_else(|| ERRL!("'Transaction::s' is not set"))?;
    let v = transaction
        .v
        .ok_or_else(|| ERRL!("'Transaction::v' is not set"))?
        .as_u64();

    // Create the signed transaction
    let unverified = match unsigned {
        TransactionWrapper::Legacy(unsigned) => UnverifiedTransactionWrapper::Legacy(
            UnverifiedLegacyTransaction::new_with_network_v(unsigned, r, s, v, transaction.hash)
                .map_err(|err| ERRL!("'Transaction::new' error {}", err.to_string()))?,
        ),
        TransactionWrapper::Eip2930(unsigned) => UnverifiedTransactionWrapper::Eip2930(
            UnverifiedEip2930Transaction::new(unsigned, r, s, v, transaction.hash)
                .map_err(|err| ERRL!("'Transaction::new' error {}", err.to_string()))?,
        ),
        TransactionWrapper::Eip1559(unsigned) => UnverifiedTransactionWrapper::Eip1559(
            UnverifiedEip1559Transaction::new(unsigned, r, s, v, transaction.hash)
                .map_err(|err| ERRL!("'Transaction::new' error {}", err.to_string()))?,
        ),
    };

    // Return the signed transaction
    Ok(try_s!(SignedEthTx::new(unverified)))
}

pub fn valid_addr_from_str(addr_str: &str) -> Result<Address, String> {
    let addr = try_s!(addr_from_str(addr_str));
    if !is_valid_checksum_addr(addr_str) {
        return ERR!("Invalid address checksum");
    }
    Ok(addr)
}

pub fn addr_from_str(addr_str: &str) -> Result<Address, String> {
    if !addr_str.starts_with("0x") {
        return ERR!("Address must be prefixed with 0x");
    };

    Ok(try_s!(Address::from_str(&addr_str[2..])))
}

/// This function fixes a bug appeared on `ethabi` update:
/// 1. `ethabi(6.1.0)::Function::decode_input` had
/// ```rust
/// decode(&self.input_param_types(), &data[4..])
/// ```
///
/// 2. `ethabi(17.2.0)::Function::decode_input` has
/// ```rust
/// decode(&self.input_param_types(), data)
/// ```
pub fn decode_contract_call(function: &Function, contract_call_bytes: &[u8]) -> Result<Vec<Token>, ethabi::Error> {
    if contract_call_bytes.len() < 4 {
        return Err(ethabi::Error::Other(
            "Contract call should contain at least 4 bytes known as a function signature".into(),
        ));
    }

    let actual_signature = &contract_call_bytes[..4];
    let expected_signature = &function.short_signature();
    if actual_signature != expected_signature {
        let error =
            format!("Unexpected contract call signature: expected {expected_signature:?}, found {actual_signature:?}");
        return Err(ethabi::Error::Other(error.into()));
    }

    function.decode_input(&contract_call_bytes[4..])
}

fn rpc_event_handlers_for_eth_transport(ctx: &MmArc, ticker: String) -> Vec<RpcTransportEventHandlerShared> {
    let metrics = ctx.metrics.weak();
    vec![CoinTransportMetrics::new(metrics, ticker, RpcClientType::Ethereum).into_shared()]
}

async fn get_max_eth_tx_type_conf(ctx: &MmArc, conf: &Json, coin_type: &EthCoinType) -> Result<Option<u64>, String> {
    fn check_max_eth_tx_type_conf(conf: &Json) -> Result<Option<u64>, String> {
        if !conf["max_eth_tx_type"].is_null() {
            let max_eth_tx_type = conf["max_eth_tx_type"]
                .as_u64()
                .ok_or_else(|| "max_eth_tx_type in coins is invalid".to_string())?;
            if max_eth_tx_type > ETH_MAX_TX_TYPE {
                return Err("max_eth_tx_type in coins is too big".to_string());
            }
            Ok(Some(max_eth_tx_type))
        } else {
            Ok(None)
        }
    }

    match &coin_type {
        EthCoinType::Eth => check_max_eth_tx_type_conf(conf),
        EthCoinType::Erc20 { platform, .. } | EthCoinType::Nft { platform } => {
            let coin_max_eth_tx_type = check_max_eth_tx_type_conf(conf)?;
            // Normally we suppose max_eth_tx_type is in platform coin but also try to get it from tokens for tests to work:
            if let Some(coin_max_eth_tx_type) = coin_max_eth_tx_type {
                Ok(Some(coin_max_eth_tx_type))
            } else {
                let platform_coin = lp_coinfind_or_err(ctx, platform).await;
                match platform_coin {
                    Ok(MmCoinEnum::EthCoin(eth_coin)) => Ok(eth_coin.max_eth_tx_type),
                    _ => Ok(None),
                }
            }
        },
    }
}

#[inline]
fn new_nonce_lock() -> HashMap<String, Arc<AsyncMutex<()>>> { HashMap::new() }

/// Activate eth coin or erc20 token from coin config and private key build policy
pub async fn eth_coin_from_conf_and_request(
    ctx: &MmArc,
    ticker: &str,
    conf: &Json,
    req: &Json,
    protocol: CoinProtocol,
    priv_key_policy: PrivKeyBuildPolicy,
) -> Result<EthCoin, String> {
    // Convert `PrivKeyBuildPolicy` to `EthPrivKeyBuildPolicy` if it's possible.
    let priv_key_policy = try_s!(EthPrivKeyBuildPolicy::try_from(priv_key_policy));

    let mut urls: Vec<String> = try_s!(json::from_value(req["urls"].clone()));
    if urls.is_empty() {
        return ERR!("Enable request for ETH coin must have at least 1 node URL");
    }
    let mut rng = small_rng();
    urls.as_mut_slice().shuffle(&mut rng);

    let swap_contract_address: Address = try_s!(json::from_value(req["swap_contract_address"].clone()));
    if swap_contract_address == Address::default() {
        return ERR!("swap_contract_address can't be zero address");
    }
    let fallback_swap_contract: Option<Address> = try_s!(json::from_value(req["fallback_swap_contract"].clone()));
    if let Some(fallback) = fallback_swap_contract {
        if fallback == Address::default() {
            return ERR!("fallback_swap_contract can't be zero address");
        }
    }
    let contract_supports_watchers = req["contract_supports_watchers"].as_bool().unwrap_or_default();

    let path_to_address = try_s!(json::from_value::<Option<HDPathAccountToAddressId>>(
        req["path_to_address"].clone()
    ))
    .unwrap_or_default();
    let (key_pair, derivation_method) =
        try_s!(build_address_and_priv_key_policy(ctx, ticker, conf, priv_key_policy, &path_to_address, None).await);

    let mut web3_instances = vec![];
    let event_handlers = rpc_event_handlers_for_eth_transport(ctx, ticker.to_string());
    for url in urls.iter() {
        let uri: Uri = try_s!(url.parse());

        let transport = match uri.scheme_str() {
            Some("ws") | Some("wss") => {
                const TMP_SOCKET_CONNECTION: Duration = Duration::from_secs(20);

                let node = WebsocketTransportNode { uri: uri.clone() };
                let websocket_transport = WebsocketTransport::with_event_handlers(node, event_handlers.clone());

                // Temporarily start the connection loop (we close the connection once we have the client version below).
                // Ideally, it would be much better to not do this workaround, which requires a lot of refactoring or
                // dropping websocket support on parity nodes.
                let fut = websocket_transport
                    .clone()
                    .start_connection_loop(Some(Instant::now() + TMP_SOCKET_CONNECTION));
                let settings = AbortSettings::info_on_abort(format!("connection loop stopped for {:?}", uri));
                ctx.spawner().spawn_with_settings(fut, settings);

                Web3Transport::Websocket(websocket_transport)
            },
            Some("http") | Some("https") => {
                let node = HttpTransportNode {
                    uri,
                    komodo_proxy: false,
                };

                Web3Transport::new_http_with_event_handlers(node, event_handlers.clone())
            },
            _ => {
                return ERR!(
                    "Invalid node address '{}'. Only http(s) and ws(s) nodes are supported",
                    uri
                );
            },
        };

        let web3 = Web3::new(transport);
        let version = match web3.web3().client_version().await {
            Ok(v) => v,
            Err(e) => {
                error!("Couldn't get client version for url {}: {}", url, e);

                continue;
            },
        };

        web3_instances.push(Web3Instance {
            web3,
            is_parity: version.contains("Parity") || version.contains("parity"),
        })
    }

    if web3_instances.is_empty() {
        return ERR!("Failed to get client version for all urls");
    }

    let (coin_type, decimals) = match protocol {
        CoinProtocol::ETH => (EthCoinType::Eth, ETH_DECIMALS),
        CoinProtocol::ERC20 {
            platform,
            contract_address,
        } => {
            let token_addr = try_s!(valid_addr_from_str(&contract_address));
            let decimals = match conf["decimals"].as_u64() {
                None | Some(0) => try_s!(
                    get_token_decimals(
                        &web3_instances
                            .first()
                            .expect("web3_instances can't be empty in ETH activation")
                            .web3,
                        token_addr
                    )
                    .await
                ),
                Some(d) => d as u8,
            };
            (EthCoinType::Erc20 { platform, token_addr }, decimals)
        },
        CoinProtocol::NFT { platform } => (EthCoinType::Nft { platform }, ETH_DECIMALS),
        _ => return ERR!("Expect ETH, ERC20 or NFT protocol"),
    };

    // param from request should override the config
    let required_confirmations = req["required_confirmations"]
        .as_u64()
        .unwrap_or_else(|| {
            conf["required_confirmations"]
                .as_u64()
                .unwrap_or(DEFAULT_REQUIRED_CONFIRMATIONS as u64)
        })
        .into();

    if req["requires_notarization"].as_bool().is_some() {
        warn!("requires_notarization doesn't take any effect on ETH/ERC20 coins");
    }

    let sign_message_prefix: Option<String> = json::from_value(conf["sign_message_prefix"].clone()).unwrap_or(None);

    let chain_id = try_s!(conf["chain_id"]
        .as_u64()
        .ok_or_else(|| format!("chain_id is not set for {}", ticker)));

    let trezor_coin: Option<String> = json::from_value(conf["trezor_coin"].clone()).unwrap_or(None);

    let initial_history_state = if req["tx_history"].as_bool().unwrap_or(false) {
        HistorySyncState::NotStarted
    } else {
        HistorySyncState::NotEnabled
    };

    let key_lock = match &coin_type {
        EthCoinType::Eth => String::from(ticker),
        EthCoinType::Erc20 { platform, .. } | EthCoinType::Nft { platform } => String::from(platform),
    };

    let address_nonce_locks = {
        let mut map = NONCE_LOCK.lock().unwrap();
        Arc::new(AsyncMutex::new(
            map.entry(key_lock).or_insert_with(new_nonce_lock).clone(),
        ))
    };

    // Create an abortable system linked to the `MmCtx` so if the context is stopped via `MmArc::stop`,
    // all spawned futures related to `ETH` coin will be aborted as well.
    let abortable_system = try_s!(ctx.abortable_system.create_subsystem());

    let max_eth_tx_type = get_max_eth_tx_type_conf(ctx, conf, &coin_type).await?;
    let gas_limit: EthGasLimit = extract_gas_limit_from_conf(conf)?;
    let gas_limit_v2: EthGasLimitV2 = extract_gas_limit_from_conf(conf)?;

    let coin = EthCoinImpl {
        priv_key_policy: key_pair,
        derivation_method: Arc::new(derivation_method),
        coin_type,
        sign_message_prefix,
        swap_contract_address,
        swap_v2_contracts: None,
        fallback_swap_contract,
        contract_supports_watchers,
        decimals,
        ticker: ticker.into(),
        web3_instances: AsyncMutex::new(web3_instances),
        history_sync_state: Mutex::new(initial_history_state),
        swap_txfee_policy: Mutex::new(SwapTxFeePolicy::Internal),
        max_eth_tx_type,
        ctx: ctx.weak(),
        required_confirmations,
        chain_id,
        trezor_coin,
        logs_block_range: conf["logs_block_range"].as_u64().unwrap_or(DEFAULT_LOGS_BLOCK_RANGE),
        address_nonce_locks,
        erc20_tokens_infos: Default::default(),
        nfts_infos: Default::default(),
        gas_limit,
        gas_limit_v2,
        abortable_system,
    };

    Ok(EthCoin(Arc::new(coin)))
}

/// Displays the address in mixed-case checksum form
/// https://github.com/ethereum/EIPs/blob/master/EIPS/eip-55.md
pub fn checksum_address(addr: &str) -> String {
    let mut addr = addr.to_lowercase();
    if addr.starts_with("0x") {
        addr.replace_range(..2, "");
    }

    let mut hasher = Keccak256::default();
    hasher.update(&addr);
    let hash = hasher.finalize();
    let mut result: String = "0x".into();
    for (i, c) in addr.chars().enumerate() {
        if c.is_ascii_digit() {
            result.push(c);
        } else {
            // https://github.com/ethereum/EIPs/blob/master/EIPS/eip-55.md#specification
            // Convert the address to hex, but if the ith digit is a letter (ie. it's one of abcdef)
            // print it in uppercase if the 4*ith bit of the hash of the lowercase hexadecimal
            // address is 1 otherwise print it in lowercase.
            if hash[i / 2] & (1 << (7 - 4 * (i % 2))) != 0 {
                result.push(c.to_ascii_uppercase());
            } else {
                result.push(c.to_ascii_lowercase());
            }
        }
    }

    result
}

/// `eth_addr_to_hex` converts Address to hex format.
/// Note: the result will be in lowercase.
fn eth_addr_to_hex(address: &Address) -> String { format!("{:#x}", address) }

/// Checks that input is valid mixed-case checksum form address
/// The input must be 0x prefixed hex string
fn is_valid_checksum_addr(addr: &str) -> bool { addr == checksum_address(addr) }

fn increase_by_percent_one_gwei(num: U256, percent: u64) -> U256 {
    let one_gwei = U256::from(10u64.pow(9));
    let percent = (num / U256::from(100)) * U256::from(percent);
    if percent < one_gwei {
        num + one_gwei
    } else {
        num + percent
    }
}

fn increase_gas_price_by_stage(pay_for_gas_option: PayForGasOption, level: &FeeApproxStage) -> PayForGasOption {
    if let PayForGasOption::Legacy(LegacyGasPrice { gas_price }) = pay_for_gas_option {
        let new_gas_price = match level {
            FeeApproxStage::WithoutApprox => gas_price,
            FeeApproxStage::StartSwap => {
                increase_by_percent_one_gwei(gas_price, GAS_PRICE_APPROXIMATION_PERCENT_ON_START_SWAP)
            },
            FeeApproxStage::OrderIssue => {
                increase_by_percent_one_gwei(gas_price, GAS_PRICE_APPROXIMATION_PERCENT_ON_ORDER_ISSUE)
            },
            FeeApproxStage::TradePreimage => {
                increase_by_percent_one_gwei(gas_price, GAS_PRICE_APPROXIMATION_PERCENT_ON_TRADE_PREIMAGE)
            },
            FeeApproxStage::WatcherPreimage => {
                increase_by_percent_one_gwei(gas_price, GAS_PRICE_APPROXIMATION_PERCENT_ON_WATCHER_PREIMAGE)
            },
        };
        PayForGasOption::Legacy(LegacyGasPrice {
            gas_price: new_gas_price,
        })
    } else {
        pay_for_gas_option
    }
}

/// Represents errors that can occur while retrieving an Ethereum address.
#[derive(Clone, Debug, Deserialize, Display, PartialEq, Serialize)]
pub enum GetEthAddressError {
    UnexpectedDerivationMethod(UnexpectedDerivationMethod),
    EthActivationV2Error(EthActivationV2Error),
    Internal(String),
}

impl From<UnexpectedDerivationMethod> for GetEthAddressError {
    fn from(e: UnexpectedDerivationMethod) -> Self { GetEthAddressError::UnexpectedDerivationMethod(e) }
}

impl From<EthActivationV2Error> for GetEthAddressError {
    fn from(e: EthActivationV2Error) -> Self { GetEthAddressError::EthActivationV2Error(e) }
}

impl From<CryptoCtxError> for GetEthAddressError {
    fn from(e: CryptoCtxError) -> Self { GetEthAddressError::Internal(e.to_string()) }
}

// Todo: `get_eth_address` should be removed since NFT is now part of the coins ctx.
/// `get_eth_address` returns wallet address for coin with `ETH` protocol type.
/// Note: result address has mixed-case checksum form.
pub async fn get_eth_address(
    ctx: &MmArc,
    conf: &Json,
    ticker: &str,
    path_to_address: &HDPathAccountToAddressId,
) -> MmResult<MyWalletAddress, GetEthAddressError> {
    let crypto_ctx = CryptoCtx::from_ctx(ctx)?;
    let priv_key_policy = if crypto_ctx.hw_ctx().is_some() {
        PrivKeyBuildPolicy::Trezor
    } else {
        PrivKeyBuildPolicy::detect_priv_key_policy(ctx)?
    }
    .into();

    let (_, derivation_method) =
        build_address_and_priv_key_policy(ctx, ticker, conf, priv_key_policy, path_to_address, None).await?;
    let my_address = match derivation_method {
        EthDerivationMethod::SingleAddress(my_address) => my_address,
        EthDerivationMethod::HDWallet(_) => {
            return Err(MmError::new(GetEthAddressError::UnexpectedDerivationMethod(
                UnexpectedDerivationMethod::UnsupportedError("HDWallet is not supported for NFT yet!".to_owned()),
            )));
        },
    };

    Ok(MyWalletAddress {
        coin: ticker.to_owned(),
        wallet_address: my_address.display_address(),
    })
}

/// Errors encountered while validating Ethereum addresses for NFT withdrawal.
#[derive(Display)]
pub enum GetValidEthWithdrawAddError {
    /// The specified coin does not support NFT withdrawal.
    #[display(fmt = "{} coin doesn't support NFT withdrawing", coin)]
    CoinDoesntSupportNftWithdraw { coin: String },
    /// The provided address is invalid.
    InvalidAddress(String),
}

/// Validates Ethereum addresses for NFT withdrawal.
/// Returns a tuple of valid `to` address, `token` address, and `EthCoin` instance on success.
/// Errors if the coin doesn't support NFT withdrawal or if the addresses are invalid.
fn get_valid_nft_addr_to_withdraw(
    coin_enum: MmCoinEnum,
    to: &str,
    token_add: &str,
) -> MmResult<(Address, Address, EthCoin), GetValidEthWithdrawAddError> {
    let eth_coin = match coin_enum {
        MmCoinEnum::EthCoin(eth_coin) => eth_coin,
        _ => {
            return MmError::err(GetValidEthWithdrawAddError::CoinDoesntSupportNftWithdraw {
                coin: coin_enum.ticker().to_owned(),
            })
        },
    };
    let to_addr = valid_addr_from_str(to).map_err(GetValidEthWithdrawAddError::InvalidAddress)?;
    let token_addr = addr_from_str(token_add).map_err(GetValidEthWithdrawAddError::InvalidAddress)?;
    Ok((to_addr, token_addr, eth_coin))
}

#[derive(Clone, Debug, Deserialize, Display, EnumFromStringify, PartialEq, Serialize)]
pub enum EthGasDetailsErr {
    #[display(fmt = "Invalid fee policy: {}", _0)]
    InvalidFeePolicy(String),
    #[from_stringify("NumConversError")]
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
    #[display(fmt = "Transport: {}", _0)]
    Transport(String),
    #[display(fmt = "Nft Protocol is not supported yet!")]
    NftProtocolNotSupported,
}

impl From<web3::Error> for EthGasDetailsErr {
    fn from(e: web3::Error) -> Self { EthGasDetailsErr::from(Web3RpcError::from(e)) }
}

impl From<Web3RpcError> for EthGasDetailsErr {
    fn from(e: Web3RpcError) -> Self {
        match e {
            Web3RpcError::Transport(tr) | Web3RpcError::InvalidResponse(tr) => EthGasDetailsErr::Transport(tr),
            Web3RpcError::Internal(internal)
            | Web3RpcError::Timeout(internal)
            | Web3RpcError::NumConversError(internal)
            | Web3RpcError::InvalidGasApiConfig(internal) => EthGasDetailsErr::Internal(internal),
            Web3RpcError::NftProtocolNotSupported => EthGasDetailsErr::NftProtocolNotSupported,
        }
    }
}

async fn get_eth_gas_details_from_withdraw_fee(
    eth_coin: &EthCoin,
    fee: Option<WithdrawFee>,
    eth_value: U256,
    data: Bytes,
    sender_address: Address,
    call_addr: Address,
    fungible_max: bool,
) -> MmResult<GasDetails, EthGasDetailsErr> {
    let pay_for_gas_option = match fee {
        Some(WithdrawFee::EthGas { gas_price, gas }) => {
            let gas_price = wei_from_big_decimal(&gas_price, ETH_GWEI_DECIMALS)?;
            return Ok((gas.into(), PayForGasOption::Legacy(LegacyGasPrice { gas_price })));
        },
        Some(WithdrawFee::EthGasEip1559 {
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_option: gas_limit,
        }) => {
            let max_fee_per_gas = wei_from_big_decimal(&max_fee_per_gas, ETH_GWEI_DECIMALS)?;
            let max_priority_fee_per_gas = wei_from_big_decimal(&max_priority_fee_per_gas, ETH_GWEI_DECIMALS)?;
            match gas_limit {
                EthGasLimitOption::Set(gas) => {
                    return Ok((
                        gas.into(),
                        PayForGasOption::Eip1559(Eip1559FeePerGas {
                            max_fee_per_gas,
                            max_priority_fee_per_gas,
                        }),
                    ))
                },
                EthGasLimitOption::Calc =>
                // go to gas estimate code
                {
                    PayForGasOption::Eip1559(Eip1559FeePerGas {
                        max_fee_per_gas,
                        max_priority_fee_per_gas,
                    })
                },
            }
        },
        Some(fee_policy) => {
            let error = format!("Expected 'EthGas' fee type, found {:?}", fee_policy);
            return MmError::err(EthGasDetailsErr::InvalidFeePolicy(error));
        },
        None => {
            // If WithdrawFee not set use legacy gas price (?)
            let gas_price = eth_coin.get_gas_price().await?;
            // go to gas estimate code
            PayForGasOption::Legacy(LegacyGasPrice { gas_price })
        },
    };

    // covering edge case by deducting the standard transfer fee when we want to max withdraw ETH
    let eth_value_for_estimate = if fungible_max && eth_coin.coin_type == EthCoinType::Eth {
        eth_value - calc_total_fee(U256::from(eth_coin.gas_limit.eth_send_coins), &pay_for_gas_option)?
    } else {
        eth_value
    };

    let gas_price = pay_for_gas_option.get_gas_price();
    let (max_fee_per_gas, max_priority_fee_per_gas) = pay_for_gas_option.get_fee_per_gas();
    let estimate_gas_req = CallRequest {
        value: Some(eth_value_for_estimate),
        data: Some(data),
        from: Some(sender_address),
        to: Some(call_addr),
        gas: None,
        // gas price must be supplied because some smart contracts base their
        // logic on gas price, e.g. TUSD: https://github.com/KomodoPlatform/atomicDEX-API/issues/643
        gas_price,
        max_priority_fee_per_gas,
        max_fee_per_gas,
        ..CallRequest::default()
    };
    // TODO Note if the wallet's balance is insufficient to withdraw, then `estimate_gas` may fail with the `Exception` error.
    // TODO Ideally we should determine the case when we have the insufficient balance and return `WithdrawError::NotSufficientBalance`.
    let gas_limit = eth_coin.estimate_gas_wrapper(estimate_gas_req).compat().await?;
    Ok((gas_limit, pay_for_gas_option))
}

/// Calc estimated total gas fee or price
fn calc_total_fee(gas: U256, pay_for_gas_option: &PayForGasOption) -> NumConversResult<U256> {
    match *pay_for_gas_option {
        PayForGasOption::Legacy(LegacyGasPrice { gas_price }) => gas
            .checked_mul(gas_price)
            .or_mm_err(|| NumConversError("total fee overflow".into())),
        PayForGasOption::Eip1559(Eip1559FeePerGas { max_fee_per_gas, .. }) => gas
            .checked_mul(max_fee_per_gas)
            .or_mm_err(|| NumConversError("total fee overflow".into())),
    }
}

#[allow(clippy::result_large_err)]
fn tx_builder_with_pay_for_gas_option(
    eth_coin: &EthCoin,
    tx_builder: UnSignedEthTxBuilder,
    pay_for_gas_option: &PayForGasOption,
) -> MmResult<UnSignedEthTxBuilder, WithdrawError> {
    let tx_builder = match *pay_for_gas_option {
        PayForGasOption::Legacy(LegacyGasPrice { gas_price }) => tx_builder.with_gas_price(gas_price),
        PayForGasOption::Eip1559(Eip1559FeePerGas {
            max_priority_fee_per_gas,
            max_fee_per_gas,
        }) => tx_builder
            .with_priority_fee_per_gas(max_fee_per_gas, max_priority_fee_per_gas)
            .with_chain_id(eth_coin.chain_id),
    };
    Ok(tx_builder)
}

/// convert fee policy for gas estimate requests
fn get_swap_fee_policy_for_estimate(swap_fee_policy: SwapTxFeePolicy) -> SwapTxFeePolicy {
    match swap_fee_policy {
        SwapTxFeePolicy::Internal => SwapTxFeePolicy::Internal,
        // always use 'high' for estimate to avoid max_fee_per_gas less than base_fee errors:
        SwapTxFeePolicy::Low | SwapTxFeePolicy::Medium | SwapTxFeePolicy::High => SwapTxFeePolicy::High,
        SwapTxFeePolicy::Unsupported => SwapTxFeePolicy::Unsupported,
    }
}

fn call_request_with_pay_for_gas_option(call_request: CallRequest, pay_for_gas_option: PayForGasOption) -> CallRequest {
    match pay_for_gas_option {
        PayForGasOption::Legacy(LegacyGasPrice { gas_price }) => CallRequest {
            gas_price: Some(gas_price),
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            ..call_request
        },
        PayForGasOption::Eip1559(Eip1559FeePerGas {
            max_fee_per_gas,
            max_priority_fee_per_gas,
        }) => CallRequest {
            gas_price: None,
            max_fee_per_gas: Some(max_fee_per_gas),
            max_priority_fee_per_gas: Some(max_priority_fee_per_gas),
            ..call_request
        },
    }
}

impl ToBytes for Signature {
    fn to_bytes(&self) -> Vec<u8> { self.to_vec() }
}

impl ToBytes for SignedEthTx {
    fn to_bytes(&self) -> Vec<u8> {
        let mut stream = RlpStream::new();
        self.rlp_append(&mut stream);
        // Handle potential panicking.
        if stream.is_finished() {
            Vec::from(stream.out())
        } else {
            // TODO: Consider returning Result<Vec<u8>, Error> in future refactoring for better error handling.
            warn!("RlpStream was not finished; returning an empty Vec as a fail-safe.");
            vec![]
        }
    }
}

#[derive(Debug, Display, EnumFromStringify)]
pub enum EthAssocTypesError {
    InvalidHexString(String),
    #[from_stringify("DecoderError")]
    TxParseError(String),
    ParseSignatureError(String),
}

#[derive(Debug, Display)]
pub enum EthNftAssocTypesError {
    Utf8Error(String),
    ParseContractTypeError(ParseContractTypeError),
    ParseTokenContractError(String),
}

impl From<ParseContractTypeError> for EthNftAssocTypesError {
    fn from(e: ParseContractTypeError) -> Self { EthNftAssocTypesError::ParseContractTypeError(e) }
}

#[async_trait]
impl ParseCoinAssocTypes for EthCoin {
    type Address = Address;
    type AddressParseError = MmError<EthAssocTypesError>;
    type Pubkey = Public;
    type PubkeyParseError = MmError<EthAssocTypesError>;
    type Tx = SignedEthTx;
    type TxParseError = MmError<EthAssocTypesError>;
    type Preimage = SignedEthTx;
    type PreimageParseError = MmError<EthAssocTypesError>;
    type Sig = Signature;
    type SigParseError = MmError<EthAssocTypesError>;

    async fn my_addr(&self) -> Self::Address {
        match self.derivation_method() {
            DerivationMethod::SingleAddress(addr) => *addr,
            // Todo: Expect should not fail but we need to handle it properly
            DerivationMethod::HDWallet(hd_wallet) => hd_wallet
                .get_enabled_address()
                .await
                .expect("Getting enabled address should not fail!")
                .address(),
        }
    }

    fn parse_address(&self, address: &str) -> Result<Self::Address, Self::AddressParseError> {
        // crate `Address::from_str` supports both address variants with and without `0x` prefix
        Address::from_str(address).map_to_mm(|e| EthAssocTypesError::InvalidHexString(e.to_string()))
    }

    /// As derive_htlc_pubkey_v2 returns coin specific pubkey we can use [Public::from_slice] directly
    fn parse_pubkey(&self, pubkey: &[u8]) -> Result<Self::Pubkey, Self::PubkeyParseError> {
        Ok(Public::from_slice(pubkey))
    }

    fn parse_tx(&self, tx: &[u8]) -> Result<Self::Tx, Self::TxParseError> {
        let unverified: UnverifiedTransactionWrapper = rlp::decode(tx).map_err(EthAssocTypesError::from)?;
        SignedEthTx::new(unverified).map_to_mm(|e| EthAssocTypesError::TxParseError(e.to_string()))
    }

    fn parse_preimage(&self, tx: &[u8]) -> Result<Self::Preimage, Self::PreimageParseError> { self.parse_tx(tx) }

    fn parse_signature(&self, sig: &[u8]) -> Result<Self::Sig, Self::SigParseError> {
        if sig.len() != 65 {
            return MmError::err(EthAssocTypesError::ParseSignatureError(
                "Signature slice is not 65 bytes long".to_string(),
            ));
        };

        let mut arr = [0; 65];
        arr.copy_from_slice(sig);
        Ok(Signature::from(arr)) // Assuming `Signature::from([u8; 65])` exists
    }
}

impl ToBytes for Address {
    fn to_bytes(&self) -> Vec<u8> { self.0.to_vec() }
}

impl AddrToString for Address {
    fn addr_to_string(&self) -> String { eth_addr_to_hex(self) }
}

impl ToBytes for BigUint {
    fn to_bytes(&self) -> Vec<u8> { self.to_bytes_be() }
}

impl ToBytes for ContractType {
    fn to_bytes(&self) -> Vec<u8> { self.to_string().into_bytes() }
}

impl ToBytes for Public {
    fn to_bytes(&self) -> Vec<u8> { self.0.to_vec() }
}

impl ParseNftAssocTypes for EthCoin {
    type ContractAddress = Address;
    type TokenId = BigUint;
    type ContractType = ContractType;
    type NftAssocTypesError = MmError<EthNftAssocTypesError>;

    fn parse_contract_address(
        &self,
        contract_address: &[u8],
    ) -> Result<Self::ContractAddress, Self::NftAssocTypesError> {
        contract_address
            .try_to_address()
            .map_to_mm(EthNftAssocTypesError::ParseTokenContractError)
    }

    fn parse_token_id(&self, token_id: &[u8]) -> Result<Self::TokenId, Self::NftAssocTypesError> {
        Ok(BigUint::from_bytes_be(token_id))
    }

    fn parse_contract_type(&self, contract_type: &[u8]) -> Result<Self::ContractType, Self::NftAssocTypesError> {
        let contract_str = from_utf8(contract_type).map_err(|e| EthNftAssocTypesError::Utf8Error(e.to_string()))?;
        ContractType::from_str(contract_str).map_to_mm(EthNftAssocTypesError::from)
    }
}

#[async_trait]
impl MakerNftSwapOpsV2 for EthCoin {
    async fn send_nft_maker_payment_v2(
        &self,
        args: SendNftMakerPaymentArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.send_nft_maker_payment_v2_impl(args).await
    }

    async fn validate_nft_maker_payment_v2(
        &self,
        args: ValidateNftMakerPaymentArgs<'_, Self>,
    ) -> ValidatePaymentResult<()> {
        self.validate_nft_maker_payment_v2_impl(args).await
    }

    async fn spend_nft_maker_payment_v2(
        &self,
        args: SpendNftMakerPaymentArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.spend_nft_maker_payment_v2_impl(args).await
    }

    async fn refund_nft_maker_payment_v2_timelock(
        &self,
        args: RefundNftMakerPaymentArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.refund_nft_maker_payment_v2_timelock_impl(args).await
    }

    async fn refund_nft_maker_payment_v2_secret(
        &self,
        args: RefundNftMakerPaymentArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.refund_nft_maker_payment_v2_secret_impl(args).await
    }
}

impl CoinWithPrivKeyPolicy for EthCoin {
    type KeyPair = KeyPair;

    fn priv_key_policy(&self) -> &PrivKeyPolicy<Self::KeyPair> { &self.priv_key_policy }
}

impl CoinWithDerivationMethod for EthCoin {
    fn derivation_method(&self) -> &DerivationMethod<HDCoinAddress<Self>, Self::HDWallet> { &self.derivation_method }
}

#[async_trait]
impl IguanaBalanceOps for EthCoin {
    type BalanceObject = CoinBalanceMap;

    async fn iguana_balances(&self) -> BalanceResult<Self::BalanceObject> {
        let platform_balance = self.my_balance().compat().await?;
        let token_balances = self.get_tokens_balance_list().await?;
        let mut balances = CoinBalanceMap::new();
        balances.insert(self.ticker().to_string(), platform_balance);
        balances.extend(token_balances.into_iter());
        Ok(balances)
    }
}

#[async_trait]
impl GetNewAddressRpcOps for EthCoin {
    type BalanceObject = CoinBalanceMap;
    async fn get_new_address_rpc_without_conf(
        &self,
        params: GetNewAddressParams,
    ) -> MmResult<GetNewAddressResponse<Self::BalanceObject>, GetNewAddressRpcError> {
        get_new_address::common_impl::get_new_address_rpc_without_conf(self, params).await
    }

    async fn get_new_address_rpc<ConfirmAddress>(
        &self,
        params: GetNewAddressParams,
        confirm_address: &ConfirmAddress,
    ) -> MmResult<GetNewAddressResponse<Self::BalanceObject>, GetNewAddressRpcError>
    where
        ConfirmAddress: HDConfirmAddress,
    {
        get_new_address::common_impl::get_new_address_rpc(self, params, confirm_address).await
    }
}

#[async_trait]
impl AccountBalanceRpcOps for EthCoin {
    type BalanceObject = CoinBalanceMap;

    async fn account_balance_rpc(
        &self,
        params: AccountBalanceParams,
    ) -> MmResult<HDAccountBalanceResponse<Self::BalanceObject>, HDAccountBalanceRpcError> {
        account_balance::common_impl::account_balance_rpc(self, params).await
    }
}

#[async_trait]
impl InitAccountBalanceRpcOps for EthCoin {
    type BalanceObject = CoinBalanceMap;

    async fn init_account_balance_rpc(
        &self,
        params: InitAccountBalanceParams,
    ) -> MmResult<HDAccountBalance<Self::BalanceObject>, HDAccountBalanceRpcError> {
        init_account_balance::common_impl::init_account_balance_rpc(self, params).await
    }
}

#[async_trait]
impl InitScanAddressesRpcOps for EthCoin {
    type BalanceObject = CoinBalanceMap;

    async fn init_scan_for_new_addresses_rpc(
        &self,
        params: ScanAddressesParams,
    ) -> MmResult<ScanAddressesResponse<Self::BalanceObject>, HDAccountBalanceRpcError> {
        init_scan_for_new_addresses::common_impl::scan_for_new_addresses_rpc(self, params).await
    }
}

#[async_trait]
impl InitCreateAccountRpcOps for EthCoin {
    type BalanceObject = CoinBalanceMap;

    async fn init_create_account_rpc<XPubExtractor>(
        &self,
        params: CreateNewAccountParams,
        state: CreateAccountState,
        xpub_extractor: Option<XPubExtractor>,
    ) -> MmResult<HDAccountBalance<Self::BalanceObject>, CreateAccountRpcError>
    where
        XPubExtractor: HDXPubExtractor + Send,
    {
        init_create_account::common_impl::init_create_new_account_rpc(self, params, state, xpub_extractor).await
    }

    async fn revert_creating_account(&self, account_id: u32) {
        init_create_account::common_impl::revert_creating_account(self, account_id).await
    }
}

/// Converts and extended public key derived using BIP32 to an Ethereum public key.
pub fn pubkey_from_extended(extended_pubkey: &Secp256k1ExtendedPublicKey) -> Public {
    let serialized = extended_pubkey.public_key().serialize_uncompressed();
    let mut pubkey_uncompressed = Public::default();
    pubkey_uncompressed.as_mut().copy_from_slice(&serialized[1..]);
    pubkey_uncompressed
}

fn extract_gas_limit_from_conf<T: ExtractGasLimit>(coin_conf: &Json) -> Result<T, String> {
    let key = T::key();
    if coin_conf[key].is_null() {
        Ok(Default::default())
    } else {
        json::from_value(coin_conf[key].clone()).map_err(|e| e.to_string())
    }
}

impl Eip1559Ops for EthCoin {
    fn get_swap_transaction_fee_policy(&self) -> SwapTxFeePolicy { self.swap_txfee_policy.lock().unwrap().clone() }

    fn set_swap_transaction_fee_policy(&self, swap_txfee_policy: SwapTxFeePolicy) {
        *self.swap_txfee_policy.lock().unwrap() = swap_txfee_policy
    }
}

#[async_trait]
impl TakerCoinSwapOpsV2 for EthCoin {
    /// Wrapper for [EthCoin::send_taker_funding_impl]
    async fn send_taker_funding(&self, args: SendTakerFundingArgs<'_>) -> Result<Self::Tx, TransactionErr> {
        self.send_taker_funding_impl(args).await
    }

    /// Wrapper for [EthCoin::validate_taker_funding_impl]
    async fn validate_taker_funding(&self, args: ValidateTakerFundingArgs<'_, Self>) -> ValidateSwapV2TxResult {
        self.validate_taker_funding_impl(args).await
    }

    async fn refund_taker_funding_timelock(
        &self,
        args: RefundTakerPaymentArgs<'_>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.refund_taker_payment_with_timelock_impl(args).await
    }

    async fn refund_taker_funding_secret(
        &self,
        args: RefundFundingSecretArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.refund_taker_funding_secret_impl(args).await
    }

    /// Wrapper for [EthCoin::search_for_taker_funding_spend_impl]
    async fn search_for_taker_funding_spend(
        &self,
        tx: &Self::Tx,
        _from_block: u64,
        _secret_hash: &[u8],
    ) -> Result<Option<FundingTxSpend<Self>>, SearchForFundingSpendErr> {
        self.search_for_taker_funding_spend_impl(tx).await
    }

    /// Eth doesnt have preimages
    async fn gen_taker_funding_spend_preimage(
        &self,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        _swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self> {
        Ok(TxPreimageWithSig {
            preimage: args.funding_tx.clone(),
            signature: args.funding_tx.signature(),
        })
    }

    /// Eth doesnt have preimages
    async fn validate_taker_funding_spend_preimage(
        &self,
        _gen_args: &GenTakerFundingSpendArgs<'_, Self>,
        _preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerFundingSpendPreimageResult {
        Ok(())
    }

    /// Wrapper for [EthCoin::taker_payment_approve]
    async fn sign_and_send_taker_funding_spend(
        &self,
        _preimage: &TxPreimageWithSig<Self>,
        args: &GenTakerFundingSpendArgs<'_, Self>,
        _swap_unique_data: &[u8],
    ) -> Result<Self::Tx, TransactionErr> {
        self.taker_payment_approve(args).await
    }

    async fn refund_combined_taker_payment(
        &self,
        args: RefundTakerPaymentArgs<'_>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.refund_taker_payment_with_timelock_impl(args).await
    }

    fn skip_taker_payment_spend_preimage(&self) -> bool { true }

    /// Eth skips taker_payment_spend_preimage, as it doesnt need it
    async fn gen_taker_payment_spend_preimage(
        &self,
        _args: &GenTakerPaymentSpendArgs<'_, Self>,
        _swap_unique_data: &[u8],
    ) -> GenPreimageResult<Self> {
        MmError::err(TxGenError::Other(
            "EVM-based coin doesn't have taker_payment_spend_preimage. Report the Bug!".to_string(),
        ))
    }

    /// Eth skips taker_payment_spend_preimage, as it doesnt need it
    async fn validate_taker_payment_spend_preimage(
        &self,
        _gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        _preimage: &TxPreimageWithSig<Self>,
    ) -> ValidateTakerPaymentSpendPreimageResult {
        MmError::err(ValidateTakerPaymentSpendPreimageError::InvalidPreimage(
            "EVM-based coin skips taker_payment_spend_preimage validation. Report the Bug!".to_string(),
        ))
    }

    /// Eth doesnt have preimages
    async fn sign_and_broadcast_taker_payment_spend(
        &self,
        _preimage: Option<&TxPreimageWithSig<Self>>,
        gen_args: &GenTakerPaymentSpendArgs<'_, Self>,
        secret: &[u8],
        _swap_unique_data: &[u8],
    ) -> Result<Self::Tx, TransactionErr> {
        self.sign_and_broadcast_taker_payment_spend_impl(gen_args, secret).await
    }

    /// Wrapper for [EthCoin::find_taker_payment_spend_tx_impl]
    async fn find_taker_payment_spend_tx(
        &self,
        taker_payment: &Self::Tx,
        from_block: u64,
        wait_until: u64,
    ) -> MmResult<Self::Tx, FindPaymentSpendError> {
        const CHECK_EVERY: f64 = 10.;
        self.find_taker_payment_spend_tx_impl(taker_payment, from_block, wait_until, CHECK_EVERY)
            .await
    }

    async fn extract_secret_v2(&self, _secret_hash: &[u8], spend_tx: &Self::Tx) -> Result<[u8; 32], String> {
        self.extract_secret_v2_impl(spend_tx).await
    }
}

impl CommonSwapOpsV2 for EthCoin {
    #[inline(always)]
    fn derive_htlc_pubkey_v2(&self, _swap_unique_data: &[u8]) -> Self::Pubkey {
        match self.priv_key_policy {
            EthPrivKeyPolicy::Iguana(ref key_pair)
            | EthPrivKeyPolicy::HDWallet {
                activated_key: ref key_pair,
                ..
            } => *key_pair.public(),
            EthPrivKeyPolicy::Trezor => todo!(),
            #[cfg(target_arch = "wasm32")]
            EthPrivKeyPolicy::Metamask(ref metamask_policy) => {
                // The metamask public key should be uncompressed
                // Remove the first byte (0x04) from the uncompressed public key
                let pubkey_bytes: [u8; 64] = metamask_policy.public_key_uncompressed[1..65]
                    .try_into()
                    .expect("slice with incorrect length");
                Public::from_slice(&pubkey_bytes)
            },
        }
    }

    #[inline(always)]
    fn derive_htlc_pubkey_v2_bytes(&self, swap_unique_data: &[u8]) -> Vec<u8> {
        self.derive_htlc_pubkey_v2(swap_unique_data).to_bytes()
    }

    #[inline(always)]
    fn taker_pubkey_bytes(&self) -> Option<Vec<u8>> {
        Some(self.derive_htlc_pubkey_v2(&[]).to_bytes()) // unique_data not used for non-private coins
    }
}

#[cfg(all(feature = "for-tests", not(target_arch = "wasm32")))]
impl EthCoin {
    pub async fn set_coin_type(&self, new_coin_type: EthCoinType) -> EthCoin {
        let coin = EthCoinImpl {
            ticker: self.ticker.clone(),
            coin_type: new_coin_type,
            priv_key_policy: self.priv_key_policy.clone(),
            derivation_method: Arc::clone(&self.derivation_method),
            sign_message_prefix: self.sign_message_prefix.clone(),
            swap_contract_address: self.swap_contract_address,
            swap_v2_contracts: self.swap_v2_contracts,
            fallback_swap_contract: self.fallback_swap_contract,
            contract_supports_watchers: self.contract_supports_watchers,
            web3_instances: AsyncMutex::new(self.web3_instances.lock().await.clone()),
            decimals: self.decimals,
            history_sync_state: Mutex::new(self.history_sync_state.lock().unwrap().clone()),
            required_confirmations: AtomicU64::new(
                self.required_confirmations.load(std::sync::atomic::Ordering::SeqCst),
            ),
            swap_txfee_policy: Mutex::new(self.swap_txfee_policy.lock().unwrap().clone()),
            max_eth_tx_type: self.max_eth_tx_type,
            ctx: self.ctx.clone(),
            chain_id: self.chain_id,
            trezor_coin: self.trezor_coin.clone(),
            logs_block_range: self.logs_block_range,
            address_nonce_locks: Arc::clone(&self.address_nonce_locks),
            erc20_tokens_infos: Arc::clone(&self.erc20_tokens_infos),
            nfts_infos: Arc::clone(&self.nfts_infos),
            gas_limit: EthGasLimit::default(),
            gas_limit_v2: EthGasLimitV2::default(),
            abortable_system: self.abortable_system.create_subsystem().unwrap(),
        };
        EthCoin(Arc::new(coin))
    }
}

#[async_trait]
impl MakerCoinSwapOpsV2 for EthCoin {
    async fn send_maker_payment_v2(&self, args: SendMakerPaymentArgs<'_, Self>) -> Result<Self::Tx, TransactionErr> {
        self.send_maker_payment_v2_impl(args).await
    }

    async fn validate_maker_payment_v2(&self, args: ValidateMakerPaymentArgs<'_, Self>) -> ValidatePaymentResult<()> {
        self.validate_maker_payment_v2_impl(args).await
    }

    async fn refund_maker_payment_v2_timelock(
        &self,
        args: RefundMakerPaymentTimelockArgs<'_>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.refund_maker_payment_v2_timelock_impl(args).await
    }

    async fn refund_maker_payment_v2_secret(
        &self,
        args: RefundMakerPaymentSecretArgs<'_, Self>,
    ) -> Result<Self::Tx, TransactionErr> {
        self.refund_maker_payment_v2_secret_impl(args).await
    }

    async fn spend_maker_payment_v2(&self, args: SpendMakerPaymentArgs<'_, Self>) -> Result<Self::Tx, TransactionErr> {
        self.spend_maker_payment_v2_impl(args).await
    }
}
