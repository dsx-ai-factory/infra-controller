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

//! # Carbide DPF SDK
//!
//! This crate provides a high-level SDK for interacting with the NVIDIA DPF
//! (DOCA Platform Framework) operator via Kubernetes CRDs.
//!
//! ## Overview
//!
//! The DPF SDK abstracts away the complexity of managing DPF CRDs, providing
//! a clean interface for:
//!
//! - Initializing DPF resources (provisioning source, DPUFlavor, and
//!   DPUDeployment with services)
//! - Registering and managing DPU devices
//! - Registering and managing DPU nodes (hosts with DPUs)
//! - Watching for DPF events via callbacks
//!
//! ## Example
//!
//! ```rust,ignore
//! use dpf::{DpfSdkBuilder, KubeRepository, InitDpfResourcesConfigBuilder};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let repo = KubeRepository::new().await?;
//!     let config = InitDpfResourcesConfigBuilder::default().build()?;
//!     let sdk = DpfSdkBuilder::new(repo, "dpf-operator-system", "secret".to_string())
//!         .initialize(&config)
//!         .await?;
//!
//!     let _watcher = sdk.watcher()
//!         .on_reboot_required(|event| async move {
//!             println!("Reboot required for: {}", event.host_bmc_ip);
//!             Ok(())
//!         })
//!         .start()?;
//!
//!     Ok(())
//! }
//! ```
#![warn(clippy::all)]
#![deny(warnings, unsafe_code)]

pub mod crds;
pub mod error;
pub mod flavor;
pub mod repository;
pub mod sdk;
mod service_vpc_slot;
pub mod services;
pub mod types;
pub mod watcher;

#[cfg(test)]
mod test;

// Re-exports for convenience
pub use error::DpfError;
pub use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
pub use repository::{DpfRepository, KubeRepository};
pub use sdk::{
    DpfSdk, DpfSdkBuilder, DpuProvisioningSource, NoLabels, ResourceLabeler, build_deployment,
    build_effective_dpu_interfaces, build_service_configuration, build_service_interface,
    build_service_nad, build_service_template, calculate_pf_total_sf, dpu_cr_name,
    dpu_device_cr_name, dpu_node_cr_name, node_id_from_dpu_node_cr_name,
};
pub use service_vpc_slot::ServiceVpcSlots;
pub use services::{DEFAULT_DOCA_HELM_REGISTRY, ServiceRegistryConfig};
pub use types::{
    BlueFieldSoftwareParams, BmcPasswordProvider, ConfigPortsServiceType, DEFAULT_DPU_NUM_OF_VFS,
    DEFAULT_PF_TOTAL_SF_RESERVED, DOCA_WEAVE_DHCP_AGENT_PF_TOTAL_SF, DPU_ENABLED_NODE_LABEL,
    DetachedDpuServiceDefinition, DetachedHelmChart, DpfInterceptBridge, DpfInterceptBridging,
    DpfInterfaceIdentity, DpuDeploymentType, DpuDeviceInfo, DpuErrorEvent, DpuEvent, DpuMismatch,
    DpuNodeInfo, DpuPhase, DpuReadyEvent, DpuServiceHelmChartObservation, DpuServiceObservation,
    DpuServiceVersion, InitDpfResourcesConfig, InitDpfResourcesConfigBuilder, MaintenanceEvent,
    PF_TOTAL_SF_BF4_ASTRA_FUDGE, RebootRequiredEvent, ServiceChainSwitch, ServiceConfigPort,
    ServiceConfigPortProtocol, ServiceDefinition, ServiceInterface, ServiceNAD,
    ServiceNADResourceType,
};
pub use watcher::{DpuWatcher, DpuWatcherBuilder};

/// Default namespace for DPF operator resources.
pub const NAMESPACE: &str = "dpf-operator-system";
