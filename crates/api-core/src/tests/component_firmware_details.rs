// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::rpc::forge::component_firmware_expected::Result as Expected;
use ::rpc::forge::component_firmware_installed::Result as Installed;
use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::get_component_firmware_details_request::Target;
use ::rpc::forge::{self as rpc};
use api_test_helper::mock_rms::MockRmsApi;
use carbide_instrument::testing::capture_logs_async;
use carbide_rack_controller::firmware_object::FirmwareObjectFetcher;
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    BmcCredentialType, CompositeCredentialManager, CredentialKey, CredentialReader,
    CredentialWriter, Credentials,
};
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use carbide_uuid::rack::RackId;
use component_manager::component_manager::ComponentManager;
use component_manager::rms::RmsBackend;
use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_machine::ExpectedMachineData;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::machine_interface::InterfaceType;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::power_shelf::NewPowerShelf;
use model::rack_type::RackFirmwareObjectConfig;
use model::switch::{NewSwitch, SwitchConfig};
use model::test_support::{TEST_RMS_RACK_PROFILE_ID, power_shelf_config};
use tonic::Request;

use crate::api::{Api, RequestReceivedAt};
use crate::cfg::file::CarbideConfig;
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;
use crate::tests::common::api_fixtures::{TestEnv, create_test_env, get_config_with_rack_profiles};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const PRIVATE: &str = "private-firmware-manifest-or-diagnostic";

/// Role-specific IDs and management links make wrong-reader routing observable.
#[derive(Clone, Copy)]
enum Role {
    Compute,
    Switch,
    Shelf,
}

/// One managed target with its original typed public selection and BMC link.
struct TargetFixture {
    id: String,
    target: Target,
    mac: MacAddress,
    bmc_interface: carbide_uuid::machine::MachineInterfaceId,
    nvos_interface: Option<carbide_uuid::machine::MachineInterfaceId>,
}

/// Fetching must begin after preparation releases the single pooled session.
/// A fixed private failure also exercises the public manifest redaction boundary.
#[derive(Debug)]
struct ReadFetcher {
    pool: sqlx::PgPool,
    fail: bool,
    calls: AtomicUsize,
    requested_urls: Mutex<Vec<String>>,
    responses: Mutex<HashMap<String, Result<String, String>>>,
}

/// Counts credential reads before the shared helper's expected-inventory
/// fallback. A BMC failure models a secret-store error containing private text;
/// the composite's reader side does not read its writer side.
#[derive(Default)]
struct CountingCredentials {
    calls: AtomicUsize,
    nvos_calls: AtomicUsize,
    fail_bmc: bool,
}

#[async_trait::async_trait]
impl CredentialReader for CountingCredentials {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(key, CredentialKey::SwitchNvosAdmin { .. }) {
            self.nvos_calls.fetch_add(1, Ordering::SeqCst);
        }
        if self.fail_bmc && matches!(key, CredentialKey::BmcCredentials { .. }) {
            return Err(SecretsError::GenericError(eyre::eyre!(
                "vault/private-credential-key: {PRIVATE}"
            )));
        }
        Ok(None)
    }
}

#[async_trait::async_trait]
impl FirmwareObjectFetcher for ReadFetcher {
    async fn fetch(&self, url: &str, _: Duration) -> Result<String, String> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|_| "test fetch could not borrow released database session".to_string())?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requested_urls.lock().unwrap().push(url.to_owned());
        if let Some(response) = self.responses.lock().unwrap().get(url).cloned() {
            return response;
        }
        if self.fail {
            Err(PRIVATE.into())
        } else {
            Ok(r#"{"Id":"configured-object"}"#.into())
        }
    }
}

/// Each credential read waits on its own owned receiver. Tests can observe the
/// admission count, release one workflow, and prove cancellation closes all peers.
struct GatedCredentials {
    arrivals: tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
}

#[async_trait::async_trait]
impl CredentialReader for GatedCredentials {
    async fn get_credentials(
        &self,
        _: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let (release, wait) = tokio::sync::oneshot::channel();
        self.arrivals
            .send(release)
            .expect("test observes credential arrivals");
        wait.await.expect("test releases or cancels the owned read");
        Ok(Some(Credentials::new(
            "firmware-reader",
            "private-password",
        )))
    }
}

/// Configured capability counts match the fixture's two hosts, switch and shelf.
fn firmware_config() -> CarbideConfig {
    let mut config = get_config_with_rack_profiles();
    config.firmware_global.firmware_directory = Default::default();
    let profile = config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap();
    profile.rack_capabilities.compute.count = 2;
    profile.rack_capabilities.switch.count = 1;
    profile.rack_capabilities.power_shelf.count = 1;
    profile.firmware_object = Some(RackFirmwareObjectConfig {
        url: "https://firmware.invalid/object?token=private-source"
            .parse()
            .unwrap(),
        access_token_credential: None,
        fetch_timeout: Duration::from_secs(30),
    });
    config
}

/// Seeds the same interface association discovery owns, including an NVOS Data
/// link on switches. An endpoint is not authorized by its MAC alone.
async fn interface(
    txn: &mut sqlx::PgConnection,
    mac: MacAddress,
    owner: MachineInterfaceAssociation,
    role: InterfaceType,
) -> TestResult<carbide_uuid::machine::MachineInterfaceId> {
    let segment = db::network_segment::find_by_name(&mut *txn, "UNDERLAY").await?;
    let interface = db::machine_interface::create_with_type(
        &mut *txn,
        &[segment],
        &mac,
        false,
        AddressSelectionStrategy::NextAvailableIp,
        role,
        None,
    )
    .await?;
    db::machine_interface::associate_interface_with_machine(&interface.id, owner, txn).await?;
    Ok(interface.id)
}

/// Minimal real DB inventory for each public target arm; no controller loop or
/// simulator default response supplies the behavior under test.
async fn seed_target(
    pool: &sqlx::PgPool,
    rack: &RackId,
    role: Role,
    seed: u8,
) -> TestResult<TargetFixture> {
    let mac = MacAddress::new([2, 0, 0, 0, 0, seed]);
    let mut txn = pool.begin().await?;
    let (id, target, association, nvos_interface) = match role {
        Role::Compute => {
            let id = HostMachineId::try_from(MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [seed; 32],
                MachineType::Host,
            ))?;
            db::machine::create(
                &mut txn,
                None,
                id.as_machine_id(),
                ManagedHostState::Ready,
                Some(&ExpectedMachineData {
                    rack_id: Some(rack.clone()),
                    ..Default::default()
                }),
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            // Stored replacement intent is visible independently of live reads.
            db::host_machine_update::trigger_host_reprovisioning_request(&mut txn, "operator", &id)
                .await?;
            (
                id.to_string(),
                Target::MachineIds(::rpc::common::HostMachineIdList {
                    machine_ids: vec![id],
                }),
                MachineInterfaceAssociation::Machine(id.into()),
                None,
            )
        }
        Role::Switch => {
            let id = carbide_uuid::switch::SwitchId::from(uuid::Uuid::from_u128(seed.into()));
            let nvos_mac = MacAddress::new([2, 0, 0, 0, 1, seed]);
            db::expected_switch::create(
                &mut txn,
                ExpectedSwitch {
                    bmc_mac_address: mac,
                    nvos_interfaces: vec![ExpectedNvosInterface {
                        mac_address: nvos_mac,
                        is_primary: true,
                    }],
                    serial_number: format!("firmware-switch-{seed}"),
                    rack_id: Some(rack.clone()),
                    ..Default::default()
                },
            )
            .await?;
            db::switch::create(
                &mut txn,
                &NewSwitch {
                    id,
                    config: SwitchConfig {
                        name: id.to_string(),
                        enable_nmxc: false,
                        fabric_manager_config: None,
                    },
                    bmc_mac_address: Some(mac),
                    metadata: None,
                    rack_id: Some(rack.clone()),
                    slot_number: None,
                    tray_index: None,
                },
            )
            .await?;
            let nvos = interface(
                &mut txn,
                nvos_mac,
                MachineInterfaceAssociation::Switch(id),
                InterfaceType::Data,
            )
            .await?;
            (
                id.to_string(),
                Target::SwitchIds(rpc::SwitchIdList { ids: vec![id] }),
                MachineInterfaceAssociation::Switch(id),
                Some(nvos),
            )
        }
        Role::Shelf => {
            let id =
                carbide_uuid::power_shelf::PowerShelfId::from(uuid::Uuid::from_u128(seed.into()));
            db::expected_power_shelf::create(
                &mut txn,
                ExpectedPowerShelf {
                    bmc_mac_address: mac,
                    ..Default::default()
                },
            )
            .await?;
            db::power_shelf::create(
                &mut txn,
                &NewPowerShelf {
                    id,
                    config: power_shelf_config("firmware-shelf"),
                    bmc_mac_address: Some(mac),
                    metadata: None,
                    rack_id: Some(rack.clone()),
                },
            )
            .await?;
            (
                id.to_string(),
                Target::PowerShelfIds(rpc::PowerShelfIdList { ids: vec![id] }),
                MachineInterfaceAssociation::PowerShelf(id),
                None,
            )
        }
    };
    let bmc_interface = interface(&mut txn, mac, association, InterfaceType::Bmc).await?;
    txn.commit().await?;
    Ok(TargetFixture {
        id,
        target,
        mac,
        bmc_interface,
        nvos_interface,
    })
}

