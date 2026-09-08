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

use health_report::HealthProbeId;

lazy_static::lazy_static! {
    pub static ref ContainerExists: HealthProbeId = "ContainerExists".parse().unwrap();
    pub static ref SupervisorctlStatus: HealthProbeId = "SupervisorctlStatus".parse().unwrap();
    pub static ref ServiceRunning: HealthProbeId = "ServiceRunning".parse().unwrap();
    pub static ref DhcpRelay: HealthProbeId = "DhcpRelay".parse().unwrap();
    pub static ref DhcpServer: HealthProbeId = "DhcpServer".parse().unwrap();
    pub static ref BgpStats: HealthProbeId = HealthProbeId::bgp_stats();
    pub static ref BgpPeeringTor: HealthProbeId = HealthProbeId::bgp_peering_tor();
    pub static ref BgpPeeringRouteServer: HealthProbeId = "BgpPeeringRouteServer".parse().unwrap();
    pub static ref UnexpectedBgpPeer: HealthProbeId = "UnexpectedBgpPeer".parse().unwrap();
    pub static ref Ifreload: HealthProbeId = "Ifreload".parse().unwrap();
    pub static ref FileExists: HealthProbeId = "FileExists".parse().unwrap();
    pub static ref FileIsValid: HealthProbeId = "FileIsValid".parse().unwrap();
    pub static ref BgpDaemonEnabled: HealthProbeId = "BgpDaemonEnabled".parse().unwrap();
    pub static ref RestrictedMode: HealthProbeId = "RestrictedMode".parse().unwrap();
    pub static ref PostConfigCheckWait: HealthProbeId = HealthProbeId::post_config_check_wait();
    pub static ref DpuDiskUtilizationCheck: HealthProbeId = "DpuDiskUtilizationCheck".parse().unwrap();
    pub static ref DpuDiskUtilizationCritical: HealthProbeId = "DpuDiskUtilizationCritical".parse().unwrap();
    pub static ref NvueApiRunning: HealthProbeId = HealthProbeId::nvue_api_running();
}
