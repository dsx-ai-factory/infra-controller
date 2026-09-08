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
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use carbide_rack::test_support::RmsSim;
use carbide_site_explorer::MachineCreator;
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::network::segment::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    DpuConfigExt as _, FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::machine::{DpuMachineId, MachineId};
use carbide_uuid::rack::{RackId, RackProfileId};
use db::ObjectFilter;
use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::expected_rack::ExpectedRack;
use model::machine::machine_search_config::MachineSearchConfig;
use model::rack::RackConfig;
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
    RackHardwareTopology, RackProductFamily, RackProfile, RackProfileConfig,
};
use model::site_explorer::{EndpointExplorationReport, ExploredDpu, ExploredManagedHost};
use model::test_support::{DpuConfig, ManagedHostConfig};
use rpc::forge::forge_server::Forge;
use rpc::{DiscoveryData, DiscoveryInfo, MachineDiscoveryInfo};
use tonic::Request;

const KEY_PRODUCT_FAMILY: &str = "product_family";
const KEY_ROLE: &str = "role";
const KEY_VENDOR: &str = "vendor";
const ROLE_COMPUTE: &str = "compute";

struct ExploredHostFixture {
    host: ExploredManagedHost,
    host_report: EndpointExplorationReport,
    dpu_machine_ids: HashMap<u8, DpuMachineId>,
}

struct Env {
    pool: PgPool,
    underlay_segment: TestNetworkSegment,
    test_harness: TestHarness,
}

const TEST_RMS_RACK_PROFILE_ID: &str = "NVL72";

impl Env {
    async fn new(pool: PgPool) -> Self {
        let test_harness = TestHarness::builder(pool.clone()).build().await;
        let domain = test_harness.test_domain().await;
        let network_controller = test_harness.network_controller();
        let underlay_segment = network_controller.create_underlay_segment(&domain).await;
        network_controller.create_admin_segment(&domain).await;
        Self {
            pool,
            underlay_segment,
            test_harness,
        }
    }

    fn api(&self) -> &Api {
        self.test_harness.api()
    }
}

fn machine_creator_config() -> SiteExplorerConfig {
    SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        explorations_per_run: 2,
        concurrent_explorations: 1,
        run_interval: Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    }
}

fn machine_creator(env: &Env, config: SiteExplorerConfig) -> MachineCreator {
    MachineCreator::new(
        env.pool.clone(),
        config,
        env.api().common_pools().clone(),
        Arc::new(env.api().runtime_config.rack_profiles.clone()),
        None,
        env.api().credential_manager().clone(),
    )
}

