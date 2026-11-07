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

use std::collections::{HashMap, HashSet};

use ::rpc::machine_discovery as rpc_discovery;

pub fn aggregate_cpus(cpus: &[rpc_discovery::Cpu]) -> Vec<rpc_discovery::CpuInfo> {
    //
    //  Process CPU data
    //

    // This logic is ported from forge-cloud/cloud-backend. The handling of multiple CPU models on
    // a single machine is possibly misleading, but possibly it handles some future build, and it
    // accumulates all the info in any case. This function should return a vector with only one
    // CpuInfo.
    //
    // Number of unique sockets is cpu count.
    // Highest core number + 1 is the number of cores per socket.
    // Highest Number + 1 is total thread count, which
    // we'll divide by number of sockets later.

    let mut cpu_map = HashMap::<String, CpuAccumulator>::new();
    let mut cpu_socket_set = HashSet::<(String, u32)>::new();
    // Go through the CPUs listed in the hardware info and accumulate the details.
    for cpu in cpus.iter() {
        match cpu_map.get_mut(&cpu.model) {
            None => {
                // Insert into the socket map so we don't keep incrementing cpu count
                // as we look for threads and cores.
                cpu_socket_set.insert((cpu.model.clone(), cpu.socket));

                cpu_map.insert(
                    cpu.model.clone(),
                    CpuAccumulator {
                        model: cpu.model.clone(),
                        vendor: cpu.vendor.clone(),
                        sockets: 1,
                        cores: cpu.core + 1,
                        threads: cpu.number + 1,
                    },
                );
            }
            Some(accumulator) => {
                // If the socket hasn't been seen yet (i.e., if it's new to the set),
                // increment the cpu count.
                if cpu_socket_set.insert((cpu.model.clone(), cpu.socket)) {
                    accumulator.sockets += 1;
                }

                let core_count = cpu.core + 1;
                if core_count > accumulator.cores {
                    accumulator.cores = core_count;
                }

                let thread_count = cpu.number + 1;
                if thread_count > accumulator.threads {
                    accumulator.threads = thread_count;
                }
            }
        };
    }

    let mut values: Vec<&CpuAccumulator> = cpu_map.values().collect();
    values.sort_by_key(|v| &v.model);
    values
        .into_iter()
        .map(rpc_discovery::CpuInfo::from)
        .collect()
}

// Same as rpc_discovery::CpuInfo but with total thread count before computing threads per socket
pub struct CpuAccumulator {
    pub model: String,
    pub vendor: String,
    pub sockets: u32,
    pub cores: u32,
    pub threads: u32,
}

impl From<&CpuAccumulator> for rpc_discovery::CpuInfo {
    fn from(src: &CpuAccumulator) -> Self {
        let threads_per_socket = src.threads.checked_div(src.sockets).unwrap_or(0);

        rpc_discovery::CpuInfo {
            model: src.model.clone(),
            vendor: src.vendor.clone(),
            sockets: src.sockets,
            cores: src.cores,
            threads: threads_per_socket,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    fn cpu(vendor: &str, model: &str, socket: u32, core: u32, number: u32) -> rpc_discovery::Cpu {
        rpc_discovery::Cpu {
            vendor: vendor.to_string(),
            model: model.to_string(),
            frequency: String::new(),
            number,
            core,
            node: 0,
            socket,
        }
    }

    fn cpu_info(
        vendor: &str,
        model: &str,
        sockets: u32,
        cores: u32,
        threads: u32,
    ) -> rpc_discovery::CpuInfo {
        rpc_discovery::CpuInfo {
            model: model.to_string(),
            vendor: vendor.to_string(),
            sockets,
            cores,
            threads,
        }
    }

    #[test]
    fn aggregates_cpu_discovery_records() {
        value_scenarios!(
            run = |cpus| aggregate_cpus(&cpus);
            "empty discovery" {
                vec![] => vec![],
            }

            "single socket" {
                vec![
                    cpu("GenuineIntel", "Xeon", 0, 0, 0),
                    cpu("GenuineIntel", "Xeon", 0, 1, 1),
                ] => vec![cpu_info("GenuineIntel", "Xeon", 1, 2, 2)],
            }

            "multiple sockets" {
                vec![
                    cpu("GenuineIntel", "Xeon", 0, 0, 0),
                    cpu("GenuineIntel", "Xeon", 1, 0, 1),
                    cpu("GenuineIntel", "Xeon", 1, 2, 3),
                // `threads` is the max threads-per-socket count, not total records.
                ] => vec![cpu_info("GenuineIntel", "Xeon", 2, 3, 2)],
            }

            "multiple models are sorted" {
                vec![
                    cpu("AMD", "Zen B", 0, 0, 0),
                    cpu("AMD", "Zen A", 0, 1, 1),
                ] => vec![
                    cpu_info("AMD", "Zen A", 1, 2, 2),
                    cpu_info("AMD", "Zen B", 1, 1, 1),
                ],
            }
        );
    }
}
