use crate::{TransactionType, TxFeeDetails, WithdrawFee};
use ethereum_types::Address;
use mm2_number::BigDecimal;
use rpc::v1::types::Bytes as BytesJson;
use serde::Deserialize;
use serde_json::Value as Json;
use std::fmt;
use std::str::FromStr;
use url::Url;

#[derive(Debug, Deserialize)]
pub struct NftListReq {
    pub(crate) chains: Vec<Chain>,
    pub(crate) url: Url,
}

#[derive(Debug, Deserialize)]
pub struct NftMetadataReq {
    pub(crate) token_address: Address,
    pub(crate) token_id: BigDecimal,
    pub(crate) chain: Chain,
    pub(crate) url: Url,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum Chain {
    Avalanche,
    Bsc,
    Eth,
    Fantom,
    Polygon,
}

impl fmt::Display for Chain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            Chain::Avalanche => write!(f, "AVALANCHE"),
            Chain::Bsc => write!(f, "BSC"),
            Chain::Eth => write!(f, "ETH"),
            Chain::Fantom => write!(f, "FANTOM"),
            Chain::Polygon => write!(f, "POLYGON"),
        }
    }
}

pub(crate) trait ConvertChain {
    fn to_ticker(&self) -> String;
}

impl ConvertChain for Chain {
    fn to_ticker(&self) -> String {
        match self {
            Chain::Avalanche => "AVAX".to_owned(),
            Chain::Bsc => "BNB".to_owned(),
            Chain::Eth => "ETH".to_owned(),
            Chain::Fantom => "FTM".to_owned(),
            Chain::Polygon => "MATIC".to_owned(),
        }
    }
}

#[derive(Debug, Display)]
pub(crate) enum ParseContractTypeError {
    UnsupportedContractType,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum ContractType {
    Erc1155,
    Erc721,
}

impl FromStr for ContractType {
    type Err = ParseContractTypeError;

