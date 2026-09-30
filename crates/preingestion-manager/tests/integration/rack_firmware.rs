// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use api_test_helper::mock_rms::MockRmsApi;
use async_trait::async_trait;
use carbide_preingestion_manager::PreingestionManager;
use carbide_rack_controller::firmware_object::FirmwareObjectFetcher;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::default_config;
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::rms::RmsBackend;
use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::expected_rack::ExpectedRack;
use model::expected_switch::ExpectedSwitch;
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityCompute, RackFirmwareObjectConfig, RackHardwareTopology,
    RackProductFamily, RackProfile, RackProfileConfig,
};
use model::site_explorer::PreingestionState;

use crate::common;

const PROFILE_ID: &str = "rack-firmware-test";
const SOT: &str = "opaque SOT forwarded unchanged to RMS";
const TOKEN_NAME: &str = "rack-artifacts";
const TOKEN: &str = " opaque token\n";

#[derive(Debug)]
struct StaticFirmwareObjectFetcher;

#[async_trait]
impl FirmwareObjectFetcher for StaticFirmwareObjectFetcher {
    async fn fetch(&self, _url: &str, _timeout: Duration) -> Result<String, String> {
        Ok(SOT.to_string())
    }
}

fn rack_profiles(access_token_credential: Option<&str>) -> RackProfileConfig {
    RackProfileConfig {
        rack_profiles: [(
            PROFILE_ID.to_string(),
            RackProfile {
                product_family: Some(RackProductFamily::Gb200),
                rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                firmware_object: Some(RackFirmwareObjectConfig {
                    url: "https://firmware.example.invalid/sot.json".parse().unwrap(),
                    access_token_credential: access_token_credential.map(str::to_string),
                    fetch_timeout: Duration::from_secs(30),
                }),
                rack_capabilities: RackCapabilitiesSet {
                    compute: RackCapabilityCompute {
                        vendor: Some("NVIDIA".to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )]
        .into(),
    }
}

fn manager(
    pool: &PgPool,
    env: &TestHarness,
    rms: Arc<MockRmsApi>,
    profiles: RackProfileConfig,
) -> PreingestionManager {
    let mut config = default_config::get();
    config.ntp_servers.clear();

    let backend = Arc::new(RmsBackend::new(
        rms,
        None,
        pool.clone(),
        Arc::new(profiles.clone()),
        true,
    ));

    PreingestionManager::new(
        pool.clone(),
        config.preingestion_manager(),
        Arc::new(RedfishSim::default()),
        env.test_meter.meter(),
        None,
        None,
        Some(env.api().credential_manager().clone()),
        env.api().work_lock_manager_handle(),
        config.ntp_servers,
    )
    .with_rack_firmware(profiles, backend, Arc::new(StaticFirmwareObjectFetcher))
}

async fn test_env(pool: &PgPool) -> TestHarness {
    let env = TestHarness::builder(pool.clone()).build().await;
    let domain = env.test_domain().await;
    env.network_controller()
        .create_underlay_segment(&domain)
        .await;

    env
}

async fn seed_expected_rack(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
) -> Result<(), db::DatabaseError> {
    db::expected_rack::create(
        txn,
        &ExpectedRack {
            rack_id: rack_id.clone(),
            rack_profile_id: RackProfileId::new(PROFILE_ID),
            ..Default::default()
        },
    )
    .await
    .map(|_| ())
}

async fn seed_compute(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    bmc_mac: MacAddress,
    bmc_ip: IpAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    db::expected_machine::create(
        &mut *txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac,
            data: ExpectedMachineData {
                serial_number: format!("compute-{bmc_mac}"),
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            },
        },
    )
    .await?;

    db::machine_interface::preallocate_bmc_machine_interface(&mut *txn, bmc_mac, bmc_ip, None)
        .await?;

    seed_endpoint(txn, bmc_ip).await
}

async fn seed_endpoint(
    txn: &mut sqlx::PgConnection,
    address: IpAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    common::insert_endpoint_version(txn, &address.to_string(), "1", "1", false).await?;
    db::explored_endpoints::set_preingestion_recheck_versions(address, txn).await?;
    Ok(())
}

async fn seed_bmc_credentials(writer: &dyn CredentialWriter, bmc_mac: MacAddress) {
    writer
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: bmc_mac,
                },
            },
            &Credentials::new("bmc-admin", "bmc-password"),
        )
        .await
        .unwrap();
}

