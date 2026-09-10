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

use std::net::SocketAddr;

use carbide_uuid::machine::{MachineId, StableHostMachineId};
use eyre::{ContextCompat, WrapErr};
use rpc::forge::instance_interface_config::NetworkDetails;
use rpc::forge::instance_operating_system_config::Variant as OperatingSystemVariant;
use rpc::forge::{
    InlineIpxe, Instance, InstanceAllocationRequest, InstanceConfig, InstanceInterfaceConfig,
    InstanceInterfaceIpv6Config, InstanceList, InstanceNetworkAutoConfig, InstanceNetworkConfig,
    InstanceOperatingSystemConfig, InstancePhoneHomeLastContactRequest, InstanceReleaseRequest,
    InstancesByIdsRequest, InterfaceFunctionType, Metadata, TenantConfig, TenantState,
};

use super::machine::wait_for_state;
use crate::api_client;

pub async fn create(
    addrs: &[SocketAddr],
    host_machine_id: &StableHostMachineId,
    segment_id: &str,
    hostname: Option<&str>,
    phone_home_enable: bool,
    wait_until_ready: bool,
    keyset_ids: &[&str],
) -> eyre::Result<String> {
    tracing::info!(
        host_machine_id = %host_machine_id,
        network_segment_id = segment_id,
        "Creating instance",
    );

    let network = InstanceNetworkConfig {
        interfaces: vec![InstanceInterfaceConfig {
            function_type: InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(segment_id.parse()?),
            ..Default::default()
        }],
        ..Default::default()
    };

    create_with_network(
        addrs,
        host_machine_id,
        network,
        hostname,
        phone_home_enable,
        wait_until_ready,
        keyset_ids,
    )
    .await
}

/// Creates an instance whose HostInband interfaces are resolved automatically
/// within the requested Flat VPC.
pub async fn create_with_auto_host_inband_networking(
    addrs: &[SocketAddr],
    host_machine_id: &StableHostMachineId,
    flat_vpc_id: &str,
) -> eyre::Result<String> {
    tracing::info!(
        host_machine_id = %host_machine_id,
        flat_vpc_id,
        "Creating automatically-networked instance",
    );

    let network = InstanceNetworkConfig {
        interfaces: Vec::new(),
        auto_config: Some(InstanceNetworkAutoConfig {
            vpc_id: Some(flat_vpc_id.parse()?),
        }),
        ..Default::default()
    };

    create_with_network(addrs, host_machine_id, network, None, false, false, &[]).await
}

