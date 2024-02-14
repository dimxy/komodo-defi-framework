use bip32::DerivationPath;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::str::FromStr;

pub trait HDAddressOps {
    // Todo: revise this
    type Address: Clone + Display + Send + Sync + FromStr + Eq + Debug + Hash;
    type Pubkey: Clone;

    fn address(&self) -> Self::Address;
    fn pubkey(&self) -> Self::Pubkey;
    fn derivation_path(&self) -> &DerivationPath;
}