async fn endpoint(pool: &PgPool, address: IpAddr) -> model::site_explorer::ExploredEndpoint {
    db::explored_endpoints::find_by_ips(pool, vec![address])
        .await
        .unwrap()
        .pop()
        .unwrap()
}

#[sqlx_test]
async fn compute_submission_is_device_scoped_and_resumes_polling(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(&pool).await;
    let rack_id = RackId::from("rack-compute");
    let bmc_mac: MacAddress = "02:00:00:00:00:10".parse()?;
    let addresses: [IpAddr; 2] = ["192.0.1.10".parse()?, "fd00::10".parse()?];
    let mut txn = pool.begin().await?;

    seed_expected_rack(txn.as_mut(), &rack_id).await?;
    seed_compute(txn.as_mut(), &rack_id, bmc_mac, addresses[0]).await?;

    let interface = db::machine_interface::find_by_mac_address(txn.as_mut(), bmc_mac)
        .await?
        .pop()
        .unwrap();

    db::machine_interface_address::insert(
        txn.as_mut(),
        interface.id,
        addresses[1],
        AllocationType::Static,
    )
    .await?;

    seed_endpoint(txn.as_mut(), addresses[1]).await?;

    db::explored_endpoints::set_preingestion_for_addresses(
        &[addresses[1]],
        PreingestionState::RecheckVersionsAfterFailure {
            reason: "previous inventory check failed".to_string(),
        },
        txn.as_mut(),
    )
    .await?;

    txn.commit().await?;

    seed_bmc_credentials(env.api().credential_manager().as_ref(), bmc_mac).await;

    let rms = Arc::new(MockRmsApi::new());
    let submission_manager = manager(&pool, &env, rms.clone(), rack_profiles(Some(TOKEN_NAME)));

    submission_manager.run_single_iteration().await?;

    assert!(rms.apply_firmware_object_calls().await.is_empty());

    env.api()
        .credential_manager()
        .set_credentials(
            &CredentialKey::FirmwareArtifactAccessToken {
                name: TOKEN_NAME.to_string(),
            },
            &Credentials::new("", TOKEN),
        )
        .await
        .unwrap();

    let submission = MockRmsApi::firmware_object_apply_ok(&bmc_mac.to_string(), "compute-job");
    rms.enqueue_apply_firmware_object(Ok(submission)).await;

    submission_manager.run_single_iteration().await?;

    assert_eq!(rms.apply_firmware_object_calls().await.len(), 1);

    for address in addresses {
        assert_eq!(
            endpoint(&pool, address).await.preingestion_state,
            PreingestionState::RackFirmwareUpdateWait {
                backend_job_id: Some("compute-job".to_string())
            }
        );
    }

    let request = &rms.apply_firmware_object_calls().await[0];
    assert_eq!(request.config_json, SOT);
    assert_eq!(request.access_token.as_deref(), Some(TOKEN));
    assert!(!request.force_update);

    assert_eq!(
        request.nodes.as_ref().unwrap().nodes[0].node_id,
        bmc_mac.to_string()
    );

    let polling_manager = manager(&pool, &env, rms.clone(), rack_profiles(Some(TOKEN_NAME)));

    let mut txn = pool.begin().await?;
    db::explored_endpoints::set_waiting_for_explorer_refresh(addresses[0], txn.as_mut()).await?;

    db::explored_endpoints::set_preingestion_for_addresses(
        &[addresses[1]],
        PreingestionState::RecheckVersions,
        txn.as_mut(),
    )
    .await?;

    txn.commit().await?;

    rms.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
        rms::FirmwareJobState::Unspecified,
    )))
    .await;

    polling_manager.run_single_iteration().await?;

    assert_eq!(
        endpoint(&pool, addresses[0]).await.preingestion_state,
        PreingestionState::RackFirmwareUpdateWait {
            backend_job_id: Some("compute-job".to_string())
        }
    );

    rms.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
        rms::FirmwareJobState::Running,
    )))
    .await;

    polling_manager.run_single_iteration().await?;

    assert_eq!(
        endpoint(&pool, addresses[0]).await.preingestion_state,
        PreingestionState::RackFirmwareUpdateWait {
            backend_job_id: Some("compute-job".to_string())
        }
    );

    assert_eq!(
        rms.get_firmware_job_status_calls().await[0].job_id,
        "compute-job"
    );

    rms.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
        rms::FirmwareJobState::Completed,
    )))
    .await;

    polling_manager.run_single_iteration().await?;

    for address in addresses {
        let endpoint = endpoint(&pool, address).await;
        assert_eq!(endpoint.preingestion_state, PreingestionState::Complete);
    }

    Ok(())
}