/// Selects the concrete owner field for a fixture's already typed ID.
fn target_owner(target: &TargetFixture) -> MachineInterfaceAssociation {
    match &target.target {
        Target::MachineIds(ids) => MachineInterfaceAssociation::Machine(ids.machine_ids[0].into()),
        Target::SwitchIds(ids) => MachineInterfaceAssociation::Switch(ids.ids[0]),
        Target::PowerShelfIds(ids) => MachineInterfaceAssociation::PowerShelf(ids.ids[0]),
    }
}

/// All three roles use the same mock service but their actual configured reader.
/// The one-slot API pool makes a held preparation session fail the fetch proof.
async fn api(
    env: &TestEnv,
    pool: sqlx::PgPool,
    config: CarbideConfig,
    mock: Arc<MockRmsApi>,
    fail_manifest: bool,
) -> TestResult<(Api, Arc<ReadFetcher>)> {
    let reader = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let backend = Arc::new(RmsBackend::new(
        mock.clone(),
        None,
        reader.clone(),
        Arc::new(config.rack_profiles.clone()),
        false,
    ));
    let manager = ComponentManager::new(
        backend.clone(),
        backend.clone(),
        backend,
        false,
        false,
        false,
    );
    let fetcher = Arc::new(ReadFetcher {
        pool: reader.clone(),
        fail: fail_manifest,
        calls: AtomicUsize::new(0),
        requested_urls: Mutex::new(Vec::new()),
        responses: Mutex::new(HashMap::new()),
    });
    let api = TestApiBuilder::new(
        reader,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(Arc::new(config))
    .with_credential_manager(env.api.credential_manager.clone())
    .with_component_manager(Arc::new(manager))
    .with_rms_client(mock)
    .with_firmware_object_fetcher(fetcher.clone())
    .build();
    Ok((api, fetcher))
}

/// Explicit mock responses prove live installed, logical expected and current
/// backend observations without depending on a simulator's unsupported defaults.
async fn replies(mock: &MockRmsApi, id: &str, inventory_fails: bool, job_fails: bool) {
    mock.enqueue_batch_get_firmware_inventory(if inventory_fails {
        Err(librms::RackManagerError::ApiInvocationError(
            tonic::Status::unavailable(PRIVATE),
        ))
    } else {
        Ok(inventory_reply(&[(id, "installed-v1")]))
    })
    .await;
    mock.enqueue_compare_firmware_object(Ok(comparison_reply(id, "expected-v2")))
        .await;
    mock.enqueue_get_firmware_job_status(if job_fails {
        Err(librms::RackManagerError::ApiInvocationError(
            tonic::Status::unavailable(PRIVATE),
        ))
    } else {
        Ok(rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            job_id: format!("job-{id}"),
            job_state: rms::FirmwareJobState::Running as i32,
            error_message: PRIVATE.into(),
            ..Default::default()
        })
    })
    .await;
}

/// Inventory replies retain node identities so concurrent singleton calls can
/// select their own version independently of which mock reply they consume.
fn inventory_reply(nodes: &[(&str, &str)]) -> rms::BatchGetFirmwareInventoryResponse {
    rms::BatchGetFirmwareInventoryResponse {
        response: Some(rms::NodeBatchResponse {
            status: rms::ReturnCode::Success as i32,
            ..Default::default()
        }),
        nodes: nodes
            .iter()
            .map(|(id, version)| rms::NodeFirmwareInventory {
                node_id: (*id).into(),
                firmware_list: vec![rms::FirmwareInventoryInfo {
                    name: "BMC".into(),
                    version: (*version).into(),
                    target: PRIVATE.into(),
                    ..Default::default()
                }],
            })
            .collect(),
    }
}

