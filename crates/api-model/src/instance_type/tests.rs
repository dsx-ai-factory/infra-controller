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

use carbide_test_support::value_scenarios;

use super::*;

fn empty_capability_set() -> MachineCapabilitiesSet {
    MachineCapabilitiesSet {
        cpu: vec![],
        gpu: vec![],
        memory: vec![],
        storage: vec![],
        network: vec![],
        infiniband: vec![],
        dpu: vec![],
    }
}

fn instance_type_with(
    desired_capabilities: Vec<InstanceTypeMachineCapabilityFilter>,
) -> InstanceType {
    InstanceType {
        id: "test_id".parse().unwrap(),
        deleted: None,
        created: "2023-01-01 00:00:00 UTC".parse().unwrap(),
        version: ConfigVersion::initial(),
        metadata: Metadata {
            name: "fancy name".to_string(),
            description: String::new(),
            labels: HashMap::new(),
        },
        desired_capabilities,
    }
}

enum FieldMatch {
    Cpu(
        InstanceTypeMachineCapabilityFilter,
        machine_caps::MachineCapabilityCpu,
    ),
    Gpu(
        InstanceTypeMachineCapabilityFilter,
        machine_caps::MachineCapabilityGpu,
    ),
    Memory(
        InstanceTypeMachineCapabilityFilter,
        machine_caps::MachineCapabilityMemory,
    ),
    Storage(
        InstanceTypeMachineCapabilityFilter,
        machine_caps::MachineCapabilityStorage,
    ),
    Network(
        InstanceTypeMachineCapabilityFilter,
        machine_caps::MachineCapabilityNetwork,
    ),
    Infiniband(
        InstanceTypeMachineCapabilityFilter,
        machine_caps::MachineCapabilityInfiniband,
    ),
    Dpu(
        InstanceTypeMachineCapabilityFilter,
        machine_caps::MachineCapabilityDpu,
    ),
}

impl FieldMatch {
    fn matches(self) -> bool {
        match self {
            Self::Cpu(filter, capability) => filter.matches_machine_cpu_capability(&capability),
            Self::Gpu(filter, capability) => filter.matches_machine_gpu_capability(&capability),
            Self::Memory(filter, capability) => {
                filter.matches_machine_memory_capability(&capability)
            }
            Self::Storage(filter, capability) => {
                filter.matches_machine_storage_capability(&capability)
            }
            Self::Network(filter, capability) => {
                filter.matches_machine_network_capability(&capability)
            }
            Self::Infiniband(filter, capability) => {
                filter.matches_machine_infiniband_capability(&capability)
            }
            Self::Dpu(filter, capability) => filter.matches_machine_dpu_capability(&capability),
        }
    }
}

fn filter_for(capability_type: MachineCapabilityType) -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type,
        ..Default::default()
    }
}

fn cpu_filter() -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: MachineCapabilityType::Cpu,
        name: Some("cpu".to_string()),
        frequency: Some("ignored frequency".to_string()),
        capacity: Some("ignored capacity".to_string()),
        vendor: Some("intel".to_string()),
        hardware_revision: Some("ignored revision".to_string()),
        cores: Some(8),
        threads: Some(16),
        inactive_devices: Some(vec![1]),
        device_type: Some(MachineCapabilityDeviceType::Unknown),
        ..Default::default()
    }
}

fn cpu_capability() -> machine_caps::MachineCapabilityCpu {
    machine_caps::MachineCapabilityCpu {
        name: "cpu".to_string(),
        count: 1,
        vendor: Some("intel".to_string()),
        cores: Some(8),
        threads: Some(16),
    }
}

fn gpu_filter() -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: MachineCapabilityType::Gpu,
        name: Some("gpu".to_string()),
        frequency: Some("1.5 GHz".to_string()),
        capacity: Some("80 GB".to_string()),
        vendor: Some("nvidia".to_string()),
        cores: Some(144),
        threads: Some(288),
        ..Default::default()
    }
}