fn machine_creator_with_rms(env: &Env, rms_sim: &RmsSim) -> MachineCreator {
    // Rack attributes are inherited by the compute descriptor, while its
    // role-level attribute replaces the identical rack-level key.
    let rack_profiles = RackProfileConfig {
        rack_profiles: [(
            TEST_RMS_RACK_PROFILE_ID.to_string(),
            RackProfile {
                product_family: Some(RackProductFamily::Gb200),
                rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                attributes: HashMap::from([
                    ("attribute1".to_string(), "rack-value".to_string()),
                    (
                        "additional_attribute2".to_string(),
                        "additional-value".to_string(),
                    ),
                ]),
                rack_capabilities: RackCapabilitiesSet {
                    compute: RackCapabilityCompute {
                        vendor: Some("NVIDIA".to_string()),
                        attributes: HashMap::from([(
                            "attribute1".to_string(),
                            "compute-value".to_string(),
                        )]),
                        ..Default::default()
                    },
                    switch: RackCapabilitySwitch {
                        vendor: Some("NVIDIA".to_string()),
                        ..Default::default()
                    },
                    power_shelf: RackCapabilityPowerShelf {
                        vendor: Some("LiteOn".to_string()),
                        ..Default::default()
                    },
                },
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
    };

    MachineCreator::new(
        env.pool.clone(),
        machine_creator_config(),
        env.api().common_pools().clone(),
        Arc::new(rack_profiles),
        rms_sim.as_rms_client(),
        env.api().credential_manager().clone(),
    )
}

fn expected_machine(managed_host: &ManagedHostConfig) -> ExpectedMachine {
    ExpectedMachine {
        id: Some(uuid::Uuid::new_v4()),
        bmc_mac_address: managed_host.bmc_mac_address,
        data: managed_host
            .expected_machine_data
            .clone()
            .unwrap_or_default(),
    }
}

async fn assert_no_machines_created(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let machines = db::machine::find(
        pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;

    assert_eq!(
        machines.len(),
        0,
        "expected no machine rows after RMS rack-profile preflight failure, got {machines:#?}"
    );
    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_compute_rms_request_uses_rack_profile(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let rms_sim = RmsSim::default();
    let creator = machine_creator_with_rms(&env, &rms_sim);
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let expected_rack = ExpectedRack {
        rack_id: rack_id.clone(),
        rack_profile_id: RackProfileId::new(TEST_RMS_RACK_PROFILE_ID),
        metadata: Default::default(),
    };

    let mut txn = env.pool.begin().await?;
    db::expected_rack::create(txn.as_mut(), &expected_rack).await?;
    txn.commit().await?;

    let managed_host =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mut fixture = explored_host_fixture(&env, &managed_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&managed_host)),
                &env.pool,
            )
            .await?
    );

    let requests = rms_sim
        .submitted_batch_get_node_device_info_requests()
        .await;
    let [request] = requests.as_slice() else {
        return Err(std::io::Error::other(format!(
            "expected one RMS BatchGetNodeDeviceInfo request, got {}",
            requests.len()
        ))
        .into());
    };
    let Some(nodes) = request.nodes.as_ref() else {
        return Err(std::io::Error::other("RMS request missing nodes").into());
    };
    let [node] = nodes.nodes.as_slice() else {
        return Err(std::io::Error::other(format!(
            "expected one RMS node in request, got {}",
            nodes.nodes.len()
        ))
        .into());
    };

    assert_eq!(node.rack_id, rack_id.to_string());

    assert_eq!(node.r#type, Some(rms::NodeType::ComputeGb200Nvidia as i32));

    let descriptor = node.node_descriptor.as_ref().expect("node descriptor");

    assert_eq!(
        descriptor.attributes.get(KEY_ROLE).map(String::as_str),
        Some(ROLE_COMPUTE)
    );

    assert_eq!(
        descriptor.attributes.get(KEY_VENDOR).map(String::as_str),
        Some("NVIDIA")
    );

    assert_eq!(
        descriptor
            .attributes
            .get(KEY_PRODUCT_FAMILY)
            .map(String::as_str),
        Some("gb200")
    );

    assert_eq!(
        descriptor
            .attributes
            .get("additional_attribute2")
            .map(String::as_str),
        Some("additional-value")
    );

    assert_eq!(
        descriptor.attributes.get("attribute1").map(String::as_str),
        Some("compute-value")
    );

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_compute_rms_request_errors_for_rack_without_profile(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let rms_sim = RmsSim::default();
    let creator = machine_creator_with_rms(&env, &rms_sim);
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());

    let mut txn = env.pool.begin().await?;
    db::rack::create(txn.as_mut(), &rack_id, None, &RackConfig::default(), None).await?;
    txn.commit().await?;

    let managed_host =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mut fixture = explored_host_fixture(&env, &managed_host).await;

    let result = creator
        .create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&managed_host)),
            &env.pool,
        )
        .await;

    let Err(error) = result else {
        return Err(std::io::Error::other("expected missing rack profile error").into());
    };

    assert!(error.to_string().contains("has no rack_profile_id"));
    assert_eq!(
        rms_sim
            .submitted_batch_get_node_device_info_requests()
            .await,
        Vec::new()
    );
    assert_no_machines_created(&env.pool).await?;

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_compute_rms_request_errors_for_unknown_profile(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let rms_sim = RmsSim::default();
    let creator = machine_creator_with_rms(&env, &rms_sim);
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());

    let mut txn = env.pool.begin().await?;
    db::rack::create(
        txn.as_mut(),
        &rack_id,
        Some(&RackProfileId::new("Unknown")),
        &RackConfig::default(),
        None,
    )
    .await?;
    txn.commit().await?;

    let managed_host =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mut fixture = explored_host_fixture(&env, &managed_host).await;

    let result = creator
        .create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&managed_host)),
            &env.pool,
        )
        .await;

    let Err(error) = result else {
        return Err(std::io::Error::other("expected unknown rack profile error").into());
    };

    assert!(error.to_string().contains("is not configured"));
    assert_eq!(
        rms_sim
            .submitted_batch_get_node_device_info_requests()
            .await,
        Vec::new()
    );
    assert_no_machines_created(&env.pool).await?;

    Ok(())
}

