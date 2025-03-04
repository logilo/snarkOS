// Copyright (C) 2019-2023 Aleo Systems Inc.
// This file is part of the snarkOS library.

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at:
// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::MAX_FETCH_TIMEOUT_IN_MS;

use parking_lot::{Mutex, RwLock};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    net::SocketAddr,
};
use time::OffsetDateTime;
use tokio::sync::oneshot;

#[cfg(not(test))]
pub const NUM_REDUNDANT_REQUESTS: usize = 2;
#[cfg(test)]
pub const NUM_REDUNDANT_REQUESTS: usize = 10;

const CALLBACK_TIMEOUT_IN_SECS: i64 = MAX_FETCH_TIMEOUT_IN_MS as i64 / 1000;

#[derive(Debug)]
pub struct Pending<T: PartialEq + Eq + Hash, V: Clone> {
    /// The map of pending `items` to `peer IPs` that have the item.
    pending: RwLock<HashMap<T, HashSet<SocketAddr>>>,
    /// The optional callback queue.
    callbacks: Mutex<HashMap<T, Vec<(oneshot::Sender<V>, i64)>>>,
}

impl<T: Copy + Clone + PartialEq + Eq + Hash, V: Clone> Default for Pending<T, V> {
    /// Initializes a new instance of the pending queue.
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Clone + PartialEq + Eq + Hash, V: Clone> Pending<T, V> {
    /// Initializes a new instance of the pending queue.
    pub fn new() -> Self {
        Self { pending: Default::default(), callbacks: Default::default() }
    }

    /// Returns `true` if the pending queue is empty.
    pub fn is_empty(&self) -> bool {
        self.pending.read().is_empty()
    }

    /// Returns the number of pending in the pending queue.
    pub fn len(&self) -> usize {
        self.pending.read().len()
    }

    /// Returns `true` if the pending queue contains the specified `item`.
    pub fn contains(&self, item: impl Into<T>) -> bool {
        self.pending.read().contains_key(&item.into())
    }

    /// Returns `true` if the pending queue contains the specified `item` for the specified `peer IP`.
    pub fn contains_peer(&self, item: impl Into<T>, peer_ip: SocketAddr) -> bool {
        self.pending.read().get(&item.into()).map_or(false, |peer_ips| peer_ips.contains(&peer_ip))
    }

    /// Returns the peer IPs for the specified `item`.
    pub fn get(&self, item: impl Into<T>) -> Option<HashSet<SocketAddr>> {
        self.pending.read().get(&item.into()).cloned()
    }

    /// Returns the number of pending callbacks for the specified `item`.
    pub fn num_callbacks(&self, item: impl Into<T>) -> usize {
        let item = item.into();
        // Clear the callbacks that have expired.
        self.clear_expired_callbacks_for_item(item);
        // Return the number of live callbacks.
        self.callbacks.lock().get(&item).map_or(0, |callbacks| callbacks.len())
    }

    /// Inserts the specified `item` and `peer IP` to the pending queue,
    /// returning `true` if the `peer IP` was newly-inserted into the entry for the `item`.
    ///
    /// In addition, an optional `callback` may be provided, that is triggered upon removal.
    /// Note: The callback, if provided, is **always** inserted into the callback queue.
    pub fn insert(&self, item: impl Into<T>, peer_ip: SocketAddr, callback: Option<oneshot::Sender<V>>) -> bool {
        let item = item.into();
        // Insert the peer IP into the pending queue.
        let result = self.pending.write().entry(item).or_default().insert(peer_ip);

        // Clear the callbacks that have expired.
        self.clear_expired_callbacks_for_item(item);

        // If a callback is provided, insert it into the callback queue.
        if let Some(callback) = callback {
            self.callbacks.lock().entry(item).or_default().push((callback, OffsetDateTime::now_utc().unix_timestamp()));
        }
        // Return the result.
        result
    }

    /// Removes the specified `item` from the pending queue.
    /// If the `item` exists and is removed, the peer IPs are returned.
    /// If the `item` does not exist, `None` is returned.
    pub fn remove(&self, item: impl Into<T>, callback_value: Option<V>) -> Option<HashSet<SocketAddr>> {
        let item = item.into();
        // Remove the item from the pending queue.
        let result = self.pending.write().remove(&item);
        // Remove the callback for the item, and process any remaining callbacks.
        if let Some(callbacks) = self.callbacks.lock().remove(&item) {
            if let Some(callback_value) = callback_value {
                // Send a notification to the callback.
                for (callback, _) in callbacks {
                    callback.send(callback_value.clone()).ok();
                }
            }
        }
        // Return the result.
        result
    }

    /// Removes the callbacks for the specified `item` that have expired.
    pub fn clear_expired_callbacks_for_item(&self, item: impl Into<T>) {
        // Clear the callbacks that have expired.
        if let Some(callbacks) = self.callbacks.lock().get_mut(&item.into()) {
            // Fetch the current timestamp.
            let now = OffsetDateTime::now_utc().unix_timestamp();
            // Remove the callbacks that have expired.
            callbacks.retain(|(_, timestamp)| now - *timestamp <= CALLBACK_TIMEOUT_IN_SECS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snarkvm::{
        ledger::{coinbase::PuzzleCommitment, narwhal::TransmissionID},
        prelude::{Rng, TestRng},
    };

    use std::{thread, time::Duration};

    type CurrentNetwork = snarkvm::prelude::MainnetV0;

    #[test]
    fn test_pending() {
        let rng = &mut TestRng::default();

        // Initialize the ready queue.
        let pending = Pending::<TransmissionID<CurrentNetwork>, ()>::new();

        // Check initially empty.
        assert!(pending.is_empty());
        assert_eq!(pending.len(), 0);

        // Initialize the commitments.
        let commitment_1 = TransmissionID::Solution(PuzzleCommitment::from_g1_affine(rng.gen()));
        let commitment_2 = TransmissionID::Solution(PuzzleCommitment::from_g1_affine(rng.gen()));
        let commitment_3 = TransmissionID::Solution(PuzzleCommitment::from_g1_affine(rng.gen()));

        // Initialize the SocketAddrs.
        let addr_1 = SocketAddr::from(([127, 0, 0, 1], 1234));
        let addr_2 = SocketAddr::from(([127, 0, 0, 1], 2345));
        let addr_3 = SocketAddr::from(([127, 0, 0, 1], 3456));

        // Insert the commitments.
        assert!(pending.insert(commitment_1, addr_1, None));
        assert!(pending.insert(commitment_2, addr_2, None));
        assert!(pending.insert(commitment_3, addr_3, None));

        // Check the number of SocketAddrs.
        assert_eq!(pending.len(), 3);
        assert!(!pending.is_empty());

        // Check the items.
        let ids = [commitment_1, commitment_2, commitment_3];
        let peers = [addr_1, addr_2, addr_3];

        for i in 0..3 {
            let id = ids[i];
            assert!(pending.contains(id));
            assert!(pending.contains_peer(id, peers[i]));
        }
        let unknown_id = TransmissionID::Solution(PuzzleCommitment::from_g1_affine(rng.gen()));
        assert!(!pending.contains(unknown_id));

        // Check get.
        assert_eq!(pending.get(commitment_1), Some(HashSet::from([addr_1])));
        assert_eq!(pending.get(commitment_2), Some(HashSet::from([addr_2])));
        assert_eq!(pending.get(commitment_3), Some(HashSet::from([addr_3])));
        assert_eq!(pending.get(unknown_id), None);

        // Check remove.
        assert!(pending.remove(commitment_1, None).is_some());
        assert!(pending.remove(commitment_2, None).is_some());
        assert!(pending.remove(commitment_3, None).is_some());
        assert!(pending.remove(unknown_id, None).is_none());

        // Check empty again.
        assert!(pending.is_empty());
    }

    #[test]
    fn test_expired_callbacks() {
        let rng = &mut TestRng::default();

        // Initialize the ready queue.
        let pending = Pending::<TransmissionID<CurrentNetwork>, ()>::new();

        // Check initially empty.
        assert!(pending.is_empty());
        assert_eq!(pending.len(), 0);

        // Initialize the commitments.
        let commitment_1 = TransmissionID::Solution(PuzzleCommitment::from_g1_affine(rng.gen()));

        // Initialize the SocketAddrs.
        let addr_1 = SocketAddr::from(([127, 0, 0, 1], 1234));
        let addr_2 = SocketAddr::from(([127, 0, 0, 1], 2345));
        let addr_3 = SocketAddr::from(([127, 0, 0, 1], 3456));

        // Initialize the callbacks.
        let (callback_sender_1, _) = oneshot::channel();
        let (callback_sender_2, _) = oneshot::channel();
        let (callback_sender_3, _) = oneshot::channel();

        // Insert the commitments.
        assert!(pending.insert(commitment_1, addr_1, Some(callback_sender_1)));
        assert!(pending.insert(commitment_1, addr_2, Some(callback_sender_2)));

        // Sleep for a few seconds.
        thread::sleep(Duration::from_secs(CALLBACK_TIMEOUT_IN_SECS as u64 - 1));

        assert!(pending.insert(commitment_1, addr_3, Some(callback_sender_3)));

        // Check that the number of callbacks has not changed.
        assert_eq!(pending.num_callbacks(commitment_1), 3);

        // Wait for 2 seconds.
        thread::sleep(Duration::from_secs(2));

        // Ensure that the expired callbacks have been removed.
        assert_eq!(pending.num_callbacks(commitment_1), 1);

        // Wait for `CALLBACK_TIMEOUT_IN_SECS` seconds.
        thread::sleep(Duration::from_secs(CALLBACK_TIMEOUT_IN_SECS as u64));

        // Ensure that the expired callbacks have been removed.
        assert_eq!(pending.num_callbacks(commitment_1), 0);
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;

    use test_strategy::{proptest, Arbitrary};

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub struct Item {
        pub id: usize,
    }

    #[derive(Arbitrary, Clone, Debug)]
    pub struct PendingInput {
        #[strategy(1..5_000usize)]
        pub count: usize,
    }

    impl PendingInput {
        pub fn to_pending(&self) -> Pending<Item, ()> {
            let pending = Pending::<Item, ()>::new();
            for i in 0..self.count {
                pending.insert(Item { id: i }, SocketAddr::from(([127, 0, 0, 1], i as u16)), None);
            }
            pending
        }
    }

    #[proptest]
    fn test_pending_proptest(input: PendingInput) {
        let pending = input.to_pending();
        assert_eq!(pending.len(), input.count);
        assert!(!pending.is_empty());
        assert!(!pending.contains(Item { id: input.count + 1 }));
        assert_eq!(pending.get(Item { id: input.count + 1 }), None);
        assert!(pending.remove(Item { id: input.count + 1 }, None).is_none());
        for i in 0..input.count {
            assert!(pending.contains(Item { id: i }));
            let peer_ip = SocketAddr::from(([127, 0, 0, 1], i as u16));
            assert!(pending.contains_peer(Item { id: i }, peer_ip));
            assert_eq!(pending.get(Item { id: i }), Some(HashSet::from([peer_ip])));
            assert!(pending.remove(Item { id: i }, None).is_some());
        }
        assert!(pending.is_empty());
    }
}
