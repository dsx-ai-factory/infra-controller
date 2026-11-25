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
use component_manager::config::{SwitchMtlsService, switch_mtls_services_as_i32};
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
use model::site_explorer::{
    InitialBmcResetPhase, NvosPasswordRotationState, PreingestionState,
    SwitchCertificateRotationState,
};
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
    switch_mtls_services: Vec<i32>,
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
            switch_mtls_services: switch_mtls_services_as_i32(
                &SwitchMtlsService::default_services(),
            ),
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
                self.switch_mtls_services.clone(),
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

async fn enqueue_mtls_disable(fixture: &Fixture, job_id: &str) {
    fixture
        .rms
        .enqueue_batch_disable_switch_mtls(Ok(rms::BatchDisableSwitchMtlsResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                job_id: job_id.to_string(),
                ..Default::default()
            }),
        }))
        .await;
}

async fn enqueue_mtls_completed(fixture: &Fixture, job_id: &str) {
    let calls = fixture.rms.batch_disable_switch_mtls_calls().await;
    let node_id = calls
        .last()
        .expect("submitted mTLS removal")
        .nodes
        .as_ref()
        .unwrap()
        .nodes[0]
        .node_id
        .clone();
    let child_id = format!("{job_id}-switch");
    fixture
        .rms
        .enqueue_get_job_status(Ok(rms::GetJobStatusResponse {
            job_states: vec![
                rms::JobStatus {
                    job_id: job_id.to_string(),
                    child_job_ids: vec![child_id.clone()],
                    execution_state: rms::JobExecutionState::Completed as i32,
                    ..Default::default()
                },
                rms::JobStatus {
                    job_id: child_id,
                    parent_job_id: Some(job_id.to_string()),
                    node_id: Some(node_id),
                    execution_state: rms::JobExecutionState::Completed as i32,
                    ..Default::default()
                },
            ],
        }))
        .await;
}

async fn enqueue_certificate(fixture: &Fixture, node_id: &str, job_id: &str) {
    fixture
        .rms
        .enqueue_configure_switch_certificate(Ok(MockRmsApi::configure_switch_certificate_ok(
            node_id, job_id,
        )))
        .await;
}

