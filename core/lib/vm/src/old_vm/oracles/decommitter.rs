use std::collections::HashMap;
use std::fmt::Debug;

use crate::implement_rollback;
use crate::rollback::history_recorder::HistoryRecorder;
use crate::rollback::Rollback;

use zk_evm::abstractions::MemoryType;
use zk_evm::{
    abstractions::{DecommittmentProcessor, Memory},
    aux_structures::{DecommittmentQuery, MemoryIndex, MemoryLocation, MemoryPage, MemoryQuery},
};

use zksync_state::{ReadStorage, StoragePtr};
use zksync_types::U256;
use zksync_utils::bytecode::bytecode_len_in_words;
use zksync_utils::{bytes_to_be_words, u256_to_h256};

/// The main job of the DecommiterOracle is to implement the DecommittmentProcessor trait - that is
/// used by the VM to 'load' bytecodes into memory.
#[derive(Debug)]
pub struct DecommitterOracle<const B: bool, S> {
    /// Pointer that enables to read contract bytecodes from the database.
    storage: StoragePtr<S>,
    /// The cache of bytecodes that the bootloader "knows", but that are not necessarily in the database.
    /// And it is also used as a database cache.
    pub known_bytecodes: HistoryRecorder<HashMap<U256, Vec<U256>>>,
    /// Stores pages of memory where certain code hashes have already been decommitted.
    /// It is expected that they all are present in the DB.
    // `decommitted_code_hashes` history is necessary
    pub decommitted_code_hashes: HistoryRecorder<HashMap<U256, u32>>,
    /// Stores history of decommitment requests.
    decommitment_requests: HistoryRecorder<Vec<()>>,
}

impl<S: ReadStorage, const B: bool> Rollback for DecommitterOracle<B, S> {
    implement_rollback! {known_bytecodes, decommitted_code_hashes, decommitment_requests}
}

impl<S: ReadStorage, const B: bool> DecommitterOracle<B, S> {
    pub fn new(storage: StoragePtr<S>) -> Self {
        Self {
            storage,
            known_bytecodes: HistoryRecorder::default(),
            decommitted_code_hashes: HistoryRecorder::default(),
            decommitment_requests: HistoryRecorder::default(),
        }
    }

    /// Gets the bytecode for a given hash (either from storage, or from 'known_bytecodes' that were populated by `populate` method).
    /// Panics if bytecode doesn't exist.
    pub fn get_bytecode(&mut self, hash: U256) -> Vec<U256> {
        let entry = self.known_bytecodes.inner().get(&hash);

        match entry {
            Some(x) => x.clone(),
            None => {
                // It is ok to panic here, since the decommitter is never called directly by
                // the users and always called by the VM. VM will never let decommit the
                // code hash which we didn't previously claim to know the preimage of.
                let value = self
                    .storage
                    .borrow_mut()
                    .load_factory_dep(u256_to_h256(hash))
                    .expect("Trying to decode unexisting hash");

                let value = bytes_to_be_words(value);
                self.known_bytecodes.insert(hash, value.clone());
                value
            }
        }
    }

    /// Adds additional bytecodes. They will take precendent over the bytecodes from storage.
    pub fn populate(&mut self, bytecodes: Vec<(U256, Vec<U256>)>) {
        for (hash, bytecode) in bytecodes {
            self.known_bytecodes.insert(hash, bytecode);
        }
    }

    pub fn get_used_bytecode_hashes(&self) -> Vec<U256> {
        self.decommitted_code_hashes
            .inner()
            .iter()
            .map(|item| *item.0)
            .collect()
    }

    pub fn get_decommitted_code_hashes_with_history(&self) -> &HistoryRecorder<HashMap<U256, u32>> {
        &self.decommitted_code_hashes
    }

    /// Returns the storage handle. Used only in tests.
    pub fn get_storage(&self) -> StoragePtr<S> {
        self.storage.clone()
    }
}

impl<S: ReadStorage + Debug, const B: bool> DecommittmentProcessor for DecommitterOracle<B, S> {
    /// Loads a given bytecode hash into memory (see trait description for more details).
    fn decommit_into_memory<M: Memory>(
        &mut self,
        monotonic_cycle_counter: u32,
        mut partial_query: DecommittmentQuery,
        memory: &mut M,
    ) -> Result<
        (
            zk_evm::aux_structures::DecommittmentQuery,
            Option<Vec<U256>>,
        ),
        anyhow::Error,
    > {
        self.decommitment_requests.push(());
        // First - check if we didn't fetch this bytecode in the past.
        // If we did - we can just return the page that we used before (as the memory is read only).
        if let Some(memory_page) = self
            .decommitted_code_hashes
            .inner()
            .get(&partial_query.hash)
            .copied()
        {
            partial_query.is_fresh = false;
            partial_query.memory_page = MemoryPage(memory_page);
            partial_query.decommitted_length =
                bytecode_len_in_words(&u256_to_h256(partial_query.hash));

            Ok((partial_query, None))
        } else {
            // We are fetching a fresh bytecode that we didn't read before.
            let values = self.get_bytecode(partial_query.hash);
            let page_to_use = partial_query.memory_page;
            let timestamp = partial_query.timestamp;
            partial_query.decommitted_length = values.len() as u16;
            partial_query.is_fresh = true;

            // Create a template query, that we'll use for writing into memory.
            // value & index are set to 0 - as they will be updated in the inner loop below.
            let mut tmp_q = MemoryQuery {
                timestamp,
                location: MemoryLocation {
                    memory_type: MemoryType::Code,
                    page: page_to_use,
                    index: MemoryIndex(0),
                },
                value: U256::zero(),
                value_is_pointer: false,
                rw_flag: true,
            };
            self.decommitted_code_hashes
                .insert(partial_query.hash, page_to_use.0);

            // Copy the bytecode (that is stored in 'values' Vec) into the memory page.
            if B {
                for (i, value) in values.iter().enumerate() {
                    tmp_q.location.index = MemoryIndex(i as u32);
                    tmp_q.value = *value;
                    memory.specialized_code_query(monotonic_cycle_counter, tmp_q);
                }
                // If we're in the witness mode - we also have to return the values.
                Ok((partial_query, Some(values)))
            } else {
                for (i, value) in values.into_iter().enumerate() {
                    tmp_q.location.index = MemoryIndex(i as u32);
                    tmp_q.value = value;
                    memory.specialized_code_query(monotonic_cycle_counter, tmp_q);
                }

                Ok((partial_query, None))
            }
        }
    }
}
