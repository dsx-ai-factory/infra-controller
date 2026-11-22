// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use carbide_utils::redfish::BmcAccessInfo;
use model::hardware_dispatch::{HardwareDispatchOwner, MlxAdminCommand};
use model::machine::ManagedHostState;
use rpc::forge::admin_power_control_request::SystemPowerControl;
use rpc::forge::forge_server::Forge;
use rpc::forge::{AdminPowerControlRequest, BmcEndpointRequest};
use tonic::Request;

use crate::api::Api;
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::{TestEnv, create_managed_host, create_test_env};

/// The shared fixture mocks endpoint exploration. Use the actual power client with its Redfish simulator.
async fn power_api(env: &TestEnv, bmc: &BmcAccessInfo) -> eyre::Result<Api> {
    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: bmc.mac_address,
                },
            },
            &Credentials::new("admin", "test-password"),
        )
        .await?;
    Ok(TestApiBuilder::new(
        env.pool.clone(),
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(env.config.clone())
    .with_credential_manager(env.api.credential_manager.clone())
    .with_redfish_pool(env.redfish_sim.clone())
    .build())
}

/// Hold the real client after its credential read, commit the competing change,
/// then release it. A failure before preparation reports its actual RPC result.
async fn refused_after_preparation(
    env: &TestEnv,
    api: &Api,
    request: AdminPowerControlRequest,
    change: impl Future<Output = eyre::Result<()>>,
) -> eyre::Result<tonic::Status> {
    let (preparing, resume) = env.redfish_sim.pause_next_client_creation();
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut rpc = std::pin::pin!(api.admin_power_control(Request::new(request)));
        tokio::select! {
            result = &mut rpc => return Err(eyre::eyre!("power RPC finished before client preparation: {result:?}")),
            started = preparing => started?,
        }
        change.await?;
        resume.send(()).expect("release the prepared client");
        Ok(rpc.await.expect_err("power request must reject the committed change"))
    }).await?
}