#[sqlx_test]
async fn successful_response_without_job_completes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(&pool).await;
    let rack_id = RackId::from("rack-noop");
    let bmc_mac: MacAddress = "02:00:00:00:00:20".parse()?;
    let bmc_ip: IpAddr = "192.0.1.20".parse()?;
    let mut txn = pool.begin().await?;

    seed_expected_rack(txn.as_mut(), &rack_id).await?;
    seed_compute(txn.as_mut(), &rack_id, bmc_mac, bmc_ip).await?;
    txn.commit().await?;

    seed_bmc_credentials(env.api().credential_manager().as_ref(), bmc_mac).await;

    let rms = Arc::new(MockRmsApi::new());
    let mut response = MockRmsApi::firmware_object_apply_ok(&bmc_mac.to_string(), "unused");

    response.jobs.clear();

    rms.enqueue_apply_firmware_object(Ok(response)).await;

    manager(&pool, &env, rms.clone(), rack_profiles(None))
        .run_single_iteration()
        .await?;

    assert_eq!(
        endpoint(&pool, bmc_ip).await.preingestion_state,
        PreingestionState::Complete
    );

    assert_eq!(
        rms.apply_firmware_object_calls().await[0]
            .access_token
            .as_deref(),
        Some("NOAUTH")
    );

    Ok(())
}

#[sqlx_test]
async fn failed_submission_with_job_id_fails_without_resubmitting(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(&pool).await;
    let rack_id = RackId::from("rack-rejected-job");
    let bmc_mac: MacAddress = "02:00:00:00:00:21".parse()?;
    let bmc_ip: IpAddr = "192.0.1.21".parse()?;
    let mut txn = pool.begin().await?;

    seed_expected_rack(txn.as_mut(), &rack_id).await?;
    seed_compute(txn.as_mut(), &rack_id, bmc_mac, bmc_ip).await?;
    txn.commit().await?;

    seed_bmc_credentials(env.api().credential_manager().as_ref(), bmc_mac).await;

    let rms = Arc::new(MockRmsApi::new());
    let mut response = MockRmsApi::firmware_object_apply_ok(&bmc_mac.to_string(), "rejected-job");
    let batch = response.response.as_mut().unwrap();
    batch.status = rms::ReturnCode::Failure as i32;
    batch.stats.as_mut().unwrap().failed_nodes = 1;

    batch.node_results[0].status = rms::ReturnCode::Failure as i32;
    batch.node_results[0].error_message = "firmware submission rejected".to_string();
    rms.enqueue_apply_firmware_object(Ok(response)).await;

    let preingestion_manager = manager(&pool, &env, rms.clone(), rack_profiles(None));

    preingestion_manager.run_single_iteration().await?;
    preingestion_manager.run_single_iteration().await?;

    let state = endpoint(&pool, bmc_ip).await.preingestion_state;

    let PreingestionState::Failed { reason } = state else {
        panic!("expected failed preingestion state, got {state:?}");
    };

    assert!(reason.contains("firmware submission rejected"));
    assert!(reason.contains("rejected-job"));
    assert_eq!(rms.apply_firmware_object_calls().await.len(), 1);

    Ok(())
}

#[sqlx_test]
async fn failed_firmware_job_fails_preingestion(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(&pool).await;
    let rack_id = RackId::from("rack-failed-job");
    let bmc_mac: MacAddress = "02:00:00:00:00:22".parse()?;
    let bmc_ip: IpAddr = "192.0.1.22".parse()?;
    let mut txn = pool.begin().await?;

    seed_expected_rack(txn.as_mut(), &rack_id).await?;
    seed_compute(txn.as_mut(), &rack_id, bmc_mac, bmc_ip).await?;

    db::explored_endpoints::set_preingestion_for_addresses(
        &[bmc_ip],
        PreingestionState::RackFirmwareUpdateWait {
            backend_job_id: Some("failed-job".to_string()),
        },
        txn.as_mut(),
    )
    .await?;

    txn.commit().await?;

    let rms = Arc::new(MockRmsApi::new());
    let mut response = MockRmsApi::firmware_job_status_ok(rms::FirmwareJobState::Failed);
    response.error_message = "component update failed".to_string();
    rms.enqueue_get_firmware_job_status(Ok(response)).await;

    manager(&pool, &env, rms, rack_profiles(None))
        .run_single_iteration()
        .await?;

    let state = endpoint(&pool, bmc_ip).await.preingestion_state;

    let PreingestionState::Failed { reason } = state else {
        panic!("expected failed preingestion state, got {state:?}");
    };

    assert_eq!(reason, "component update failed");

    Ok(())
}