/// One explicit comparison response identifies the configured version and node;
/// source-selection tests vary these values instead of relying on empty defaults.
fn comparison_reply(id: &str, version: &str) -> rms::CompareFirmwareObjectResponse {
    rms::CompareFirmwareObjectResponse {
        response: Some(rms::NodeBatchResponse {
            status: rms::ReturnCode::Success as i32,
            node_results: vec![rms::NodeOperationResult {
                node_id: id.into(),
                status: rms::ReturnCode::Success as i32,
                ..Default::default()
            }],
            ..Default::default()
        }),
        nodes: vec![rms::NodeFirmwareManifestComparison {
            node_id: id.into(),
            components: vec![rms::FirmwareManifestComponentComparison {
                component: "BMC".into(),
                proposed_version: version.into(),
                status: rms::FirmwareComparisonStatus::Mismatched as i32,
                reason: PRIVATE.into(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Seeds credentials and a durable reference without invoking a firmware writer.
async fn seed_credentials_and_tracking(
    env: &TestEnv,
    pool: &sqlx::PgPool,
    target: &TargetFixture,
) -> TestResult {
    for key in [
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: target.mac,
            },
        },
        CredentialKey::SwitchNvosAdmin {
            bmc_mac_address: target.mac,
        },
    ] {
        env.api
            .credential_manager
            .set_credentials(
                &key,
                &Credentials::new("firmware-reader", "private-password"),
            )
            .await
            .expect("firmware test credentials can be stored");
    }
    db::direct_dispatch_rms_job::save(
        pool,
        target.mac,
        db::direct_dispatch_rms_job::RmsJobKind::FirmwareObject,
        &format!("job-{}", target.id),
    )
    .await?;
    Ok(())
}

/// Every public target arm reaches its configured passive reader and comparison
/// descriptor, preserving independent receipt/source and exact caller identity.
#[crate::sqlx_test]
async fn component_firmware_details_wire_all_roles_and_preserve_independent_failures(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let config = firmware_config();
    let rack = RackId::new("firmware-public");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let host = seed_target(&pool, &rack, Role::Compute, 61).await?;
    let second_host = seed_target(&pool, &rack, Role::Compute, 62).await?;
    let switch = seed_target(&pool, &rack, Role::Switch, 63).await?;
    let shelf = seed_target(&pool, &rack, Role::Shelf, 64).await?;
    for target in [&host, &second_host, &switch, &shelf] {
        seed_credentials_and_tracking(&env, &pool, target).await?;
    }

    struct Case<'a> {
        name: &'static str,
        target: &'a TargetFixture,
        inventory_fails: bool,
        manifest_fails: bool,
        job_fails: bool,
    }
    for Case {
        name,
        target,
        inventory_fails,
        manifest_fails,
        job_fails,
    } in [
        Case {
            name: "compute",
            target: &host,
            inventory_fails: false,
            manifest_fails: false,
            job_fails: false,
        },
        Case {
            name: "switch",
            target: &switch,
            inventory_fails: false,
            manifest_fails: false,
            job_fails: false,
        },
        Case {
            name: "shelf",
            target: &shelf,
            inventory_fails: false,
            manifest_fails: false,
            job_fails: false,
        },
        Case {
            name: "installed failure",
            target: &host,
            inventory_fails: true,
            manifest_fails: false,
            job_fails: false,
        },
        Case {
            name: "expected failure",
            target: &host,
            inventory_fails: false,
            manifest_fails: true,
            job_fails: false,
        },
        Case {
            name: "backend failure",
            target: &host,
            inventory_fails: false,
            manifest_fails: false,
            job_fails: true,
        },
    ] {
        let mock = Arc::new(MockRmsApi::new());
        replies(&mock, &target.id, inventory_fails, job_fails).await;
        let (api, fetcher) = api(
            &env,
            pool.clone(),
            config.clone(),
            mock.clone(),
            manifest_fails,
        )
        .await?;

        let (response, logs) = capture_logs_async(api.get_component_firmware_details(
            Request::new(rpc::GetComponentFirmwareDetailsRequest {
                target: Some(target.target.clone()),
            }),
        ))
        .await;
        let response = response?.into_inner();
        let [entry] = response.entries.as_slice() else {
            panic!("{name}: one entry")
        };

        assert_eq!(entry.component_id, target.id, "{name}");
        let actual = entry.actual.as_ref().unwrap();
        assert_eq!(
            actual.source,
            Some(rpc::ComponentFirmwareSource::Rms as i32)
        );
        if inventory_fails {
            assert!(
                matches!(&actual.result, Some(Installed::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::ReadFailed as i32)
            );
            assert!(actual.observed_at.is_none());
            // The RMS reader logs this RPC failure before returning a typed
            // per-device error inside a successful batch-reader reply.
            assert!(
                logs.iter().any(|log| {
                    log.target == "component_manager::rms::firmware_inventory"
                        && log.level == tracing::Level::DEBUG
                        && log.message == "RMS firmware inventory RPC failed"
                        && log.field("category") == Some("rpc")
                        && log.field("grpc_status_code") == Some("Unavailable")
                        && log.field("rack_id") == Some(rack.as_str())
                        && log.field("node_count") == Some("1")
                }),
                "positive diagnostic from the reader that owns the failure"
            );
        } else {
            let Some(Installed::Inventory(rows)) = &actual.result else {
                panic!("{name}: installed inventory")
            };
            let Some(rpc::component_firmware_inventory_entry::Result::Component(component)) =
                &rows.entries[0].result
            else {
                panic!("{name}: readable member")
            };
            assert_eq!(component.version.as_deref(), Some("installed-v1"));
            assert!(actual.observed_at.is_some());
        }
        let desired = entry.desired.as_ref().unwrap();
        assert_eq!(
            desired.source,
            Some(rpc::ComponentFirmwareSource::Rms as i32)
        );
        if manifest_fails {
            assert!(
                matches!(&desired.result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::ManifestUnavailable as i32)
            );
            assert!(desired.observed_at.is_none());
        } else {
            let Some(Expected::Comparisons(rows)) = &desired.result else {
                panic!("{name}: expected comparison")
            };
            assert_eq!(
                rows.entries[0].proposed_version.as_deref(),
                Some("expected-v2")
            );
            assert_eq!(
                rows.entries[0].status,
                rpc::ComponentFirmwareComparisonStatus::Mismatched as i32
            );
            assert!(desired.observed_at.is_some());
        }
        let work = entry.actions.as_ref().unwrap().backend.as_ref().unwrap();
        assert_eq!(work.source, Some(rpc::ComponentFirmwareSource::Rms as i32));
        assert!(work.observed_at.is_some());
        if job_fails {
            assert_eq!(
                work.unresolved_references[0].error.as_ref().unwrap().code,
                rpc::ComponentFirmwareErrorCode::PollFailed as i32
            );
        } else {
            assert_eq!(work.active_jobs[0].job_id, format!("job-{}", target.id));
            assert_eq!(
                work.active_jobs[0].kind,
                rpc::ComponentFirmwareJobKind::FirmwareObject as i32
            );
            assert_eq!(
                work.active_jobs[0].state,
                rpc::ComponentFirmwareActiveJobState::InProgress as i32
            );
        }
        if matches!(target.target, Target::MachineIds(_)) {
            assert_eq!(
                entry
                    .actions
                    .as_ref()
                    .unwrap()
                    .stored
                    .as_ref()
                    .unwrap()
                    .outstanding_requests
                    .len(),
                1
            );
        }
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1, "{name}");
        assert_eq!(
            mock.get_firmware_job_status_calls().await.len(),
            1,
            "{name}"
        );
        for marker in [
            PRIVATE,
            "private-password",
            "private-source",
            "configured-object",
        ] {
            assert!(
                !format!("{response:?}").contains(marker),
                "{name}: {marker}"
            );
            assert!(
                logs.iter().all(|log| !format!("{log:?}").contains(marker)),
                "{name}: {marker}"
            );
        }
        assert!(
            mock.apply_firmware_object_calls().await.is_empty(),
            "{name}"
        );
        let installed = mock.batch_get_firmware_inventory_calls().await;
        assert_eq!(
            installed[0].nodes.as_ref().unwrap().nodes[0].node_id,
            target.id,
            "{name}"
        );
        if !manifest_fails {
            let compared = mock.compare_firmware_object_calls().await;
            assert_eq!(
                compared[0].nodes.as_ref().unwrap().nodes[0].node_id,
                target.id,
                "{name}"
            );
            assert_eq!(compared[0].config_json, r#"{"Id":"configured-object"}"#);
            if let Some(nvos_interface) = target.nvos_interface {
                let node = &compared[0].nodes.as_ref().unwrap().nodes[0];
                let nvos = node
                    .host_endpoint
                    .as_ref()
                    .expect("owned switch NVOS endpoint");
                let interface = nvos
                    .interface
                    .as_ref()
                    .expect("owned NVOS network identity");
                let mut conn = pool.acquire().await?;
                let expected_ip =
                    db::machine_interface_address::find_for_interface(&mut conn, nvos_interface)
                        .await?[0]
                        .address;
                let expected_mac: MacAddress =
                    sqlx::query_scalar("SELECT mac_address FROM machine_interfaces WHERE id = $1")
                        .bind(nvos_interface)
                        .fetch_one(&mut *conn)
                        .await?;

                assert_eq!(interface.ip_address, expected_ip.to_string());
                assert_eq!(interface.mac_address, expected_mac.to_string());
                let Some(rms::credentials::Auth::UserPass(credentials)) = nvos
                    .credentials
                    .as_ref()
                    .and_then(|credentials| credentials.auth.as_ref())
                else {
                    panic!("owned NVOS credentials accompany the endpoint")
                };
                assert_eq!(credentials.username, "firmware-reader");
                assert_eq!(credentials.password, "private-password");
            }
        }
    }

    // Both real slots share a rack comparison, but each retains distinct
    // successful inventory, desired version and durable job evidence. Inventory
    // calls may race; each identity-rich reply contains both known node values.
    let mock = Arc::new(MockRmsApi::new());
    let inventory = inventory_reply(&[
        (&host.id, "installed-first"),
        (&second_host.id, "installed-second"),
    ]);
    mock.enqueue_batch_get_firmware_inventory(Ok(inventory.clone()))
        .await;
    mock.enqueue_batch_get_firmware_inventory(Ok(inventory))
        .await;
    let mut compared = comparison_reply(&host.id, "expected-first");
    let second = comparison_reply(&second_host.id, "expected-second");
    compared
        .response
        .as_mut()
        .unwrap()
        .node_results
        .extend(second.response.unwrap().node_results);
    compared.nodes.extend(second.nodes);
    // Returned comparison rows are opposite the requested found-slot order.
    mock.enqueue_compare_firmware_object(Ok(compared)).await;
    // Durable reference recovery is serialized in the supplied MAC order.
    for target in [&second_host, &host] {
        mock.enqueue_get_firmware_job_status(Ok(rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            job_id: format!("job-{}", target.id),
            job_state: rms::FirmwareJobState::Running as i32,
            ..Default::default()
        }))
        .await;
    }
    let (api, fetcher) = api(&env, pool.clone(), config, mock.clone(), false).await?;
    let request = rpc::GetComponentFirmwareDetailsRequest {
        target: Some(Target::MachineIds(::rpc::common::HostMachineIdList {
            machine_ids: vec![
                second_host.id.parse()?,
                HostMachineId::try_from(MachineId::new(
                    MachineIdSource::ProductBoardChassisSerial,
                    [199; 32],
                    MachineType::Host,
                ))?,
                host.id.parse()?,
            ],
        })),
    };

    let response = api
        .get_component_firmware_details(Request::new(request))
        .await?
        .into_inner();

    assert_eq!(
        response
            .entries
            .iter()
            .map(|entry| entry.component_id.clone())
            .collect::<Vec<_>>(),
        [
            second_host.id.clone(),
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [199; 32],
                MachineType::Host
            )
            .to_string(),
            host.id.clone()
        ]
    );
    for (index, target, installed, expected) in [
        (0, &second_host, "installed-second", "expected-second"),
        (2, &host, "installed-first", "expected-first"),
    ] {
        let entry = &response.entries[index];
        let Some(Installed::Inventory(inventory)) = &entry.actual.as_ref().unwrap().result else {
            panic!("healthy installed slot {index}")
        };
        let Some(rpc::component_firmware_inventory_entry::Result::Component(component)) =
            &inventory.entries[0].result
        else {
            panic!("healthy installed member {index}")
        };
        assert_eq!(component.version.as_deref(), Some(installed));
        let Some(Expected::Comparisons(rows)) = &entry.desired.as_ref().unwrap().result else {
            panic!("healthy expected slot {index}")
        };
        assert_eq!(rows.entries[0].proposed_version.as_deref(), Some(expected));
        let jobs = &entry
            .actions
            .as_ref()
            .unwrap()
            .backend
            .as_ref()
            .unwrap()
            .active_jobs;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, format!("job-{}", target.id));
    }
    let missing = &response.entries[1];
    assert!(
        matches!(&missing.actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::NotFound as i32)
    );
    assert!(
        matches!(&missing.desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::NotFound as i32)
    );
    assert_eq!(
        missing
            .actions
            .as_ref()
            .unwrap()
            .stored
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .code,
        rpc::ComponentFirmwareErrorCode::NotFound as i32
    );
    assert_eq!(
        missing
            .actions
            .as_ref()
            .unwrap()
            .backend
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .code,
        rpc::ComponentFirmwareErrorCode::NotFound as i32
    );
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(mock.get_firmware_job_status_calls().await.len(), 2);

    // Deleting this owner removes its profile context and prevents another
    // fetch while the host's independent saved request remains visible.
    let mut txn = pool.begin().await?;
    db::rack::mark_as_deleted(&rack, &mut txn).await?;
    txn.commit().await?;

    let response = api
        .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
            target: Some(host.target),
        }))
        .await?
        .into_inner();

    let stored = response.entries[0]
        .actions
        .as_ref()
        .unwrap()
        .stored
        .as_ref()
        .unwrap();
    assert_eq!(
        stored.rack_context_error.as_ref().unwrap().code,
        rpc::ComponentFirmwareErrorCode::Ineligible as i32
    );
    assert_eq!(stored.outstanding_requests.len(), 1);
    assert!(
        matches!(&response.entries[0].desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::Ineligible as i32)
    );
    assert_eq!(
        response.entries[0].desired.as_ref().unwrap().source,
        Some(rpc::ComponentFirmwareSource::Rms as i32)
    );
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