    #[inline]
    fn from_str(s: &str) -> Result<ContractType, ParseContractTypeError> {
        match s {
            "ERC1155" => Ok(ContractType::Erc1155),
            "ERC721" => Ok(ContractType::Erc721),
            _ => Err(ParseContractTypeError::UnsupportedContractType),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct UriMeta {
    pub(crate) image: Option<String>,
    #[serde(rename(deserialize = "name"))]
    pub(crate) token_name: Option<String>,
    description: Option<String>,
    attributes: Option<Json>,
    animation_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Nft {
    pub(crate) chain: Chain,
    pub(crate) token_address: String,
    pub(crate) token_id: BigDecimal,
    pub(crate) amount: BigDecimal,
    pub(crate) owner_of: String,
    pub(crate) token_hash: String,
    pub(crate) block_number_minted: u64,
    pub(crate) block_number: u64,
    pub(crate) contract_type: Option<ContractType>,
    pub(crate) collection_name: Option<String>,
    pub(crate) symbol: Option<String>,
    pub(crate) token_uri: Option<String>,
    pub(crate) metadata: Option<String>,
    pub(crate) last_token_uri_sync: Option<String>,
    pub(crate) last_metadata_sync: Option<String>,
    pub(crate) minter_address: Option<String>,
    pub(crate) possible_spam: Option<bool>,
    pub(crate) uri_meta: UriMeta,
}

/// This structure is for deserializing NFT json to struct.
/// Its needed to convert fields properly, because all fields in json have string type.
#[derive(Debug, Deserialize)]
pub(crate) struct NftWrapper {
    pub(crate) token_address: String,
    pub(crate) token_id: SerdeStringWrap<BigDecimal>,
    pub(crate) amount: SerdeStringWrap<BigDecimal>,
    pub(crate) owner_of: String,
    pub(crate) token_hash: String,
    pub(crate) block_number_minted: SerdeStringWrap<u64>,
    pub(crate) block_number: SerdeStringWrap<u64>,
    pub(crate) contract_type: Option<SerdeStringWrap<ContractType>>,
    pub(crate) name: Option<String>,
    pub(crate) symbol: Option<String>,
    pub(crate) token_uri: Option<String>,
    pub(crate) metadata: Option<String>,
    pub(crate) last_token_uri_sync: Option<String>,
    pub(crate) last_metadata_sync: Option<String>,
    pub(crate) minter_address: Option<String>,
    pub(crate) possible_spam: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct SerdeStringWrap<T>(pub(crate) T);

impl<'de, T> Deserialize<'de> for SerdeStringWrap<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Debug + std::fmt::Display,
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value: &str = Deserialize::deserialize(deserializer)?;
        let value: T = match value.parse() {
            Ok(v) => v,
            Err(e) => return Err(<D::Error as serde::de::Error>::custom(e)),
        };
        Ok(SerdeStringWrap(value))
    }
}

impl<T> std::ops::Deref for SerdeStringWrap<T> {
    type Target = T;
    fn deref(&self) -> &T { &self.0 }
}

#[derive(Debug, Serialize)]
pub struct NftList {
    pub(crate) nfts: Vec<Nft>,
}

#[derive(Clone, Deserialize)]
pub struct WithdrawErc1155 {
    pub(crate) chain: Chain,
    pub(crate) to: String,
    pub(crate) token_address: String,
    pub(crate) token_id: BigDecimal,
    pub(crate) amount: Option<BigDecimal>,
    #[serde(default)]
    pub(crate) max: bool,
    pub(crate) fee: Option<WithdrawFee>,
}

#[derive(Clone, Deserialize)]
pub struct WithdrawErc721 {
    pub(crate) chain: Chain,
    pub(crate) to: String,
    pub(crate) token_address: String,
    pub(crate) token_id: BigDecimal,
    pub(crate) fee: Option<WithdrawFee>,
}

#[derive(Clone, Deserialize)]
pub struct WithdrawNftReq {
    pub(crate) url: Url,
    pub(crate) withdraw_type: WithdrawNftType,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "type", content = "withdraw_data")]
#[serde(rename_all = "snake_case")]
pub enum WithdrawNftType {
    WithdrawErc1155(WithdrawErc1155),
    WithdrawErc721(WithdrawErc721),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TransactionNftDetails {
    /// Raw bytes of signed transaction, this should be sent as is to `send_raw_transaction` RPC to broadcast the transaction
    pub(crate) tx_hex: BytesJson,
    pub(crate) tx_hash: String,
    /// NFTs are sent from these addresses
    pub(crate) from: Vec<String>,
    /// NFTs are sent to these addresses
    pub(crate) to: Vec<String>,
    pub(crate) contract_type: ContractType,
    pub(crate) token_address: String,
    pub(crate) token_id: BigDecimal,
    pub(crate) amount: BigDecimal,
    pub(crate) fee_details: Option<TxFeeDetails>,
    /// The coin transaction belongs to
    pub(crate) coin: String,
    /// Block height
    pub(crate) block_height: u64,
    /// Transaction timestamp
    pub(crate) timestamp: u64,
    /// Internal MM2 id used for internal transaction identification, for some coins it might be equal to transaction hash
    pub(crate) internal_id: i64,
    /// Type of transactions, default is StandardTransfer
    #[serde(default)]
    pub(crate) transaction_type: TransactionType,
}

#[derive(Debug, Deserialize)]
pub struct NftTransfersReq {
    pub(crate) chains: Vec<Chain>,
    pub(crate) url: Url,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) enum TransferStatus {
    Receive,
    Send,
}

#[derive(Debug, Serialize)]
pub(crate) struct NftTransferHistory {
    pub(crate) chain: Chain,
    pub(crate) block_number: u64,
    pub(crate) block_timestamp: String,
    pub(crate) block_hash: String,
    /// Transaction hash in hexadecimal format
    pub(crate) transaction_hash: String,
    pub(crate) transaction_index: u64,
    pub(crate) log_index: u64,
    pub(crate) value: BigDecimal,
    pub(crate) contract_type: ContractType,
    pub(crate) transaction_type: String,
    pub(crate) token_address: String,
    pub(crate) token_id: BigDecimal,
    pub(crate) collection_name: Option<String>,
    pub(crate) image: Option<String>,
    pub(crate) token_name: Option<String>,
    pub(crate) from_address: String,
    pub(crate) to_address: String,
    pub(crate) status: TransferStatus,
    pub(crate) amount: BigDecimal,
    pub(crate) verified: u64,
    pub(crate) operator: Option<String>,
    pub(crate) possible_spam: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NftTransferHistoryWrapper {
    pub(crate) block_number: SerdeStringWrap<u64>,
    pub(crate) block_timestamp: String,
    pub(crate) block_hash: String,
    /// Transaction hash in hexadecimal format
    pub(crate) transaction_hash: String,
    pub(crate) transaction_index: u64,
    pub(crate) log_index: u64,
    pub(crate) value: SerdeStringWrap<BigDecimal>,
    pub(crate) contract_type: SerdeStringWrap<ContractType>,
    pub(crate) transaction_type: String,
    pub(crate) token_address: String,
    pub(crate) token_id: SerdeStringWrap<BigDecimal>,
    pub(crate) from_address: String,
    pub(crate) to_address: String,
    pub(crate) amount: SerdeStringWrap<BigDecimal>,
    pub(crate) verified: u64,
    pub(crate) operator: Option<String>,
    pub(crate) possible_spam: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct NftsTransferHistoryList {
    pub(crate) transfer_history: Vec<NftTransferHistory>,
}
