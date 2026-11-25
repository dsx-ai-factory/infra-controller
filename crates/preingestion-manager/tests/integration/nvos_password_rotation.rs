// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use api_test_helper::mock_rms::MockRmsApi;
use carbide_preingestion_manager::PreingestionManager;
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::default_config;
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::component_manager::ComponentManager;
use component_manager::mock::{MockComputeTrayManager, MockNvSwitchManager, MockPowerShelfManager};
use component_manager::rms::RmsBackend;
use db::TransactionVending as _;
use db::credential_rotation::CredentialRotationType;
use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::expected_rack::ExpectedRack;
use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
use model::firmware::FirmwareComponentType;
use model::machine_interface::InterfaceType;
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilitySwitch, RackHardwareTopology, RackProductFamily,
    RackProfile, RackProfileConfig,
};
use model::site_explorer::{NvosPasswordRotationState, PreingestionState};
use model::switch::{NewSwitch, SwitchConfig};
use rpc::forge::DhcpDiscovery;

use crate::common;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PROFILE_ID: &str = "preingestion-nvos";
const CURRENT_PASSWORD: &str = "Current-Nvos-Password-0!";
const INITIAL_TARGET_PASSWORD: &str = "Older-Nvos-Password-0!";
const TARGET_PASSWORD: &str = "Next-Nvos-Password-1!";

struct Fixture {
    env: TestHarness,
    rms: Arc<MockRmsApi>,
    bmc_mac: MacAddress,
    bmc_ip: IpAddr,
    nvos_mac: MacAddress,
    nvos_ip: IpAddr,
    firmware_updates_enabled: bool,
}

impl Fixture {
    async fn new(pool: &PgPool, with_nvos_address: bool) -> TestResult<Self> {
        let env = TestHarness::builder(pool.clone()).build().await;
        let domain = env.test_domain().await;
        let underlay = env
            .network_controller()
            .create_underlay_segment(&domain)
            .await;

        let bmc_mac: MacAddress = "02:00:00:00:00:40".parse()?;
        let nvos_mac = "02:00:00:00:00:41".parse()?;
        let nvos_ip = "192.0.1.41".parse()?;
        let rack_id = RackId::from("preingestion-switch");
        let mut txn = pool.txn_begin().await?;

        db::expected_rack::create(
            txn.as_mut(),
            &ExpectedRack {
                rack_id: rack_id.clone(),
                rack_profile_id: RackProfileId::new(PROFILE_ID),
                ..Default::default()
            },
        )
        .await?;
        db::expected_switch::create(
            txn.as_mut(),
            ExpectedSwitch {
                bmc_mac_address: bmc_mac,
                bmc_username: "bmc-admin".to_string(),
                bmc_password: "bmc-password".to_string(),
                serial_number: "preingestion-switch-1".to_string(),
                rack_id: Some(rack_id),
                nvos_interfaces: vec![ExpectedNvosInterface {
                    mac_address: nvos_mac,
                    is_primary: true,
                }],
                nvos_username: Some("nvos-admin".to_string()),
                nvos_password: Some(CURRENT_PASSWORD.to_string()),
                ..Default::default()
            },
        )
        .await?;
        txn.commit().await?;

        // Discover the BMC through DHCP without a static expected-switch address.
        // Its interface is not classified as Bmc until the switch is created.
        let response = env
            .api()
            .discover_dhcp(
                DhcpDiscovery::builder(bmc_mac.to_string(), underlay.relay_address)
                    .vendor_string("NVIDIA")
                    .tonic_request(),
            )
            .await?
            .into_inner();
        let bmc_ip: IpAddr = response.address.parse()?;
        let interface = db::machine_interface::find_by_ip(pool, bmc_ip)
            .await?
            .expect("DHCP-discovered BMC interface");
        assert_eq!(interface.interface_type, InterfaceType::Data);
        assert_eq!(interface.switch_id, None);

        let mut txn = pool.txn_begin().await?;
        if with_nvos_address {
            db::machine_interface::preallocate_machine_interface(
                txn.as_mut(),
                nvos_mac,
                nvos_ip,
                None,
            )
            .await?;
        }
        common::insert_endpoint_version(txn.as_mut(), &bmc_ip.to_string(), "1", "1.13.2", false)
            .await?;
        sqlx::query(
            "UPDATE explored_endpoints SET exploration_report = \
             jsonb_set(exploration_report, '{Chassis,0,Id}', to_jsonb('MGX_NVSwitch_0'::text)) \
             WHERE address = $1",
        )
        .bind(bmc_ip)
        .execute(txn.as_mut())
        .await?;
        db::explored_endpoints::set_preingestion_set_ntp_servers(bmc_ip, None, 0, txn.as_mut())
            .await?;
        txn.commit().await?;

        Ok(Self {
            env,
            rms: Arc::new(MockRmsApi::new()),
            bmc_mac,
            bmc_ip,
            nvos_mac,
            nvos_ip,
            firmware_updates_enabled: false,
        })
    }

