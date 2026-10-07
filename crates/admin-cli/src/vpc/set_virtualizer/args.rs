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
use carbide_uuid::vpc::VpcId;
use clap::{Parser, ValueEnum};
#[derive(ValueEnum, Debug, Clone)]
#[clap(rename_all = "kebab-case")]
pub(super) enum VpcVirtualizationTypeArg {
    #[clap(alias = "etv")]
    EthernetVirtualizer,
    #[clap(hide = true, alias = "etv_nvue")]
    EthernetVirtualizerWithNvue,
    #[clap(name = "fnn")]
    Fnn,
    /// `Flat` is for VPCs whose tenant instances live directly on the
    /// underlay (zero-DPU hosts, or hosts with their DPU in NIC mode) and
    /// whose interfaces are bound to `HostInband` network segments rather
    /// than a NICo-managed overlay. Flat VPCs are still real tenant
    /// VPCs with a VNI and NSGs, but NICo doesn't drive their data
    /// plane -- routing and ACL enforcement between Flat VPCs and other
    /// VPCs is the network operator's responsibility.
    #[clap(name = "flat")]
    Flat,
}

impl From<VpcVirtualizationTypeArg> for ::rpc::forge::VpcVirtualizationType {
    fn from(t: VpcVirtualizationTypeArg) -> Self {
        match t {
            VpcVirtualizationTypeArg::EthernetVirtualizer
            | VpcVirtualizationTypeArg::EthernetVirtualizerWithNvue => {
                ::rpc::forge::VpcVirtualizationType::EthernetVirtualizer
            }
            VpcVirtualizationTypeArg::Fnn => ::rpc::forge::VpcVirtualizationType::Fnn,
            VpcVirtualizationTypeArg::Flat => ::rpc::forge::VpcVirtualizationType::Flat,
        }
    }
}
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:
Set virtualizer to FNN on VPC:
    $ nico-admin-cli vpc set-virtualizer 12345678-1234-5678-90ab-cdef01234567 fnn

")]
pub(crate) struct Args {
    #[clap(help = "The VPC ID for the VPC to update")]
    pub(super) id: VpcId,
    #[clap(value_enum, help = "The virtualizer to use for this VPC")]
    pub(super) virtualizer: VpcVirtualizationTypeArg,
}