/// Loader-rejected compute/shelf associations and the explicit switch owner/role
/// guard must reject before credentials, manifests or backend observations.
/// Eligible stored-work controls distinguish lifecycle suppression from absence.
#[crate::sqlx_test]
async fn component_firmware_details_reject_unowned_and_ineligible_targets(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let mut config = firmware_config();
    let capabilities = &mut config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .rack_capabilities;
    capabilities.compute.count = 2;
    capabilities.switch.count = 2;
    capabilities.power_shelf.count = 2;
    let rack = RackId::new("firmware-ownership");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let host = seed_target(&pool, &rack, Role::Compute, 71).await?;
    let foreign_host = seed_target(&pool, &rack, Role::Compute, 72).await?;
    let target = seed_target(&pool, &rack, Role::Switch, 73).await?;
    let foreign_switch = seed_target(&pool, &rack, Role::Switch, 74).await?;
    let shelf = seed_target(&pool, &rack, Role::Shelf, 75).await?;
    let foreign_shelf = seed_target(&pool, &rack, Role::Shelf, 76).await?;
    let mock = Arc::new(MockRmsApi::new());
    let (mut api, fetcher) = api(&env, pool.clone(), config, mock.clone(), false).await?;
    let configured_manager = api.component_manager.clone();
    let credentials = Arc::new(CountingCredentials::default());
    api.credential_manager = Arc::new(CompositeCredentialManager::new(
        credentials.clone(),
        env.test_credential_manager.clone(),
    ));

    // The host/shelf loaders omit endpoints after these association changes;
    // the MAC-joined switch endpoint instead reaches the explicit owner guard.
    // All three public paths must stop before credentials.
    for (member, foreign) in [
        (&host, &foreign_host),
        (&target, &foreign_switch),
        (&shelf, &foreign_shelf),
    ] {
        let mut txn = pool.begin().await?;
        db::machine_interface::associate_bmc_interface(
            &member.bmc_interface,
            target_owner(foreign),
            &mut txn,
        )
        .await?;
        txn.commit().await?;

        let response = api
            .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
                target: Some(member.target.clone()),
            }))
            .await?
            .into_inner();

        assert!(
            matches!(&response.entries[0].actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::EndpointUnavailable as i32)
        );
        assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
        assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
        assert!(mock.get_firmware_job_status_calls().await.is_empty());
        let mut txn = pool.begin().await?;
        db::machine_interface::associate_bmc_interface(
            &member.bmc_interface,
            target_owner(member),
            &mut txn,
        )
        .await?;
        txn.commit().await?;
    }
    sqlx::query("UPDATE machine_interfaces SET interface_type = 'Data' WHERE id = $1")
        .bind(target.bmc_interface)
        .execute(&pool)
        .await?;

    let response = api
        .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
            target: Some(target.target.clone()),
        }))
        .await?
        .into_inner();

    assert!(
        matches!(&response.entries[0].actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::EndpointUnavailable as i32)
    );
    assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
    sqlx::query("UPDATE machine_interfaces SET interface_type = 'Bmc' WHERE id = $1")
        .bind(target.bmc_interface)
        .execute(&pool)
        .await?;
    let unsupported = ComponentManager::new(
        Arc::new(component_manager::mock::MockNvSwitchManager::default()),
        Arc::new(component_manager::mock::MockPowerShelfManager),
        Arc::new(component_manager::mock::MockComputeTrayManager),
        false,
        false,
        false,
    );
    for (manager, code) in [
        (
            Some(unsupported),
            rpc::ComponentFirmwareErrorCode::Unsupported,
        ),
        (None, rpc::ComponentFirmwareErrorCode::Disabled),
    ] {
        api.component_manager = manager;

        let response = api
            .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
                target: Some(target.target.clone()),
            }))
            .await?
            .into_inner();

        assert!(
            matches!(&response.entries[0].actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == code as i32)
        );
        assert!(
            response.entries[0]
                .actions
                .as_ref()
                .unwrap()
                .stored
                .as_ref()
                .unwrap()
                .error
                .is_none()
        );
        if code == rpc::ComponentFirmwareErrorCode::Disabled {
            let entry = &response.entries[0];
            assert!(
                matches!(&entry.desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == code as i32)
            );
            assert_eq!(
                entry
                    .actions
                    .as_ref()
                    .unwrap()
                    .backend
                    .as_ref()
                    .unwrap()
                    .error
                    .as_ref()
                    .unwrap()
                    .code,
                code as i32
            );
        }
        assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
    }
    api.component_manager = configured_manager;
    // Real saved requests and references make lifecycle suppression meaningful
    // with configured readers, rather than relying on an already disabled API.
    let mut txn = pool.begin().await?;
    db::switch::set_switch_reprovisioning_requested(
        &mut txn,
        target.id.parse()?,
        &format!("rack-{rack}"),
        vec![],
    )
    .await?;
    db::power_shelf::set_power_shelf_reprovisioning_requested(
        &mut txn,
        shelf.id.parse()?,
        &format!("rack-{rack}"),
        vec![],
    )
    .await?;
    txn.commit().await?;
    for member in [&target, &shelf] {
        seed_credentials_and_tracking(&env, &pool, member).await?;
    }
    sqlx::query("UPDATE switches SET controller_state = $1 WHERE id = $2")
        .bind(sqlx::types::Json(
            model::switch::SwitchControllerState::ReProvisioning {
                reprovisioning_state:
                    model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
            },
        ))
        .bind(target.id.parse::<carbide_uuid::switch::SwitchId>()?)
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE power_shelves SET controller_state = $1 WHERE id = $2")
        .bind(sqlx::types::Json(
            model::power_shelf::PowerShelfControllerState::ReProvisioning {
                reprovisioning_state:
                    model::power_shelf::ReProvisioningState::WaitingForRackFirmwareUpgrade,
            },
        ))
        .bind(
            shelf
                .id
                .parse::<carbide_uuid::power_shelf::PowerShelfId>()?,
        )
        .execute(&pool)
        .await?;
    for member in [&target, &shelf] {
        mock.enqueue_compare_firmware_object(Ok(comparison_reply(
            &member.id,
            "configured-default",
        )))
        .await;
        mock.enqueue_get_firmware_job_status(Ok(rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            job_id: format!("job-{}", member.id),
            job_state: rms::FirmwareJobState::Running as i32,
            ..Default::default()
        }))
        .await;

        let eligible = api
            .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
                target: Some(member.target.clone()),
            }))
            .await?
            .into_inner();

        let actions = eligible.entries[0].actions.as_ref().unwrap();
        let stored = actions.stored.as_ref().unwrap();
        assert!(
            !stored.current.is_empty(),
            "eligible controller work is visible"
        );
        assert!(
            !stored.outstanding_requests.is_empty(),
            "eligible saved intent is visible"
        );
        assert_eq!(
            actions.backend.as_ref().unwrap().active_jobs[0].job_id,
            format!("job-{}", member.id)
        );
    }
    let credential_reads = credentials.calls.load(Ordering::SeqCst);
    let manifest_reads = fetcher.calls.load(Ordering::SeqCst);
    let job_polls = mock.get_firmware_job_status_calls().await.len();
    let inventory_reads = mock.batch_get_firmware_inventory_calls().await.len();
    let comparison_reads = mock.compare_firmware_object_calls().await.len();
    let switch_state = model::switch::SwitchControllerState::Decommissioning {
        decommissioning_state: model::switch::SwitchDecommissioningState::SuppressingSiteExplorer,
    };
    let shelf_state = model::power_shelf::PowerShelfControllerState::Decommissioning {
        decommissioning_state: model::power_shelf::PowerShelfDecommissioningState::Decommissioned,
    };
    for (member, state_query, delete_query, state) in [
        (
            &target,
            "UPDATE switches SET controller_state = $1 WHERE id::text = $2",
            "UPDATE switches SET deleted = now(), controller_state = $1 WHERE id::text = $2",
            serde_json::to_value(switch_state)?,
        ),
        (
            &shelf,
            "UPDATE power_shelves SET controller_state = $1 WHERE id::text = $2",
            "UPDATE power_shelves SET deleted = now(), controller_state = $1 WHERE id::text = $2",
            serde_json::to_value(shelf_state)?,
        ),
    ] {
        sqlx::query(state_query)
            .bind(state)
            .bind(&member.id)
            .execute(&pool)
            .await?;
        for deleted in [false, true] {
            if deleted {
                // Clear Decommissioning so this row proves the deletion mark.
                sqlx::query(delete_query)
                    .bind(serde_json::json!({"state":"ready"}))
                    .bind(&member.id)
                    .execute(&pool)
                    .await?;
            }

            let response = api
                .get_component_firmware_details(Request::new(
                    rpc::GetComponentFirmwareDetailsRequest {
                        target: Some(member.target.clone()),
                    },
                ))
                .await?
                .into_inner();

            let entry = &response.entries[0];
            let stored = entry.actions.as_ref().unwrap().stored.as_ref().unwrap();
            assert_eq!(
                stored.error.as_ref().unwrap().code,
                rpc::ComponentFirmwareErrorCode::Ineligible as i32
            );
            assert!(stored.current.is_empty() && stored.outstanding_requests.is_empty());
            assert!(
                matches!(&entry.actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::Ineligible as i32)
            );
            assert!(
                matches!(&entry.desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::Ineligible as i32)
            );
            assert_eq!(
                entry
                    .actions
                    .as_ref()
                    .unwrap()
                    .backend
                    .as_ref()
                    .unwrap()
                    .error
                    .as_ref()
                    .unwrap()
                    .code,
                rpc::ComponentFirmwareErrorCode::Ineligible as i32
            );
            assert_eq!(credentials.calls.load(Ordering::SeqCst), credential_reads);
            assert_eq!(fetcher.calls.load(Ordering::SeqCst), manifest_reads);
            assert_eq!(mock.get_firmware_job_status_calls().await.len(), job_polls);
            assert_eq!(
                mock.batch_get_firmware_inventory_calls().await.len(),
                inventory_reads
            );
            assert_eq!(
                mock.compare_firmware_object_calls().await.len(),
                comparison_reads
            );
        }
    }
    Ok(())
}

