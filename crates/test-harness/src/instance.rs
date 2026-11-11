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

use std::sync::Arc;

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::StableHostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use model::instance::snapshot::InstanceSnapshot;
use tonic::IntoRequest;

use crate::rpc::forge::forge_server::Forge;
use crate::rpc::forge::{self as rpc};
use crate::{ApiHandle, TestHarness, TestManagedHost};

/// An instance allocated through the Forge API.
///
/// Keeps the harness runtime alive for subsequent API and database reads.
pub struct TestInstance {
    /// The ID returned by AllocateInstance.
    pub id: InstanceId,
    api: Arc<ApiHandle>,
}

impl TestInstance {
    /// Reloads this instance through FindInstancesByIds and checks that exactly one was returned.
    pub async fn rpc_instance(&self) -> rpc::Instance {
        let mut instances = self
            .api
            .find_instances_by_ids(
                rpc::InstancesByIdsRequest {
                    instance_ids: vec![self.id],
                }
                .into_request(),
            )
            .await
            .expect("instance lookup should succeed")
            .into_inner()
            .instances;
        assert_eq!(instances.len(), 1);
        instances.remove(0)
    }

    /// Reloads this instance within the caller's transaction.
    pub async fn db_instance(&self, txn: &mut sqlx::PgTransaction<'_>) -> InstanceSnapshot {
        db::instance::find_by_id(txn.as_mut(), self.id)
            .await
            .unwrap()
            .expect("instance should exist")
    }
}

/// Builds an allocation request for a managed host and submits it through the Forge API.
///
/// Defaults to Tenant1, no hostname or keysets, and inline iPXE with `SomeRandomiPxe`
/// and `SomeRandomData` user data. Phone-home is disabled and metadata is omitted.
/// Supply Ethernet networking with [`Self::network`] or [`Self::config`].
/// Creation leaves the instance provisioning; controller convergence belongs to the calling suite.
pub struct TestInstanceBuilder<'a> {
    harness: &'a TestHarness,
    mh: &'a TestManagedHost,
    config: rpc::InstanceConfig,
    tenant: rpc::TenantConfig,
    metadata: Option<rpc::Metadata>,
}

impl<'a> TestInstanceBuilder<'a> {
    pub(crate) fn new(harness: &'a TestHarness, mh: &'a TestManagedHost) -> Self {
        Self {
            harness,
            mh,
            config: rpc::InstanceConfig {
                os: Some(rpc::InstanceOperatingSystemConfig {
                    phone_home_enabled: false,
                    run_provisioning_instructions_on_every_boot: false,
                    user_data: Some("SomeRandomData".to_string()),
                    variant: Some(rpc::instance_operating_system_config::Variant::Ipxe(
                        rpc::InlineIpxe {
                            ipxe_script: "SomeRandomiPxe".to_string(),
                        },
                    )),
                }),
                ..Default::default()
            },
            tenant: rpc::TenantConfig {
                tenant_organization_id: "Tenant1".to_string(),
                tenant_keyset_ids: vec![],
                hostname: None,
            },
            metadata: None,
        }
    }

    /// Replaces the instance configuration; an omitted tenant uses the builder's tenant defaults.
    pub fn config(self, config: rpc::InstanceConfig) -> Self {
        Self { config, ..self }
    }

    /// Sets the Ethernet network configuration.
    pub fn network(self, network: rpc::InstanceNetworkConfig) -> Self {
        Self {
            config: rpc::InstanceConfig {
                network: Some(network),
                ..self.config
            },
            ..self
        }
    }

    /// Selects one physical interface in the supplied tenant segment.
    pub fn single_interface_network_config(self, segment_id: NetworkSegmentId) -> Self {
        self.network(rpc::InstanceNetworkConfig {
            interfaces: vec![rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: Some(segment_id),
                network_details: Some(rpc::instance_interface_config::NetworkDetails::SegmentId(
                    segment_id,
                )),
                ..Default::default()
            }],
            ..Default::default()
        })
    }

    /// Sets allocation metadata; by default it is omitted.
    pub fn metadata(self, metadata: rpc::Metadata) -> Self {
        Self {
            metadata: Some(metadata),
            ..self
        }
    }

    /// Sets the hostname used when no tenant is supplied through [`Self::config`].
    pub fn hostname(self, hostname: impl Into<String>) -> Self {
        Self {
            tenant: rpc::TenantConfig {
                hostname: Some(hostname.into()),
                ..self.tenant
            },
            ..self
        }
    }

    /// Sets the tenant organization used when no tenant is supplied through [`Self::config`].
    pub fn tenant_org(self, tenant_org: impl Into<String>) -> Self {
        Self {
            tenant: rpc::TenantConfig {
                tenant_organization_id: tenant_org.into(),
                ..self.tenant
            },
            ..self
        }
    }

    /// Allocates through the API and returns the instance without running controllers.
    ///
    /// The host must be allocatable. API errors fail the fixture immediately.
    pub async fn build(self) -> TestInstance {
        let Self {
            harness,
            mh,
            config,
            tenant,
            metadata,
        } = self;
        let config = rpc::InstanceConfig {
            tenant: config.tenant.or(Some(tenant)),
            ..config
        };
        let id = harness
            .api()
            .allocate_instance(
                rpc::InstanceAllocationRequest {
                    instance_id: None,
                    machine_id: Some(StableHostMachineId::try_from(mh.host.id).unwrap()),
                    instance_type_id: None,
                    config: Some(config),
                    metadata,
                    allow_unhealthy_machine: false,
                    external_validation_request_id: None,
                }
                .into_request(),
            )
            .await
            .expect("Create instance failed.")
            .into_inner()
            .id
            .expect("Missing instance ID");

        TestInstance {
            id,
            api: harness.api.clone(),
        }
    }
}