fn gpu_capability() -> machine_caps::MachineCapabilityGpu {
    machine_caps::MachineCapabilityGpu {
        name: "gpu".to_string(),
        count: 1,
        vendor: Some("nvidia".to_string()),
        frequency: Some("1.5 GHz".to_string()),
        memory_capacity: Some("80 GB".to_string()),
        cores: Some(144),
        threads: Some(288),
        device_type: Some(MachineCapabilityDeviceType::Unknown),
    }
}

fn memory_filter() -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: MachineCapabilityType::Memory,
        name: Some("memory".to_string()),
        capacity: Some("256 GB".to_string()),
        vendor: Some("micron".to_string()),
        ..Default::default()
    }
}

fn memory_capability() -> machine_caps::MachineCapabilityMemory {
    machine_caps::MachineCapabilityMemory {
        name: "memory".to_string(),
        count: 1,
        vendor: Some("micron".to_string()),
        capacity: Some("256 GB".to_string()),
    }
}

fn storage_filter() -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: MachineCapabilityType::Storage,
        name: Some("storage".to_string()),
        capacity: Some("2 TB".to_string()),
        vendor: Some("solidigm".to_string()),
        ..Default::default()
    }
}

fn storage_capability() -> machine_caps::MachineCapabilityStorage {
    machine_caps::MachineCapabilityStorage {
        name: "storage".to_string(),
        count: 1,
        vendor: Some("solidigm".to_string()),
        capacity: Some("2 TB".to_string()),
    }
}

fn network_filter() -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: MachineCapabilityType::Network,
        name: Some("network".to_string()),
        vendor: Some("nvidia".to_string()),
        device_type: Some(MachineCapabilityDeviceType::Dpu),
        ..Default::default()
    }
}

fn network_capability() -> machine_caps::MachineCapabilityNetwork {
    machine_caps::MachineCapabilityNetwork {
        name: "network".to_string(),
        count: 1,
        vendor: Some("nvidia".to_string()),
        device_type: Some(MachineCapabilityDeviceType::Dpu),
    }
}

fn infiniband_filter() -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: MachineCapabilityType::Infiniband,
        name: Some("infiniband".to_string()),
        vendor: Some("nvidia".to_string()),
        inactive_devices: Some(vec![1, 3]),
        ..Default::default()
    }
}

fn infiniband_capability() -> machine_caps::MachineCapabilityInfiniband {
    machine_caps::MachineCapabilityInfiniband {
        name: "infiniband".to_string(),
        count: 1,
        vendor: "nvidia".to_string(),
        inactive_devices: vec![1, 3],
    }
}

fn dpu_filter() -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: MachineCapabilityType::Dpu,
        name: Some("dpu".to_string()),
        hardware_revision: Some("A1".to_string()),
        ..Default::default()
    }
}

fn dpu_capability() -> machine_caps::MachineCapabilityDpu {
    machine_caps::MachineCapabilityDpu {
        name: "dpu".to_string(),
        count: 1,
        hardware_revision: Some("A1".to_string()),
    }
}