fn certificate_state(rotation_state: SwitchCertificateRotationState) -> PreingestionState {
    PreingestionState::SwitchCertificateRotation { rotation_state }
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
    // persisting the certificate state. Resume must poll the retained job instead of
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
        certificate_state(SwitchCertificateRotationState::DisableMtls { job_id: None })
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

    // A fresh manager submits the persisted phase when it has no job ID yet.
    enqueue_mtls_disable(&fixture, "disable-job").await;
    fixture.manager(&pool).run_single_iteration().await?;
    let disable_wait = certificate_state(SwitchCertificateRotationState::DisableMtls {
        job_id: Some("disable-job".to_string()),
    });
    assert_eq!(fixture.state(&pool).await?, disable_wait);
    let disable_calls = fixture.rms.batch_disable_switch_mtls_calls().await;
    assert_eq!(disable_calls.len(), 1);
    assert_eq!(
        disable_calls[0].services,
        vec![
            rms::SwitchService::NvueApi as i32,
            rms::SwitchService::ScaleUpFabricTelemetryInterface as i32,
        ]
    );
    let node = &disable_calls[0].nodes.as_ref().unwrap().nodes[0];
    let host = node.host_endpoint.as_ref().unwrap();
    let Some(rms::credentials::Auth::UserPass(current)) = &host.credentials.as_ref().unwrap().auth
    else {
        panic!("mTLS removal must authenticate with the rotated password");
    };
    assert_eq!(current.username, "admin");
    assert_eq!(current.password, INITIAL_TARGET_PASSWORD);
    assert_eq!(
        host.interface.as_ref().unwrap().ip_address,
        fixture.nvos_ip.to_string()
    );
    assert_eq!(firmware_uploads(), 0.0);

    for response in [
        Err(tonic::Status::unavailable("temporary mTLS observation failure").into()),
        Err(tonic::Status::not_found("expired mTLS job").into()),
        Ok(MockRmsApi::job_status_ok(
            "disable-job",
            rms::JobExecutionState::Running,
        )),
    ] {
        fixture.rms.enqueue_get_job_status(response).await;
        fixture.manager(&pool).run_single_iteration().await?;
        assert_eq!(fixture.state(&pool).await?, disable_wait);
        assert!(
            fixture
                .rms
                .configure_switch_certificate_calls()
                .await
                .is_empty()
        );
        assert_eq!(firmware_uploads(), 0.0);
    }
    enqueue_mtls_completed(&fixture, "disable-job").await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        certificate_state(SwitchCertificateRotationState::ConfigureCertificate { job_id: None })
    );
    assert!(
        fixture
            .rms
            .configure_switch_certificate_calls()
            .await
            .is_empty()
    );
    assert_eq!(fixture.rms.batch_disable_switch_mtls_calls().await.len(), 1);
    let polls = fixture.rms.get_job_status_calls().await;
    assert_eq!(
        polls
            .iter()
            .filter(|poll| poll.job_id == "disable-job")
            .count(),
        4,
    );

    // Restart between phases must also submit the unstarted certificate operation.
    enqueue_certificate(&fixture, &node.node_id, "certificate-job").await;
    fixture.manager(&pool).run_single_iteration().await?;
    let certificate_wait =
        certificate_state(SwitchCertificateRotationState::ConfigureCertificate {
            job_id: Some("certificate-job".to_string()),
        });
    assert_eq!(fixture.state(&pool).await?, certificate_wait);
    let calls = fixture.rms.configure_switch_certificate_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].domain, None);
    assert_eq!(calls[0].services, disable_calls[0].services);
    assert_eq!(calls[0].nodes.as_ref().unwrap().nodes[0], *node);
    assert_eq!(firmware_uploads(), 0.0);

    for response in [
        Err(tonic::Status::unavailable("temporary certificate observation failure").into()),
        Ok(rms::GetConfigureSwitchCertificateJobStatusResponse {
            status: rms::ReturnCode::Failure as i32,
            error_message: "expired certificate job".to_string(),
            ..Default::default()
        }),
        Ok(MockRmsApi::configure_switch_certificate_job_status_ok(
            "in_progress",
        )),
    ] {
        fixture
            .rms
            .enqueue_get_configure_switch_certificate_job_status(response)
            .await;
        fixture.manager(&pool).run_single_iteration().await?;
        assert_eq!(fixture.state(&pool).await?, certificate_wait);
        assert_eq!(firmware_uploads(), 0.0);
    }
    fixture
        .rms
        .enqueue_get_configure_switch_certificate_job_status(Ok(
            MockRmsApi::configure_switch_certificate_job_status_ok("completed"),
        ))
        .await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(firmware_uploads(), 0.0);
    assert_eq!(
        fixture.rms.configure_switch_certificate_calls().await.len(),
        1
    );
    let polls = fixture
        .rms
        .get_configure_switch_certificate_job_status_calls()
        .await;
    assert_eq!(polls.len(), 4);
    assert!(polls.iter().all(|poll| poll.job_id == "certificate-job"));

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
        "expected the BMC firmware job after certificate rotation, got {state:?}"
    );
    assert_eq!(firmware_uploads(), 1.0);
    Ok(())
}

