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

//! One domain's partition table.
//!
//! The rules here are the ones NICo's partition monitor is written against:
//! ids are allocated from 1, a GPU belongs to at most one partition, and the
//! in-memory `NmxcSimClient` in `nvlink-manager` tolerates deleting an id that
//! is not there, so this does too and the two doubles agree.

use std::collections::{BTreeMap, BTreeSet};

use crate::nmx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Partition {
    pub id: u32,
    pub name: String,
    pub gpu_uids: Vec<u64>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PartitionError {
    #[error("partition {0} is not provisioned")]
    IdNotInUse(u32),
    #[error("partition {0:?} is not provisioned")]
    NameNotInUse(String),
    #[error("partition id {0} is already in use")]
    IdInUse(u32),
    #[error("partition name {0:?} is already in use")]
    NameInUse(String),
    #[error("GPU {0:#018x} is not in this NVLink domain")]
    UnknownGpu(u64),
    #[error("GPU {gpu:#018x} already belongs to partition {partition}")]
    GpuInUse { gpu: u64, partition: u32 },
    #[error("neither a partition id nor a name was given")]
    Unidentified,
    #[error("location-based GPU references are not supported; send GPU uids")]
    LocationBased,
}

impl PartitionError {
    /// The `server_header.return_code` a real controller reports for this.
    pub(crate) fn return_code(&self) -> nmx::StReturnCode {
        match self {
            Self::IdNotInUse(_) => nmx::StReturnCode::NmxStPartitionIdNotInUse,
            Self::NameNotInUse(_) => nmx::StReturnCode::NmxStPartitionNameNotInUse,
            Self::IdInUse(_) => nmx::StReturnCode::NmxStPartitionIdInUse,
            Self::NameInUse(_) => nmx::StReturnCode::NmxStPartitionNameInUse,
            Self::GpuInUse { .. } => nmx::StReturnCode::NmxStResourceInUse,
            Self::UnknownGpu(_) | Self::Unidentified => nmx::StReturnCode::NmxStBadparam,
            Self::LocationBased => nmx::StReturnCode::NmxStNotSupported,
        }
    }
}

/// How a request names a partition: NMX-C accepts an id, or a name when the
/// id is left at zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PartitionRef {
    Id(u32),
    Name(String),
}

impl PartitionRef {
    /// A reference by non-zero ID when one is given, else by non-empty name;
    /// neither is `Unidentified`.
    pub(crate) fn new(id: Option<u32>, name: &str) -> Result<Self, PartitionError> {
        match id {
            Some(id) if id != 0 => Ok(Self::Id(id)),
            _ if !name.is_empty() => Ok(Self::Name(name.to_string())),
            _ => Err(PartitionError::Unidentified),
        }
    }
}

pub(crate) struct DomainState {
    gpu_uids: BTreeSet<u64>,
    partitions: BTreeMap<u32, Partition>,
    next_id: u32,
}

impl DomainState {
    /// State for a domain owning `gpu_uids`, optionally with all of them in a
    /// default partition.
    pub(crate) fn new(
        gpu_uids: impl IntoIterator<Item = u64>,
        default_partition: Option<(u32, &str)>,
    ) -> Self {
        let gpu_uids: BTreeSet<u64> = gpu_uids.into_iter().collect();
        let mut partitions = BTreeMap::new();
        if let Some((id, name)) = default_partition {
            partitions.insert(
                id,
                Partition {
                    id,
                    name: name.to_string(),
                    gpu_uids: gpu_uids.iter().copied().collect(),
                },
            );
        }
        Self {
            gpu_uids,
            partitions,
            next_id: 1,
        }
    }

    /// All partitions in ID order.
    pub(crate) fn partitions(&self) -> impl Iterator<Item = &Partition> {
        self.partitions.values()
    }

    /// Partitions matching any of the ids or names; every partition when
    /// both filters are empty, as the proto specifies.
    pub(crate) fn find(&self, ids: &[u32], names: &[String]) -> Vec<&Partition> {
        if ids.is_empty() && names.is_empty() {
            return self.partitions().collect();
        }
        self.partitions()
            .filter(|partition| ids.contains(&partition.id) || names.contains(&partition.name))
            .collect()
    }

    /// The partition holding `gpu_uid`, if any.
    pub(crate) fn partition_of(&self, gpu_uid: u64) -> Option<u32> {
        self.partitions()
            .find(|partition| partition.gpu_uids.contains(&gpu_uid))
            .map(|partition| partition.id)
    }