async fn discover_bmc(
    api: &Api,
    segment: TestNetworkSegment,
    mac_address: MacAddress,
    vendor_string: &str,
) -> IpAddr {
    api.discover_dhcp(
        rpc::forge::DhcpDiscovery::builder(mac_address, segment.relay_address)
            .vendor_string(vendor_string)
            .tonic_request(),
    )
    .await
    .expect("BMC DHCP discovery should succeed")
    .into_inner()
    .address
    .parse()
    .expect("DHCP response address should be an IP address")
}

async fn dhcp_discover_dpu_oob_iface(
    api: &Api,
    segment: TestNetworkSegment,
    mac_address: MacAddress,
) {
    api.discover_dhcp(
        rpc::forge::DhcpDiscovery::builder(mac_address, segment.relay_address)
            .vendor_string("bluefield")
            .tonic_request(),
    )
    .await
    .expect("DPU OOB DHCP discovery should succeed");
}

async fn explored_host_fixture(env: &Env, managed_host: &ManagedHostConfig) -> ExploredHostFixture {
    let host_bmc_ip = discover_bmc(
        env.api(),
        env.underlay_segment,
        managed_host.bmc_mac_address,
        "NVIDIA/OOB",
    )
    .await;

    let mut dpu_bmc_ips = Vec::new();
    for (dpu_index, dpu) in managed_host.dpus.iter().enumerate() {
        let dpu_index = dpu_index.try_into().expect("DPU index should fit into u8");
        let dpu_bmc_ip = discover_bmc(
            env.api(),
            env.underlay_segment,
            dpu.bmc_mac_address,
            "NVIDIA/BF/BMC",
        )
        .await;
        dpu_bmc_ips.push((dpu_index, dpu_bmc_ip));
    }

    let results = managed_host
        .exploration_results(Some(host_bmc_ip), &dpu_bmc_ips)
        .expect("managed host exploration results should be generated");
    let dpu_machine_ids = results.dpu_machine_ids();
    let (_, host_report) = results
        .host_report
        .expect("host exploration report should exist");
    let dpus = results
        .dpu_reports
        .into_iter()
        .map(|dpu| ExploredDpu {
            bmc_ip: dpu.bmc_ip,
            host_pf_mac_address: managed_host
                .dpus
                .get(dpu.dpu_index as usize)
                .map(|config| config.host_mac_address),
            host_chassis_id: None,
            report: Arc::new(dpu.report),
        })
        .collect();

    ExploredHostFixture {
        host: ExploredManagedHost { host_bmc_ip, dpus },
        host_report,
        dpu_machine_ids,
    }
}

