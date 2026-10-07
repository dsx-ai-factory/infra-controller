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
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::resource_pool::{ResourcePool, ResourcePoolStats};

/// IPv4 DPU loopback IP pool.
pub const LOOPBACK_IP: &str = "lo-ip";
/// IPv6 DPU loopback IP pool used by the FNN underlay.
pub const LOOPBACK_IP_V6: &str = "lo-ip-v6";
/// VNI pool. FabricNetworkConfiguration
/// Must match a pool defined in dev/resource_pools.toml
pub const VNI: &str = "vni";
/// vlan-id pool. FabricNetworkConfiguration
/// Must match a pool defined in dev/resource_pools.toml
pub const VLANID: &str = "vlan-id";
/// vpc-vni pool: L3VNI for the whole VPC
/// Must match a pool defined in dev/resource_pools.toml
pub const VPC_VNI: &str = "vpc-vni";
/// Must match a pool defined in dev/resource_pools.toml
pub const EXTERNAL_VPC_VNI: &str = "external-vpc-vni";
/// DPU Specific ASN for use with FNN
/// Must match a pool defined in dev/resource_pools.toml
pub const FNN_ASN: &str = "fnn-asn";
/// VPC DPU loopback IP, used as in FNN.
/// Must match a pool defined in dev/resource_pools.toml
pub const VPC_DPU_LOOPBACK: &str = "vpc-dpu-lo";

// DPA VNI pool: VNI for the DPA
pub const DPA_VNI: &str = "dpa-vni";

/// Returns the name of the resource pool used for a certain IB fabric
pub fn ib_pkey_pool_name(fabric: &str) -> String {
    format!("ib_fabrics.{fabric}.pkey")
}

/// ResourcePools that are used throughout the application
#[derive(Debug)]
pub struct CommonPools {
    pub ethernet: EthernetPools,
    pub infiniband: IbPools,
    pub pool_stats: Arc<Mutex<HashMap<String, ResourcePoolStats>>>,
    /// Instructs the metric task to stop.
    /// We rely on `CommonPools` being dropped to instruct the metric task to stop
    pub _stop_sender: oneshot::Sender<()>,
}

/// ResourcePools that are used for ethernet virtualization
#[derive(Debug)]
pub struct EthernetPools {
    pub pool_loopback_ip: Arc<ResourcePool<IpAddr>>,
    pub pool_loopback_ip_v6: Arc<ResourcePool<Ipv6Addr>>,
    pub pool_vlan_id: Arc<ResourcePool<i16>>,
    pub pool_vni: Arc<ResourcePool<i32>>,
    pub pool_vpc_vni: Arc<ResourcePool<i32>>,
    pub pool_external_vpc_vni: Arc<ResourcePool<i32>>,
    pub pool_dpa_vni: Arc<ResourcePool<i32>>,
    pub pool_fnn_asn: Arc<ResourcePool<u32>>,
    pub pool_vpc_dpu_loopback_ip: Arc<ResourcePool<IpAddr>>,
}

/// ResourcePools that are used for infiniband
#[derive(Clone, Debug, Default)]
pub struct IbPools {
    pub pkey_pools: Arc<HashMap<String, ResourcePool<u16>>>,
}