    fn manager(&self, pool: &PgPool) -> PreingestionManager {
        let profiles = RackProfileConfig {
            rack_profiles: [(
                PROFILE_ID.to_string(),
                RackProfile {
                    product_family: Some(RackProductFamily::Gb200),
                    rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                    rack_capabilities: RackCapabilitiesSet {
                        switch: RackCapabilitySwitch {
                            count: 1,
                            vendor: Some("NVIDIA".to_string()),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )]
            .into(),
        };
        let backend = Arc::new(RmsBackend::new(
            self.rms.clone(),
            None,
            pool.clone(),
            Arc::new(profiles),
            false,
            carbide_instrument::Instrumentation::noop(),
        ));
        let component_manager = Arc::new(ComponentManager::new(
            backend,
            Arc::new(MockPowerShelfManager),
            Arc::new(MockComputeTrayManager),
            false,
            false,
            false,
        ));
        self.manager_with_component_manager(pool, Some(component_manager))
    }

    fn manager_with_component_manager(
        &self,
        pool: &PgPool,
        component_manager: Option<Arc<ComponentManager>>,
    ) -> PreingestionManager {
        let config = default_config::get();
        config
            .site_explorer
            .create_switches
            .store(false, Ordering::Relaxed);
        let mut manager_config = config.preingestion_manager();
        manager_config.autoupdate = self.firmware_updates_enabled;
        if !self.firmware_updates_enabled {
            manager_config.firmware = Default::default();
        }
        let manager = PreingestionManager::new(
            pool.clone(),
            manager_config,
            Arc::new(RedfishSim::default()),
            self.env.test_meter.meter(),
            None,
            None,
            Some(self.env.api().credential_manager().clone()),
            self.env.api().work_lock_manager_handle(),
            vec![],
            self.env.api().instrumentation().clone(),
        );
        match component_manager {
            Some(component_manager) => manager.with_component_manager(
                component_manager,
                self.env.api().credential_manager().clone(),
            ),
            None => manager,
        }
    }

    async fn publish_target(&self, pool: &PgPool, password: &str) -> TestResult {
        let current =
            db::credential_rotation::current_target_version(pool, CredentialRotationType::Nvos)
                .await?;
        let next_version = current.map_or(0, |version| version + 1);
        self.env
            .api()
            .credential_manager()
            .create_credentials(
                &CredentialKey::switch_nvos_site_admin(u32::try_from(next_version)?),
                &Credentials::new("", password),
            )
            .await
            .expect("seed immutable NVOS target");
        let mut txn = pool.txn_begin().await?;
        let result = match current {
            Some(current) => {
                db::credential_rotation::set_next_target_version(
                    txn.as_mut(),
                    CredentialRotationType::Nvos,
                    current,
                    Default::default(),
                )
                .await?
            }
            None => {
                db::credential_rotation::set_initial_target_version(
                    txn.as_mut(),
                    CredentialRotationType::Nvos,
                    Default::default(),
                )
                .await?
            }
        };
        assert!(matches!(result, db::ConditionalWrite::Applied(_)));
        txn.commit().await?;
        Ok(())
    }

    async fn state(&self, pool: &PgPool) -> TestResult<PreingestionState> {
        self.state_at(pool, self.bmc_ip).await
    }

    async fn state_at(&self, pool: &PgPool, address: IpAddr) -> TestResult<PreingestionState> {
        Ok(db::explored_endpoints::find_by_ips(pool, vec![address])
            .await?
            .pop()
            .expect("explored switch endpoint")
            .preingestion_state)
    }

    async fn credentials(&self) -> TestResult<Option<Credentials>> {
        Ok(self
            .env
            .api()
            .credential_manager()
            .get_credentials(&CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: self.bmc_mac,
            })
            .await
            .expect("read per-switch NVOS credentials"))
    }

    async fn store_credentials(&self, credentials: &Credentials) {
        self.env
            .api()
            .credential_manager()
            .set_credentials(
                &CredentialKey::SwitchNvosAdmin {
                    bmc_mac_address: self.bmc_mac,
                },
                credentials,
            )
            .await
            .expect("store per-switch NVOS credentials");
    }

    async fn run_to_state(
        &self,
        pool: &PgPool,
        manager: &PreingestionManager,
        expected: PreingestionState,
    ) -> TestResult {
        for _ in 0..4 {
            manager.run_single_iteration().await?;
            if self.state(pool).await? == expected {
                return Ok(());
            }
        }
        assert_eq!(self.state(pool).await?, expected);
        Ok(())
    }
}

async fn enqueue_rotation(fixture: &Fixture, job_id: &str) {
    fixture
        .rms
        .enqueue_update_switch_system_password(Ok(rms::UpdateSwitchSystemPasswordResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                job_id: job_id.to_string(),
                ..Default::default()
            }),
        }))
        .await;
}

async fn operation(
    pool: &PgPool,
    fixture: &Fixture,
) -> TestResult<db::credential_rotation::DeviceRotationOperationState> {
    Ok(db::credential_rotation::device_rotation_operation_state(
        pool,
        CredentialRotationType::Nvos,
        fixture.bmc_mac,
    )
    .await?
    .expect("persisted NVOS password operation"))
}

fn check_state() -> PreingestionState {
    PreingestionState::NvosPasswordRotation {
        rotation_state: NvosPasswordRotationState::Check,
    }
}