#[sqlx_test]
async fn test_mi_attach_dpu_if_mi_exists_during_machine_creation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    let mock_host = ManagedHostConfig::default();
    let mock_dpu = mock_host.dpus.first().unwrap();

    // Create MI now.
    dhcp_discover_dpu_oob_iface(env.api(), env.underlay_segment, mock_dpu.oob_mac_address).await;

    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    // Machine interface should not have any machine id associated with it right now.
    let mut txn = env.pool.begin().await?;
    let mi =
        db::machine_interface::find_by_mac_address(txn.as_mut(), mock_dpu.oob_mac_address).await?;
    assert!(mi[0].attached_dpu_machine_id.is_none());
    assert!(mi[0].machine_id.is_none());
    txn.rollback().await?;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    // At this point, create_managed_host must have updated the associated machine id in
    // machine_interfaces table.
    let mut txn = env.pool.begin().await?;
    let mi =
        db::machine_interface::find_by_mac_address(txn.as_mut(), mock_dpu.oob_mac_address).await?;
    assert!(mi[0].attached_dpu_machine_id.is_some());
    assert!(mi[0].machine_id.is_some());
    txn.rollback().await?;

    Ok(())
}

#[sqlx_test]
async fn test_dpu_interface_predictions_apply_when_dhcp_follows_machine_creation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let mock_host = ManagedHostConfig::default();
    let mock_dpu = mock_host.dpus.first().unwrap();
    let mut fixture = explored_host_fixture(&env, &mock_host).await;
    let dpu_machine_id = fixture.dpu_machine_ids[&0];

    // The DPU machine exists before its first DHCP request, so there is no real interface yet.
    let mut txn = env.pool.begin().await?;
    let mi = db::machine_interface::find_by_machine_ids(&mut txn, &[dpu_machine_id]).await?;
    assert!(mi.is_empty());
    txn.rollback().await?;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    // Site Explorer cannot associate an interface that does not exist yet, but it must leave a
    // trusted MAC claim for DHCP to promote without waiting for another exploration sweep.
    let mut txn = env.pool.begin().await?;
    let machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig {
            include_dpus: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?;
    assert!(machine.is_some());

    let mi = db::machine_interface::find_by_machine_ids(&mut txn, &[dpu_machine_id]).await?;
    assert!(mi.is_empty());
    let prediction =
        db::predicted_machine_interface::find_by_mac_address(&mut txn, mock_dpu.oob_mac_address)
            .await?
            .expect("DPU OOB prediction should exist");
    assert_eq!(prediction.machine_id, dpu_machine_id.into());
    assert_eq!(
        prediction.expected_network_segment_type,
        model::network_segment::NetworkSegmentType::Underlay
    );
    assert!(prediction.primary_interface);
    txn.rollback().await?;

    // DHCP must consume the trusted prediction and create an already-owned interface.
    dhcp_discover_dpu_oob_iface(env.api(), env.underlay_segment, mock_dpu.oob_mac_address).await;

    let mut txn = env.pool.begin().await?;
    let interfaces =
        db::machine_interface::find_by_mac_address(txn.as_mut(), mock_dpu.oob_mac_address).await?;
    let [interface] = interfaces.as_slice() else {
        panic!("expected one promoted DPU OOB interface, got {interfaces:#?}");
    };
    assert_eq!(interface.machine_id, Some(dpu_machine_id.into()));
    assert_eq!(interface.attached_dpu_machine_id, Some(dpu_machine_id));
    assert!(interface.primary_interface);
    assert!(
        db::predicted_machine_interface::find_by_mac_address(&mut txn, mock_dpu.oob_mac_address,)
            .await?
            .is_none()
    );

    let topologies = db::machine_topology::find_by_machine_ids(&mut txn, &[dpu_machine_id]).await?;
    let topology = &topologies[&dpu_machine_id][0];
    let discovery_info = DiscoveryInfo::try_from(topology.topology().discovery_data.info.clone())?;
    let interface_id = interface.id;
    txn.commit().await?;

    let response = env
        .api()
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(interface_id),
            discovery_data: Some(DiscoveryData::Info(discovery_info)),
            create_machine: true,
            ..Default::default()
        }))
        .await?
        .into_inner();
    assert_eq!(response.machine_id, Some(dpu_machine_id.into()));

    Ok(())
}

