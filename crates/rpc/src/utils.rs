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

use carbide_utils::arch::CpuArchitecture;

use crate as rpc;

pub fn cpu_architecture_to_rpc(a: CpuArchitecture) -> i32 {
    match a {
        CpuArchitecture::Aarch64 => rpc::machine_discovery::CpuArchitecture::Aarch64 as i32,
        CpuArchitecture::X86_64 => rpc::machine_discovery::CpuArchitecture::X8664 as i32,
        CpuArchitecture::Unknown => rpc::machine_discovery::CpuArchitecture::Unknown as i32,
    }
}

pub fn cpu_architecture_from_rpc(a: i32) -> CpuArchitecture {
    match rpc::machine_discovery::CpuArchitecture::try_from(a) {
        Ok(rpc::machine_discovery::CpuArchitecture::Aarch64) => CpuArchitecture::Aarch64,
        Ok(rpc::machine_discovery::CpuArchitecture::X8664) => CpuArchitecture::X86_64,
        _ => CpuArchitecture::Unknown,
    }
}
