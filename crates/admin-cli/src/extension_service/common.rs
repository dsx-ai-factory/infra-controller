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

use clap::ValueEnum;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab_case")]
#[repr(i32)]
pub(super) enum ExtensionServiceType {
    #[value(alias = "k8s")]
    KubernetesPod = 0, // Kubernetes pod service type
    DpfHelmChart = 1, // DPF-managed Helm service type
}

impl From<ExtensionServiceType> for i32 {
    fn from(v: ExtensionServiceType) -> Self {
        v as i32
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum DpuTarget {
    Primary,
    AllActive,
    All,
}