#[sqlx_test]
async fn dual_stack_aliases_only_run_preingestion_on_ipv4(pool: PgPool) -> TestResult {
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
        PreingestionState::Initial,
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    let manager = fixture.manager(&pool);
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        PreingestionState::InitialBMCReset {
            phase: InitialBmcResetPhase::Start { attempts: 0 },
        }
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );

    // Advance the admitted primary past reset and time synchronization.
    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_preingestion_for_addresses(
        &[fixture.bmc_ip],
        check_state(),
        &mut txn,
    )
    .await?;
    txn.commit().await?;
    manager.run_single_iteration().await?;
    assert_eq!(fixture.state(&pool).await?, rotation_state(0, None));
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );
    assert_no_controller_operation(&pool, &fixture).await?;

    enqueue_rotation(&fixture, "shared-password-job").await;
    manager.run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        rotation_state(0, Some("shared-password-job"))
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );
    fixture
        .rms
        .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
            "shared-password-job",
            rms::JobExecutionState::Completed,
        )))
        .await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        certificate_state(SwitchCertificateRotationState::DisableMtls { job_id: None })
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );
    assert_eq!(operation(&pool, &fixture).await?.current_version, Some(0));
    assert_eq!(
        fixture.credentials().await?,
        Some(Credentials::new("nvos-admin", TARGET_PASSWORD))
    );

    enqueue_mtls_disable(&fixture, "shared-disable-job").await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        certificate_state(SwitchCertificateRotationState::DisableMtls {
            job_id: Some("shared-disable-job".to_string()),
        })
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );
    enqueue_mtls_completed(&fixture, "shared-disable-job").await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        certificate_state(SwitchCertificateRotationState::ConfigureCertificate { job_id: None })
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );
    let calls = fixture.rms.batch_disable_switch_mtls_calls().await;
    assert_eq!(calls.len(), 1);
    let node_id = &calls[0].nodes.as_ref().unwrap().nodes[0].node_id;
    enqueue_certificate(&fixture, node_id, "shared-certificate-job").await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        certificate_state(SwitchCertificateRotationState::ConfigureCertificate {
            job_id: Some("shared-certificate-job".to_string()),
        })
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );
    fixture
        .rms
        .enqueue_get_configure_switch_certificate_job_status(Ok(
            MockRmsApi::configure_switch_certificate_job_status_ok("completed"),
        ))
        .await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        PreingestionState::CheckFirmware
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Initial
    );

    // The secondary becomes ingestible only after the primary finishes all preingestion.
    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_preingestion_complete(fixture.bmc_ip, txn.as_mut()).await?;
    txn.commit().await?;
    fixture.manager(&pool).run_single_iteration().await?;
    for address in [fixture.bmc_ip, ipv6] {
        assert_eq!(
            fixture.state_at(&pool, address).await?,
            PreingestionState::Complete
        );
    }
    assert_eq!(
        fixture
            .rms
            .update_switch_system_password_calls()
            .await
            .len(),
        1
    );
    assert_eq!(fixture.rms.batch_disable_switch_mtls_calls().await.len(), 1);
    assert_eq!(
        fixture.rms.configure_switch_certificate_calls().await.len(),
        1
    );
    let polls = fixture.rms.get_job_status_calls().await;
    assert_eq!(polls.len(), 2);
    assert_eq!(
        polls
            .iter()
            .filter(|poll| poll.job_id == "shared-password-job")
            .count(),
        1
    );
    assert_eq!(
        polls
            .iter()
            .filter(|poll| poll.job_id == "shared-disable-job")
            .count(),
        1
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
        certificate_state(SwitchCertificateRotationState::DisableMtls { job_id: None })
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
        certificate_state(SwitchCertificateRotationState::DisableMtls { job_id: None })
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
async fn confirmed_version_with_completed_secondary_still_starts_tls(pool: PgPool) -> TestResult {
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
    db::explored_endpoints::set_preingestion_complete(ipv6, txn.as_mut()).await?;
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
        certificate_state(SwitchCertificateRotationState::DisableMtls { job_id: None })
    );
    assert_eq!(
        fixture.state_at(&pool, ipv6).await?,
        PreingestionState::Complete
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
            certificate_state(SwitchCertificateRotationState::DisableMtls { job_id: None }),
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

#[sqlx_test]
async fn certificate_failures_block_firmware_without_repeating_mutations(
    pool: PgPool,
) -> TestResult {
    enum Response {
        DisableSubmissionUnknown,
        CertificateSubmissionUnknown,
        DisableMissingJobId,
        CertificateMissingJobId,
        DisableUnsupported,
        DisableFailed,
        CertificateFailed,
    }

    struct Case {
        scenario: &'static str,
        state: SwitchCertificateRotationState,
        response: Response,
        submissions: (usize, usize),
        error_contains: Option<&'static str>,
    }

    let cases = [
        Case {
            scenario: "mTLS submission response is lost",
            state: SwitchCertificateRotationState::DisableMtls { job_id: None },
            response: Response::DisableSubmissionUnknown,
            submissions: (1, 0),
            error_contains: Some("lost mTLS submission response"),
        },
        Case {
            scenario: "certificate submission response is lost",
            state: SwitchCertificateRotationState::ConfigureCertificate { job_id: None },
            response: Response::CertificateSubmissionUnknown,
            submissions: (0, 1),
            error_contains: Some("lost certificate submission response"),
        },
        Case {
            scenario: "mTLS submission succeeds without a job ID",
            state: SwitchCertificateRotationState::DisableMtls { job_id: None },
            response: Response::DisableMissingJobId,
            submissions: (1, 0),
            error_contains: Some("returned no job ID"),
        },
        Case {
            scenario: "certificate submission succeeds without a job ID",
            state: SwitchCertificateRotationState::ConfigureCertificate { job_id: None },
            response: Response::CertificateMissingJobId,
            submissions: (0, 1),
            error_contains: Some("returned no job id"),
        },
        Case {
            scenario: "mTLS removal is unsupported",
            state: SwitchCertificateRotationState::DisableMtls { job_id: None },
            response: Response::DisableUnsupported,
            submissions: (1, 0),
            error_contains: Some("RMS does not support switch mTLS disable"),
        },
        Case {
            scenario: "mTLS job failed",
            state: SwitchCertificateRotationState::DisableMtls {
                job_id: Some("failed-disable-job".to_string()),
            },
            response: Response::DisableFailed,
            submissions: (0, 0),
            error_contains: None,
        },
        Case {
            scenario: "certificate job failed",
            state: SwitchCertificateRotationState::ConfigureCertificate {
                job_id: Some("failed-certificate-job".to_string()),
            },
            response: Response::CertificateFailed,
            submissions: (0, 0),
            error_contains: None,
        },
    ];

    let mut fixture = Fixture::new(&pool, true).await?;
    fixture.firmware_updates_enabled = true;
    fixture
        .store_credentials(&Credentials::new("nvos-admin", TARGET_PASSWORD))
        .await;
    let metrics = carbide_instrument::testing::MetricsCapture::for_registry(
        fixture.env.test_meter.registry(),
    );
    for case in cases {
        let mut txn = pool.txn_begin().await?;
        db::explored_endpoints::set_preingestion_for_addresses(
            &[fixture.bmc_ip],
            certificate_state(case.state),
            &mut txn,
        )
        .await?;
        txn.commit().await?;
        match case.response {
            Response::DisableSubmissionUnknown => {
                fixture
                    .rms
                    .enqueue_batch_disable_switch_mtls(Err(tonic::Status::unavailable(
                        "lost mTLS submission response",
                    )
                    .into()))
                    .await
            }
            Response::CertificateSubmissionUnknown => {
                fixture
                    .rms
                    .enqueue_configure_switch_certificate(Err(tonic::Status::unavailable(
                        "lost certificate submission response",
                    )
                    .into()))
                    .await
            }
            Response::DisableMissingJobId => enqueue_mtls_disable(&fixture, "").await,
            Response::CertificateMissingJobId => {
                fixture
                    .rms
                    .enqueue_configure_switch_certificate(Ok(
                        rms::ConfigureSwitchCertificateResponse {
                            response: Some(rms::NodeBatchResponse {
                                status: rms::ReturnCode::Success as i32,
                                ..Default::default()
                            }),
                            jobs: Vec::new(),
                        },
                    ))
                    .await
            }
            Response::DisableUnsupported => {
                fixture
                    .rms
                    .enqueue_batch_disable_switch_mtls(Err(tonic::Status::unimplemented(
                        "unknown method",
                    )
                    .into()))
                    .await
            }
            Response::DisableFailed => {
                fixture
                    .rms
                    .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
                        "failed-disable-job",
                        rms::JobExecutionState::Failed,
                    )))
                    .await
            }
            Response::CertificateFailed => {
                fixture
                    .rms
                    .enqueue_get_configure_switch_certificate_job_status(Ok(
                        MockRmsApi::configure_switch_certificate_job_status_ok("failed"),
                    ))
                    .await;
                fixture
                    .rms
                    .enqueue_get_job_status(Ok(MockRmsApi::job_status_ok(
                        "failed-certificate-job",
                        rms::JobExecutionState::Failed,
                    )))
                    .await;
            }
        }
        let disable_before = fixture.rms.batch_disable_switch_mtls_calls().await.len();
        let certificate_before = fixture.rms.configure_switch_certificate_calls().await.len();
        let (result, logs) = carbide_instrument::testing::capture_logs_async(
            fixture.manager(&pool).run_single_iteration(),
        )
        .await;
        result?;
        let failed = fixture.state(&pool).await?;
        assert!(
            matches!(&failed, PreingestionState::Failed { reason } if !reason.is_empty()),
            "{}: {failed:?}",
            case.scenario
        );
        if let Some(expected_error) = case.error_contains {
            assert!(
                matches!(&failed, PreingestionState::Failed { reason } if reason.contains(expected_error)),
                "{}: {failed:?}",
                case.scenario
            );
            assert!(
                logs.iter().any(|log| {
                    log.message == "Error handling preingestion update"
                        && log
                            .field("error")
                            .is_some_and(|error| error.contains(expected_error))
                }),
                "{}: expected returned TLS error, got {logs:?}",
                case.scenario
            );
        }
        // A fresh manager reloads terminal failure and must not retry either mutation.
        fixture.manager(&pool).run_single_iteration().await?;
        assert_eq!(fixture.state(&pool).await?, failed, "{}", case.scenario);
        assert_eq!(
            fixture.rms.batch_disable_switch_mtls_calls().await.len() - disable_before,
            case.submissions.0,
            "{}",
            case.scenario
        );
        assert_eq!(
            fixture.rms.configure_switch_certificate_calls().await.len() - certificate_before,
            case.submissions.1,
            "{}",
            case.scenario
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_preingestion_firmware_upload_total",
                &[("outcome", "ok"), ("method", "multipart")]
            ),
            0.0,
            "{}",
            case.scenario
        );
    }
    Ok(())
}