#[sqlx_test]
async fn test_dpu_interface_predictions_apply_when_dhcp_follows_multi_dpu_machine_creation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let mock_host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    for dpu in &mock_host.dpus {
        dhcp_discover_dpu_oob_iface(env.api(), env.underlay_segment, dpu.oob_mac_address).await;
    }

    let mut txn = env.pool.begin().await?;
    for (dpu_index, dpu) in mock_host.dpus.iter().enumerate() {
        let dpu_index = dpu_index.try_into().expect("DPU index should fit into u8");
        let interfaces =
            db::machine_interface::find_by_mac_address(&mut *txn, dpu.oob_mac_address).await?;
        assert_eq!(interfaces.len(), 1);
        assert_eq!(
            interfaces[0].machine_id,
            Some(fixture.dpu_machine_ids[&dpu_index].into())
        );
        assert_eq!(
            interfaces[0].attached_dpu_machine_id,
            Some(fixture.dpu_machine_ids[&dpu_index])
        );
    }
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_rejects_partial_dpu_machine_set(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());
    let mock_host = ManagedHostConfig::default().with_dpu_count(NUM_DPUS);
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    // Ingest an earlier report containing only the first DPU so that both its machine and its
    // host association exist. The complete report below should then be rejected as a partial set.
    let partial_host = ExploredManagedHost {
        host_bmc_ip: fixture.host.host_bmc_ip,
        dpus: vec![fixture.host.dpus[0].clone()],
    };
    let mut partial_host_report = fixture.host_report.clone();
    assert!(
        creator
            .create_managed_host(
                &partial_host,
                &mut partial_host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    creator
        .create_managed_host(
            &fixture.host,
            &mut fixture.host_report,
            Some(&expected_machine(&mock_host)),
            &env.pool,
        )
        .await
        .expect_err("a partial DPU machine set should be rejected");

    let mut txn = env.pool.begin().await?;
    assert!(
        db::machine::find_one(
            &mut *txn,
            &fixture.dpu_machine_ids[&1],
            MachineSearchConfig::default(),
        )
        .await?
        .is_none()
    );
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_creates_managed_host_with_dpf_disabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    let mock_dpu = DpuConfig::with_serial("MT2328XZ185R".to_string());
    let mock_host = ManagedHostConfig {
        expected_machine_data: Some(ExpectedMachineData {
            dpf_enabled: Some(false),
            ..Default::default()
        }),
        ..ManagedHostConfig::default().with_dpus(vec![mock_dpu.clone()])
    };
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;

    assert_eq!(machines.len(), 2);
    for machine in machines {
        if machine.is_dpu() {
            // DPU has no expected-machine entry, so it always defaults to `true`.
            assert!(machine.config.dpf.enabled);
        } else {
            // Host has expected-machine entry with `dpf_enabled: Some(false)`.
            assert!(!machine.config.dpf.enabled);
        }
    }

    Ok(())
}

#[sqlx_test]
async fn test_machine_creator_creates_managed_host_with_dpf_enabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    let mock_dpu = DpuConfig::with_serial("MT2328XZ185R".to_string());
    let mock_host = ManagedHostConfig {
        expected_machine_data: Some(ExpectedMachineData {
            dpf_enabled: Some(true),
            ..Default::default()
        }),
        ..ManagedHostConfig::default().with_dpus(vec![mock_dpu.clone()])
    };
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&mock_host)),
                &env.pool,
            )
            .await?
    );

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;

    assert_eq!(machines.len(), 2);
    for machine in machines {
        assert!(machine.config.dpf.enabled);
    }

    Ok(())
}

/// `create_managed_host` must refuse to create a Managed Host when no
/// `expected_machines` entry is supplied.
#[sqlx_test]
async fn test_machine_creator_rejects_unexpected_host(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let creator = machine_creator(&env, machine_creator_config());

    let mock_host = ManagedHostConfig::default();
    let mut fixture = explored_host_fixture(&env, &mock_host).await;

    assert!(
        !creator
            .create_managed_host(&fixture.host, &mut fixture.host_report, None, &env.pool,)
            .await?
    );

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(
        machines.len(),
        0,
        "expected no Machine rows for an unexpected host, got {machines:#?}"
    );

    Ok(())
}