#[test]
fn capability_field_filters_match() {
    value_scenarios!(
        run = FieldMatch::matches;
        "CPU constraints" {
            FieldMatch::Cpu(filter_for(MachineCapabilityType::Cpu), cpu_capability()) => true,
            FieldMatch::Cpu(cpu_filter(), cpu_capability()) => true,
            FieldMatch::Cpu(
                InstanceTypeMachineCapabilityFilter {
                    name: Some("other".to_string()),
                    ..cpu_filter()
                },
                cpu_capability(),
            ) => false,
            FieldMatch::Cpu(
                cpu_filter(),
                machine_caps::MachineCapabilityCpu {
                    cores: None,
                    ..cpu_capability()
                },
            ) => false,
            FieldMatch::Cpu(
                InstanceTypeMachineCapabilityFilter {
                    cores: Some(9),
                    ..cpu_filter()
                },
                cpu_capability(),
            ) => false,
            FieldMatch::Cpu(
                cpu_filter(),
                machine_caps::MachineCapabilityCpu {
                    threads: None,
                    ..cpu_capability()
                },
            ) => false,
            FieldMatch::Cpu(
                InstanceTypeMachineCapabilityFilter {
                    threads: Some(17),
                    ..cpu_filter()
                },
                cpu_capability(),
            ) => false,
            FieldMatch::Cpu(
                cpu_filter(),
                machine_caps::MachineCapabilityCpu {
                    vendor: None,
                    ..cpu_capability()
                },
            ) => false,
            FieldMatch::Cpu(
                InstanceTypeMachineCapabilityFilter {
                    vendor: Some("other".to_string()),
                    ..cpu_filter()
                },
                cpu_capability(),
            ) => false,
        }
    );

    value_scenarios!(
        run = FieldMatch::matches;
        "GPU constraints" {
            FieldMatch::Gpu(filter_for(MachineCapabilityType::Gpu), gpu_capability()) => true,
            FieldMatch::Gpu(gpu_filter(), gpu_capability()) => true,
            FieldMatch::Gpu(
                InstanceTypeMachineCapabilityFilter {
                    name: Some("other".to_string()),
                    ..gpu_filter()
                },
                gpu_capability(),
            ) => false,
            FieldMatch::Gpu(
                gpu_filter(),
                machine_caps::MachineCapabilityGpu {
                    cores: None,
                    ..gpu_capability()
                },
            ) => false,
            FieldMatch::Gpu(
                InstanceTypeMachineCapabilityFilter {
                    cores: Some(145),
                    ..gpu_filter()
                },
                gpu_capability(),
            ) => false,
            FieldMatch::Gpu(
                gpu_filter(),
                machine_caps::MachineCapabilityGpu {
                    threads: None,
                    ..gpu_capability()
                },
            ) => false,
            FieldMatch::Gpu(
                InstanceTypeMachineCapabilityFilter {
                    threads: Some(289),
                    ..gpu_filter()
                },
                gpu_capability(),
            ) => false,
            FieldMatch::Gpu(
                gpu_filter(),
                machine_caps::MachineCapabilityGpu {
                    vendor: None,
                    ..gpu_capability()
                },
            ) => false,
            FieldMatch::Gpu(
                InstanceTypeMachineCapabilityFilter {
                    vendor: Some("other".to_string()),
                    ..gpu_filter()
                },
                gpu_capability(),
            ) => false,
            FieldMatch::Gpu(
                gpu_filter(),
                machine_caps::MachineCapabilityGpu {
                    frequency: None,
                    ..gpu_capability()
                },
            ) => false,
            FieldMatch::Gpu(
                InstanceTypeMachineCapabilityFilter {
                    frequency: Some("1.6 GHz".to_string()),
                    ..gpu_filter()
                },
                gpu_capability(),
            ) => false,
            FieldMatch::Gpu(
                gpu_filter(),
                machine_caps::MachineCapabilityGpu {
                    memory_capacity: None,
                    ..gpu_capability()
                },
            ) => false,
            FieldMatch::Gpu(
                InstanceTypeMachineCapabilityFilter {
                    capacity: Some("96 GB".to_string()),
                    ..gpu_filter()
                },
                gpu_capability(),
            ) => false,
        }
    );

    value_scenarios!(
        run = FieldMatch::matches;
        "memory constraints" {
            FieldMatch::Memory(
                filter_for(MachineCapabilityType::Memory),
                memory_capability(),
            ) => true,
            FieldMatch::Memory(memory_filter(), memory_capability()) => true,
            FieldMatch::Memory(
                InstanceTypeMachineCapabilityFilter {
                    name: Some("other".to_string()),
                    ..memory_filter()
                },
                memory_capability(),
            ) => false,
            FieldMatch::Memory(
                memory_filter(),
                machine_caps::MachineCapabilityMemory {
                    vendor: None,
                    ..memory_capability()
                },
            ) => false,
            FieldMatch::Memory(
                InstanceTypeMachineCapabilityFilter {
                    vendor: Some("other".to_string()),
                    ..memory_filter()
                },
                memory_capability(),
            ) => false,
            FieldMatch::Memory(
                memory_filter(),
                machine_caps::MachineCapabilityMemory {
                    capacity: None,
                    ..memory_capability()
                },
            ) => false,
            FieldMatch::Memory(
                InstanceTypeMachineCapabilityFilter {
                    capacity: Some("512 GB".to_string()),
                    ..memory_filter()
                },
                memory_capability(),
            ) => false,
        }
    );

    value_scenarios!(
        run = FieldMatch::matches;
        "storage constraints" {
            FieldMatch::Storage(
                filter_for(MachineCapabilityType::Storage),
                storage_capability(),
            ) => true,
            FieldMatch::Storage(storage_filter(), storage_capability()) => true,
            FieldMatch::Storage(
                InstanceTypeMachineCapabilityFilter {
                    name: Some("other".to_string()),
                    ..storage_filter()
                },
                storage_capability(),
            ) => false,
            FieldMatch::Storage(
                storage_filter(),
                machine_caps::MachineCapabilityStorage {
                    vendor: None,
                    ..storage_capability()
                },
            ) => false,
            FieldMatch::Storage(
                InstanceTypeMachineCapabilityFilter {
                    vendor: Some("other".to_string()),
                    ..storage_filter()
                },
                storage_capability(),
            ) => false,
            FieldMatch::Storage(
                storage_filter(),
                machine_caps::MachineCapabilityStorage {
                    capacity: None,
                    ..storage_capability()
                },
            ) => false,
            FieldMatch::Storage(
                InstanceTypeMachineCapabilityFilter {
                    capacity: Some("4 TB".to_string()),
                    ..storage_filter()
                },
                storage_capability(),
            ) => false,
        }
    );

    value_scenarios!(
        run = FieldMatch::matches;
        "network constraints" {
            FieldMatch::Network(
                filter_for(MachineCapabilityType::Network),
                network_capability(),
            ) => true,
            FieldMatch::Network(network_filter(), network_capability()) => true,
            FieldMatch::Network(
                InstanceTypeMachineCapabilityFilter {
                    name: Some("other".to_string()),
                    ..network_filter()
                },
                network_capability(),
            ) => false,
            FieldMatch::Network(
                network_filter(),
                machine_caps::MachineCapabilityNetwork {
                    vendor: None,
                    ..network_capability()
                },
            ) => false,
            FieldMatch::Network(
                InstanceTypeMachineCapabilityFilter {
                    vendor: Some("other".to_string()),
                    ..network_filter()
                },
                network_capability(),
            ) => false,
            FieldMatch::Network(
                network_filter(),
                machine_caps::MachineCapabilityNetwork {
                    device_type: None,
                    ..network_capability()
                },
            ) => false,
            FieldMatch::Network(
                InstanceTypeMachineCapabilityFilter {
                    device_type: Some(MachineCapabilityDeviceType::NvLink),
                    ..network_filter()
                },
                network_capability(),
            ) => false,
        }
    );

    value_scenarios!(
        run = FieldMatch::matches;
        "InfiniBand constraints" {
            FieldMatch::Infiniband(
                filter_for(MachineCapabilityType::Infiniband),
                infiniband_capability(),
            ) => true,
            FieldMatch::Infiniband(infiniband_filter(), infiniband_capability()) => true,
            FieldMatch::Infiniband(
                InstanceTypeMachineCapabilityFilter {
                    name: Some("other".to_string()),
                    ..infiniband_filter()
                },
                infiniband_capability(),
            ) => false,
            FieldMatch::Infiniband(
                InstanceTypeMachineCapabilityFilter {
                    vendor: Some("other".to_string()),
                    ..infiniband_filter()
                },
                infiniband_capability(),
            ) => false,
            FieldMatch::Infiniband(
                InstanceTypeMachineCapabilityFilter {
                    inactive_devices: Some(vec![0, 2]),
                    ..infiniband_filter()
                },
                infiniband_capability(),
            ) => false,
        }
    );

    value_scenarios!(
        run = FieldMatch::matches;
        "DPU constraints" {
            FieldMatch::Dpu(filter_for(MachineCapabilityType::Dpu), dpu_capability()) => true,
            FieldMatch::Dpu(dpu_filter(), dpu_capability()) => true,
            FieldMatch::Dpu(
                InstanceTypeMachineCapabilityFilter {
                    name: Some("other".to_string()),
                    ..dpu_filter()
                },
                dpu_capability(),
            ) => false,
            FieldMatch::Dpu(
                dpu_filter(),
                machine_caps::MachineCapabilityDpu {
                    hardware_revision: None,
                    ..dpu_capability()
                },
            ) => false,
            FieldMatch::Dpu(
                InstanceTypeMachineCapabilityFilter {
                    hardware_revision: Some("B1".to_string()),
                    ..dpu_filter()
                },
                dpu_capability(),
            ) => false,
        }
    );
}

