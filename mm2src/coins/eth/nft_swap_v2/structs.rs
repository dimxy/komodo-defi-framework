use ethereum_types::{Address, U256};

pub(crate) struct ExpectedHtlcParams {
    pub(crate) swap_id: Vec<u8>,
    pub(crate) taker_address: Address,
    pub(crate) token_address: Address,
    pub(crate) taker_secret_hash: Vec<u8>,
    pub(crate) maker_secret_hash: Vec<u8>,
    pub(crate) time_lock: U256,
}

pub(crate) struct ValidationParams<'a> {
    pub(crate) maker_address: Address,
    pub(crate) nft_maker_swap_v2_contract: Address,
    pub(crate) token_id: &'a [u8],
    // Optional, as it's not needed for ERC721
    pub(crate) amount: Option<String>,
}
