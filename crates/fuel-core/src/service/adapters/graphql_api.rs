use super::{
    BlockImporterAdapter,
    BlockProducerAdapter,
    ChainStateInfoProvider,
    SharedMemoryPool,
    StaticGasPrice,
    TxStatusManagerAdapter,
};
use crate::{
    database::OnChainIterableKeyValueView,
    fuel_core_graphql_api::ports::{
        worker::{
            self,
            BlockAt,
        },
        BlockProducerPort,
        ChainStateProvider,
        DatabaseMessageProof,
        GasPriceEstimate,
        P2pPort,
        TxPoolPort,
    },
    graphql_api::ports::{
        MemoryPool,
        TxStatusManager,
    },
    service::{
        adapters::{
            import_result_provider::ImportResultProvider,
            P2PAdapter,
            TxPoolAdapter,
        },
        vm_pool::MemoryFromPool,
    },
};
use async_trait::async_trait;
use fuel_core_services::stream::BoxStream;
use fuel_core_storage::Result as StorageResult;
use fuel_core_tx_status_manager::TxStatusMessage;
use fuel_core_txpool::TxPoolStats;
use fuel_core_types::{
    blockchain::header::{
        ConsensusParametersVersion,
        StateTransitionBytecodeVersion,
    },
    entities::relayer::message::MerkleProof,
    fuel_tx::{
        Bytes32,
        ConsensusParameters,
        Transaction,
        TxId,
    },
    fuel_types::BlockHeight,
    services::{
        block_importer::SharedImportResult,
        executor::{
            StorageReadReplayEvent,
            TransactionExecutionStatus,
        },
        p2p::PeerInfo,
        txpool::TransactionStatus,
    },
    tai64::Tai64,
};
use std::{
    ops::Deref,
    sync::Arc,
};

mod off_chain;
mod on_chain;

#[async_trait]
impl TxStatusManager for TxStatusManagerAdapter {
    async fn status(&self, tx_id: TxId) -> anyhow::Result<Option<TransactionStatus>> {
        self.tx_status_manager_shared_data.get_status(tx_id).await
    }

    async fn tx_update_subscribe(
        &self,
        tx_id: TxId,
    ) -> anyhow::Result<BoxStream<TxStatusMessage>> {
        self.tx_status_manager_shared_data.subscribe(tx_id).await
    }
}

#[async_trait]
impl TxPoolPort for TxPoolAdapter {
    async fn transaction(&self, id: TxId) -> anyhow::Result<Option<Transaction>> {
        Ok(self
            .service
            .find_one(id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?
            .map(|info| info.tx().clone().deref().into()))
    }

    async fn insert(&self, tx: Transaction) -> anyhow::Result<()> {
        self.service
            .insert(tx)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    fn latest_pool_stats(&self) -> TxPoolStats {
        self.service.latest_stats()
    }
}

impl DatabaseMessageProof for OnChainIterableKeyValueView {
    fn block_history_proof(
        &self,
        message_block_height: &BlockHeight,
        commit_block_height: &BlockHeight,
    ) -> StorageResult<MerkleProof> {
        self.block_history_proof(message_block_height, commit_block_height)
    }
}

#[async_trait]
impl BlockProducerPort for BlockProducerAdapter {
    async fn dry_run_txs(
        &self,
        transactions: Vec<Transaction>,
        height: Option<BlockHeight>,
        time: Option<Tai64>,
        utxo_validation: Option<bool>,
        gas_price: Option<u64>,
    ) -> anyhow::Result<Vec<(Transaction, TransactionExecutionStatus)>> {
        self.block_producer
            .dry_run(transactions, height, time, utxo_validation, gas_price)
            .await
    }

    async fn storage_read_replay(
        &self,
        height: BlockHeight,
    ) -> anyhow::Result<Vec<StorageReadReplayEvent>> {
        self.block_producer.storage_read_replay(height).await
    }
}

#[async_trait::async_trait]
impl P2pPort for P2PAdapter {
    async fn all_peer_info(&self) -> anyhow::Result<Vec<PeerInfo>> {
        #[cfg(feature = "p2p")]
        {
            use fuel_core_types::services::p2p::HeartbeatData;
            if let Some(service) = &self.service {
                let peers = service.get_all_peers().await?;
                Ok(peers
                    .into_iter()
                    .map(|(peer_id, peer_info)| PeerInfo {
                        id: fuel_core_types::services::p2p::PeerId::from(
                            peer_id.to_bytes(),
                        ),
                        peer_addresses: peer_info
                            .peer_addresses
                            .iter()
                            .map(|addr| addr.to_string())
                            .collect(),
                        client_version: None,
                        heartbeat_data: HeartbeatData {
                            block_height: peer_info.heartbeat_data.block_height,
                            last_heartbeat: peer_info.heartbeat_data.last_heartbeat_sys,
                        },
                        app_score: peer_info.score,
                    })
                    .collect())
            } else {
                Ok(vec![])
            }
        }
        #[cfg(not(feature = "p2p"))]
        {
            Ok(vec![])
        }
    }
}

impl worker::TxStatusCompletion for TxStatusManagerAdapter {
    fn send_complete(
        &self,
        id: Bytes32,
        block_height: &BlockHeight,
        status: TransactionStatus,
    ) {
        tracing::info!("Transaction {id} successfully included in block {block_height}");
        self.tx_status_manager_shared_data.update_status(id, status);
    }
}

impl GasPriceEstimate for StaticGasPrice {
    fn worst_case_gas_price(&self, _height: BlockHeight) -> Option<u64> {
        Some(self.gas_price)
    }
}

impl ChainStateProvider for ChainStateInfoProvider {
    fn current_consensus_params(&self) -> Arc<ConsensusParameters> {
        self.shared_state.latest_consensus_parameters()
    }

    fn current_consensus_parameters_version(&self) -> ConsensusParametersVersion {
        self.shared_state.latest_consensus_parameters_version()
    }

    fn consensus_params_at_version(
        &self,
        version: &ConsensusParametersVersion,
    ) -> anyhow::Result<Arc<ConsensusParameters>> {
        Ok(self.shared_state.get_consensus_parameters(version)?)
    }

    fn current_stf_version(&self) -> StateTransitionBytecodeVersion {
        self.shared_state.latest_stf_version()
    }
}

#[derive(Clone)]
pub struct GraphQLBlockImporter {
    block_importer_adapter: BlockImporterAdapter,
    import_result_provider_adapter: ImportResultProvider,
}

impl GraphQLBlockImporter {
    pub fn new(
        block_importer_adapter: BlockImporterAdapter,
        import_result_provider_adapter: ImportResultProvider,
    ) -> Self {
        Self {
            block_importer_adapter,
            import_result_provider_adapter,
        }
    }
}

impl worker::BlockImporter for GraphQLBlockImporter {
    fn block_events(&self) -> BoxStream<SharedImportResult> {
        self.block_importer_adapter.events_shared_result()
    }

    fn block_event_at_height(
        &self,
        height: BlockAt,
    ) -> anyhow::Result<SharedImportResult> {
        self.import_result_provider_adapter.result_at_height(height)
    }
}

#[async_trait::async_trait]
impl MemoryPool for SharedMemoryPool {
    type Memory = MemoryFromPool;

    async fn get_memory(&self) -> Self::Memory {
        self.memory_pool.take_raw().await
    }
}