fn rotation_state(target_version: u32, job_id: Option<&str>) -> PreingestionState {
    PreingestionState::NvosPasswordRotation {
        rotation_state: NvosPasswordRotationState::Rotate {
            target_version,
            job_id: job_id.map(str::to_owned),
        },
    }
}

async fn assert_no_controller_operation(pool: &PgPool, fixture: &Fixture) -> TestResult {
    assert!(
        db::credential_rotation::device_rotation_operation_state(
            pool,
            CredentialRotationType::Nvos,
            fixture.bmc_mac,
        )
        .await?
        .is_none(),
        "preingestion must keep its pending job in the explored endpoint"
    );
    Ok(())
}

#[sqlx_test]
async fn rotates_factory_password_before_firmware_and_resumes_job(pool: PgPool) -> TestResult {
    let mut fixture = Fixture::new(&pool, true).await?;
    // BMC version 1 is below the configured minimum of 5; UEFI is already current.
    fixture.firmware_updates_enabled = true;
    let metrics = carbide_instrument::testing::MetricsCapture::for_registry(
        fixture.env.test_meter.registry(),
    );
    let firmware_uploads = || {
        metrics.counter_delta(
            "carbide_preingestion_firmware_upload_total",
            &[("outcome", "ok"), ("method", "multipart")],
        )
    };
    sqlx::query("UPDATE expected_switches SET nvos_username = 'admin', nvos_password = 'admin' WHERE bmc_mac_address = $1")
        .bind(fixture.bmc_mac).execute(&pool).await?;
    assert!(fixture.credentials().await?.is_none());
    fixture
        .publish_target(&pool, INITIAL_TARGET_PASSWORD)
        .await?;
    let manager = fixture.manager(&pool);
    fixture.run_to_state(&pool, &manager, check_state()).await?;
    assert!(fixture.credentials().await?.is_none());
    assert_eq!(firmware_uploads(), 0.0);
    manager.run_single_iteration().await?;
    assert_eq!(fixture.state(&pool).await?, rotation_state(0, None));
    assert!(fixture.credentials().await?.is_none());
    assert!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .is_empty()
    );
    assert_no_controller_operation(&pool, &fixture).await?;

    enqueue_rotation(&fixture, "password-job").await;
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(0, Some("password-job"))
    );
    assert_no_controller_operation(&pool, &fixture).await?;
    assert!(fixture.credentials().await?.is_none());
    let calls = fixture.rms.update_switch_system_password_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].username, "admin");
    assert_eq!(calls[0].password, INITIAL_TARGET_PASSWORD);
    let node = &calls[0].nodes.as_ref().unwrap().nodes[0];
    assert!(node.bmc_endpoint.is_none());
    let host = node.host_endpoint.as_ref().unwrap();
    assert_eq!(
        host.interface.as_ref().unwrap().ip_address,
        fixture.nvos_ip.to_string()
    );
    let Some(rms::credentials::Auth::UserPass(current)) = &host.credentials.as_ref().unwrap().auth
    else {
        panic!("initial rotation must authenticate using the expired factory credential");
    };
    assert_eq!(current.username, "admin");
    assert_eq!(current.password, "admin");

    let restarted = fixture.manager(&pool);
    fixture
        .rms
        .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
            "password-job",
            rms::JobExecutionState::Running,
        )))
        .await;
    restarted.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(0, Some("password-job"))
    );
    assert_no_controller_operation(&pool, &fixture).await?;
    assert!(fixture.credentials().await?.is_none());
    assert_eq!(firmware_uploads(), 0.0);

    // The endpoint keeps its captured target even if the site's target advances.
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    // Simulate a crash after writing the confirmed password to Vault, before
    // persisting CheckFirmware. Resume must poll the retained job instead of
    // submitting another rotation with the newly stored credential.
    fixture
        .store_credentials(&Credentials::new("admin", INITIAL_TARGET_PASSWORD))
        .await;
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(0, Some("password-job"))
    );
    fixture
        .rms
        .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
            "password-job",
            rms::JobExecutionState::Completed,
        )))
        .await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(firmware_uploads(), 0.0);
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("admin", INITIAL_TARGET_PASSWORD))
    );
    let enrolled = operation(&pool, &fixture).await?;
    assert_eq!(enrolled.current_version, Some(0));
    assert_eq!(enrolled.rotating_to_version, None);
    assert_eq!(enrolled.rotate_job_id, None);
    assert_eq!(enrolled.rotate_attempts, 0);
    assert_eq!(
        db::credential_rotation::current_target_version(&pool, CredentialRotationType::Nvos)
            .await?,
        Some(1)
    );
    let polls = fixture.rms.get_job_status_calls().await;
    assert_eq!(polls.len(), 2);
    assert!(polls.iter().all(|poll| poll.job_id == "password-job"));
    assert_eq!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .len(),
        1
    );
    assert!(
        fixture
            .rms
            .batch_disable_switch_mtls_calls()
            .await
            .is_empty()
    );
    assert!(
        fixture
            .rms
            .configure_switch_certificate_calls()
            .await
            .is_empty()
    );
    assert!(
        db::switch::find_by_bmc_mac_address(&pool, fixture.bmc_mac)
            .await?
            .is_none()
    );

    // Only the firmware continuation may start the required BMC upload.
    fixture.manager(&pool).run_single_iteration().await?;
    let state = fixture.state(&pool).await?;
    assert!(
        matches!(
            &state,
            PreingestionState::UpgradeFirmwareWait {
                task_id,
                final_version,
                upgrade_type: FirmwareComponentType::Bmc,
                ..
            } if task_id == "0" && final_version == "6.00.30.00"
        ),
        "expected the BMC firmware job after NVOS rotation, got {state:?}"
    );
    assert_eq!(firmware_uploads(), 1.0);
    Ok(())
}