#[derive(Clone, Copy)]
enum CapabilityKind {
    Cpu,
    Gpu,
    Memory,
    Storage,
    Network,
    Infiniband,
    Dpu,
}

impl CapabilityKind {
    fn capability_type(self) -> MachineCapabilityType {
        match self {
            Self::Cpu => MachineCapabilityType::Cpu,
            Self::Gpu => MachineCapabilityType::Gpu,
            Self::Memory => MachineCapabilityType::Memory,
            Self::Storage => MachineCapabilityType::Storage,
            Self::Network => MachineCapabilityType::Network,
            Self::Infiniband => MachineCapabilityType::Infiniband,
            Self::Dpu => MachineCapabilityType::Dpu,
        }
    }
}

fn capabilities(kind: CapabilityKind, entries: &[(&str, u32)]) -> MachineCapabilitiesSet {
    let mut capabilities = empty_capability_set();

    match kind {
        CapabilityKind::Cpu => {
            capabilities.cpu = entries
                .iter()
                .map(|(name, count)| machine_caps::MachineCapabilityCpu {
                    name: (*name).to_string(),
                    count: *count,
                    vendor: Some("intel".to_string()),
                    cores: Some(8),
                    threads: Some(16),
                })
                .collect();
        }
        CapabilityKind::Gpu => {
            capabilities.gpu = entries
                .iter()
                .map(|(name, count)| machine_caps::MachineCapabilityGpu {
                    name: (*name).to_string(),
                    count: *count,
                    vendor: Some("nvidia".to_string()),
                    frequency: Some("1.5 GHz".to_string()),
                    memory_capacity: Some("80 GB".to_string()),
                    cores: Some(144),
                    threads: Some(288),
                    device_type: Some(MachineCapabilityDeviceType::Unknown),
                })
                .collect();
        }
        CapabilityKind::Memory => {
            capabilities.memory = entries
                .iter()
                .map(|(name, count)| machine_caps::MachineCapabilityMemory {
                    name: (*name).to_string(),
                    count: *count,
                    vendor: Some("micron".to_string()),
                    capacity: Some("256 GB".to_string()),
                })
                .collect();
        }
        CapabilityKind::Storage => {
            capabilities.storage = entries
                .iter()
                .map(|(name, count)| machine_caps::MachineCapabilityStorage {
                    name: (*name).to_string(),
                    count: *count,
                    vendor: Some("solidigm".to_string()),
                    capacity: Some("2 TB".to_string()),
                })
                .collect();
        }
        CapabilityKind::Network => {
            capabilities.network = entries
                .iter()
                .map(|(name, count)| machine_caps::MachineCapabilityNetwork {
                    name: (*name).to_string(),
                    count: *count,
                    vendor: Some("nvidia".to_string()),
                    device_type: Some(MachineCapabilityDeviceType::Dpu),
                })
                .collect();
        }
        CapabilityKind::Infiniband => {
            capabilities.infiniband = entries
                .iter()
                .map(|(name, count)| machine_caps::MachineCapabilityInfiniband {
                    name: (*name).to_string(),
                    count: *count,
                    vendor: "nvidia".to_string(),
                    inactive_devices: vec![1, 3],
                })
                .collect();
        }
        CapabilityKind::Dpu => {
            capabilities.dpu = entries
                .iter()
                .map(|(name, count)| machine_caps::MachineCapabilityDpu {
                    name: (*name).to_string(),
                    count: *count,
                    hardware_revision: Some("A1".to_string()),
                })
                .collect();
        }
    }

    capabilities
}