#[sqlx_test]
async fn certificate_prerequisite_errors_preserve_phase_and_retry_after_repair(
    pool: PgPool,
) -> TestResult {
    let mut fixture = Fixture::new(&pool, true).await?;
    fixture
        .store_credentials(&Credentials::new("nvos-admin", TARGET_PASSWORD))
        .await;
    for rotation_state in [
        SwitchCertificateRotationState::DisableMtls { job_id: None },
        SwitchCertificateRotationState::ConfigureCertificate { job_id: None },
    ] {
        let disable = matches!(
            rotation_state,
            SwitchCertificateRotationState::DisableMtls { .. }
        );
        let entry = certificate_state(rotation_state);
        let disable_before = fixture.rms.batch_disable_switch_mtls_calls().await.len();
        let certificate_before = fixture.rms.configure_switch_certificate_calls().await.len();
        for (scenario, rack_id, services, expected_error) in [
            (
                "no rack association",
                None,
                SwitchMtlsService::default_services(),
                "switch certificate rotation requires a rack association",
            ),
            (
                "only primary-switch services selected",
                Some("preingestion-switch"),
                vec![
                    SwitchMtlsService::ScaleUpFabricManager,
                    SwitchMtlsService::ScaleUpFabricTelemetry,
                ],
                "switch certificate rotation has no applicable mTLS services",
            ),
        ] {
            let scenario = format!("{entry:?}: {scenario}");
            fixture.switch_mtls_services = switch_mtls_services_as_i32(&services);
            let mut txn = pool.txn_begin().await?;
            sqlx::query("UPDATE expected_switches SET rack_id = $2 WHERE bmc_mac_address = $1")
                .bind(fixture.bmc_mac)
                .bind(rack_id)
                .execute(txn.as_mut())
                .await?;
            db::explored_endpoints::set_preingestion_for_addresses(
                &[fixture.bmc_ip],
                entry.clone(),
                &mut txn,
            )
            .await?;
            txn.commit().await?;
            let (result, logs) = carbide_instrument::testing::capture_logs_async(
                fixture.manager(&pool).run_single_iteration(),
            )
            .await;
            result?;
            assert_eq!(fixture.state(&pool).await?, entry, "{scenario}");
            let expected_error = format!(
                "internal error: {expected_error} for BMC {}",
                fixture.bmc_mac
            );
            assert!(
                logs.iter().any(|log| {
                    log.message == "Error handling preingestion update"
                        && log.field("error") == Some(expected_error.as_str())
                }),
                "{scenario}: expected prerequisite error, got {logs:?}"
            );
            assert_eq!(
                fixture.rms.batch_disable_switch_mtls_calls().await.len(),
                disable_before,
                "{scenario}"
            );
            assert_eq!(
                fixture.rms.configure_switch_certificate_calls().await.len(),
                certificate_before,
                "{scenario}"
            );
        }

        fixture.switch_mtls_services =
            switch_mtls_services_as_i32(&SwitchMtlsService::default_services());
        let job_id = "prerequisites-repaired-job";
        let next_rotation_state = if disable {
            enqueue_mtls_disable(&fixture, job_id).await;
            SwitchCertificateRotationState::DisableMtls {
                job_id: Some(job_id.to_string()),
            }
        } else {
            fixture
                .rms
                .enqueue_configure_switch_certificate(Ok(rms::ConfigureSwitchCertificateResponse {
                    response: Some(rms::NodeBatchResponse {
                        status: rms::ReturnCode::Success as i32,
                        job_id: job_id.to_string(),
                        ..Default::default()
                    }),
                    jobs: Vec::new(),
                }))
                .await;
            SwitchCertificateRotationState::ConfigureCertificate {
                job_id: Some(job_id.to_string()),
            }
        };
        fixture.manager(&pool).run_single_iteration().await?;
        assert_eq!(
            fixture.state(&pool).await?,
            certificate_state(next_rotation_state)
        );
        assert_eq!(
            fixture.rms.batch_disable_switch_mtls_calls().await.len() - disable_before,
            usize::from(disable)
        );
        assert_eq!(
            fixture.rms.configure_switch_certificate_calls().await.len() - certificate_before,
            usize::from(!disable)
        );
    }
    Ok(())
}

