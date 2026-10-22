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

//! Context types for the extension-service state handler.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use carbide_dpf::{DpuDeploymentType, ServiceVpcSlots};
use carbide_machine_controller::dpf::DpfOperations;
use model::extension_service::ServiceVpcAddressFamily;
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

/// Controller context for DPF Helm chart lifecycle reconciliation.
pub struct ExtensionServiceStateHandlerContextObjects {}

/// Startup-qualified service-VPC inventory used by registration reconciliation.
/// Preserves deployment-specific family sets so later per-DPU activation can enforce the same
/// policy.
#[derive(Clone, Debug, Default)]
pub struct ServiceVpcRegistrationConfig {
    /// Validated number of dormant activation paths reserved per networked registration.
    pub slots: ServiceVpcSlots,
    /// Address families enabled by each deployment's startup configuration.
    pub enabled_address_families: BTreeMap<DpuDeploymentType, BTreeSet<ServiceVpcAddressFamily>>,
}

impl ServiceVpcRegistrationConfig {
    /// Returns whether any supported deployment declares this family available.
    /// Registration parents are global and dormant; deployment-specific enforcement happens at
    /// activation.
    pub fn family_enabled(&self, family: ServiceVpcAddressFamily) -> bool {
        self.enabled_address_families
            .iter()
            .any(|(deployment, families)| {
                *deployment != DpuDeploymentType::Bf4Astra && families.contains(&family)
            })
    }

    /// Returns whether registration reconciliation can create DPF resources for the declared
    /// family.
    /// IPv4 is implemented, while IPv6 retains durable identity and waits without creating DPF
    /// resources.
    pub fn family_implemented(&self, family: ServiceVpcAddressFamily) -> bool {
        matches!(family, ServiceVpcAddressFamily::Ipv4)
    }
}

/// Services available to the extension-service state handler.
#[derive(Clone)]
pub struct ExtensionServiceStateHandlerServices {
    pub db_pool: PgPool,

    /// Absent only when DPF is disabled.
    pub dpf_sdk: Option<Arc<dyn DpfOperations>>,

    /// Configured deployment capability and fixed slot inventory for dormant registration parents.
    pub service_vpc: ServiceVpcRegistrationConfig,
}

impl StateHandlerContextObjects for ExtensionServiceStateHandlerContextObjects {
    type Services = ExtensionServiceStateHandlerServices;
    type ObjectMetrics = ();
}