#[sqlx_test]
async fn dual_stack_aliases_keep_one_rotation_owner_until_completion(pool: PgPool) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    let ipv6: IpAddr = "2001:db8::40".parse()?;
    let interface = db::machine_interface::find_by_ip(&pool, fixture.bmc_ip)
        .await?
        .expect("BMC interface");
    let endpoint = db::explored_endpoints::find_by_ips(&pool, vec![fixture.bmc_ip])
        .await?
        .pop()
        .expect("explored BMC endpoint");
    let mut txn = pool.txn_begin().await?;
    db::machine_interface_address::insert(&mut txn, interface.id, ipv6, AllocationType::Dhcp)
        .await?;
    db::explored_endpoints::insert(ipv6, &endpoint.report, false, &mut txn).await?;
    db::explored_endpoints::set_preingestion_for_addresses(
        &[fixture.bmc_ip, ipv6],
        check_state(),
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    // Both aliases are handled concurrently, but only one may stage Rotate.
    let manager = fixture.manager(&pool);
    manager.run_single_iteration().await?;
    let endpoints = db::explored_endpoints::find_by_ips(&pool, vec![fixture.bmc_ip, ipv6]).await?;
    assert_eq!(endpoints.len(), 2);
    let owners: Vec<_> = endpoints
        .iter()
        .filter(|endpoint| endpoint.preingestion_state == rotation_state(0, None))
        .collect();
    assert_eq!(owners.len(), 1, "persisted endpoints: {endpoints:?}");
    let owner = owners[0].address;
    let follower = if owner == ipv6 { fixture.bmc_ip } else { ipv6 };
    assert_eq!(fixture.state_at(&pool, follower).await?, check_state());
    assert_no_controller_operation(&pool, &fixture).await?;

    // An owner excluded from this iteration still owns the rotation, even
    // before it has submitted a job. Recreating the manager cannot steal it.
    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_waiting_for_explorer_refresh(owner, &mut txn).await?;
    txn.commit().await?;
    let runnable = db::explored_endpoints::find_preingest_not_waiting_not_error(&pool).await?;
    assert_eq!(runnable.len(), 1);
    assert_eq!(runnable[0].address, follower);
    let restarted = fixture.manager(&pool);
    restarted.run_single_iteration().await?;
    assert_eq!(
        fixture.state_at(&pool, owner).await?,
        rotation_state(0, None)
    );
    assert_eq!(fixture.state_at(&pool, follower).await?, check_state());
    assert!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .is_empty()
    );
    assert!(fixture.credentials().await?.is_none());

    sqlx::query(
        "UPDATE explored_endpoints SET waiting_for_explorer_refresh = false WHERE address = $1",
    )
    .bind(owner)
    .execute(&pool)
    .await?;
    enqueue_rotation(&fixture, "shared-password-job").await;
    restarted.run_single_iteration().await?;
    assert_eq!(
        fixture.state_at(&pool, owner).await?,
        rotation_state(0, Some("shared-password-job"))
    );
    assert_eq!(fixture.state_at(&pool, follower).await?, check_state());
    fixture
        .rms
        .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
            "shared-password-job",
            rms::JobExecutionState::Running,
        )))
        .await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state_at(&pool, owner).await?,
        rotation_state(0, Some("shared-password-job"))
    );
    assert_eq!(fixture.state_at(&pool, follower).await?, check_state());
    assert_no_controller_operation(&pool, &fixture).await?;

    // Run completion before the follower resumes so its next iteration must
    // observe the committed version and skip rotation.
    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_waiting_for_explorer_refresh(follower, &mut txn).await?;
    txn.commit().await?;
    fixture
        .rms
        .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
            "shared-password-job",
            rms::JobExecutionState::Completed,
        )))
        .await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state_at(&pool, owner).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(operation(&pool, &fixture).await?.current_version, Some(0));
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("nvos-admin", TARGET_PASSWORD))
    );
    sqlx::query("UPDATE explored_endpoints SET waiting_for_explorer_refresh = (address = $1) WHERE address = ANY($2)")
        .bind(owner)
        .bind(vec![owner, follower])
        .execute(&pool)
        .await?;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state_at(&pool, follower).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .len(),
        1
    );
    let polls = fixture.rms.get_job_status_calls().await;
    assert_eq!(polls.len(), 2);
    assert!(
        polls
            .iter()
            .all(|poll| poll.job_id == "shared-password-job")
    );
    Ok(())
}