    /// Creates a partition holding `gpu_uids`, allocating the next free ID when
    /// none is requested.
    pub(crate) fn create(
        &mut self,
        name: &str,
        gpu_uids: &[u64],
        requested_id: Option<u32>,
    ) -> Result<u32, PartitionError> {
        if !name.is_empty() && self.partitions().any(|partition| partition.name == name) {
            return Err(PartitionError::NameInUse(name.to_string()));
        }
        if let Some(id) = requested_id.filter(|id| *id != 0)
            && self.partitions.contains_key(&id)
        {
            return Err(PartitionError::IdInUse(id));
        }
        let gpu_uids = self.claimable(gpu_uids, None)?;
        let id = match requested_id {
            Some(id) if id != 0 => id,
            _ => self.allocate_id(),
        };
        let name = if name.is_empty() {
            // A generated name can collide with one a caller chose earlier,
            // and names are a lookup key, so it is checked like any other.
            let generated = format!("partition-{id}");
            if self
                .partitions()
                .any(|partition| partition.name == generated)
            {
                return Err(PartitionError::NameInUse(generated));
            }
            generated
        } else {
            name.to_string()
        };
        self.partitions.insert(id, Partition { id, name, gpu_uids });
        Ok(id)
    }

    /// Deleting an id that is not provisioned succeeds, as it does in the
    /// in-memory double; deleting by an unknown name does not, since a name
    /// is the only handle the caller has in that case.
    pub(crate) fn delete(&mut self, target: PartitionRef) -> Result<u32, PartitionError> {
        match target {
            PartitionRef::Id(id) => {
                self.partitions.remove(&id);
                Ok(id)
            }
            PartitionRef::Name(name) => {
                let id = self.id_by_name(&name)?;
                self.partitions.remove(&id);
                Ok(id)
            }
        }
    }

    /// Adds GPUs to the target partition and returns its ID.
    pub(crate) fn add_gpus(
        &mut self,
        target: PartitionRef,
        gpu_uids: &[u64],
    ) -> Result<u32, PartitionError> {
        let id = self.resolve(target)?;
        let added = self.claimable(gpu_uids, Some(id))?;
        let partition = self
            .partitions
            .get_mut(&id)
            .expect("resolved partitions exist");
        let new_members: Vec<u64> = added
            .into_iter()
            .filter(|uid| !partition.gpu_uids.contains(uid))
            .collect();
        partition.gpu_uids.extend(new_members);
        Ok(id)
    }

    /// Removing a GPU that is not in the partition is not an error, as in the
    /// in-memory double.
    pub(crate) fn remove_gpus(
        &mut self,
        target: PartitionRef,
        gpu_uids: &[u64],
    ) -> Result<u32, PartitionError> {
        let id = self.resolve(target)?;
        let partition = self
            .partitions
            .get_mut(&id)
            .expect("resolved partitions exist");
        partition.gpu_uids.retain(|uid| !gpu_uids.contains(uid));
        Ok(id)
    }

    /// The ID of the partition `target` refers to.
    fn resolve(&self, target: PartitionRef) -> Result<u32, PartitionError> {
        match target {
            PartitionRef::Id(id) => self
                .partitions
                .contains_key(&id)
                .then_some(id)
                .ok_or(PartitionError::IdNotInUse(id)),
            PartitionRef::Name(name) => self.id_by_name(&name),
        }
    }

    /// The ID of the partition named `name`.
    fn id_by_name(&self, name: &str) -> Result<u32, PartitionError> {
        self.partitions()
            .find(|partition| partition.name == name)
            .map(|partition| partition.id)
            .ok_or_else(|| PartitionError::NameNotInUse(name.to_string()))
    }

