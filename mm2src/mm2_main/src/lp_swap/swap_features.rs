/// Framework to activate new swap protocol features at certain protocol version

#[derive(PartialEq)]
pub(crate) enum SwapFeature {
    SendToPreBurnAccount,
}

impl SwapFeature {
    // add new features to activate
    const SWAP_FEATURE_ACTIVATION: &[(u16, SwapFeature)] = &[(1, SwapFeature::SendToPreBurnAccount)];

    pub(crate) fn is_active(feature: SwapFeature, version: u16) -> bool {
        if let Some(found) = Self::SWAP_FEATURE_ACTIVATION.iter().find(|fv| fv.1 == feature) {
            return version >= found.0;
        }
        false
    }
}