#[sqlx_test]
async fn factory_copy_waits_for_address_then_rotates(pool: PgPool) -> TestResult {
    let fixture = Fixture::new(&pool, false).await?;
    fixture.publish_target(&pool, CURRENT_PASSWORD).await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    // Older Site Explorer deployments may have copied the factory pair to Vault.
    fixture
        .store_credentials(&Credentials::new("nvos-admin", CURRENT_PASSWORD))
        .await;
    let manager = fixture.manager(&pool);
    fixture
        .run_to_state(&pool, &manager, rotation_state(1, None))
        .await?;
    let (result, logs) =
        carbide_instrument::testing::capture_logs_async(manager.run_single_iteration()).await;
    result?;
    let errors = logs
        .iter()
        .filter(|log| log.message == "Error handling preingestion update")
        .collect::<Vec<_>>();
    assert_eq!(
        errors.len(),
        1,
        "expected the missing endpoint error, got {logs:?}"
    );
    assert_eq!(errors[0].level, tracing::Level::WARN);
    let error = errors[0].field("error").expect("preingestion error field");
    assert!(
        error.starts_with("component manager operation failed: not found:"),
        "{error}"
    );
    assert!(error.contains(&fixture.bmc_mac.to_string()), "{error}");
    assert!(error.contains("NVOS MAC or IP not available"), "{error}");
    assert_eq!(fixture.state(&pool).await?, rotation_state(1, None));
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("nvos-admin", CURRENT_PASSWORD))
    );
    assert_no_controller_operation(&pool, &fixture).await?;
    assert!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .is_empty()
    );

    let mut txn = pool.txn_begin().await?;
    db::machine_interface::preallocate_machine_interface(
        txn.as_mut(),
        fixture.nvos_mac,
        fixture.nvos_ip,
        None,
    )
    .await?;
    txn.commit().await?;
    enqueue_rotation(&fixture, "address-ready-job").await;
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(1, Some("address-ready-job"))
    );
    assert_no_controller_operation(&pool, &fixture).await?;
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("nvos-admin", CURRENT_PASSWORD))
    );
    fixture
        .rms
        .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
            "address-ready-job",
            rms::JobExecutionState::Completed,
        )))
        .await;
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("nvos-admin", TARGET_PASSWORD))
    );
    let enrolled = operation(&pool, &fixture).await?;
    assert_eq!(enrolled.current_version, Some(1));
    assert_eq!(enrolled.rotating_to_version, None);
    assert_eq!(enrolled.rotate_job_id, None);
    assert_eq!(enrolled.rotate_attempts, 0);
    assert_eq!(
        db::credential_rotation::current_target_version(&pool, CredentialRotationType::Nvos)
            .await?,
        Some(1)
    );
    assert_eq!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .len(),
        1
    );
    Ok(())
}

#[sqlx_test]
async fn credential_errors_preserve_claim_and_retry_after_repair(pool: PgPool) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    let manager = fixture.manager(&pool);
    let target_key = CredentialKey::switch_nvos_site_admin(0);
    let cases = [
        (
            "missing factory credentials",
            None,
            None,
            "no credentials found for SwitchNvos",
        ),
        (
            "missing target credential",
            Some(("nvos-admin", CURRENT_PASSWORD)),
            None,
            "NVOS target credential version 0 is missing",
        ),
        (
            "empty target password",
            Some(("nvos-admin", CURRENT_PASSWORD)),
            Some(""),
            "NVOS target password must not be empty",
        ),
    ];
    for (index, (label, factory, target_password, expected_error)) in cases.into_iter().enumerate()
    {
        // Repair the preceding failure while injecting the next one. Every
        // retry keeps the same captured target and persisted owner.
        match target_password {
            Some(password) => fixture
                .env
                .api()
                .credential_manager()
                .set_credentials(&target_key, &Credentials::new("", password))
                .await
                .expect("inject target credential for retry scenario"),
            None => fixture
                .env
                .api()
                .credential_manager()
                .delete_credentials(&target_key)
                .await
                .expect("remove target credential for retry scenario"),
        }
        sqlx::query("UPDATE expected_switches SET nvos_username = $2, nvos_password = $3 WHERE bmc_mac_address = $1")
            .bind(fixture.bmc_mac)
            .bind(factory.map(|(username, _)| username))
            .bind(factory.map(|(_, password)| password))
            .execute(&pool).await?;

        if index == 0 {
            // Check claims the published version even while both current
            // and target credentials are absent.
            fixture
                .run_to_state(&pool, &manager, rotation_state(0, None))
                .await?;
        }
        assert_eq!(
            fixture.state(&pool).await?,
            rotation_state(0, None),
            "{label}"
        );
        let (result, logs) =
            carbide_instrument::testing::capture_logs_async(manager.run_single_iteration()).await;
        result?;
        assert!(
            logs.iter().any(|log| {
                log.message == "Error handling preingestion update"
                    && log
                        .field("error")
                        .is_some_and(|error| error.contains(expected_error))
            }),
            "{label}: expected credential error, got {logs:?}"
        );
        assert_eq!(
            fixture.state(&pool).await?,
            rotation_state(0, None),
            "{label}"
        );
        assert!(fixture.credentials().await?.is_none(), "{label}");
        assert_no_controller_operation(&pool, &fixture).await?;
        assert!(
            fixture
                .rms
                .update_switch_system_password_calls()
                .await
                .is_empty(),
            "{label}"
        );
    }

    fixture
        .env
        .api()
        .credential_manager()
        .set_credentials(&target_key, &Credentials::new("", TARGET_PASSWORD))
        .await
        .expect("repair captured target credential");
    enqueue_rotation(&fixture, "credentials-ready-job").await;
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(0, Some("credentials-ready-job"))
    );
    assert!(fixture.credentials().await?.is_none());
    assert_no_controller_operation(&pool, &fixture).await?;
    let calls = fixture.rms.update_switch_system_password_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].password, TARGET_PASSWORD);

    fixture
        .rms
        .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
            "credentials-ready-job",
            rms::JobExecutionState::Completed,
        )))
        .await;
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("nvos-admin", TARGET_PASSWORD))
    );
    let enrolled = operation(&pool, &fixture).await?;
    assert_eq!(enrolled.current_version, Some(0));
    assert_eq!(enrolled.rotating_to_version, None);
    assert_eq!(enrolled.rotate_job_id, None);
    Ok(())
}

