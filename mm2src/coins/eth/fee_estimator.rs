use std::convert::TryInto;

use ethereum_types::U256;
use super::{Eip1559EstimatedGasFees, PriorityFee};
use super::web3_transport::FeeHistoryResult;

/// Simple fee estimator paramss for 3 priority levels
/// normally used if provider is not available

pub struct EthPriorityFeeEstimator {}

impl EthPriorityFeeEstimator {

    /// depth to look for fee history to estimate priority fees 
    const FEE_PRIORITY_DEPTH: u64 = 5u64;

    /// percentiles to pass to eth_feeHistory
    const HISTORY_PERCENTILES: [f64; 3] = [ 25.0, 50.0, 75.0 ];

    /// percentiles to calc max priority fee over historical rewards
    const CALC_PERCENTILES: [f64; 3] = [ 50.0, 50.0, 50.0 ];

    /// block depth for eth_feeHistory
    pub fn history_depth() -> u64 {
        Self::FEE_PRIORITY_DEPTH
    }

    /// percentiles for priority rewards obtained with eth_feeHistory
    pub fn history_percentiles() -> &'static [f64] {
        &Self::HISTORY_PERCENTILES
    }

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
    pub fn estimate_fees(fee_history: &FeeHistoryResult) -> Eip1559EstimatedGasFees {

        let base_fee = *fee_history.base_fee_per_gas.first().unwrap_or(&U256::from(0));
        let mut priority_fees = vec![];
        for i in 0..Self::HISTORY_PERCENTILES.len() {
            let mut level_rewards = fee_history.priority_rewards
                .iter()
                .map(|rewards| if i < rewards.len() { rewards[i] } else { U256::from(0) })
                .collect::<Vec<_>>();
            let max_priority_fee_per_gas = Self::percentile_of(&mut level_rewards, Self::CALC_PERCENTILES[i]);
            let priority_fee = PriorityFee {
                max_priority_fee_per_gas,
                max_fee_per_gas: base_fee + max_priority_fee_per_gas,
                min_wait_time: None,
                max_wait_time: None,
            };
            priority_fees.push(priority_fee);
        }
        Eip1559EstimatedGasFees {
            base_fee,
            priority_fees: priority_fees.try_into().unwrap_or_default(),
        }
    }

}