#[crate::sqlx_test]
async fn admin_power_rechecks_the_owner_of_an_explicit_endpoint(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await.host();
    let mut txn = env.db_txn().await;
    let bmc = host.bmc_access(&mut txn).await;
    if db::power_options::get_by_ids(&[*host.id], txn.as_mut())
        .await?
        .is_empty()
    {
        db::power_options::create(&host.id.into(), txn.as_mut()).await?;
    }
    txn.commit().await?;
    let mut api = power_api(&env, &bmc).await?;
    std::sync::Arc::make_mut(&mut api.runtime_config)
        .power_manager_options
        .enabled = true;
    let request = AdminPowerControlRequest {
        bmc_endpoint_request: Some(BmcEndpointRequest {
            ip_address: bmc.host.clone(),
            mac_address: Some(bmc.mac_address.to_string()),
        }),
        machine_id: None,
        action: SystemPowerControl::GracefulRestart.into(),
    };
    let before_warning = env.redfish_sim.timepoint();
    let warning = api
        .admin_power_control(Request::new(AdminPowerControlRequest {
            action: SystemPowerControl::ForceOff.into(),
            ..request.clone()
        }))
        .await?
        .into_inner();
    assert!(
        warning.msg.as_deref().is_some_and(
            |message| message.contains("Desired power state for the host is set as On")
        )
    );
    let actions = env.redfish_sim.actions_since(&before_warning);
    assert_eq!(actions.all_hosts().len(), 1);
    assert_eq!(
        actions.for_host(&bmc.host),
        vec![RedfishSimAction::Power(
            libredfish::SystemPowerControl::ForceOff
        )]
    );
    let before = env.redfish_sim.timepoint();
    // Re-enrollment can replace only the interface row while keeping the same device,
    // address and credential MAC.
    let refusal = refused_after_preparation(&env, &api, request.clone(), async {
        let mut txn = env.db_txn().await;
        let original: carbide_uuid::machine::MachineInterfaceId = sqlx::query_scalar(
            "UPDATE machine_interfaces SET mac_address = '02:00:00:00:03:fe', machine_id = NULL
             WHERE machine_id = $1 AND mac_address = $2 RETURNING id",
        ).bind(host.id).bind(bmc.mac_address).fetch_one(txn.as_mut()).await?;
        let replacement: carbide_uuid::machine::MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces (machine_id, segment_id, mac_address, domain_id, primary_interface, hostname, association_type, interface_type)
             SELECT $2, segment_id, $3, domain_id, primary_interface, 'replacement-bmc', association_type, interface_type
             FROM machine_interfaces WHERE id = $1 RETURNING id",
        ).bind(original).bind(host.id).bind(bmc.mac_address).fetch_one(txn.as_mut()).await?;
        let address = bmc.host.parse::<std::net::IpAddr>()?;
        sqlx::query("UPDATE machine_interface_addresses SET interface_id = $1 WHERE interface_id = $2 AND address = $3")
            .bind(replacement).bind(original).bind(address).execute(txn.as_mut()).await?;
        let interface = db::machine_interface::find_by_ip(txn.as_mut(), address).await?.expect("replacement owns the same address");
        assert_ne!(original, replacement);
        assert_eq!(interface.id, replacement);
        assert_eq!(interface.mac_address, bmc.mac_address);
        assert_eq!(interface.machine_id, Some(host.id.into()));
        txn.commit().await?;
        Ok(())
    }).await?;
    assert_eq!(refusal.code(), tonic::Code::FailedPrecondition);
    assert!(
        refusal
            .message()
            .contains("changed during client preparation")
    );
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );

    // First change only the credential MAC. A second invocation then loses only its device owner.
    let replacement_mac: mac_address::MacAddress = "02:00:00:00:02:fe".parse()?;
    for (scenario, change_mac) in [
        ("credential MAC changed", true),
        ("device owner removed", false),
    ] {
        let error = refused_after_preparation(&env, &api, request.clone(), async {
            let result = if change_mac {
                sqlx::query(
                    "UPDATE machine_interfaces SET mac_address = $2
                             WHERE machine_id = $1 AND interface_type = 'Bmc'",
                )
                .bind(host.id)
                .bind(replacement_mac)
                .execute(&env.pool)
                .await?
            } else {
                sqlx::query(
                    "UPDATE machine_interfaces SET machine_id = NULL, association_type = 'None'
                             WHERE machine_id = $1 AND mac_address = $2",
                )
                .bind(host.id)
                .bind(bmc.mac_address)
                .execute(&env.pool)
                .await?
            };
            assert_eq!(result.rows_affected(), 1);

            Ok(())
        })
        .await?;
        assert_eq!(
            error.code(),
            tonic::Code::FailedPrecondition,
            "{scenario}: {error}"
        );
        assert!(
            error
                .message()
                .contains("changed during client preparation"),
            "{scenario}: {error}"
        );
        assert!(
            env.redfish_sim
                .actions_since(&before)
                .all_hosts()
                .is_empty(),
            "{scenario} sent a power action"
        );
        if change_mac {
            sqlx::query("UPDATE machine_interfaces SET mac_address = $2 WHERE mac_address = $1")
                .bind(replacement_mac)
                .bind(bmc.mac_address)
                .execute(&env.pool)
                .await?;
        }
    }
    // A fresh endpoint-only request can still power an unassociated BMC.
    let mut unassociated = request;
    unassociated
        .bmc_endpoint_request
        .as_mut()
        .unwrap()
        .mac_address = None;
    assert!(
        api.admin_power_control(Request::new(unassociated))
            .await?
            .into_inner()
            .msg
            .is_none()
    );
    assert_eq!(env.redfish_sim.actions_since(&before).all_hosts().len(), 1);
    assert_eq!(
        env.redfish_sim.actions_since(&before).for_host(&bmc.host),
        vec![RedfishSimAction::Power(
            libredfish::SystemPowerControl::GracefulRestart
        )]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn admin_power_rechecks_firmware_ownership_on_an_attached_dpus_host(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let dpu = host.dpu();
    let mut txn = env.db_txn().await;
    let bmc = dpu.bmc_access(&mut txn).await;
    txn.commit().await?;
    let api = power_api(&env, &bmc).await?;
    let before = env.redfish_sim.timepoint();
    let request = AdminPowerControlRequest {
        machine_id: Some(dpu.id.to_string()),
        action: SystemPowerControl::ForceOff.into(),
        ..Default::default()
    };
    let admitted = ManagedHostState::NicFirmwareUpdating {
        operation: model::test_support::nic_firmware::operation(),
    };
    let error = refused_after_preparation(&env, &api, request, async {
        // Only the attached host enters NIC firmware updating. Checking the DPU row would miss it.
        let result = sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
            .bind(host.id)
            .bind(sqlx::types::Json(&admitted))
            .execute(&env.pool)
            .await?;
        assert_eq!(result.rows_affected(), 1);
        assert!(!matches!(
            dpu.db_machine(&env.pool).await.state.value,
            ManagedHostState::NicFirmwareUpdating { .. }
        ));

        Ok(())
    })
    .await?;
    assert_eq!(
        host.host().db_machine(&env.pool).await.state.value,
        admitted
    );
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("NIC firmware"));
    assert!(error.message().contains(&host.id.to_string()));
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-412"
    );
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );
    Ok(())
}