#[sqlx_test]
async fn certificate_preparation_failure_requires_reset_after_rack_profile_repair(
    pool: PgPool,
) -> TestResult {
    let fixture = Fixture::new(&pool, true).await?;
    fixture
        .store_credentials(&Credentials::new("nvos-admin", TARGET_PASSWORD))
        .await;
    let entry = certificate_state(SwitchCertificateRotationState::DisableMtls { job_id: None });
    let mut txn = pool.txn_begin().await?;
    sqlx::query("UPDATE expected_racks SET rack_profile_id = 'missing-profile' WHERE rack_id = $1")
        .bind("preingestion-switch")
        .execute(txn.as_mut())
        .await?;
    db::explored_endpoints::set_preingestion_for_addresses(
        &[fixture.bmc_ip],
        entry.clone(),
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    let (result, logs) = carbide_instrument::testing::capture_logs_async(
        fixture.manager(&pool).run_single_iteration(),
    )
    .await;
    result?;
    let failed = fixture.state(&pool).await?;
    assert!(
        matches!(&failed, PreingestionState::Failed { reason } if reason.contains("missing-profile")),
        "{failed:?}"
    );
    assert!(
        logs.iter().any(|log| {
            log.message == "Error handling preingestion update"
                && log.field("error").is_some_and(|error| {
                    error.contains("rejected before dispatch") && error.contains("missing-profile")
                })
        }),
        "expected rejection before dispatch, got {logs:?}"
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

    sqlx::query("UPDATE expected_racks SET rack_profile_id = $2 WHERE rack_id = $1")
        .bind("preingestion-switch")
        .bind(PROFILE_ID)
        .execute(&pool)
        .await?;
    enqueue_mtls_disable(&fixture, "repaired-disable-job").await;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(fixture.state(&pool).await?, failed);
    assert!(
        fixture
            .rms
            .batch_disable_switch_mtls_calls()
            .await
            .is_empty()
    );

    // Repair alone does not resume a failed submission; explicitly reset the phase.
    let mut txn = pool.txn_begin().await?;
    db::explored_endpoints::set_preingestion_for_addresses(&[fixture.bmc_ip], entry, &mut txn)
        .await?;
    txn.commit().await?;
    fixture.manager(&pool).run_single_iteration().await?;
    assert_eq!(
        fixture.state(&pool).await?,
        certificate_state(SwitchCertificateRotationState::DisableMtls {
            job_id: Some("repaired-disable-job".to_string()),
        })
    );
    assert_eq!(fixture.rms.batch_disable_switch_mtls_calls().await.len(), 1);
    assert!(
        fixture
            .rms
            .configure_switch_certificate_calls()
            .await
            .is_empty()
    );
    Ok(())
}
