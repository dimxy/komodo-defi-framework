use crate::tendermint::htlc::HtlcState;

#[derive(prost::Message)]
pub(crate) struct IrisCreateHtlcProto {
    #[prost(string, tag = "1")]
    pub(crate) sender: prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub(crate) to: prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub(crate) receiver_on_other_chain: prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub(crate) sender_on_other_chain: prost::alloc::string::String,
    #[prost(message, repeated, tag = "5")]
    pub(crate) amount: prost::alloc::vec::Vec<cosmrs::proto::cosmos::base::v1beta1::Coin>,
    #[prost(string, tag = "6")]
    pub(crate) hash_lock: prost::alloc::string::String,
    #[prost(uint64, tag = "7")]
    pub(crate) timestamp: u64,
    #[prost(uint64, tag = "8")]
    pub(crate) time_lock: u64,
    #[prost(bool, tag = "9")]
    pub(crate) transfer: bool,
}

#[derive(prost::Message)]
pub(crate) struct IrisClaimHtlcProto {
    #[prost(string, tag = "1")]
    pub(crate) sender: prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub(crate) id: prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub(crate) secret: prost::alloc::string::String,
}

#[derive(prost::Message)]
pub struct IrisHtlcProto {
    #[prost(string, tag = "1")]
    pub(crate) id: prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub(crate) sender: prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub(crate) to: prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub(crate) receiver_on_other_chain: prost::alloc::string::String,
    #[prost(string, tag = "5")]
    pub(crate) sender_on_other_chain: prost::alloc::string::String,
    #[prost(message, repeated, tag = "6")]
    pub(crate) amount: prost::alloc::vec::Vec<cosmrs::proto::cosmos::base::v1beta1::Coin>,
    #[prost(string, tag = "7")]
    pub(crate) hash_lock: prost::alloc::string::String,
    #[prost(string, tag = "8")]
    pub(crate) secret: prost::alloc::string::String,
    #[prost(uint64, tag = "9")]
    pub(crate) timestamp: u64,
    #[prost(uint64, tag = "10")]
    pub(crate) expiration_height: u64,
    #[prost(enumeration = "HtlcState", tag = "11")]
    pub(crate) state: i32,
    #[prost(uint64, tag = "12")]
    pub(crate) closed_block: u64,
    #[prost(bool, tag = "13")]
    pub(crate) transfer: bool,
}

#[derive(prost::Message)]
pub(crate) struct IrisQueryHtlcResponseProto {
    #[prost(message, tag = "1")]
    pub(crate) htlc: Option<IrisHtlcProto>,
}
