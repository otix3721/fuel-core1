use std::ops::Deref;

use fuel_core_types::{
    blockchain::{
        block::CompressedBlock,
        header::{
            ApplicationHeader,
            ConsensusHeader,
            PartialBlockHeader,
        },
    },
    entities::relayer::message::MerkleProof,
    fuel_tx::{
        AssetId,
        ContractId,
    },
    fuel_types::BlockHeight,
    tai64::Tai64,
};

use super::*;

const fn txn_id(i: u8) -> Bytes32 {
    Bytes32::new([i; 32])
}

fn receipt(i: Option<u8>) -> Receipt {
    match i {
        Some(i) => {
            let sender = Address::new([i; 32]);
            let recipient = Address::new([i; 32]);
            let amount = 0;
            let nonce = Nonce::new([i; 32]);
            let data = Some(Vec::new());
            Receipt::MessageOut {
                len: 0,
                digest: Bytes32::new([0; 32]),
                sender,
                recipient,
                amount,
                nonce,
                data,
            }
        }
        None => Receipt::Call {
            id: ContractId::new([0; 32]),
            to: ContractId::new([0; 32]),
            amount: 0,
            asset_id: AssetId::new([0; 32]),
            gas: 0,
            param1: 0,
            param2: 0,
            pc: 0,
            is: 0,
        },
    }
}

mockall::mock! {
    pub ProofDataStorage {}
    impl MessageProofData for ProofDataStorage {
        fn block(&self, height: &BlockHeight) -> StorageResult<CompressedBlock>;
        fn block_history_proof(
            &self,
            message_block_height: &BlockHeight,
            commit_block_height: &BlockHeight,
        ) -> StorageResult<MerkleProof>;
        fn transaction_status(&self, transaction_id: &TxId) -> StorageResult<TransactionExecutionStatus>;
    }
}

#[tokio::test]
async fn can_build_message_proof() {
    use mockall::predicate::*;
    let commit_block_height = BlockHeight::from(2u32);
    let message_block_height = BlockHeight::from(1u32);
    let expected_receipt = receipt(Some(11));
    let nonce = expected_receipt.nonce().unwrap();
    let receipts: [Receipt; 4] = [
        receipt(Some(10)),
        receipt(None),
        receipt(Some(3)),
        expected_receipt.clone(),
    ];
    static TXNS: [Bytes32; 4] = [txn_id(20), txn_id(24), txn_id(1), txn_id(33)];
    let transaction_id = TXNS[3];
    let other_receipts: [Receipt; 3] =
        [receipt(Some(4)), receipt(Some(5)), receipt(Some(6))];

    let message_ids: Vec<MessageId> = other_receipts
        .iter()
        .chain(receipts.iter())
        .filter_map(|r| r.message_id())
        .collect();

    let mut data = MockProofDataStorage::new();
    let mut count = 0;

    let commit_block_header = PartialBlockHeader {
        application: ApplicationHeader {
            da_height: 0u64.into(),
            consensus_parameters_version: Default::default(),
            state_transition_bytecode_version: Default::default(),
            generated: Default::default(),
        },
        consensus: ConsensusHeader {
            prev_root: Bytes32::zeroed(),
            height: commit_block_height,
            time: Tai64::UNIX_EPOCH,
            generated: Default::default(),
        },
    }
    .generate(
        &[],
        &[],
        Default::default(),
        #[cfg(feature = "fault-proving")]
        &Default::default(),
    )
    .unwrap();
    let commit_block = CompressedBlock::test(commit_block_header, vec![]);
    let message_block_header = PartialBlockHeader {
        application: ApplicationHeader {
            da_height: 0u64.into(),
            consensus_parameters_version: Default::default(),
            state_transition_bytecode_version: Default::default(),
            generated: Default::default(),
        },
        consensus: ConsensusHeader {
            prev_root: Bytes32::zeroed(),
            height: message_block_height,
            time: Tai64::UNIX_EPOCH,
            generated: Default::default(),
        },
    }
    .generate(
        &[],
        &message_ids,
        Default::default(),
        #[cfg(feature = "fault-proving")]
        &Default::default(),
    )
    .unwrap();
    let message_block = CompressedBlock::test(message_block_header, TXNS.to_vec());

    let block_proof = MerkleProof {
        proof_set: vec![message_block.id().into(), commit_block.id().into()],
        proof_index: 2,
    };
    data.expect_block_history_proof()
        .once()
        .with(
            eq(message_block_height),
            eq(commit_block_height.pred().expect("Non-zero block height")),
        )
        .returning({
            let block_proof = block_proof.clone();
            move |_, _| Ok(block_proof.clone())
        });

    let message_block_height = *message_block.header().height();
    data.expect_transaction_status().returning(move |tx_id| {
        let receipts = if *tx_id == transaction_id {
            receipts.to_vec()
        } else {
            let r = other_receipts[count..=count].to_vec();
            count += 1;
            r
        };
        Ok(TransactionExecutionStatus::Success {
            block_height: message_block_height,
            time: Tai64::UNIX_EPOCH,
            result: None,
            receipts,
            total_gas: 0,
            total_fee: 0,
        })
    });

    data.expect_block().times(2).returning({
        let commit_block = commit_block.clone();
        let message_block = message_block.clone();
        move |block_height| {
            let block = if commit_block.header().height() == block_height {
                commit_block.clone()
            } else if message_block.header().height() == block_height {
                message_block.clone()
            } else {
                panic!("Shouldn't request any other block")
            };
            Ok(block)
        }
    });

    let data: Box<dyn MessageProofData> = Box::new(data);

    let proof = message_proof(
        data.deref(),
        transaction_id,
        nonce.to_owned(),
        *commit_block.header().height(),
    )
    .unwrap();
    assert_eq!(
        proof.message_block_header.message_outbox_root(),
        message_block.header().message_outbox_root()
    );
    assert_eq!(
        proof.message_block_header.height(),
        message_block.header().height()
    );
    assert_eq!(
        proof.commit_block_header.height(),
        commit_block.header().height()
    );
    assert_eq!(proof.block_proof, block_proof);
}
