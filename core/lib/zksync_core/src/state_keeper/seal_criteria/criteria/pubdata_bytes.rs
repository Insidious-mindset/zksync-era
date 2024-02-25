use zksync_types::ProtocolVersionId;

use crate::state_keeper::seal_criteria::{
    SealCriterion, SealData, SealResolution, StateKeeperConfig,
};

#[derive(Debug)]
pub struct PubDataBytesCriterion {
    pub max_pubdata_per_da_slot: u64,
    pub num_da_slots: u8,
}

impl SealCriterion for PubDataBytesCriterion {
    fn should_seal(
        &self,
        config: &StateKeeperConfig,
        _block_open_timestamp_ms: u128,
        _tx_count: usize,
        block_data: &SealData,
        tx_data: &SealData,
        protocol_version: ProtocolVersionId,
    ) -> SealResolution {
        let max_pubdata_per_l1_batch =
            (self.max_pubdata_per_da_slot * (self.num_da_slots as u64)) as usize;
        let reject_bound =
            (max_pubdata_per_l1_batch as f64 * config.reject_tx_at_eth_params_percentage).round();
        let include_and_seal_bound =
            (max_pubdata_per_l1_batch as f64 * config.close_block_at_eth_params_percentage).round();

        let block_size =
            block_data.execution_metrics.size() + block_data.writes_metrics.size(protocol_version);
        // For backward compatibility, we need to keep calculating the size of the pubdata based
        // `StorageDeduplication` metrics. All vm versions
        // after vm with virtual blocks will provide the size of the pubdata in the execution metrics.
        let tx_size = if tx_data.execution_metrics.pubdata_published == 0 {
            tx_data.execution_metrics.size() + tx_data.writes_metrics.size(protocol_version)
        } else {
            tx_data.execution_metrics.pubdata_published as usize
        };
        if tx_size > reject_bound as usize {
            let message = "Transaction cannot be sent to L1 due to pubdata limits";
            SealResolution::Unexecutable(message.into())
        } else if block_size > max_pubdata_per_l1_batch {
            SealResolution::ExcludeAndSeal
        } else if block_size > include_and_seal_bound as usize {
            SealResolution::IncludeAndSeal
        } else {
            SealResolution::NoSeal
        }
    }

    fn prom_criterion_name(&self) -> &'static str {
        "pub_data_size"
    }
}

#[cfg(test)]
mod tests {
    use zksync_types::tx::ExecutionMetrics;

    use super::*;

    #[test]
    fn seal_criterion() {
        // Create an empty config and only setup fields relevant for the test.
        let config = StateKeeperConfig {
            reject_tx_at_eth_params_percentage: 0.95,
            close_block_at_eth_params_percentage: 0.95,
            max_pubdata_per_batch: 100000,
            ..Default::default()
        };

        let criterion = PubDataBytesCriterion {
            max_pubdata_per_da_slot: 100000,
            num_da_slots: 1,
        };

        let block_execution_metrics = ExecutionMetrics {
            l2_l1_long_messages: ((config.max_pubdata_per_batch
                * (config.max_number_da_slots as u64)) as f64
                * config.close_block_at_eth_params_percentage
                - 1.0)
                .round() as usize,
            ..ExecutionMetrics::default()
        };

        let empty_block_resolution = criterion.should_seal(
            &config,
            0,
            0,
            &SealData {
                execution_metrics: block_execution_metrics,
                ..SealData::default()
            },
            &SealData::default(),
            ProtocolVersionId::latest(),
        );
        assert_eq!(empty_block_resolution, SealResolution::NoSeal);

        let block_execution_metrics = ExecutionMetrics {
            l2_l1_long_messages: (config.max_pubdata_per_batch as f64
                * config.close_block_at_eth_params_percentage
                + 1f64)
                .round() as usize,
            ..ExecutionMetrics::default()
        };

        let full_block_resolution = criterion.should_seal(
            &config,
            0,
            0,
            &SealData {
                execution_metrics: block_execution_metrics,
                ..SealData::default()
            },
            &SealData::default(),
            ProtocolVersionId::latest(),
        );
        assert_eq!(full_block_resolution, SealResolution::IncludeAndSeal);

        let block_execution_metrics = ExecutionMetrics {
            l2_l1_long_messages: config.max_pubdata_per_batch as usize + 1,
            ..ExecutionMetrics::default()
        };
        let full_block_resolution = criterion.should_seal(
            &config,
            0,
            0,
            &SealData {
                execution_metrics: block_execution_metrics,
                ..SealData::default()
            },
            &SealData::default(),
            ProtocolVersionId::latest(),
        );
        assert_eq!(full_block_resolution, SealResolution::ExcludeAndSeal);
    }
}
