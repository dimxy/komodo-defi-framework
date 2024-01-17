use std::convert::TryInto;

use super::web3_transport::FeeHistoryResult;
use ethereum_types::U256;

/// Simple priority fee estimator for 3 levels based on fee history
/// normally used if eth provider is not available

// Gas fee estimator types

/// Estimated priority gas fee
#[derive(Clone)]
pub struct PriorityFee {
    /// estimated max priority tip fee per gas
    pub max_priority_fee_per_gas: U256,
    /// estimated max fee per gas
    pub max_fee_per_gas: U256,
    /// estimated transaction min wait time in mempool in ms for this priority level
    pub min_wait_time: Option<u32>,
    /// estimated transaction max wait time in mempool in ms for this priority level
    pub max_wait_time: Option<u32>,
}

/// Estimated gas price for several priority levels
/// we support low/medium/high levels as we can use api providers which normally support such levels
#[derive(Default, Clone)]
pub struct GasFeeEstimatedInternal {
    /// base fee for the next block
    pub base_fee: U256,
    /// estimated priority fees
    pub priority_fees: [PriorityFee; 3],
}

impl Default for PriorityFee {
    fn default() -> Self {
        Self {
            max_priority_fee_per_gas: U256::from(0),
            max_fee_per_gas: U256::from(0),
            min_wait_time: None,
            max_wait_time: None,
        }
    }
}

/// Eth gas priority fee simple estimator based on eth fee history
pub struct GasPriorityFeeEstimator {}

impl GasPriorityFeeEstimator {
    /// depth to look for fee history to estimate priority fees
    const FEE_PRIORITY_DEPTH: u64 = 5u64;

    /// percentiles to pass to eth_feeHistory
    const HISTORY_PERCENTILES: [f64; 3] = [25.0, 50.0, 75.0];

    /// percentiles to calc max priority fee over historical rewards
    const CALC_PERCENTILES: [f64; 3] = [50.0, 50.0, 50.0];

    /// block depth for eth_feeHistory
    pub fn history_depth() -> u64 { Self::FEE_PRIORITY_DEPTH }

    /// percentiles for priority rewards obtained with eth_feeHistory
    pub fn history_percentiles() -> &'static [f64] { &Self::HISTORY_PERCENTILES }

    /// percentile for vector
    fn percentile_of(v: &mut Vec<U256>, percent: f64) -> U256 {
        v.sort();

        // validate bounds:
        let percent = if percent > 100.0 { 100.0 } else { percent };
        let percent = if percent < 0.0 { 0.0 } else { percent };

        let value_pos = ((v.len() - 1) as f64 * percent / 100.0).round() as usize;
        v[value_pos]
    }

    /// estimate max priority fee by fee history
    pub fn estimate_fees(fee_history: &FeeHistoryResult) -> GasFeeEstimatedInternal {
        let base_fee = *fee_history.base_fee_per_gas.first().unwrap_or(&U256::from(0));
        let mut priority_fees = vec![];
        for i in 0..Self::HISTORY_PERCENTILES.len() {
            let mut level_rewards = fee_history
                .priority_rewards
                .iter()
                .map(|rewards| if i < rewards.len() { rewards[i] } else { U256::from(0) })
                .collect::<Vec<_>>();
            let max_priority_fee_per_gas = Self::percentile_of(&mut level_rewards, Self::CALC_PERCENTILES[i]);
            let priority_fee = PriorityFee {
                max_priority_fee_per_gas,
                max_fee_per_gas: base_fee + max_priority_fee_per_gas,
                min_wait_time: None,
                max_wait_time: None, // TODO: maybe fill with some default values (and mark as uncertain)?
            };
            priority_fees.push(priority_fee);
        }
        GasFeeEstimatedInternal {
            base_fee,
            priority_fees: priority_fees.try_into().unwrap_or_default(),
        }
    }
}