#[crate::sqlx_test]
async fn admin_power_rejects_a_reparented_dpu_without_redirecting_its_restart(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(pool).await;
    let first = create_managed_host(&env).await;
    let second = create_managed_host(&env).await;
    let dpu = first.dpu();
    let mut txn = env.db_txn().await;
    let bmc = dpu.bmc_access(&mut txn).await;
    txn.commit().await?;
    let api = power_api(&env, &bmc).await?;
    let request = AdminPowerControlRequest {
        machine_id: Some(dpu.id.to_string()),
        action: SystemPowerControl::ForceRestart.into(),
        ..Default::default()
    };
    let before = env.redfish_sim.timepoint();
    // The machine selector can lose its BMC address while the DPU still has
    // the same owner, or the owner alone can change on an otherwise stable BMC.
    for (scenario, change_address) in [("BMC address moved", true), ("DPU reparented", false)] {
        let error = refused_after_preparation(&env, &api, request.clone(), async {
            let mut txn = env.db_txn().await;
            if change_address {
                let result = sqlx::query(
                    "UPDATE machine_interface_addresses SET address = '192.0.2.249'
                                      WHERE address = $1",
                )
                .bind(bmc.host.parse::<std::net::IpAddr>()?)
                .execute(txn.as_mut())
                .await?;
                assert_eq!(result.rows_affected(), 1);
            } else {
                sqlx::query(
                    "UPDATE machine_interfaces SET attached_dpu_machine_id = NULL
                     WHERE machine_id = $1 AND attached_dpu_machine_id = $2",
                )
                .bind(first.id)
                .bind(dpu.id)
                .execute(txn.as_mut())
                .await?;
                sqlx::query(
                    "UPDATE machine_interfaces SET attached_dpu_machine_id = $2
                     WHERE machine_id = $1 AND attached_dpu_machine_id = $3",
                )
                .bind(second.id)
                .bind(dpu.id)
                .bind(second.dpu().id)
                .execute(txn.as_mut())
                .await?;
                assert_eq!(
                    db::hardware_dispatch::owning_host(txn.as_mut(), dpu.id.into()).await?,
                    Some(*second.id)
                );
            }
            txn.commit().await?;

            Ok(())
        })
        .await?;
        assert_eq!(
            error.code(),
            tonic::Code::FailedPrecondition,
            "{scenario}: {error}"
        );
        assert!(
            error
                .message()
                .contains("changed during client preparation"),
            "{scenario}: {error}"
        );
        assert!(
            env.redfish_sim
                .actions_since(&before)
                .all_hosts()
                .is_empty(),
            "{scenario} sent a power action"
        );

        if change_address {
            sqlx::query(
                "UPDATE machine_interface_addresses SET address = $1 WHERE address = '192.0.2.249'",
            )
            .bind(bmc.host.parse::<std::net::IpAddr>()?)
            .execute(&env.pool)
            .await?;
        }
    }

    let response = api
        .admin_power_control(Request::new(request.clone()))
        .await?
        .into_inner();
    assert!(response.msg.is_none());
    assert_eq!(env.redfish_sim.actions_since(&before).all_hosts().len(), 1);
    assert_eq!(
        env.redfish_sim.actions_since(&before).for_host(&bmc.host),
        vec![RedfishSimAction::Power(
            libredfish::SystemPowerControl::ForceRestart
        )]
    );

    // Both hosts now report a link to this DPU. Core must not select either one.
    sqlx::query(
        "UPDATE machine_interfaces SET attached_dpu_machine_id = $2
         WHERE machine_id = $1 AND interface_type != 'Bmc' AND attached_dpu_machine_id IS NULL",
    )
    .bind(first.id)
    .bind(dpu.id)
    .execute(&env.pool)
    .await?;
    let attached_hosts: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT machine_id) FROM machine_interfaces
         WHERE attached_dpu_machine_id = $1 AND machine_id != $1 AND interface_type != 'Bmc'",
    )
    .bind(dpu.id)
    .fetch_one(&env.pool)
    .await?;
    assert_eq!(attached_hosts, 2);
    let before = env.redfish_sim.timepoint();
    let error = api
        .admin_power_control(Request::new(request.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("unambiguous host attachment"));
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );
    sqlx::query("UPDATE machine_interfaces SET attached_dpu_machine_id = NULL WHERE attached_dpu_machine_id = $1 AND machine_id != $1 AND interface_type != 'Bmc'")
        .bind(dpu.id).execute(&env.pool).await?;
    assert_eq!(
        db::hardware_dispatch::owning_host(&env.pool, dpu.id.into()).await?,
        None
    );
    assert!(
        api.admin_power_control(Request::new(request))
            .await?
            .into_inner()
            .msg
            .is_none()
    );
    assert_eq!(
        env.redfish_sim.actions_since(&before).all_hosts(),
        vec![RedfishSimAction::Power(
            libredfish::SystemPowerControl::ForceRestart
        )]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn admin_power_blocks_only_a_pending_instance_reboot_with_the_queue_disabled(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await.host();
    let mut txn = env.db_txn().await;
    let bmc = host.bmc_access(&mut txn).await;
    let owner = HardwareDispatchOwner::InstanceReboot {
        machine_id: *host.id,
        instance_id: carbide_uuid::instance::InstanceId::new(),
        bmc: host.db_machine(&env.pool).await.status.bmc_info,
        last_attempt_at: None,
        reconcile_after: None,
    };
    let token = uuid::Uuid::new_v4();
    txn.commit().await?;
    let api = power_api(&env, &bmc).await?;
    assert!(
        !api.runtime_config
            .machine_state_controller
            .instance_reboot_queue_enabled
    );
    let request = AdminPowerControlRequest {
        machine_id: Some(host.id.to_string()),
        action: SystemPowerControl::ForceRestart.into(),
        ..Default::default()
    };
    let before = env.redfish_sim.timepoint();
    let error = refused_after_preparation(&env, &api, request.clone(), async {
        let mut txn = env.db_txn().await;
        db::hardware_dispatch::begin(txn.as_mut(), token, &owner).await?;
        txn.commit().await?;

        Ok(())
    })
    .await?;
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains(&token.to_string()));
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );

    let mut txn = env.db_txn().await;
    assert!(db::hardware_dispatch::settle(txn.as_mut(), *host.id, token, &owner).await?);
    txn.commit().await?;
    let settled = host
        .db_machine(&env.pool)
        .await
        .status
        .hardware_dispatch
        .expect("stored reboot");
    assert_eq!(settled.token, token);
    assert!(!settled.is_pending());
    let response = api
        .admin_power_control(Request::new(request.clone()))
        .await?
        .into_inner();
    assert!(response.msg.is_none());
    assert_eq!(env.redfish_sim.actions_since(&before).all_hosts().len(), 1);
    assert_eq!(
        env.redfish_sim.actions_since(&before).for_host(&bmc.host),
        vec![RedfishSimAction::Power(
            libredfish::SystemPowerControl::ForceRestart
        )]
    );

    let mut txn = env.db_txn().await;
    let mlx = HardwareDispatchOwner::MlxAdmin {
        machine_id: host.id.into(),
        command: MlxAdminCommand::ConfigSet,
    };
    db::hardware_dispatch::begin(txn.as_mut(), uuid::Uuid::new_v4(), &mlx).await?;
    txn.commit().await?;
    env.redfish_sim.fail_next_power_action(
        libredfish::SystemPowerControl::ForceRestart,
        "BMC refused test restart",
    );
    let error = api
        .admin_power_control(Request::new(request))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(error.message().contains("BMC refused test restart"));
    assert_eq!(
        env.redfish_sim.actions_since(&before).for_host(&bmc.host),
        vec![
            RedfishSimAction::Power(libredfish::SystemPowerControl::ForceRestart),
            RedfishSimAction::PowerFailed(libredfish::SystemPowerControl::ForceRestart)
        ]
    );
    Ok(())
}

