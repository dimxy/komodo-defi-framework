use crypto::ChecksumType;
use {Address, AddressFormat, AddressHashEnum, AddressPrefixes, AddressScriptType, NetworkAddressPrefixes};

/// Params for AddressBuilder to select output script type
#[derive(PartialEq)]
pub enum AddressBuilderOption {
    /// build for pay to pubkey hash output (witness or legacy)
    BuildP2Pkh,
    /// build for pay to script hash output (witness or legacy)
    BuildP2Sh,
}

/// Builder for Address
/// Returns Address struct depending on addr_format value checking other properties
pub struct AddressBuilder {
    /// Coin base58 address prefixes from coin config
    pub prefixes: NetworkAddressPrefixes,
    /// Segwit addr human readable part
    pub hrp: Option<String>,
    /// Public key hash.
    pub hash: AddressHashEnum,
    /// Checksum type
    pub checksum_type: ChecksumType,
    /// Address Format
    pub addr_format: AddressFormat,
}

impl AddressBuilder {
    /// Builds Address with the script type corresponding to p2pkh or p2wpkh
    pub fn build_p2pkh(&self) -> Result<Address, String> { self.build(AddressBuilderOption::BuildP2Pkh) }

    /// Builds Address with the script type corresponding to p2sh or p2wsh
    pub fn build_p2sh(&self) -> Result<Address, String> { self.build(AddressBuilderOption::BuildP2Sh) }

    pub fn build(&self, build_option: AddressBuilderOption) -> Result<Address, String> {
        match &self.addr_format {
            AddressFormat::Standard => Ok(Address {
                prefixes: self.get_address_prefixes(&build_option)?,
                hrp: None,
                hash: self.hash.clone(),
                checksum_type: self.checksum_type,
                addr_format: self.addr_format.clone(),
                script_type: self.get_legacy_script_type(&build_option),
            }),
            AddressFormat::Segwit => {
                self.check_segwit_hrp()?;
                self.check_segwit_hash(&build_option)?;
                Ok(Address {
                    prefixes: AddressPrefixes::default(),
                    hrp: self.hrp.clone(),
                    hash: self.hash.clone(),
                    checksum_type: self.checksum_type,
                    addr_format: self.addr_format.clone(),
                    script_type: self.get_segwit_script_type(&build_option),
                })
            },
            AddressFormat::CashAddress { .. } => Ok(Address {
                prefixes: self.get_address_prefixes(&build_option)?,
                hrp: None,
                hash: self.hash.clone(),
                checksum_type: self.checksum_type,
                addr_format: self.addr_format.clone(),
                script_type: self.get_legacy_script_type(&build_option),
            }),
        }
    }

    fn get_address_prefixes(&self, build_option: &AddressBuilderOption) -> Result<AddressPrefixes, String> {
        let prefixes = match build_option {
            AddressBuilderOption::BuildP2Pkh => &self.prefixes.p2pkh,
            AddressBuilderOption::BuildP2Sh => &self.prefixes.p2sh,
        };
        if prefixes.is_empty() {
            return Err("no prefixes for address".to_owned());
        }
        Ok(prefixes.clone())
    }

    fn get_legacy_script_type(&self, build_option: &AddressBuilderOption) -> AddressScriptType {
        match build_option {
            AddressBuilderOption::BuildP2Pkh => AddressScriptType::P2PKH,
            AddressBuilderOption::BuildP2Sh => AddressScriptType::P2SH,
        }
    }

    fn get_segwit_script_type(&self, build_option: &AddressBuilderOption) -> AddressScriptType {
        match build_option {
            AddressBuilderOption::BuildP2Pkh => AddressScriptType::P2WPKH,
            AddressBuilderOption::BuildP2Sh => AddressScriptType::P2WSH,
        }
    }

    fn check_segwit_hrp(&self) -> Result<(), String> {
        if self.hrp.is_none() {
            return Err("no hrp for address".to_owned());
        }
        Ok(())
    }

    fn check_segwit_hash(&self, build_option: &AddressBuilderOption) -> Result<(), String> {
        let is_hash_valid = match build_option {
            AddressBuilderOption::BuildP2Pkh => self.hash.is_address_hash(),
            AddressBuilderOption::BuildP2Sh => self.hash.is_witness_script_hash(),
        };
        if !is_hash_valid {
            return Err("invalid hash for segwit address".to_owned());
        }
        Ok(())
    }
}