async fn create_with_network(
    addrs: &[SocketAddr],
    host_machine_id: &StableHostMachineId,
    network: InstanceNetworkConfig,
    hostname: Option<&str>,
    phone_home_enable: bool,
    wait_until_ready: bool,
    keyset_ids: &[&str],
) -> eyre::Result<String> {
    let request = InstanceAllocationRequest {
        machine_id: Some(*host_machine_id),
        config: Some(InstanceConfig {
            tenant: Some(TenantConfig {
                tenant_organization_id: "tenant_organization".to_string(),
                hostname: hostname.map(str::to_string),
                tenant_keyset_ids: keyset_ids.iter().map(ToString::to_string).collect(),
            }),
            os: Some(inline_ipxe_config(phone_home_enable)),
            network: Some(network),
            ..Default::default()
        }),
        metadata: Some(Metadata {
            name: "test_instance".to_string(),
            description: "tests/integration/instance".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let instance = api_client::call(addrs, "AllocateInstance", |mut client| async move {
        client.allocate_instance(request).await
    })
    .await?;
    let instance_id = instance
        .id
        .context("AllocateInstance response has no instance ID")?
        .to_string();
    tracing::info!(
        instance_id = %instance_id,
        "Instance created",
    );

    if !wait_until_ready {
        return Ok(instance_id);
    }

    wait_for_state(addrs, host_machine_id, "Assigned/Ready").await?;

    if phone_home_enable {
        wait_for_instance_state(addrs, &instance_id, "PROVISIONING").await?;
        let before_phone = get_instance_state(addrs, &instance_id).await?;
        assert_eq!(before_phone, "PROVISIONING");
        // Phone home to transition to the ready state
        phone_home(addrs, &instance_id, host_machine_id).await?;
        wait_for_instance_state(addrs, &instance_id, "READY").await?;
        let after_phone = get_instance_state(addrs, &instance_id).await?;
        assert_eq!(after_phone, "READY");
    }

    // These 2 states should be equivalent
    wait_for_instance_state(addrs, &instance_id, "READY").await?;
    wait_for_state(addrs, host_machine_id, "Assigned/Ready").await?;

    tracing::info!(
        instance_id = %instance_id,
        "Instance is ready",
    );

    Ok(instance_id)
}

/// Allocates an instance with dual-stack VPC prefixes.
/// Takes a primary (v4) VPC prefix ID and an optional v6 VPC prefix ID.
pub async fn create_with_vpc_prefixes(
    addrs: &[SocketAddr],
    host_machine_id: &StableHostMachineId,
    tenant_organization_id: &str,
    vpc_prefix_ids: &[&str],
) -> eyre::Result<String> {
    tracing::info!(
        %host_machine_id,
        ?vpc_prefix_ids,
        "Creating instance",
    );

    let v4_id = vpc_prefix_ids
        .first()
        .ok_or_else(|| eyre::eyre!("at least one VPC prefix ID required"))?;

    let ipv6_interface_config = vpc_prefix_ids
        .get(1)
        .map(|v6_id| v6_id.parse::<carbide_uuid::vpc::VpcPrefixId>())
        .transpose()
        .wrap_err("invalid IPv6 VPC prefix ID")?
        .map(|vpc_prefix_id| InstanceInterfaceIpv6Config {
            vpc_prefix_id: Some(vpc_prefix_id),
            ip_address: None,
        });
    let interface = InstanceInterfaceConfig {
        function_type: InterfaceFunctionType::Physical as i32,
        network_details: Some(NetworkDetails::VpcPrefixId(v4_id.parse()?)),
        ipv6_interface_config,
        ..Default::default()
    };
    let request = InstanceAllocationRequest {
        machine_id: Some(*host_machine_id),
        config: Some(InstanceConfig {
            tenant: Some(TenantConfig {
                tenant_organization_id: tenant_organization_id.to_string(),
                ..Default::default()
            }),
            network: Some(InstanceNetworkConfig {
                interfaces: vec![interface],
                ..Default::default()
            }),
            os: Some(inline_ipxe_config(false)),
            ..Default::default()
        }),
        metadata: Some(Metadata {
            name: "test_instance_dual_stack".to_string(),
            description: "tests/integration/dual_stack_instance".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let instance = api_client::call(addrs, "AllocateInstance", |mut client| async move {
        client.allocate_instance(request).await
    })
    .await?;
    let instance_id = instance
        .id
        .context("AllocateInstance response has no instance ID")?
        .to_string();
    tracing::info!(
        instance_id = %instance_id,
        ?vpc_prefix_ids,
        "Instance created",
    );
    Ok(instance_id)
}

pub async fn release(
    addrs: &[SocketAddr],
    host_machine_id: &MachineId,
    instance_id: &str,
    wait_until_ready: bool,
) -> eyre::Result<()> {
    tracing::info!(
        instance_id,
        host_machine_id = %host_machine_id,
        "Releasing instance",
    );

    let request = InstanceReleaseRequest {
        id: Some(instance_id.parse()?),
        ..Default::default()
    };
    api_client::call(addrs, "ReleaseInstance", |mut client| async move {
        client.release_instance(request).await
    })
    .await?;
    tracing::info!("ReleaseInstance response received");

    if !wait_until_ready {
        return Ok(());
    }

    wait_for_instance_state(addrs, instance_id, "TERMINATING").await?;
    wait_for_state(addrs, host_machine_id, "Assigned/BootingWithDiscoveryImage").await?;

    let instances = find_instances_by_ids(addrs, instance_id).await?;
    let ip_address = instances
        .instances
        .first()
        .and_then(|instance| instance.status.as_ref())
        .and_then(|status| status.network.as_ref())
        .and_then(|network| {
            network
                .interfaces
                .iter()
                .find_map(|interface| interface.addresses.first())
        });
    if let Some(ip_address) = ip_address {
        tracing::info!(instance_id, ip_address, "Instance is terminating",);
    } else {
        tracing::info!(instance_id, "Instance is terminating",);
    }

    wait_for_state(addrs, host_machine_id, "WaitingForCleanup/HostCleanup").await?;
    let instances = find_instances_by_ids(addrs, instance_id).await?;
    tracing::info!(
        instance_count = instances.instances.len(),
        "FindInstancesByIds response received",
    );
    eyre::ensure!(
        instances.instances.is_empty(),
        "FindInstancesByIds returned released instance {instance_id}"
    );

    tracing::info!(instance_id, "Instance is released",);

    Ok(())
}

pub async fn phone_home(
    addrs: &[SocketAddr],
    instance_id: &str,
    host_machine_id: &MachineId,
) -> eyre::Result<()> {
    tracing::info!(
        %host_machine_id,
        instance_id,
        "Phoning home",
    );

    let request = InstancePhoneHomeLastContactRequest {
        instance_id: Some(instance_id.parse()?),
    };
    api_client::call(
        addrs,
        "UpdateInstancePhoneHomeLastContact",
        |mut client| async move {
            client
                .update_instance_phone_home_last_contact(request)
                .await
        },
    )
    .await?;

    Ok(())
}

pub async fn get_instance_state(addrs: &[SocketAddr], instance_id: &str) -> eyre::Result<String> {
    let instance = get_by_id(addrs, instance_id).await?;
    let state = instance
        .status
        .context("instance has no status")?
        .tenant
        .context("instance has no tenant status")?
        .state;
    let state = TenantState::try_from(state)
        .wrap_err("instance has an unknown tenant state")?
        .as_str_name()
        .to_string();
    tracing::info!(
        instance_state = %state,
        "Current instance state",
    );

    Ok(state)
}

pub async fn get_by_machine_id(
    addrs: &[SocketAddr],
    machine_id: &str,
) -> eyre::Result<InstanceList> {
    let machine_id: MachineId = machine_id.parse()?;
    api_client::call(addrs, "FindInstanceByMachineID", |mut client| async move {
        client.find_instance_by_machine_id(machine_id).await
    })
    .await
}

pub async fn get_by_id(addrs: &[SocketAddr], instance_id: &str) -> eyre::Result<Instance> {
    find_instances_by_ids(addrs, instance_id)
        .await?
        .instances
        .into_iter()
        .next()
        .ok_or_else(|| eyre::eyre!("instance {instance_id} was not returned by FindInstancesByIds"))
}

async fn find_instances_by_ids(
    addrs: &[SocketAddr],
    instance_id: &str,
) -> eyre::Result<InstanceList> {
    let request = InstancesByIdsRequest {
        instance_ids: vec![instance_id.parse()?],
    };
    api_client::call(addrs, "FindInstancesByIds", |mut client| async move {
        client.find_instances_by_ids(request).await
    })
    .await
}

fn inline_ipxe_config(phone_home_enabled: bool) -> InstanceOperatingSystemConfig {
    InstanceOperatingSystemConfig {
        variant: Some(OperatingSystemVariant::Ipxe(InlineIpxe {
            ipxe_script: "chain --autofree https://boot.netboot.xyz".to_string(),
        })),
        phone_home_enabled,
        user_data: Some("hello".to_string()),
        ..Default::default()
    }
}

/// Waits for an instance to reach a certain state
pub async fn wait_for_instance_state(
    addrs: &[SocketAddr],
    instance_id: &str,
    target_state: &str,
) -> eyre::Result<()> {
    const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    let mut latest_state = String::new();

    tracing::info!(instance_id, target_state, "Waiting for Instance state",);
    while start.elapsed() < MAX_WAIT {
        latest_state = get_instance_state(addrs, instance_id).await?;

        if latest_state.contains(target_state) {
            return Ok(());
        }
        tracing::info!(
            instance_state = %latest_state,
            "Current instance state",
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    eyre::bail!(
        "even after {MAX_WAIT:?} time, {instance_id} did not reach state {target_state}\n
        latest state: {latest_state}"
    );
}