/// Public validation rejects malformed requests before the closed pool is read;
/// valid expired and nonexpired requests retain distinct safe preparation logs.
#[crate::sqlx_test]
async fn component_firmware_details_validate_before_external_work(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let mock = Arc::new(MockRmsApi::new());
    let (api, fetcher) = api(&env, pool, firmware_config(), mock.clone(), false).await?;
    api.pg_pool().close().await;

    // These rows prove public wiring and preparation precedence; parser and
    // selection boundary matrices remain in the narrow reader tests.
    for (name, count, timeout, expired, category) in [
        // Generated defaults cannot silently select an empty target set.
        ("missing target", 0, None, false, None),
        // The public selection bound must run before database preparation.
        ("oversized selection", 65, None, false, None),
        // Timeout parsing must be wired into the real handler.
        ("malformed timeout", 1, Some("invalid"), false, None),
        // An expired caller takes precedence over the unavailable database.
        ("expired caller", 1, Some("1S"), true, Some("deadline")),
        // A valid caller reaches the database and receives only fixed text.
        ("closed database", 1, Some("30S"), false, Some("database")),
    ] {
        let mut request = Request::new(rpc::GetComponentFirmwareDetailsRequest {
            target: (count > 0).then(|| {
                Target::SwitchIds(rpc::SwitchIdList {
                    ids: (0..count)
                        .map(|id| uuid::Uuid::from_u128(id).into())
                        .collect(),
                })
            }),
        });
        if let Some(timeout) = timeout {
            request
                .metadata_mut()
                .insert("grpc-timeout", timeout.parse()?);
        }
        let elapsed = if expired {
            Duration::from_secs(2)
        } else {
            Duration::ZERO
        };
        request
            .extensions_mut()
            .insert(RequestReceivedAt(tokio::time::Instant::now() - elapsed));

        let (result, logs) = capture_logs_async(api.get_component_firmware_details(request)).await;
        let error = result.unwrap_err();

        if let Some(category) = category {
            assert_eq!(error.code(), tonic::Code::Internal, "{name}");
            assert_eq!(
                error.message(),
                "internal error: firmware details preparation failed",
                "{name}"
            );
            assert!(
                logs.iter().any(|log| {
                    log.message == "firmware details preparation failed"
                        && log.field("error_category") == Some(category)
                }),
                "{name}"
            );
        } else {
            assert_eq!(error.code(), tonic::Code::InvalidArgument, "{name}");
            assert!(
                logs.iter()
                    .all(|log| log.message != "firmware details preparation failed"),
                "{name}: validation preceded preparation"
            );
        }
    }

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
    assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
    assert!(mock.compare_firmware_object_calls().await.is_empty());
    assert!(mock.get_firmware_job_status_calls().await.is_empty());
    Ok(())
}

/// Standalone inventory must use the direct pool even at a proxied site. The
/// configured default comes from the runtime overlay, not the installed version.
#[crate::sqlx_test]
async fn component_firmware_details_use_direct_core_and_effective_host_defaults(
    pool: sqlx::PgPool,
) -> TestResult {
    use std::collections::HashMap;

    use carbide_redfish::libredfish::test_support::RedfishSim;
    use model::firmware::{
        Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry, HostFirmwareConfig,
    };
    let env = create_test_env(pool.clone()).await;
    let mut config = firmware_config();
    let rack = RackId::new("standalone-fixture");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let target = seed_target(&pool, &rack, Role::Compute, 66).await?;
    seed_credentials_and_tracking(&env, &pool, &target).await?;
    // Standalone ownership has no rack; this fixture does not instantiate any
    // of the configured rack's expected component inventory.
    sqlx::query("UPDATE machines SET rack_id = NULL WHERE id = $1")
        .bind(target.id.parse::<HostMachineId>()?)
        .execute(&pool)
        .await?;
    let capabilities = &mut config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .rack_capabilities;
    capabilities.compute.count = 0;
    capabilities.switch.count = 0;
    capabilities.power_shelf.count = 0;
    let component = |version: &str| FirmwareComponent {
        known_firmware: vec![FirmwareEntry {
            version: version.into(),
            default: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    config.host_models = HashMap::from([(
        "standalone".into(),
        Firmware {
            vendor: bmc_vendor::BMCVendor::Dell,
            model: "standalone-model".into(),
            components: HashMap::from([(FirmwareComponentType::Bmc, component("static-default"))]),
            ..Default::default()
        },
    )]);
    let mut txn = pool.begin().await?;
    let ip = db::machine_interface_address::find_for_interface(&mut txn, target.bmc_interface)
        .await?[0]
        .address;
    db::explored_endpoints::insert(
        ip,
        &model::site_explorer::EndpointExplorationReport {
            vendor: Some(bmc_vendor::BMCVendor::Dell),
            model: Some("standalone-model".into()),
            ..Default::default()
        },
        false,
        &mut txn,
    )
    .await?;
    db::host_firmware_config::upsert(
        &mut txn,
        &HostFirmwareConfig {
            vendor: bmc_vendor::BMCVendor::Dell,
            model: "standalone-model".into(),
            components: HashMap::from([(FirmwareComponentType::Bmc, component("runtime-default"))]),
            ..Default::default()
        },
    )
    .await?;
    txn.commit().await?;
    let direct = Arc::new(RedfishSim::default());
    direct.set_enforce_auth(true);
    direct.seed_user("firmware-reader", "private-password");
    direct.set_resource("/redfish/v1/UpdateService/FirmwareInventory", serde_json::json!({
        "Members@odata.count": 1, "Members": [{"@odata.id": "/redfish/v1/UpdateService/FirmwareInventory/BMC_Firmware"}]
    }));
    let general = Arc::new(RedfishSim::default());
    general.set_create_client_error("general pool must not serve standalone firmware");
    let mock = Arc::new(MockRmsApi::new());
    let (mut api, fetcher) = api(&env, pool, config, mock.clone(), false).await?;
    api.bmc_credential_ops = direct.clone();
    api.redfish_pool = general.clone();

    let response = api
        .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
            target: Some(target.target.clone()),
        }))
        .await?
        .into_inner();

    let entry = &response.entries[0];
    assert!(matches!(
        entry.actual.as_ref().unwrap().result,
        Some(Installed::Inventory(_))
    ));
    assert_eq!(
        entry.actual.as_ref().unwrap().source,
        Some(rpc::ComponentFirmwareSource::Redfish as i32)
    );
    let Some(Expected::Host(expected)) = entry.desired.as_ref().unwrap().result.as_ref() else {
        panic!("effective host defaults")
    };
    assert_eq!(expected.entries[0].version, "runtime-default");
    assert_eq!(expected.entries[0].component, "bmc");
    assert!(entry.desired.as_ref().unwrap().observed_at.is_none());
    let backend = entry.actions.as_ref().unwrap().backend.as_ref().unwrap();
    assert_eq!(
        backend.error.as_ref().unwrap().code,
        rpc::ComponentFirmwareErrorCode::Unsupported as i32
    );
    assert!(backend.source.is_none());
    assert_eq!(
        entry
            .actions
            .as_ref()
            .unwrap()
            .stored
            .as_ref()
            .unwrap()
            .outstanding_requests
            .len(),
        1,
        "eligible standalone saved intent is visible before lifecycle exclusion"
    );
    assert!(mock.get_firmware_job_status_calls().await.is_empty());
    assert_eq!(direct.create_client_calls().len(), 1);
    assert!(general.create_client_calls().is_empty());
    assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);

    let configured_manager = api.component_manager.take();

    let disabled = api
        .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
            target: Some(target.target.clone()),
        }))
        .await?
        .into_inner();

    assert!(
        matches!(&disabled.entries[0].actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::Disabled as i32)
    );
    let disabled_entry = &disabled.entries[0];
    assert!(
        matches!(&disabled_entry.desired.as_ref().unwrap().result,
        Some(Expected::Host(expected)) if expected.entries[0].version == "runtime-default"),
        "effective host defaults remain readable without a component manager"
    );
    assert_eq!(
        disabled_entry
            .actions
            .as_ref()
            .unwrap()
            .backend
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .code,
        rpc::ComponentFirmwareErrorCode::Disabled as i32
    );
    assert_eq!(direct.create_client_calls().len(), 1);
    api.component_manager = configured_manager;

    // These host lifecycle states are distinct from a switch/shelf deletion
    // mark. Persist the branch input and prove no configured default, saved
    // request or hardware read escapes the ineligible-member boundary.
    for state in [
        ManagedHostState::ForceDeletion,
        ManagedHostState::Decommissioning {
            decommissioning_state: model::machine::DecommissioningState::SuppressingSiteExplorer,
        },
    ] {
        let mut txn = api.pg_pool().begin().await?;
        db::machine::update_state(&mut txn, &target.id.parse::<HostMachineId>()?, &state).await?;
        txn.commit().await?;

        let response = api
            .get_component_firmware_details(Request::new(rpc::GetComponentFirmwareDetailsRequest {
                target: Some(target.target.clone()),
            }))
            .await?
            .into_inner();

        let stored = response.entries[0]
            .actions
            .as_ref()
            .unwrap()
            .stored
            .as_ref()
            .unwrap();
        assert_eq!(
            stored.error.as_ref().unwrap().code,
            rpc::ComponentFirmwareErrorCode::Ineligible as i32
        );
        assert!(stored.outstanding_requests.is_empty());
        assert!(
            matches!(response.entries[0].desired.as_ref().unwrap().result, Some(Expected::Error(ref error)) if error.code == rpc::ComponentFirmwareErrorCode::Ineligible as i32)
        );
        assert_eq!(direct.create_client_calls().len(), 1);
    }
    Ok(())
}