#[sqlx_test]
async fn rack_compute_without_firmware_object_skips_standalone_update(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(&pool).await;
    let rack_id = RackId::from("rack-no-firmware-object");
    let bmc_mac: MacAddress = "02:00:00:00:00:26".parse()?;
    let bmc_ip: IpAddr = "192.0.1.26".parse()?;
    let mut txn = pool.begin().await?;

    seed_expected_rack(txn.as_mut(), &rack_id).await?;
    seed_compute(txn.as_mut(), &rack_id, bmc_mac, bmc_ip).await?;
    txn.commit().await?;

    let mut profiles = rack_profiles(None);
    profiles
        .rack_profiles
        .get_mut(PROFILE_ID)
        .unwrap()
        .firmware_object = None;

    let rms = Arc::new(MockRmsApi::new());

    manager(&pool, &env, rms.clone(), profiles)
        .run_single_iteration()
        .await?;

    assert!(rms.apply_firmware_object_calls().await.is_empty());

    assert_eq!(
        endpoint(&pool, bmc_ip).await.preingestion_state,
        PreingestionState::Complete
    );

    Ok(())
}

#[sqlx_test]
async fn missing_job_id_fails_closed_without_resubmitting(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(&pool).await;
    let rack_id = RackId::from("rack-retry");
    let bmc_mac: MacAddress = "02:00:00:00:00:30".parse()?;
    let bmc_ip: IpAddr = "192.0.1.30".parse()?;

    let mut txn = pool.begin().await?;
    seed_expected_rack(txn.as_mut(), &rack_id).await?;

    seed_compute(txn.as_mut(), &rack_id, bmc_mac, bmc_ip).await?;

    db::explored_endpoints::set_preingestion_for_addresses(
        &[bmc_ip],
        PreingestionState::RackFirmwareUpdateWait {
            backend_job_id: None,
        },
        txn.as_mut(),
    )
    .await?;

    txn.commit().await?;

    let rms = Arc::new(MockRmsApi::new());

    manager(&pool, &env, rms.clone(), rack_profiles(None))
        .run_single_iteration()
        .await?;

    assert!(rms.apply_firmware_object_calls().await.is_empty());

    let state = endpoint(&pool, bmc_ip).await.preingestion_state;

    let PreingestionState::Failed { reason } = state else {
        panic!("expected failed preingestion state, got {state:?}");
    };

    assert!(reason.contains("no job ID was persisted"));

    Ok(())
}

#[sqlx_test]
async fn expected_switch_skips_compute_firmware(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(&pool).await;
    let rack_id = RackId::from("rack-switch");
    let bmc_mac: MacAddress = "02:00:00:00:00:40".parse()?;
    let bmc_ip: IpAddr = "192.0.1.40".parse()?;
    let mut txn = pool.begin().await?;

    seed_expected_rack(txn.as_mut(), &rack_id).await?;

    db::expected_switch::create(
        txn.as_mut(),
        ExpectedSwitch {
            bmc_mac_address: bmc_mac,
            serial_number: "switch-1".to_string(),
            rack_id: Some(rack_id),
            bmc_ip_address: Some(bmc_ip),
            ..Default::default()
        },
    )
    .await?;

    db::machine_interface::preallocate_bmc_machine_interface(txn.as_mut(), bmc_mac, bmc_ip, None)
        .await?;

    seed_endpoint(txn.as_mut(), bmc_ip).await?;
    txn.commit().await?;

    let rms = Arc::new(MockRmsApi::new());

    manager(&pool, &env, rms.clone(), rack_profiles(None))
        .run_single_iteration()
        .await?;

    assert!(rms.apply_firmware_object_calls().await.is_empty());

    assert!(matches!(
        endpoint(&pool, bmc_ip).await.preingestion_state,
        PreingestionState::UpgradeFirmwareWait { .. }
    ));

    Ok(())
}