#[sqlx_test]
async fn confirmed_version_skips_preingestion_rotation_even_if_site_target_is_newer(
    pool: PgPool,
) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture.publish_target(&pool, CURRENT_PASSWORD).await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    // Eligibility skips a confirmed rotation without reading the newer secret.
    fixture
        .env
        .api()
        .credential_manager()
        .delete_credentials(&CredentialKey::switch_nvos_site_admin(1))
        .await
        .expect("remove newer target credential for confirmed-version skip");
    let stored = Credentials::new("nvos-admin", CURRENT_PASSWORD);
    fixture.store_credentials(&stored).await;
    let mut txn = pool.txn_begin().await?;
    db::credential_rotation::record_device_enrolled(
        txn.as_mut(),
        fixture.bmc_mac,
        CredentialRotationType::Nvos,
        Some(0),
    )
    .await?;
    txn.commit().await?;
    let before = operation(&pool, &fixture).await?;
    let manager = fixture.manager(&pool);
    fixture.run_to_state(&pool, &manager, check_state()).await?;

    manager.run_single_iteration().await?;

    assert_eq!(
        fixture.state(&pool).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(operation(&pool, &fixture).await?, before);
    assert_eq!(fixture.credentials().await?, Some(stored));
    assert!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .is_empty()
    );
    assert!(fixture.rms.get_job_status_calls().await.is_empty());
    Ok(())
}

#[sqlx_test]
async fn unversioned_stored_credentials_authenticate_initial_rotation(pool: PgPool) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    let manager = fixture.manager(&pool);
    let stored = Credentials::new("other-admin", "Previously-Changed-Password!");
    for (index, (label, seed_unknown_version)) in [
        ("missing enrollment", false),
        ("unknown enrolled version", true),
    ]
    .into_iter()
    .enumerate()
    {
        fixture.store_credentials(&stored).await;
        let mut txn = pool.txn_begin().await?;
        // Reset only this fixture's previous case; production preserves enrollment.
        db::credential_rotation::delete_device_converged(
            txn.as_mut(),
            fixture.bmc_mac,
            CredentialRotationType::Nvos,
        )
        .await?;
        if seed_unknown_version {
            db::credential_rotation::record_device_enrolled(
                txn.as_mut(),
                fixture.bmc_mac,
                CredentialRotationType::Nvos,
                None,
            )
            .await?;
            sqlx::query(
                "UPDATE device_credential_rotation SET rotating_to_version = 1, \
                 rotate_job_id = 'stale-managed-job', rotate_attempts = 3, \
                 rotate_last_attempt_at = NOW(), rotate_last_error_redacted = 'previous failure', \
                 rotate_quarantined_until = NOW() + INTERVAL '1 hour' \
                 WHERE device_mac = $1 AND credential_type = 'nvos'",
            )
            .bind(fixture.bmc_mac)
            .execute(txn.as_mut())
            .await?;
        }
        db::explored_endpoints::set_preingestion_for_addresses(
            &[fixture.bmc_ip],
            check_state(),
            txn.as_mut(),
        )
        .await?;
        txn.commit().await?;
        let before = db::credential_rotation::device_rotation_operation_state(
            &pool,
            CredentialRotationType::Nvos,
            fixture.bmc_mac,
        )
        .await?;

        manager.run_single_iteration().await?;
        assert_eq!(
            fixture.state(&pool).await?,
            rotation_state(0, None),
            "{label}"
        );
        assert_eq!(
            fixture.credentials().await?,
            Some(stored.clone()),
            "{label}"
        );
        if seed_unknown_version {
            assert_eq!(Some(operation(&pool, &fixture).await?), before, "{label}");
        } else {
            assert_no_controller_operation(&pool, &fixture).await?;
        }

        let job_id = format!("unversioned-{index}");
        enqueue_rotation(&fixture, &job_id).await;
        manager.run_single_iteration().await?;
        assert_eq!(
            fixture.state(&pool).await?,
            rotation_state(0, Some(&job_id)),
            "{label}"
        );
        let calls = fixture.rms.update_switch_system_password_calls().await;
        assert_eq!(calls.len(), index + 1, "{label}");
        let request = &calls[index];
        assert_eq!(request.username, "other-admin", "{label}");
        assert_eq!(request.password, TARGET_PASSWORD, "{label}");
        let host = request.nodes.as_ref().unwrap().nodes[0]
            .host_endpoint
            .as_ref()
            .unwrap();
        let Some(rms::credentials::Auth::UserPass(current)) =
            &host.credentials.as_ref().unwrap().auth
        else {
            panic!("{label}: initial rotation must authenticate with the stored credential");
        };
        assert_eq!(current.username, "other-admin", "{label}");
        assert_eq!(current.password, "Previously-Changed-Password!", "{label}");
        assert_eq!(
            fixture.credentials().await?,
            Some(stored.clone()),
            "{label}"
        );

        fixture
            .rms
            .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
                &job_id,
                rms::JobExecutionState::Completed,
            )))
            .await;
        manager.run_single_iteration().await?;
        assert_eq!(
            fixture.state(&pool).await?,
            PreingestionState::CheckFirmware,
            "{label}"
        );
        assert_eq!(
            fixture.credentials().await?,
            Some(Credentials::new("other-admin", TARGET_PASSWORD)),
            "{label}"
        );
        let enrolled = operation(&pool, &fixture).await?;
        assert_eq!(enrolled.current_version, Some(0), "{label}");
        assert_eq!(enrolled.rotating_to_version, None, "{label}");
        assert_eq!(enrolled.rotate_job_id, None, "{label}");
        assert_eq!(enrolled.rotate_last_error_redacted, None, "{label}");
        assert_eq!(
            enrolled.rotate_attempts,
            before
                .as_ref()
                .map_or(0, |operation| operation.rotate_attempts),
            "{label}"
        );
        assert_eq!(
            enrolled.rotate_last_attempt_at,
            before.and_then(|operation| operation.rotate_last_attempt_at),
            "{label}"
        );
        let quarantine_cleared: bool = sqlx::query_scalar(
            "SELECT rotate_quarantined_until IS NULL FROM device_credential_rotation \
             WHERE device_mac = $1 AND credential_type = 'nvos'",
        )
        .bind(fixture.bmc_mac)
        .fetch_one(&pool)
        .await?;
        assert!(quarantine_cleared, "{label}");
    }
    Ok(())
}