/// Caller drop and earlier caller expiry cancel owned hardware/comparison futures;
/// completed backend/stored sections remain available when a reply is possible.
#[crate::sqlx_test]
async fn component_firmware_details_own_pending_reads_and_preserve_completed_sections(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let mut config = firmware_config();
    let capabilities = &mut config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .rack_capabilities;
    capabilities.compute.count = 1;
    capabilities.switch.count = 0;
    capabilities.power_shelf.count = 0;
    let rack = RackId::new("firmware-cancellation");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let target = seed_target(&pool, &rack, Role::Compute, 67).await?;
    seed_credentials_and_tracking(&env, &pool, &target).await?;

    for (name, expires) in [
        ("earlier caller deadline", true),
        ("caller cancellation", false),
    ] {
        let mock = Arc::new(MockRmsApi::new());
        replies(&mock, &target.id, false, false).await;
        let (inventory_arrived, inventory_release) =
            mock.block_next_batch_get_firmware_inventory().await;
        let (comparison_arrived, comparison_release) =
            mock.block_next_compare_firmware_object().await;
        let (job_arrived, job_release) = mock.block_next_get_firmware_job_status().await;
        let (api, _) = api(&env, pool.clone(), config.clone(), mock.clone(), false).await?;
        let started = tokio::time::Instant::now();
        let mut input = Request::new(rpc::GetComponentFirmwareDetailsRequest {
            target: Some(target.target.clone()),
        });
        if expires {
            input.extensions_mut().insert(RequestReceivedAt(started));
            input.set_timeout(Duration::from_secs(15));
        }
        let mut request = Box::pin(api.get_component_firmware_details(input));
        tokio::select! {
            arrival = tokio::time::timeout(Duration::from_secs(10), async { inventory_arrived.await.unwrap(); comparison_arrived.await.unwrap(); job_arrived.await.unwrap(); }) => arrival.expect("all passive readers reached gates"),
            reply = &mut request => panic!("{name}: read completed before reader gates: {reply:?}"),
        }
        // Release only the job read and drive its ready reply before expiring
        // the other sections. The later assertion proves retained completion,
        // not a race between the job poll and the caller deadline.
        job_release.send(()).expect("release job status");
        assert!(futures::poll!(&mut request).is_pending());

        if expires {
            tokio::time::pause();
            let paused_at = tokio::time::Instant::now();
            tokio::time::advance(Duration::from_secs(16)).await;
            // Poll once: awaiting here could auto-advance to the backend's
            // later 30-second timeout and conceal a lost caller deadline.
            let std::task::Poll::Ready(response) = futures::poll!(&mut request) else {
                panic!("caller expiry must already have completed the public read")
            };
            assert_eq!(
                tokio::time::Instant::now(),
                paused_at + Duration::from_secs(16)
            );
            assert!(
                tokio::time::Instant::now() - started < Duration::from_secs(30),
                "backend-local budgets have not expired"
            );
            tokio::time::resume();
            let response = response?.into_inner();

            let entry = &response.entries[0];
            assert!(
                matches!(entry.actual.as_ref().unwrap().result, Some(Installed::Error(ref error)) if error.code == rpc::ComponentFirmwareErrorCode::DeadlineExceeded as i32)
            );
            assert!(
                matches!(entry.desired.as_ref().unwrap().result, Some(Expected::Error(ref error)) if error.code == rpc::ComponentFirmwareErrorCode::DeadlineExceeded as i32)
            );
            let actions = entry.actions.as_ref().unwrap();
            assert_eq!(
                actions.stored.as_ref().unwrap().outstanding_requests.len(),
                1
            );
            assert_eq!(
                actions.backend.as_ref().unwrap().active_jobs[0].job_id,
                format!("job-{}", target.id)
            );
        } else {
            drop(request);
        }

        assert!(inventory_release.is_closed(), "{name}");
        assert!(comparison_release.is_closed(), "{name}");
        assert!(mock.apply_firmware_object_calls().await.is_empty());
    }
    Ok(())
}

/// NVOS prerequisites cannot erase independent BMC evidence. A foreign endpoint
/// receives no credentials; a secret-store failure retains safe diagnostics and
/// tracked work while preventing inventory/comparison dispatch.
#[crate::sqlx_test]
async fn component_firmware_details_keep_bmc_evidence_without_nvos_readiness(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let mut config = firmware_config();
    let caps = &mut config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .rack_capabilities;
    caps.compute.count = 0;
    caps.switch.count = 2;
    caps.power_shelf.count = 0;
    let rack = RackId::new("firmware-nvos-boundary");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let target = seed_target(&pool, &rack, Role::Switch, 81).await?;
    let foreign = seed_target(&pool, &rack, Role::Switch, 82).await?;
    seed_credentials_and_tracking(&env, &pool, &target).await?;

    // Endpoint rejection, absent NVOS credentials and a private BMC reader
    // error exercise different boundaries without another inventory fixture.
    for (owned, fail_bmc) in [(false, false), (true, false), (true, true)] {
        let mut txn = pool.begin().await?;
        db::machine_interface::associate_interface_with_machine(
            &target.nvos_interface.unwrap(),
            target_owner(if owned { &target } else { &foreign }),
            &mut txn,
        )
        .await?;
        txn.commit().await?;
        let mock = Arc::new(MockRmsApi::new());
        replies(&mock, &target.id, false, false).await;
        let (mut api, _) = api(&env, pool.clone(), config.clone(), mock.clone(), false).await?;
        // Absence forces the shared expected-inventory policy. BMC credentials
        // exist there; NVOS credentials do not, so the owned row proves optionality.
        let credentials = Arc::new(CountingCredentials {
            fail_bmc,
            ..Default::default()
        });
        api.credential_manager = Arc::new(CompositeCredentialManager::new(
            credentials.clone(),
            env.test_credential_manager.clone(),
        ));

        let (response, logs) = capture_logs_async(api.get_component_firmware_details(
            Request::new(rpc::GetComponentFirmwareDetailsRequest {
                target: Some(target.target.clone()),
            }),
        ))
        .await;
        let response = response?.into_inner();

        let entry = &response.entries[0];
        let expected_error = if owned {
            rpc::ComponentFirmwareErrorCode::CredentialsUnavailable
        } else {
            rpc::ComponentFirmwareErrorCode::EndpointUnavailable
        };
        assert!(
            matches!(&entry.actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == expected_error as i32)
        );
        if fail_bmc {
            assert!(matches!(&entry.actual.as_ref().unwrap().result,
                Some(Installed::Error(error)) if error.message == "management credentials are unavailable"));
            assert!(matches!(&entry.desired.as_ref().unwrap().result,
                Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::CredentialsUnavailable as i32
                    && error.message == "management credentials are unavailable"));
        } else {
            assert!(
                matches!(&entry.desired.as_ref().unwrap().result, Some(Expected::Comparisons(rows)) if rows.entries[0].proposed_version.as_deref() == Some("expected-v2"))
            );
        }
        assert_eq!(
            entry
                .actions
                .as_ref()
                .unwrap()
                .backend
                .as_ref()
                .unwrap()
                .active_jobs[0]
                .job_id,
            format!("job-{}", target.id)
        );
        assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
        let calls = mock.compare_firmware_object_calls().await;
        if fail_bmc {
            assert!(
                calls.is_empty(),
                "failed BMC credentials prevent comparison dispatch"
            );
            let management_mac = target.mac.to_string();
            assert!(
                logs.iter().any(|log| {
                    log.target
                        == "carbide_api_core::handlers::component_manager::firmware_details::read"
                        && log.level == tracing::Level::WARN
                        && log.message == "component firmware credentials are unavailable"
                        && log.field("error_category") == Some("internal")
                        && log.field("rack_component") == Some("SwitchBmc")
                        && log.field("management_mac_address") == Some(management_mac.as_str())
                }),
                "reader errors keep the safe owner/role/MAC diagnostic"
            );
            assert!(credentials.calls.load(Ordering::SeqCst) > 0);
            assert_eq!(
                credentials.nvos_calls.load(Ordering::SeqCst),
                0,
                "BMC reader failure precedes NVOS credential preparation"
            );
        } else {
            let descriptor = &calls[0].nodes.as_ref().unwrap().nodes[0];
            assert_eq!(descriptor.node_id, target.id);
            let os = descriptor
                .host_endpoint
                .as_ref()
                .expect("switch descriptor retains its optional endpoint container");
            assert!(os.interface.is_none());
            assert!(os.credentials.is_none());
        }
        if owned && !fail_bmc {
            assert!(
                credentials.nvos_calls.load(Ordering::SeqCst) > 0,
                "owned NVOS credential failure was reached"
            );
            // Missing expected NVOS credentials is NotFound, classified by
            // the read boundary without formatting the dependency error.
            let management_mac = target.mac.to_string();
            assert!(logs.iter().any(|log| {
                log.target
                    == "carbide_api_core::handlers::component_manager::firmware_details::read"
                    && log.level == tracing::Level::WARN
                    && log.message == "component firmware credentials are unavailable"
                    && log.field("error_category") == Some("credential_read_failed")
                    && log.field("rack_component") == Some("SwitchNvos")
                    && log.field("management_mac_address") == Some(management_mac.as_str())
            }));
        } else if !owned {
            assert!(
                credentials.calls.load(Ordering::SeqCst) > 0,
                "owned BMC credential control"
            );
            assert_eq!(
                credentials.nvos_calls.load(Ordering::SeqCst),
                0,
                "no NVOS credential lookup for the foreign endpoint"
            );
        }
        for marker in [PRIVATE, "vault/private-credential-key", "private-password"] {
            assert!(!format!("{response:?}").contains(marker));
            assert!(logs.iter().all(|log| !format!("{log:?}").contains(marker)));
        }
        assert!(mock.apply_firmware_object_calls().await.is_empty());
    }
    Ok(())
}

