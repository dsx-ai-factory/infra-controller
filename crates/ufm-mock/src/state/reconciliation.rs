/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use super::types::{
    DEFAULT_PARTITION_KEY, Fabric, FabricError, FabricState, PortCandidate, PortRecord,
    ReconcileOutcome, SourceState,
};
use crate::config::FailureAction;
use crate::inventory::{Guid, InfinibandPortState, InventoryId, InventoryLease, InventorySnapshot};

impl Fabric {
    pub(crate) fn reconcile(
        &self,
        snapshot: InventorySnapshot,
        lease: InventoryLease,
    ) -> Result<ReconcileOutcome, FabricError> {
        if snapshot.inventory_id.as_ref().is_empty() || snapshot.epoch_id.as_ref().is_empty() {
            return Err(FabricError::InvalidInventoryIdentity);
        }

        let mut advertised = HashMap::new();
        for machine in snapshot.machines {
            for port in machine.infiniband_ports.unwrap_or_default() {
                advertised.insert(
                    port.guid,
                    PortCandidate {
                        mat_id: machine.mat_id.clone(),
                        machine_id: machine.machine_id.clone(),
                        state: port.state,
                    },
                );
            }
        }

        let mut state = self.inner.write().expect("fabric lock poisoned");
        if let Some(previous) = state.sources.get_mut(&snapshot.inventory_id)
            && previous.epoch_id == snapshot.epoch_id
            && !previous.failed
        {
            if previous.generation == snapshot.generation {
                previous.lease = lease;
                return Ok(ReconcileOutcome::Duplicate);
            }
            if previous.generation > snapshot.generation {
                return Ok(ReconcileOutcome::Stale);
            }
        }

        let previous_guids = state
            .sources
            .get(&snapshot.inventory_id)
            .map(|source| source.guids.clone())
            .unwrap_or_default();
        let new_guids = advertised.keys().copied().collect::<BTreeSet<_>>();
        let affected = previous_guids
            .union(&new_guids)
            .copied()
            .collect::<Vec<_>>();

        for guid in &previous_guids {
            if let Some(candidates) = state.candidates.get_mut(guid) {
                candidates.remove(&snapshot.inventory_id);
            }
        }
        for (guid, candidate) in advertised {
            state
                .candidates
                .entry(guid)
                .or_default()
                .insert(snapshot.inventory_id.clone(), candidate);
        }
        state.sources.insert(
            snapshot.inventory_id,
            SourceState {
                epoch_id: snapshot.epoch_id,
                generation: snapshot.generation,
                guids: new_guids,
                failed: false,
                lease,
            },
        );
        refresh_ports(&mut state, affected);
        Ok(ReconcileOutcome::Applied)
    }

    pub(crate) fn source_failed(
        &self,
        inventory_id: &InventoryId,
        lease: InventoryLease,
        action: FailureAction,
    ) {
        let mut state = self.inner.write().expect("fabric lock poisoned");
        let Some(source) = state.sources.get(inventory_id) else {
            return;
        };
        if source.lease != lease || source.failed {
            return;
        }
        let guids = source.guids.iter().copied().collect::<Vec<_>>();
        match action {
            FailureAction::MarkDown => {
                if let Some(source) = state.sources.get_mut(inventory_id) {
                    source.failed = true;
                }
                for guid in &guids {
                    if let Some(candidate) = state
                        .candidates
                        .get_mut(guid)
                        .and_then(|candidates| candidates.get_mut(inventory_id))
                    {
                        candidate.state = InfinibandPortState::Down;
                    }
                }
            }
            FailureAction::Remove => {
                state.sources.remove(inventory_id);
                for guid in &guids {
                    if let Some(candidates) = state.candidates.get_mut(guid) {
                        candidates.remove(inventory_id);
                    }
                }
            }
        }
        refresh_ports(&mut state, guids);
    }
}

fn refresh_ports(state: &mut FabricState, guids: Vec<Guid>) {
    for guid in guids {
        let winner = state
            .candidates
            .get(&guid)
            .and_then(|candidates| candidates.first_key_value())
            .map(|(owner, candidate)| (owner.clone(), candidate.clone()));
        match winner {
            Some((owner, candidate)) => {
                if let Some(port) = state.ports.get_mut(&guid) {
                    port.owner = owner;
                    port.candidate = candidate;
                } else {
                    let lid = state.next_lid;
                    state.next_lid = state
                        .next_lid
                        .checked_add(1)
                        .expect("LID allocation overflow");
                    state.ports.insert(
                        guid,
                        PortRecord {
                            owner,
                            candidate,
                            lid,
                        },
                    );
                }
            }
            None => {
                state.candidates.remove(&guid);
                state.ports.remove(&guid);
                for partition in state.partitions.values_mut() {
                    partition.members.remove(&guid);
                }
                state.partitions.retain(|key, partition| {
                    *key == DEFAULT_PARTITION_KEY || !partition.members.is_empty()
                });
            }
        }
    }

    for (guid, candidates) in &state.candidates {
        if candidates.len() > 1 {
            tracing::warn!(
                %guid,
                inventories = %LogCandidates(candidates),
                winner = %candidates.first_key_value().expect("conflicting candidates must have a winner").0,
                "InfiniBand GUID is advertised by multiple inventories"
            );
        }
    }
}

struct LogCandidates<'a>(&'a BTreeMap<InventoryId, PortCandidate>);

impl fmt::Display for LogCandidates<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[")?;
        for (index, inventory_id) in self.0.keys().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{inventory_id}")?;
        }
        formatter.write_str("]")
    }
}