#[sqlx_test]
async fn password_job_errors_retry_the_captured_target(pool: PgPool) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture
        .publish_target(&pool, INITIAL_TARGET_PASSWORD)
        .await?;
    let manager = fixture.manager(&pool);
    fixture
        .run_to_state(&pool, &manager, rotation_state(0, None))
        .await?;
    enqueue_rotation(&fixture, "retry-job").await;
    manager.run_single_iteration().await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;

    fixture
        .rms
        .enqueue_get_job_status(Err(
            tonic::Status::unavailable("temporary RMS outage").into()
        ))
        .await;
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(0, Some("retry-job"))
    );
    assert_eq!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .len(),
        1
    );
    assert_no_controller_operation(&pool, &fixture).await?;
    assert!(fixture.credentials().await?.is_none());

    for (index, (label, response)) in [
        (
            "failed",
            Ok(MockRmsApi::job_status_ok(
                "retry-job",
                rms::JobExecutionState::Failed,
            )),
        ),
        (
            "not found",
            Err(tonic::Status::not_found("expired RMS job").into()),
        ),
        (
            "unknown",
            Ok(MockRmsApi::job_status_ok(
                "retry-job",
                rms::JobExecutionState::Unspecified,
            )),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        fixture.rms.enqueue_get_job_status(response).await;
        manager.run_single_iteration().await?;
        assert_eq!(
            fixture.state(&pool).await?,
            rotation_state(0, None),
            "{label}"
        );
        assert_no_controller_operation(&pool, &fixture).await?;
        assert!(fixture.credentials().await?.is_none(), "{label}");
        assert_eq!(
            fixture
                .rms
                .update_switch_system_password_calls()
                .await
                .len(),
            index + 1,
            "{label}"
        );
        enqueue_rotation(&fixture, "retry-job").await;
        fixture.manager(&pool).run_single_iteration().await?;
        assert_eq!(
            fixture.state(&pool).await?,
            rotation_state(0, Some("retry-job")),
            "{label}"
        );
        let calls = fixture.rms.update_switch_system_password_calls().await;
        assert_eq!(calls.len(), index + 2, "{label}");
        assert!(
            calls
                .iter()
                .all(|call| call.password == INITIAL_TARGET_PASSWORD),
            "{label}"
        );
        assert!(fixture.credentials().await?.is_none(), "{label}");
    }
    Ok(())
}

#[sqlx_test]
async fn missing_nvos_target_enters_rotation_then_reports_error_and_retries(
    pool: PgPool,
) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    let manager = fixture.manager(&pool);
    manager.run_single_iteration().await?;
    assert_eq!(fixture.state(&pool).await?, check_state());
    let (result, logs) =
        carbide_instrument::testing::capture_logs_async(manager.run_single_iteration()).await;
    result?;
    let errors = logs
        .iter()
        .filter(|log| log.message == "Error handling preingestion update")
        .collect::<Vec<_>>();
    assert_eq!(
        errors.len(),
        1,
        "expected the missing target error, got {logs:?}"
    );
    assert_eq!(errors[0].level, tracing::Level::WARN);
    let expected_error = format!(
        "internal error: NVOS site password target is missing during preingestion for BMC {}",
        fixture.bmc_mac
    );
    assert_eq!(errors[0].field("error"), Some(expected_error.as_str()));
    assert_eq!(fixture.state(&pool).await?, check_state());
    assert!(fixture.credentials().await?.is_none());
    assert_no_controller_operation(&pool, &fixture).await?;

    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    manager.run_single_iteration().await?;
    assert_eq!(fixture.state(&pool).await?, rotation_state(0, None));
    assert!(fixture.credentials().await?.is_none());
    assert_no_controller_operation(&pool, &fixture).await?;
    assert!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .is_empty()
    );
    assert!(fixture.rms.get_job_status_calls().await.is_empty());
    Ok(())
}