fn merge_capability_sets(
    sets: impl IntoIterator<Item = MachineCapabilitiesSet>,
) -> MachineCapabilitiesSet {
    let mut merged = empty_capability_set();

    for mut capabilities in sets {
        merged.cpu.append(&mut capabilities.cpu);
        merged.gpu.append(&mut capabilities.gpu);
        merged.memory.append(&mut capabilities.memory);
        merged.storage.append(&mut capabilities.storage);
        merged.network.append(&mut capabilities.network);
        merged.infiniband.append(&mut capabilities.infiniband);
        merged.dpu.append(&mut capabilities.dpu);
    }

    merged
}

fn capability_filter(
    kind: CapabilityKind,
    name: Option<&str>,
    count: Option<u32>,
) -> InstanceTypeMachineCapabilityFilter {
    InstanceTypeMachineCapabilityFilter {
        capability_type: kind.capability_type(),
        name: name.map(str::to_string),
        count,
        ..Default::default()
    }
}

fn match_case(
    kind: CapabilityKind,
    name: Option<&str>,
    count: Option<u32>,
    entries: &[(&str, u32)],
) -> (InstanceType, MachineCapabilitiesSet) {
    (
        instance_type_with(vec![capability_filter(kind, name, count)]),
        capabilities(kind, entries),
    )
}