/// A fresh exploration can move IPMI while the driver is authenticating its Redfish client.
#[crate::sqlx_test]
async fn admin_power_rechecks_the_persisted_drivers_ipmi_port(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Request as HttpRequest, State};
    use axum::http::HeaderMap;
    use axum::middleware::{Next, from_fn};
    use axum::{Json, Router};
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use model::site_explorer::{ComputerSystem, Manager};
    use serde_json::{Value, json};

    type IpmiCalls = Arc<Mutex<Vec<(String, Value)>>>;
    async fn reset(
        State(calls): State<IpmiCalls>,
        headers: HeaderMap,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        use base64::Engine;
        assert_eq!(
            headers["authorization"].to_str().unwrap(),
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("admin:test-password")
            )
        );
        calls
            .lock()
            .unwrap()
            .push((headers["forwarded"].to_str().unwrap().to_string(), payload));
        Json(json!({"success":true}))
    }
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await.host();
    let mut txn = env.db_txn().await;
    let bmc = host.bmc_access(&mut txn).await;
    let address = bmc.host.parse::<std::net::IpAddr>()?;
    let endpoint = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![address])
        .await?
        .pop()
        .expect("ingested endpoint");
    let mut report = endpoint.report;
    report
        .service_root
        .get_or_insert_with(Default::default)
        .vendor = Some("Lenovo".into());
    report.systems = vec![ComputerSystem {
        id: "1".into(),
        model: Some("ThinkSystem SR650 V4".into()),
        ..Default::default()
    }];
    report.managers = vec![Manager {
        id: "1".into(),
        ipmi_port: Some(6623),
        ..Default::default()
    }];
    assert_eq!(
        db::explored_endpoints::try_update(
            address,
            endpoint.report_version,
            &report,
            false,
            txn.as_mut()
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await?;

    let calls = IpmiCalls::default();
    let (started, preparing) = tokio::sync::oneshot::channel();
    let (resume, gate) = tokio::sync::oneshot::channel();
    let pause = Arc::new(Mutex::new(Some((started, gate))));
    let router = Router::new()
        .route("/ipmi", axum::routing::post(reset))
        .with_state(calls.clone())
        .fallback_service(bmc_mock::test_support::lenovo_thinksystem_sr670_router())
        .layer(from_fn(move |req: HttpRequest, next: Next| {
            let gate = pause.lock().unwrap().take();
            async move {
                if let Some((started, gate)) = gate {
                    started.send(()).unwrap();
                    gate.await.unwrap();
                }
                next.run(req).await
            }
        }));
    let (_server, url) = bmc_mock::test_support::serve_https("admin-driver-power", router);
    let proxy = Arc::new(arc_swap::ArcSwap::from_pointee(Some(
        carbide_utils::HostPortPair::HostAndPort("127.0.0.1".into(), url.port().unwrap()),
    )));
    let pool = carbide_redfish::nv_redfish::new_pool(proxy.clone());
    let empty_store = Arc::new(TestCredentialManager::default());
    let ipmi = carbide_ipmi::bmc_mock(
        proxy,
        empty_store.clone(),
        carbide_instrument::Instrumentation::noop(),
    );
    let connector = bmc_runtime::DriverConnector::new(
        bmc_runtime::DriverPools {
            routed: pool.clone(),
            direct: pool.clone(),
        },
        empty_store,
        "admin-power-test",
        bmc_drivers::built_in_rules(),
        ipmi.clone(),
        carbide_instrument::Instrumentation::noop(),
    )?;
    let mut api = power_api(&env, &bmc).await?;
    api.bmc_client = Arc::new(
        carbide_site_explorer::AuthenticatedBmcClient::new(
            env.redfish_sim.clone(),
            pool,
            None,
            ipmi,
            env.api.credential_manager.clone(),
        )
        .with_driver_connector(Some(Arc::new(connector))),
    );
    let request = AdminPowerControlRequest {
        machine_id: Some(host.id.to_string()),
        action: SystemPowerControl::GracefulRestart.into(),
        ..Default::default()
    };
    let mut rpc = std::pin::pin!(api.admin_power_control(Request::new(request.clone())));
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::select! {
            result = &mut rpc => panic!("RPC ended before driver client preparation: {result:?}"),
            started = preparing => started.unwrap(),
        }
    })
    .await?;
    let mut txn = env.db_txn().await;
    let mut endpoint = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![address])
        .await?
        .pop()
        .expect("prepared endpoint");
    assert_eq!(endpoint.report.managers[0].ipmi_port, Some(6623));
    endpoint.report.managers[0].ipmi_port = Some(7623);
    assert_eq!(
        db::explored_endpoints::try_update(
            address,
            endpoint.report_version,
            &endpoint.report,
            false,
            txn.as_mut()
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await?;
    resume.send(()).unwrap();
    let refusal = rpc.await.unwrap_err();
    assert_eq!(refusal.code(), tonic::Code::FailedPrecondition);
    assert!(
        refusal
            .message()
            .contains("changed during client preparation")
    );
    assert!(calls.lock().unwrap().is_empty());
    let response = api
        .admin_power_control(Request::new(request))
        .await?
        .into_inner();
    assert!(response.msg.is_none());
    assert_eq!(
        *calls.lock().unwrap(),
        [(
            format!("host={address}"),
            json!({"action":"chassis_power_reset"})
        )]
    );
    Ok(())
}