    /// The lowest unused ID at or after the allocation cursor, moving the cursor
    /// past it.
    fn allocate_id(&mut self) -> u32 {
        while self.partitions.contains_key(&self.next_id) {
            self.next_id += 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The uids, de-duplicated, once each is known to the domain and free, or
    /// already in partition `member_of`.
    fn claimable(
        &self,
        gpu_uids: &[u64],
        member_of: Option<u32>,
    ) -> Result<Vec<u64>, PartitionError> {
        let mut claimed = Vec::with_capacity(gpu_uids.len());
        for &uid in gpu_uids {
            if !self.gpu_uids.contains(&uid) {
                return Err(PartitionError::UnknownGpu(uid));
            }
            match self.partition_of(uid) {
                Some(partition) if Some(partition) != member_of => {
                    return Err(PartitionError::GpuInUse {
                        gpu: uid,
                        partition,
                    });
                }
                _ => {}
            }
            if !claimed.contains(&uid) {
                claimed.push(uid);
            }
        }
        Ok(claimed)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    const GPUS: [u64; 4] = [0x10, 0x11, 0x12, 0x13];

    /// A domain whose factory partition has been removed, so its GPUs are free.
    fn empty_domain() -> DomainState {
        DomainState::new(GPUS, None)
    }

    /// Creation allocates free IDs and rejects conflicting names, IDs and GPUs.
    #[test]
    fn create_validates_its_request() {
        scenarios!(run = |(name, uids, requested): (&str, Vec<u64>, Option<u32>)| {
            let mut domain = empty_domain();
            domain.create("taken", &[0x13], Some(7)).unwrap();
            domain.create("partition-2", &[0x12], Some(9)).unwrap();
            domain.create(name, &uids, requested)
        };
            "ids are allocated from 1, skipping ids in use" {
                ("p", vec![0x10], None) => Yields(1),
                ("p", vec![0x10], Some(0)) => Yields(1),
                ("p", vec![0x10, 0x10], None) => Yields(1),
                ("", vec![], None) => Yields(1),
            }

            "a requested id is honoured when free" {
                ("p", vec![0x10], Some(42)) => Yields(42),
                ("p", vec![0x10], Some(7)) => FailsWith(PartitionError::IdInUse(7)),
            }

            "names are unique, including generated ones" {
                ("taken", vec![0x10], None) => FailsWith(PartitionError::NameInUse("taken".into())),
                ("", vec![0x10], Some(2)) => FailsWith(PartitionError::NameInUse("partition-2".into())),
            }

            "GPUs must be in the domain and free" {
                ("p", vec![0x99], None) => FailsWith(PartitionError::UnknownGpu(0x99)),
                ("p", vec![0x13], None) => FailsWith(PartitionError::GpuInUse { gpu: 0x13, partition: 7 }),
            }
        );
    }

    /// Adding and removing GPUs require an existing target and GPUs the domain owns.
    #[test]
    fn membership_changes_validate_their_target_and_gpus() {
        scenarios!(run = |(op, target, uids): (&str, PartitionRef, Vec<u64>)| {
            let mut domain = empty_domain();
            domain.create("a", &[0x10], Some(1)).unwrap();
            domain.create("b", &[0x11], Some(2)).unwrap();
            let result = match op {
                "add" => domain.add_gpus(target, &uids),
                _ => domain.remove_gpus(target, &uids),
            };
            result.map(|id| {
                let partition = domain.find(&[id], &[]).remove(0);
                (id, partition.gpu_uids.clone())
            })
        };
            "add by id or name, de-duplicating and keeping members" {
                ("add", PartitionRef::Id(1), vec![0x12, 0x12, 0x10]) => Yields((1, vec![0x10, 0x12])),
                ("add", PartitionRef::Name("b".into()), vec![0x13]) => Yields((2, vec![0x11, 0x13])),
            }

            "add rejects GPUs held elsewhere or unknown, and unknown targets" {
                ("add", PartitionRef::Id(1), vec![0x11]) => FailsWith(PartitionError::GpuInUse { gpu: 0x11, partition: 2 }),
                ("add", PartitionRef::Id(1), vec![0x99]) => FailsWith(PartitionError::UnknownGpu(0x99)),
                ("add", PartitionRef::Id(9), vec![0x12]) => FailsWith(PartitionError::IdNotInUse(9)),
                ("add", PartitionRef::Name("zz".into()), vec![0x12]) => FailsWith(PartitionError::NameNotInUse("zz".into())),
            }

            "remove tolerates GPUs that are not members" {
                ("remove", PartitionRef::Id(1), vec![0x10, 0x11]) => Yields((1, vec![])),
                ("remove", PartitionRef::Id(2), vec![0x99]) => Yields((2, vec![0x11])),
                ("remove", PartitionRef::Id(9), vec![]) => FailsWith(PartitionError::IdNotInUse(9)),
            }
        );
    }

    /// A non-zero ID identifies the partition even when a name is also given.
    #[test]
    fn partition_ref_prefers_a_nonzero_id() {
        scenarios!(run = |(id, name): (Option<u32>, &str)| PartitionRef::new(id, name);
            "id wins" {
                (Some(3), "n") => Yields(PartitionRef::Id(3)),
                (Some(3), "") => Yields(PartitionRef::Id(3)),
            }
            "zero or absent id falls back to the name" {
                (Some(0), "n") => Yields(PartitionRef::Name("n".into())),
                (None, "n") => Yields(PartitionRef::Name("n".into())),
            }
            "nothing identifies nothing" {
                (Some(0), "") => FailsWith(PartitionError::Unidentified),
                (None, "") => FailsWith(PartitionError::Unidentified),
            }
        );
    }

    /// The lifecycle NICo drives on a fresh controller: the factory partition
    /// holds every GPU until it is deleted, after which GPUs can be provisioned.
    #[test]
    fn factory_partition_holds_every_gpu_until_deleted() {
        let mut domain = DomainState::new(GPUS, Some((32766, "Default")));
        assert_eq!(
            domain.create("p", &[0x10], None),
            Err(PartitionError::GpuInUse {
                gpu: 0x10,
                partition: 32766
            })
        );
        assert_eq!(domain.partition_of(0x12), Some(32766));

        assert_eq!(domain.delete(PartitionRef::Id(32766)), Ok(32766));
        assert_eq!(domain.delete(PartitionRef::Id(32766)), Ok(32766));
        assert_eq!(
            domain.delete(PartitionRef::Name("Default".into())),
            Err(PartitionError::NameNotInUse("Default".into()))
        );

        assert_eq!(domain.create("p", &[0x10], None), Ok(1));
        assert_eq!(domain.partition_of(0x10), Some(1));
        assert_eq!(domain.partition_of(0x11), None);
        assert_eq!(domain.find(&[], &[]).len(), 1);
        assert_eq!(domain.find(&[1], &[]).len(), 1);
        assert_eq!(domain.find(&[], &["p".into()]).len(), 1);
        assert_eq!(domain.find(&[2], &[]).len(), 0);
    }
}