/// A short caller budget remains anchored at ingress across real preparation
/// and a pending credential prerequisite, before any inventory RPC may start.
#[crate::sqlx_test]
async fn component_firmware_details_caller_budget_bounds_credentials(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = create_test_env(pool.clone()).await;
    let mut config = firmware_config();
    let profile = config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap();
    profile.rack_capabilities.compute.count = 1;
    profile.rack_capabilities.switch.count = 0;
    profile.rack_capabilities.power_shelf.count = 0;
    profile.firmware_object = None;
    let rack = RackId::new("firmware-caller-budget");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let target = seed_target(&pool, &rack, Role::Compute, 83).await?;
    let mock = Arc::new(MockRmsApi::new());
    let (mut api, fetcher) = api(&env, pool, config, mock.clone(), false).await?;
    api.component_manager.as_mut().unwrap().compute_tray = Arc::new(
        component_manager::core_compute_manager::CoreComputeTrayManager::new(
            api.bmc_credential_ops.clone(),
        ),
    );
    let (arrivals, mut arrived) = tokio::sync::mpsc::unbounded_channel();
    api.credential_manager = Arc::new(CompositeCredentialManager::new(
        Arc::new(GatedCredentials { arrivals }),
        env.test_credential_manager.clone(),
    ));
    let mut request = Request::new(rpc::GetComponentFirmwareDetailsRequest {
        target: Some(target.target),
    });
    request.extensions_mut().insert(RequestReceivedAt(
        tokio::time::Instant::now() - Duration::from_secs(20),
    ));
    request.set_timeout(Duration::from_secs(30));
    let mut read = Box::pin(api.get_component_firmware_details(request));
    let release = tokio::select! {
        gate = tokio::time::timeout(Duration::from_secs(5), arrived.recv()) => gate?.expect("credential prerequisite entered"),
        reply = &mut read => panic!("read completed before credentials: {reply:?}"),
    };

    // The default/backend budgets still have time; only the original caller
    // budget has expired. No real SQL operation runs while time is paused.
    tokio::time::pause();
    let paused_at = tokio::time::Instant::now();
    tokio::time::advance(Duration::from_secs(11)).await;
    // A single poll proves readiness now; await could advance the clock to a
    // later default/backend timeout and turn this caller-budget proof vacuous.
    let std::task::Poll::Ready(response) = futures::poll!(&mut read) else {
        panic!("original ingress budget must expire before the backend budget")
    };
    assert_eq!(
        tokio::time::Instant::now(),
        paused_at + Duration::from_secs(11)
    );
    tokio::time::resume();
    let response = response?.into_inner();

    assert!(
        matches!(&response.entries[0].actual.as_ref().unwrap().result, Some(Installed::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::DeadlineExceeded as i32)
    );
    assert!(release.is_closed());
    assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

/// Credential gates isolate the public installed-workflow admission boundary.
/// More than eight targets cannot start; releasing one admits exactly one peer,
/// and cancellation drops both active gates and work still queued by the caller.
#[crate::sqlx_test]
async fn component_firmware_details_bound_installed_workflows(pool: sqlx::PgPool) -> TestResult {
    use carbide_redfish::libredfish::test_support::RedfishSim;

    let env = create_test_env(pool.clone()).await;
    let mut config = firmware_config();
    let profile = config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap();
    profile.rack_capabilities.compute.count = 10;
    profile.rack_capabilities.switch.count = 0;
    profile.rack_capabilities.power_shelf.count = 0;
    profile.firmware_object = None;
    let rack = RackId::new("firmware-workflow-bound");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack.clone())
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let mut ids = Vec::new();
    for seed in 90..100 {
        ids.push(
            seed_target(&pool, &rack, Role::Compute, seed)
                .await?
                .id
                .parse()?,
        );
    }
    let mock = Arc::new(MockRmsApi::new());
    let (mut api, _) = api(&env, pool, config, mock.clone(), false).await?;
    // Core has inventory support but no passive job reader or RMS comparison.
    // Those independent workflows finish immediately, so asynchronous tracking
    // SQL cannot occupy a slot and hide a ninth installed workflow.
    let redfish = Arc::new(RedfishSim::default());
    redfish.set_create_client_error("released workflow completes without hardware");
    api.component_manager.as_mut().unwrap().compute_tray = Arc::new(
        component_manager::core_compute_manager::CoreComputeTrayManager::new(redfish.clone()),
    );
    let (arrivals, mut arrived) = tokio::sync::mpsc::unbounded_channel();
    api.credential_manager = Arc::new(CompositeCredentialManager::new(
        Arc::new(GatedCredentials { arrivals }),
        env.test_credential_manager.clone(),
    ));
    let mut read = Box::pin(api.get_component_firmware_details(Request::new(
        rpc::GetComponentFirmwareDetailsRequest {
            target: Some(Target::MachineIds(::rpc::common::HostMachineIdList {
                machine_ids: ids,
            })),
        },
    )));
    let mut releases = Vec::new();
    for _ in 0..8 {
        let gate = tokio::select! {
            gate = tokio::time::timeout(Duration::from_secs(10), arrived.recv()) => gate?.expect("installed workflow entered"),
            reply = &mut read => panic!("read completed with blocked prerequisites: {reply:?}"),
        };
        releases.push(gate);
    }
    assert!(futures::poll!(&mut read).is_pending());
    assert!(
        arrived.try_recv().is_err(),
        "ninth workflow remains queued after an explicit poll"
    );
    assert!(redfish.create_client_calls().is_empty());
    assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());

    releases
        .remove(0)
        .send(())
        .expect("release one installed workflow");
    let ninth = tokio::select! {
        gate = tokio::time::timeout(Duration::from_secs(10), arrived.recv()) => gate?.expect("one queued peer entered"),
        reply = &mut read => panic!("read completed with blocked peers: {reply:?}"),
    };
    releases.push(ninth);
    assert!(futures::poll!(&mut read).is_pending());
    assert!(arrived.try_recv().is_err(), "tenth workflow remains queued");
    assert_eq!(redfish.create_client_calls().len(), 1);
    assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
    drop(read);

    assert!(releases.iter().all(|release| release.is_closed()));
    assert!(
        arrived.try_recv().is_err(),
        "cancelled queued work never starts"
    );
    Ok(())
}