fn composed_filter_case(gpu_filter_name: &str) -> (InstanceType, MachineCapabilitiesSet) {
    (
        instance_type_with(vec![
            capability_filter(CapabilityKind::Cpu, Some("cpu"), Some(1)),
            capability_filter(CapabilityKind::Gpu, Some(gpu_filter_name), Some(1)),
        ]),
        merge_capability_sets([
            capabilities(CapabilityKind::Cpu, &[("cpu", 1)]),
            capabilities(CapabilityKind::Gpu, &[("gpu", 1)]),
        ]),
    )
}

fn matches_capability_set(
    (instance_type, capability_set): (InstanceType, MachineCapabilitiesSet),
) -> bool {
    instance_type.matches_capability_set(&capability_set)
}

#[test]
fn instance_type_matches_capability_set() {
    value_scenarios!(
        run = matches_capability_set;
        "empty requirements accept an empty capability set" {
            (
                instance_type_with(vec![]),
                empty_capability_set(),
            ) => true,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "unbounded filters find each capability type" {
            match_case(CapabilityKind::Cpu, None, None, &[("cpu", 1)]) => true,
            match_case(CapabilityKind::Gpu, None, None, &[("gpu", 1)]) => true,
            match_case(CapabilityKind::Memory, None, None, &[("memory", 1)]) => true,
            match_case(CapabilityKind::Storage, None, None, &[("storage", 1)]) => true,
            match_case(CapabilityKind::Network, None, None, &[("network", 1)]) => true,
            match_case(CapabilityKind::Infiniband, None, None, &[("infiniband", 1)]) => true,
            match_case(CapabilityKind::Dpu, None, None, &[("dpu", 1)]) => true,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "unbounded filters reject missing matches" {
            match_case(CapabilityKind::Cpu, None, None, &[]) => false,
            match_case(CapabilityKind::Gpu, None, None, &[]) => false,
            match_case(CapabilityKind::Memory, None, None, &[]) => false,
            match_case(CapabilityKind::Storage, None, None, &[]) => false,
            match_case(CapabilityKind::Network, None, None, &[]) => false,
            match_case(CapabilityKind::Infiniband, None, None, &[]) => false,
            match_case(CapabilityKind::Dpu, None, None, &[]) => false,
            match_case(CapabilityKind::Cpu, Some("match"), None, &[("other", 1)]) => false,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "exact counts aggregate matching records" {
            match_case(CapabilityKind::Cpu, None, Some(3), &[("one", 1), ("two", 2)]) => true,
            match_case(CapabilityKind::Gpu, None, Some(3), &[("one", 1), ("two", 2)]) => true,
            match_case(CapabilityKind::Memory, None, Some(3), &[("one", 1), ("two", 2)]) => true,
            match_case(CapabilityKind::Storage, None, Some(3), &[("one", 1), ("two", 2)]) => true,
            match_case(CapabilityKind::Network, None, Some(3), &[("one", 1), ("two", 2)]) => true,
            match_case(
                CapabilityKind::Infiniband,
                None,
                Some(3),
                &[("one", 1), ("two", 2)],
            ) => true,
            match_case(CapabilityKind::Dpu, None, Some(3), &[("one", 1), ("two", 2)]) => true,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "nonmatching records do not contribute to an exact count" {
            match_case(
                CapabilityKind::Cpu,
                Some("match"),
                Some(3),
                &[("match", 1), ("other", 99), ("match", 2)],
            ) => true,
            match_case(
                CapabilityKind::Gpu,
                Some("match"),
                Some(3),
                &[("match", 1), ("other", 99), ("match", 2)],
            ) => true,
            match_case(
                CapabilityKind::Memory,
                Some("match"),
                Some(3),
                &[("match", 1), ("other", 99), ("match", 2)],
            ) => true,
            match_case(
                CapabilityKind::Storage,
                Some("match"),
                Some(3),
                &[("match", 1), ("other", 99), ("match", 2)],
            ) => true,
            match_case(
                CapabilityKind::Network,
                Some("match"),
                Some(3),
                &[("match", 1), ("other", 99), ("match", 2)],
            ) => true,
            match_case(
                CapabilityKind::Infiniband,
                Some("match"),
                Some(3),
                &[("match", 1), ("other", 99), ("match", 2)],
            ) => true,
            match_case(
                CapabilityKind::Dpu,
                Some("match"),
                Some(3),
                &[("match", 1), ("other", 99), ("match", 2)],
            ) => true,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "under-counted capability types are rejected" {
            match_case(CapabilityKind::Cpu, None, Some(2), &[("cpu", 1)]) => false,
            match_case(CapabilityKind::Gpu, None, Some(2), &[("gpu", 1)]) => false,
            match_case(CapabilityKind::Memory, None, Some(2), &[("memory", 1)]) => false,
            match_case(CapabilityKind::Storage, None, Some(2), &[("storage", 1)]) => false,
            match_case(CapabilityKind::Network, None, Some(2), &[("network", 1)]) => false,
            match_case(
                CapabilityKind::Infiniband,
                None,
                Some(2),
                &[("infiniband", 1)],
            ) => false,
            match_case(CapabilityKind::Dpu, None, Some(2), &[("dpu", 1)]) => false,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "overflowing capability counts are rejected" {
            match_case(
                CapabilityKind::Cpu,
                None,
                Some(u32::MAX),
                &[("one", u32::MAX), ("two", 1)],
            ) => false,
            match_case(
                CapabilityKind::Gpu,
                None,
                Some(u32::MAX),
                &[("one", u32::MAX), ("two", 1)],
            ) => false,
            match_case(
                CapabilityKind::Memory,
                None,
                Some(u32::MAX),
                &[("one", u32::MAX), ("two", 1)],
            ) => false,
            match_case(
                CapabilityKind::Storage,
                None,
                Some(u32::MAX),
                &[("one", u32::MAX), ("two", 1)],
            ) => false,
            match_case(
                CapabilityKind::Network,
                None,
                Some(u32::MAX),
                &[("one", u32::MAX), ("two", 1)],
            ) => false,
            match_case(
                CapabilityKind::Infiniband,
                None,
                Some(u32::MAX),
                &[("one", u32::MAX), ("two", 1)],
            ) => false,
            match_case(
                CapabilityKind::Dpu,
                None,
                Some(u32::MAX),
                &[("one", u32::MAX), ("two", 1)],
            ) => false,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "an exact zero count accepts an absent capability type" {
            match_case(CapabilityKind::Dpu, None, Some(0), &[]) => true,
        }
    );

    value_scenarios!(
        run = matches_capability_set;
        "composed filters require every capability type to match" {
            composed_filter_case("gpu") => true,
            composed_filter_case("other") => false,
        }
    );
}