#[sqlx_test]
async fn unavailable_rotation_backend_enters_check_then_reports_error(pool: PgPool) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    let unsupported = Arc::new(ComponentManager::new(
        Arc::new(MockNvSwitchManager::default()),
        Arc::new(MockPowerShelfManager),
        Arc::new(MockComputeTrayManager),
        false,
        false,
        false,
    ));
    for (label, component_manager, expected_error) in [
        (
            "missing dependencies",
            None,
            "internal error: NVOS password rotation dependencies are unavailable",
        ),
        (
            "unsupported backend",
            Some(unsupported),
            "internal error: NVOS password rotation is not supported by the configured backend",
        ),
    ] {
        let mut txn = pool.txn_begin().await?;
        db::explored_endpoints::set_preingestion_set_ntp_servers(
            fixture.bmc_ip,
            None,
            0,
            txn.as_mut(),
        )
        .await?;
        txn.commit().await?;
        let manager = fixture.manager_with_component_manager(&pool, component_manager);
        manager.run_single_iteration().await?;
        assert_eq!(fixture.state(&pool).await?, check_state(), "{label}");

        let (result, logs) =
            carbide_instrument::testing::capture_logs_async(manager.run_single_iteration()).await;
        result?;
        let errors = logs
            .iter()
            .filter(|log| log.message == "Error handling preingestion update")
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "{label}: expected an error, got {logs:?}");
        assert_eq!(errors[0].level, tracing::Level::WARN, "{label}");
        assert_eq!(errors[0].field("error"), Some(expected_error), "{label}");
        assert_eq!(fixture.state(&pool).await?, check_state(), "{label}");
        assert!(fixture.credentials().await?.is_none(), "{label}");
        assert_no_controller_operation(&pool, &fixture).await?;
    }
    Ok(())
}

#[sqlx_test]
async fn live_switch_during_preingestion_reports_invariant_error_and_preserves_jobs(
    pool: PgPool,
) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture.publish_target(&pool, TARGET_PASSWORD).await?;
    fixture
        .store_credentials(&Credentials::new("managed-admin", "Managed-Nvos-Password!"))
        .await;
    let mut txn = pool.txn_begin().await?;
    let switch_id = "sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60";
    // A live switch and an unfinished preingestion job violate the lifecycle invariant.
    db::switch::create(
        txn.as_mut(),
        &NewSwitch {
            id: switch_id.parse()?,
            config: SwitchConfig {
                name: "managed switch".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(fixture.bmc_mac),
            metadata: None,
            rack_id: None,
            slot_number: None,
            tray_index: None,
        },
    )
    .await?;
    db::explored_endpoints::set_preingestion_for_addresses(
        &[fixture.bmc_ip],
        rotation_state(0, Some("old-preingestion-job")),
        txn.as_mut(),
    )
    .await?;
    let db::ConditionalWrite::Applied(attempt) =
        db::credential_rotation::record_device_rotation_started(
            txn.as_mut(),
            fixture.bmc_mac,
            CredentialRotationType::Nvos,
            0,
        )
        .await?
    else {
        panic!("controller rotation must be staged");
    };
    assert_eq!(
        db::credential_rotation::record_device_rotation_submitted(
            txn.as_mut(),
            fixture.bmc_mac,
            CredentialRotationType::Nvos,
            0,
            attempt,
            "managed-password-job",
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await?;
    let controller_operation = operation(&pool, &fixture).await?;

    let manager = fixture.manager(&pool);
    let (result, logs) =
        carbide_instrument::testing::capture_logs_async(manager.run_single_iteration()).await;
    result?;
    let errors = logs
        .iter()
        .filter(|log| log.message == "Error handling preingestion update")
        .collect::<Vec<_>>();
    assert_eq!(
        errors.len(),
        1,
        "expected the invariant error, got {logs:?}"
    );
    assert_eq!(errors[0].level, tracing::Level::WARN);
    let error = errors[0].field("error").expect("preingestion error field");
    assert!(error.starts_with("internal error:"), "{error}");
    assert!(error.contains(switch_id), "{error}");
    assert!(error.contains(&fixture.bmc_mac.to_string()), "{error}");
    assert!(
        error.contains("NVOS password preingestion must finish before switch creation"),
        "{error}"
    );
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(0, Some("old-preingestion-job"))
    );
    assert_eq!(operation(&pool, &fixture).await?, controller_operation);
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("managed-admin", "Managed-Nvos-Password!"))
    );
    assert!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .is_empty()
    );
    assert!(fixture.rms.get_job_status_calls().await.is_empty());
    Ok(())
}