/// Distinct owning profiles complete in rack order while public slots use the
/// opposite order. Configured comparisons, frozen work, saved intent and alias
/// association must remain separate, including a missing managed slot.
#[crate::sqlx_test]
async fn component_firmware_details_keep_owning_sources_and_target_identity(
    pool: sqlx::PgPool,
) -> TestResult {
    use model::machine::HostReprovisionState;
    use model::rack::{
        FirmwareUpgradeDeviceStatus, FirmwareUpgradeJob, FirmwareUpgradeState,
        RackMaintenanceState, RackState,
    };
    let env = create_test_env(pool.clone()).await;
    let mut config = firmware_config();
    let first_profile = config
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap();
    first_profile.rack_capabilities.compute.count = 1;
    first_profile.rack_capabilities.switch.count = 0;
    first_profile.rack_capabilities.power_shelf.count = 0;
    first_profile.firmware_object.as_mut().unwrap().url =
        "https://firmware.invalid/profile-a".parse()?;
    let mut second_profile = first_profile.clone();
    second_profile.firmware_object.as_mut().unwrap().url =
        "https://firmware.invalid/profile-b".parse()?;
    config
        .rack_profiles
        .rack_profiles
        .insert("second-firmware-profile".into(), second_profile);
    let first_rack = RackId::new("firmware-source-a");
    let second_rack = RackId::new("firmware-source-b");
    let mut txn = pool.begin().await?;
    TestRackDbBuilder::new()
        .with_rack_id(first_rack.clone())
        .persist(&mut txn)
        .await?;
    TestRackDbBuilder::new()
        .with_rack_id(second_rack.clone())
        .with_rack_profile_id("second-firmware-profile")
        .persist(&mut txn)
        .await?;
    txn.commit().await?;
    let first = seed_target(&pool, &first_rack, Role::Compute, 101).await?;
    let second = seed_target(&pool, &second_rack, Role::Compute, 102).await?;
    for target in [&first, &second] {
        seed_credentials_and_tracking(&env, &pool, target).await?;
    }
    let now = chrono::Utc::now();
    let frozen_state = ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
            upgrade_task_id: "frozen-scout-task".into(),
            firmware_type: model::firmware::FirmwareComponentType::Bmc,
            final_version: "frozen-host-version".into(),
            power_drains_needed: None,
            started_at: now,
            deadline: now + chrono::Duration::hours(1),
            task_json: PRIVATE.into(),
            result: None,
        },
        retry_count: 0,
    };
    let alias = MacAddress::new([2, 0, 0, 0, 2, 102]);
    let mut txn = pool.begin().await?;
    db::machine::update_state(&mut txn, &first.id.parse::<HostMachineId>()?, &frozen_state).await?;
    interface(&mut txn, alias, target_owner(&second), InterfaceType::Bmc).await?;
    let rack_state = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
        },
    };
    sqlx::query("UPDATE racks SET controller_state = $1 WHERE id = $2")
        .bind(sqlx::types::Json(rack_state))
        .bind(&second_rack)
        .execute(&mut *txn)
        .await?;
    db::rack::update_firmware_upgrade_job(
        &mut txn,
        &second_rack,
        Some(&FirmwareUpgradeJob {
            firmware_id: Some("frozen-rack-object".into()),
            machines: vec![FirmwareUpgradeDeviceStatus {
                node_id: String::new(),
                mac: alias.to_string(),
                bmc_ip: "192.0.2.1".into(),
                status: model::rack::FirmwareProgressState::InProgress,
                job_id: Some("attached-alias-job".into()),
                parent_job_id: None,
                error_message: None,
            }],
            ..Default::default()
        }),
    )
    .await?;
    txn.commit().await?;
    let before: (serde_json::Value, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT controller_state, host_reprovisioning_requested FROM machines WHERE id = $1",
    )
    .bind(first.id.parse::<HostMachineId>()?)
    .fetch_one(&pool)
    .await?;
    let mock = Arc::new(MockRmsApi::new());
    let (mut api, fetcher) = api(&env, pool.clone(), config, mock.clone(), false).await?;
    // This route uses the profile comparison even with a non-RMS inventory HAL.
    let manager = api.component_manager.as_mut().unwrap();
    manager.compute_tray = Arc::new(component_manager::mock::MockComputeTrayManager);
    manager.compute_tray_use_state_controller = true;
    fetcher.responses.lock().unwrap().extend([
        (
            "https://firmware.invalid/profile-a".into(),
            Ok(r#"{"Id":"manifest-a"}"#.into()),
        ),
        (
            "https://firmware.invalid/profile-b".into(),
            Ok(r#"{"Id":"manifest-b"}"#.into()),
        ),
    ]);
    mock.enqueue_compare_firmware_object(Ok(comparison_reply(&first.id, "expected-a")))
        .await;
    mock.enqueue_compare_firmware_object(Ok(comparison_reply(&second.id, "expected-b")))
        .await;
    let missing = HostMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [198; 32],
        MachineType::Host,
    ))?;
    let request = rpc::GetComponentFirmwareDetailsRequest {
        target: Some(Target::MachineIds(::rpc::common::HostMachineIdList {
            machine_ids: vec![second.id.parse()?, missing, first.id.parse()?],
        })),
    };

    let response = api
        .get_component_firmware_details(Request::new(request.clone()))
        .await?
        .into_inner();

    assert_eq!(
        response
            .entries
            .iter()
            .map(|entry| entry.component_id.clone())
            .collect::<Vec<_>>(),
        [second.id.clone(), missing.to_string(), first.id.clone()]
    );
    for (slot, version) in [(0, "expected-b"), (2, "expected-a")] {
        assert!(
            matches!(&response.entries[slot].desired.as_ref().unwrap().result, Some(Expected::Comparisons(rows)) if rows.entries[0].proposed_version.as_deref() == Some(version))
        );
    }
    assert!(
        matches!(&response.entries[1].desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::NotFound as i32)
    );
    let current = &response.entries[2]
        .actions
        .as_ref()
        .unwrap()
        .stored
        .as_ref()
        .unwrap()
        .current[0];
    assert!(
        matches!(current.target.as_ref().unwrap().target, Some(rpc::component_firmware_work_target::Target::Host(ref target)) if target.version.as_deref() == Some("frozen-host-version"))
    );
    let pending = &response.entries[2]
        .actions
        .as_ref()
        .unwrap()
        .stored
        .as_ref()
        .unwrap()
        .outstanding_requests[0];
    assert!(
        matches!(pending.target.as_ref().unwrap().target, Some(rpc::component_firmware_work_target::Target::Host(ref target)) if target.version.is_none())
    );
    assert!(
        response.entries[0]
            .actions
            .as_ref()
            .unwrap()
            .stored
            .as_ref()
            .unwrap()
            .current
            .iter()
            .any(|work| work.job_id.as_deref() == Some("attached-alias-job")),
        "public handler supplies attached aliases to the projector"
    );
    assert_eq!(
        *fetcher.requested_urls.lock().unwrap(),
        [
            "https://firmware.invalid/profile-a",
            "https://firmware.invalid/profile-b"
        ]
    );
    let compared = mock.compare_firmware_object_calls().await;
    assert_eq!(compared[0].config_json, r#"{"Id":"manifest-a"}"#);
    assert_eq!(
        compared[0].nodes.as_ref().unwrap().nodes[0].node_id,
        first.id
    );
    assert_eq!(compared[1].config_json, r#"{"Id":"manifest-b"}"#);
    assert_eq!(
        compared[1].nodes.as_ref().unwrap().nodes[0].node_id,
        second.id
    );
    let after: (serde_json::Value, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT controller_state, host_reprovisioning_requested FROM machines WHERE id = $1",
    )
    .bind(first.id.parse::<HostMachineId>()?)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        after, before,
        "passive public read preserves controller state and saved request"
    );
    assert_eq!(
        db::direct_dispatch_rms_job::get(
            &pool,
            first.mac,
            db::direct_dispatch_rms_job::RmsJobKind::FirmwareObject
        )
        .await?,
        Some(format!("job-{}", first.id))
    );
    assert!(mock.apply_firmware_object_calls().await.is_empty());
    assert!(!format!("{response:?}").contains(PRIVATE));

    // An invalid manifest is rejected before that group's comparison; the other
    // rack's correctly attributed configured default remains useful.
    fetcher.responses.lock().unwrap().insert(
        "https://firmware.invalid/profile-a".into(),
        Ok(PRIVATE.into()),
    );
    mock.enqueue_compare_firmware_object(Ok(comparison_reply(&second.id, "expected-b")))
        .await;

    let response = api
        .get_component_firmware_details(Request::new(request.clone()))
        .await?
        .into_inner();

    assert!(
        matches!(&response.entries[2].desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::ManifestUnavailable as i32)
    );
    assert!(
        matches!(&response.entries[0].desired.as_ref().unwrap().result, Some(Expected::Comparisons(rows)) if rows.entries[0].proposed_version.as_deref() == Some("expected-b"))
    );

    // A selected source with no RMS client is explicit Unsupported, without a
    // new fetch or a synthetic successful empty comparison.
    let client = api.rms_client.take();
    let fetches = fetcher.calls.load(Ordering::SeqCst);

    let response = api
        .get_component_firmware_details(Request::new(request.clone()))
        .await?
        .into_inner();

    assert!(
        matches!(&response.entries[2].desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::Unsupported as i32)
    );
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), fetches);
    api.rms_client = client;
    fetcher.responses.lock().unwrap().insert(
        "https://firmware.invalid/profile-a".into(),
        Ok(r#"{"Id":"manifest-a"}"#.into()),
    );
    Arc::make_mut(&mut api.runtime_config)
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .attributes
        .insert("oversized".into(), "x".repeat(1025));
    mock.enqueue_compare_firmware_object(Ok(comparison_reply(&second.id, "expected-b")))
        .await;

    let response = api
        .get_component_firmware_details(Request::new(request.clone()))
        .await?
        .into_inner();

    assert!(
        matches!(&response.entries[2].desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::LimitExceeded as i32)
    );
    assert!(
        matches!(&response.entries[0].desired.as_ref().unwrap().result, Some(Expected::Comparisons(rows)) if rows.entries[0].proposed_version.as_deref() == Some("expected-b"))
    );
    Arc::make_mut(&mut api.runtime_config)
        .rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .attributes
        .remove("oversized");
    env.api
        .credential_manager
        .delete_credentials(&CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: first.mac,
            },
        })
        .await
        .expect("remove the seeded BMC credential for the comparison failure case");
    mock.enqueue_compare_firmware_object(Ok(comparison_reply(&second.id, "expected-b")))
        .await;

    let response = api
        .get_component_firmware_details(Request::new(request))
        .await?
        .into_inner();

    assert!(
        matches!(&response.entries[2].desired.as_ref().unwrap().result, Some(Expected::Error(error)) if error.code == rpc::ComponentFirmwareErrorCode::CredentialsUnavailable as i32)
    );
    assert!(
        matches!(&response.entries[0].desired.as_ref().unwrap().result, Some(Expected::Comparisons(rows)) if rows.entries[0].proposed_version.as_deref() == Some("expected-b"))
    );
    Ok(())
}
