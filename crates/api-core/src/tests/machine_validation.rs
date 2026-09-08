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

use std::str::FromStr;
use std::time::SystemTime;

use carbide_machine_controller::config::machine_validation::{
    MachineValidationConfig, MachineValidationTestConfig, MachineValidationTestSelectionMode,
};
use carbide_machine_controller::handler::MachineStateHandlerBuilder;
use carbide_uuid::machine_validation::{MachineValidationId, MachineValidationRunItemId};
use common::api_fixtures::{
    TestEnvOverrides, create_host_with_machine_validation, create_test_env,
    create_test_env_with_overrides, get_config, get_machine_validation_results,
    get_machine_validation_runs, on_demand_machine_validation,
};
use config_version::ConfigVersion;
use model::machine::{
    FailureCause, FailureDetails, FailureSource, MachineState, MachineValidatingState,
    MachineValidationFilter, ManagedHostState, ValidationState,
};
use rpc::Timestamp;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    MachineValidationTestFullHostApprovalRequest, MachineValidationTestNextVersionRequest,
    MachineValidationTestVerfiedRequest,
};

use crate::handlers::machine_validation::apply_config_on_startup;
use crate::tests::common;

#[crate::sqlx_test]
async fn test_machine_validation_complete_with_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mh = create_host_with_machine_validation(&env, None, Some("Test Error".to_owned())).await;

    let mut txn = env.pool.begin().await?;

    let machine = mh.dpu().db_machine(&mut txn).await;
    match machine.current_state() {
        ManagedHostState::Failed {
            details,
            machine_id: _,
            retry_count: _,
        } => {
            let FailureDetails { cause, source, .. } = details;
            assert_eq!(
                cause,
                &FailureCause::MachineValidation {
                    err: "Test Error".to_owned()
                }
            );
            assert_eq!(source, &FailureSource::Scout);
        }
        s => {
            panic!("Incorrect state: {s}");
        }
    }

    let machine = mh.host().rpc_machine().await;
    let health = machine.status.as_ref().unwrap().health.as_ref().unwrap();
    assert_eq!(health.alerts.len(), 1);
    let mut alert = health.alerts[0].clone();
    assert!(alert.in_alert_since.is_some());
    alert.in_alert_since = None;
    assert_eq!(
        alert,
        health_report::HealthProbeAlert {
            id: "FailedValidationTestCompletion".parse().unwrap(),
            target: None,
            in_alert_since: None,
            message: "Validation test failed to run to completion:\nTest Error".to_string(),
            tenant_message: None,
            classifications: vec![health_report::HealthAlertClassification::prevent_allocations()],
        }
        .into()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_validation_with_error(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let machine_validation_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "Some Error".to_string(),
        context: "Discovery".to_string(),
        exit_code: -1,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };

    let mh =
        create_host_with_machine_validation(&env, Some(machine_validation_result.clone()), None)
            .await;

    let mut txn = env.pool.begin().await?;

    let machine = mh.dpu().db_machine(&mut txn).await;

    match machine.current_state() {
        ManagedHostState::Failed {
            details,
            machine_id: _,
            retry_count,
        } => {
            let FailureDetails { cause, source, .. } = details;
            assert_eq!(
                cause,
                &FailureCause::MachineValidation {
                    err: format!("{} is failed", machine_validation_result.name),
                }
            );
            assert_eq!(source, &FailureSource::Scout);
            // assert_eq!(machine_id, host_machine_id);
            assert_eq!(*retry_count, 0);
        }
        s => {
            panic!("Incorrect state: {s}");
        }
    }

    let machine = mh.host().rpc_machine().await;
    let health = machine.status.as_ref().unwrap().health.as_ref().unwrap();
    assert_eq!(health.alerts.len(), 1);
    let mut alert = health.alerts[0].clone();
    assert!(alert.in_alert_since.is_some());
    alert.in_alert_since = None;
    assert_eq!(
        alert,
        health_report::HealthProbeAlert {
            id: "FailedValidationTest".parse().unwrap(),
            target: Some("test1".to_string()),
            in_alert_since: None,
            message: "Failed validation test:\nName:test1\nCommand:echo\nArgs:test".to_string(),
            tenant_message: None,
            classifications: vec![health_report::HealthAlertClassification::prevent_allocations()],
        }
        .into()
    );

    let _ = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        Vec::new(),
        false,
        Vec::new(),
    )
    .await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "OnDemand".to_string(),
                    id: MachineValidationId::new(),
                    completed: 1,
                    total: 1,
                    is_enabled: env.config.machine_validation_config.enabled,
                },
            },
        },
    )
    .await;
    let response = mh.host().forge_agent_control().await;
    let Some(rpc::forge_agent_control_response::Action::MachineValidation(machine_validation)) =
        response.action
    else {
        panic!("expected machine validation action");
    };
    let validation_id = machine_validation
        .validation_id
        .expect("machine validation action missing validation_id");

    env.api
        .machine_validation_completed(tonic::Request::new(
            rpc::forge::MachineValidationCompletedRequest {
                machine_id: Some(mh.host().id.into()),
                machine_validation_error: None,
                validation_id: Some(validation_id),
            },
        ))
        .await?;

    // Regression for #4876: the host state has not advanced yet, so this
    // reproduces a Scout poll after the run was completed. It must not
    // re-dispatch the terminal run.
    let response = mh.host().forge_agent_control().await;
    assert!(matches!(
        response.action,
        Some(rpc::forge_agent_control_response::Action::Noop(_))
    ));

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: !env.config.machine_validation_config.enabled,
            },
        },
    )
    .await;

    let machine = mh.host().rpc_machine().await;
    let health = machine.status.as_ref().unwrap().health.as_ref().unwrap();
    assert_eq!(health.alerts.len(), 0);
    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_validation(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let machine_validation_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Discovery".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };

    let mh =
        create_host_with_machine_validation(&env, Some(machine_validation_result.clone()), None)
            .await;

    let mut txn = env.pool.begin().await?;

    let machine = mh.dpu().db_machine(&mut txn).await;
    txn.commit().await.unwrap();

    match machine.current_state() {
        ManagedHostState::Ready => {}
        s => {
            panic!("Incorrect state: {s}");
        }
    }

    let machine = mh.host().rpc_machine().await;
    assert!(
        machine
            .status
            .as_ref()
            .unwrap()
            .health
            .as_ref()
            .unwrap()
            .alerts
            .is_empty()
    );

    let _ = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        Vec::new(),
        false,
        Vec::new(),
    )
    .await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "OnDemand".to_string(),
                    id: MachineValidationId::new(),
                    completed: 1,
                    total: 1,
                    is_enabled: env.config.machine_validation_config.enabled,
                },
            },
        },
    )
    .await;
    mh.machine_validation_completed().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: !env.config.machine_validation_config.enabled,
            },
        },
    )
    .await;
    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_validation_get_results(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let machine_validation_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Discovery".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };

    let mh =
        create_host_with_machine_validation(&env, Some(machine_validation_result.clone()), None)
            .await;

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let runs = get_machine_validation_runs(&env, &mh.host().id, false).await;
    assert_eq!(runs.runs.len(), 1);
    assert_eq!(
        runs.runs[0].context.clone().unwrap_or_default(),
        "Discovery".to_owned()
    );
    let discovery_validation_id = runs.runs[0].validation_id;
    tinstance.delete().await;

    // one for cleanup and one for discovery
    let runs = get_machine_validation_runs(&env, &mh.host().id, false).await;
    assert_eq!(runs.runs.len(), 2);

    let results = get_machine_validation_results(&env, Some(&mh.host().id), true, None).await;
    assert_eq!(results.results.len(), 2);
    assert_eq!(results.results[0].name, machine_validation_result.name);
    assert_eq!(results.results[1].name, "instance".to_owned());
    let cleanup_validation_id = results.results[1].validation_id;

    // find using validation id
    let results = get_machine_validation_results(&env, None, true, discovery_validation_id).await;
    assert_eq!(results.results.len(), 1);
    assert_eq!(results.results[0].name, machine_validation_result.name);

    // find using machine and validation id
    let results =
        get_machine_validation_results(&env, Some(&mh.host().id), true, cleanup_validation_id)
            .await;
    assert_eq!(results.results.len(), 1);
    assert_eq!(results.results[0].name, "instance".to_owned());

    let machine = mh.host().rpc_machine().await;
    assert!(
        machine
            .status
            .as_ref()
            .unwrap()
            .health
            .as_ref()
            .unwrap()
            .alerts
            .is_empty()
    );

    Ok(())
}

#[crate::sqlx_test]
#[ignore = "RBAC (secure_mv): AddUpdateMachineValidationExternalConfig has no principals until external config + MV path is hardened"]
async fn test_create_update_external_config(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let input = r#"
    {
        "ADDRESS": "shoreline.nvidia.com",
        "SECRET": "somesecret"
    }
    "#;
    let name = "shoreline";
    let desc = "shoreline description";
    env.api
        .add_update_machine_validation_external_config(tonic::Request::new(
            rpc::forge::AddUpdateMachineValidationExternalConfigRequest {
                name: name.to_string(),
                description: Some(desc.to_string()),
                config: input.as_bytes().to_vec(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let res = env
        .api
        .get_machine_validation_external_config(tonic::Request::new(
            rpc::forge::GetMachineValidationExternalConfigRequest {
                name: name.to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(res.config.clone().unwrap().name, name);
    assert_eq!(res.config.clone().unwrap().description.unwrap(), desc);
    assert_eq!(res.config.clone().unwrap().version, "1");
    assert_eq!(res.config.unwrap().config, input.as_bytes().to_vec());
    // Update one more time
    env.api
        .add_update_machine_validation_external_config(tonic::Request::new(
            rpc::forge::AddUpdateMachineValidationExternalConfigRequest {
                name: name.to_string(),
                description: Some(desc.to_string()),
                config: input.as_bytes().to_vec(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let res_next = env
        .api
        .get_machine_validation_external_config(tonic::Request::new(
            rpc::forge::GetMachineValidationExternalConfigRequest {
                name: name.to_string(),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(res_next.config.clone().unwrap().name, name);
    assert_eq!(res_next.config.clone().unwrap().description.unwrap(), desc);
    assert_eq!(&res_next.config.clone().unwrap().version, "2");
    assert_eq!(res_next.config.unwrap().config, input.as_bytes().to_vec());
    let res_list = env
        .api
        .get_machine_validation_external_configs(tonic::Request::new(
            rpc::forge::GetMachineValidationExternalConfigsRequest {
                names: vec!["shoreline".to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res_list.configs[0].name, "shoreline");
    assert_eq!(res_list.configs.len(), 1);

    // remove
    env.api
        .remove_machine_validation_external_config(tonic::Request::new(
            rpc::forge::RemoveMachineValidationExternalConfigRequest {
                name: res_list.configs[0].name.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let remove_res_list = env
        .api
        .get_machine_validation_external_configs(tonic::Request::new(
            rpc::forge::GetMachineValidationExternalConfigsRequest { names: Vec::new() },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(remove_res_list.configs.len(), 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_validation_test_on_demand_filter(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let machine_validation_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Discovery".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };

    let mh =
        create_host_with_machine_validation(&env, Some(machine_validation_result.clone()), None)
            .await;

    let mut txn = env.pool.begin().await?;
    let machine = mh.dpu().db_machine(&mut txn).await;
    txn.commit().await.unwrap();

    match machine.current_state() {
        ManagedHostState::Ready => {}
        s => {
            panic!("Incorrect state: {s}");
        }
    }

    let machine = mh.host().rpc_machine().await;
    assert!(
        machine
            .status
            .as_ref()
            .unwrap()
            .health
            .as_ref()
            .unwrap()
            .alerts
            .is_empty()
    );
    let allowed_tests = vec!["test1".to_string(), "test2".to_string()];
    let on_demand_response = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        allowed_tests.clone(),
        false,
        Vec::new(),
    )
    .await;

    let run = on_demand_response.run.as_ref().unwrap();
    assert_eq!(run.validation_id, on_demand_response.validation_id);
    assert_eq!(run.machine_id, machine.id);
    assert!(run.start_time.is_some());
    assert_eq!(run.context.as_deref(), Some("OnDemand"));

    let validation_id: MachineValidationId = on_demand_response.validation_id.unwrap();
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost { validation_id },
            },
        },
    )
    .await;

    let _ = mh.host().reboot_completed().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "OnDemand".to_string(),
                    id: validation_id,
                    completed: 1,
                    total: 1,
                    is_enabled: env.config.machine_validation_config.enabled,
                },
            },
        },
    )
    .await;

    let response = mh.host().forge_agent_control().await;
    let Some(rpc::forge_agent_control_response::Action::MachineValidation(machine_validation)) =
        response.action.as_ref()
    else {
        panic!("expected typed machine validation action");
    };
    let typed_filter = machine_validation
        .filter
        .as_ref()
        .expect("typed machine validation filter");
    assert!(
        allowed_tests
            .iter()
            .all(|item| typed_filter.allowed_tests.contains(item))
    );

    for item in response.data.unwrap().pair {
        if item.key == "MachineValidationFilter" {
            let machine_validation_filter: MachineValidationFilter =
                serde_json::from_str(&item.value)?;
            assert!(
                allowed_tests
                    .clone()
                    .iter()
                    .all(|item| machine_validation_filter.allowed_tests.contains(item))
            );
        }
    }

    mh.machine_validation_completed().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: !env.config.machine_validation_config.enabled,
            },
        },
    )
    .await;
    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_validation_disabled_waits_for_in_flight_reboot(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mh = create_host_with_machine_validation(&env, None, None).await;
    let machine = mh.host().rpc_machine().await;
    assert!(
        machine
            .status
            .as_ref()
            .unwrap()
            .health
            .as_ref()
            .unwrap()
            .alerts
            .is_empty()
    );

    let on_demand_response = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        Vec::new(),
        false,
        Vec::new(),
    )
    .await;
    let validation_id = on_demand_response.validation_id.unwrap();

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost { validation_id },
            },
        },
    )
    .await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "OnDemand".to_string(),
                    id: validation_id,
                    completed: 1,
                    total: 1,
                    is_enabled: env.config.machine_validation_config.enabled,
                },
            },
        },
    )
    .await;

    let reboot_request_before_wait = {
        let mut txn = env.pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;
        let reboot = machine
            .status
            .last_reboot_requested
            .map(|last_reboot| (last_reboot.time, last_reboot.mode));
        txn.commit().await?;
        reboot
    };

    let mut machine_validation_config = env.config.machine_validation_config.clone();
    machine_validation_config.enabled = false;
    let handler = MachineStateHandlerBuilder::builder()
        .hardware_models(env.config.get_firmware_config())
        .reachability_params(env.reachability_params)
        .attestation_enabled(env.attestation_enabled)
        .common_pools(env.common_pools.clone())
        .dpu_enable_secure_boot(env.config.dpu_config.dpu_enable_secure_boot)
        .machine_validation_config(machine_validation_config)
        .bom_validation(env.config.bom_validation)
        .instance_autoreboot_period(
            env.config
                .machine_updater
                .instance_autoreboot_period
                .clone(),
        )
        .power_options_config(env.config.power_manager_options.clone().into())
        .build();
    env.override_machine_state_controller_handler(handler).await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "OnDemand".to_string(),
                    id: validation_id,
                    completed: 1,
                    total: 1,
                    is_enabled: env.config.machine_validation_config.enabled,
                },
            },
        },
    )
    .await;

    let reboot_request_after_wait = {
        let mut txn = env.pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;
        let reboot = machine
            .status
            .last_reboot_requested
            .map(|last_reboot| (last_reboot.time, last_reboot.mode));
        txn.commit().await?;
        reboot
    };
    assert_eq!(reboot_request_after_wait, reboot_request_before_wait);

    mh.host().reboot_completed().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: true,
            },
        },
    )
    .await;

    let skipped_state_int =
        rpc::forge::machine_validation_status::MachineValidationState::Completed(
            rpc::forge::machine_validation_status::MachineValidationCompleted::Skipped.into(),
        );
    let runs = get_machine_validation_runs(&env, &mh.host().id, true).await;
    let mut status_asserted = false;
    for run in runs.runs {
        if run.validation_id.unwrap_or_default() == validation_id {
            status_asserted = true;
            assert_eq!(
                run.status
                    .unwrap_or_default()
                    .machine_validation_state
                    .unwrap_or(skipped_state_int),
                skipped_state_int
            );
        }
    }
    assert!(status_asserted);

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Ready,
    )
    .await;

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
#[ignore = "RBAC (secure_mv): AddMachineValidationTest has no principals until MV execution path is hardened"]
async fn test_machine_validation_add_new_test_case(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let request = rpc::forge::MachineValidationTestAddRequest {
        name: "dcgm_short_test".to_string(),
        description: Some("Run run level 3 test cases".to_string()),
        contexts: vec![
            "Discovery".to_string(),
            "CleanUp".to_string(),
            "OnDemand".to_string(),
        ],
        img_name: Some("".to_string()),
        execute_in_host: Some(false),
        container_arg: Some("".to_string()),
        command: "dcgmi".to_string(),
        args: "diag -r 2".to_string(),
        extra_output_file: Some("/tmp/output".to_string()),
        extra_err_file: Some("/tmp/error".to_string()),
        external_config_file: Some("".to_string()),
        pre_condition: Some("nvdia-smi".to_string()),
        timeout: Some(10),
        supported_platforms: vec![
            "Sku 090e modelname poweredge r750".to_string(),
            "7z73cto1ww".to_string(),
        ],
        read_only: None,
        custom_tags: vec!["dgxcloud".to_string()],
        components: vec!["GPU".to_string()],
        is_enabled: Some(true),
        plugin: None,
    };
    let add_update_response = env
        .api
        .add_machine_validation_test(tonic::Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();

    let test_list = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some(add_update_response.clone().test_id),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    assert_eq!(test_list.len(), 1);
    assert_eq!(add_update_response.clone().test_id, test_list[0].test_id);
    assert_eq!(add_update_response.clone().version, test_list[0].version);

    assert_eq!(test_list[0].name, request.name);
    assert!(!test_list[0].verified);

    assert_eq!(test_list[0].name, request.name);

    assert_eq!(test_list[0].command, request.command);

    assert_eq!(test_list[0].description, request.description);
    assert_eq!(test_list[0].contexts, request.contexts);
    assert_eq!(
        test_list[0].supported_platforms,
        vec![
            "sku_090e_modelname_poweredge_r750".to_string(),
            "7z73cto1ww".to_string(),
        ]
    );
    assert_eq!(test_list[0].img_name, request.img_name);
    assert_eq!(test_list[0].execute_in_host, request.execute_in_host);

    assert_eq!(test_list[0].container_arg, request.container_arg);
    assert_eq!(test_list[0].command, request.command);
    assert_eq!(test_list[0].args, request.args);

    assert_eq!(test_list[0].extra_output_file, request.extra_output_file);
    assert_eq!(test_list[0].extra_err_file, request.extra_err_file);
    assert_eq!(test_list[0].pre_condition, request.pre_condition);
    assert_eq!(test_list[0].timeout, request.timeout);
    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
#[ignore = "RBAC (secure_mv): UpdateMachineValidationTest has no principals until MV execution path is hardened"]
async fn test_machine_validation_update_existing_test(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let existing_test_list = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some("forge_dcgm_long_test".to_string()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    let update_payload = rpc::forge::machine_validation_test_update_request::Payload {
        contexts: vec!["Discovery".to_string(), "CleanUp".to_string()],
        img_name: Some("nvcr.io/nvidia/shoreline:latest".to_string()),
        execute_in_host: Some(true),
        extra_output_file: Some("/tmp/output".to_string()),
        external_config_file: Some("/tmp/shoreline".to_string()),
        timeout: Some(100),
        supported_platforms: vec![
            "Sku 090e modelname poweredge r750".to_string(),
            "7z73cto1ww".to_string(),
        ],
        ..rpc::forge::machine_validation_test_update_request::Payload::default()
    };
    let update_request = rpc::forge::MachineValidationTestUpdateRequest {
        test_id: existing_test_list[0].test_id.clone(),
        payload: Some(update_payload.clone()),
        version: existing_test_list[0].version.clone(),
    };

    let add_update_response = env
        .api
        .update_machine_validation_test(tonic::Request::new(update_request.clone()))
        .await
        .unwrap()
        .into_inner();
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some(add_update_response.test_id.clone()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;

    assert_eq!(updated_tests[0].contexts, update_payload.contexts);
    assert_eq!(updated_tests[0].test_id, add_update_response.test_id);
    assert_eq!(updated_tests[0].version, add_update_response.version);
    assert_eq!(updated_tests[0].img_name, update_payload.img_name);
    assert_eq!(
        updated_tests[0].execute_in_host,
        update_payload.execute_in_host
    );

    assert_eq!(
        updated_tests[0].external_config_file,
        update_payload.external_config_file
    );
    assert_eq!(
        updated_tests[0].extra_output_file,
        update_payload.extra_output_file
    );
    assert_eq!(updated_tests[0].timeout, update_payload.timeout);
    assert!(!updated_tests[0].verified);
    assert_eq!(
        updated_tests[0].supported_platforms,
        vec![
            "sku_090e_modelname_poweredge_r750".to_string(),
            "7z73cto1ww".to_string(),
        ]
    );
    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_mark_test_as_verfied(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let existing_test_list = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some("forge_dcgm_long_test".to_string()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    assert!(!existing_test_list[0].verified);

    let return_message = env
        .api
        .machine_validation_test_verfied(tonic::Request::new(MachineValidationTestVerfiedRequest {
            test_id: existing_test_list[0].test_id.clone(),
            version: existing_test_list[0].version.to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .message;
    let tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some(existing_test_list[0].test_id.clone()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    assert!(tests[0].verified);
    assert_eq!(return_message, "Success");
    Ok(())
}

#[crate::sqlx_test]
async fn plugin_full_host_approval_and_enablement_are_server_managed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.machine_validation_config.approved_plugin_registries =
        vec!["registry.example.com".to_owned()];
    config.machine_validation_config.allow_privileged_plugins = true;
    config.machine_validation_config.allow_full_host_plugins = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let plugin = rpc::forge::MachineValidationPlugin {
        image: "registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        entrypoint: vec!["/plugin/entrypoint".to_owned()],
        parameters_json: "{}".to_owned(),
        privileged: true,
        host_access_full: true,
    };
    let created = env
        .api
        .add_machine_validation_test(tonic::Request::new(
            rpc::forge::MachineValidationTestAddRequest {
                name: "gpu-health".to_owned(),
                contexts: vec!["Discovery".to_owned()],
                command: String::new(),
                args: String::new(),
                timeout: Some(60),
                plugin: Some(plugin),
                ..Default::default()
            },
        ))
        .await?
        .into_inner();

    let enable_before_verification = env
        .api
        .machine_validation_test_enable_disable_test(tonic::Request::new(
            rpc::forge::MachineValidationTestEnableDisableTestRequest {
                test_id: created.test_id.clone(),
                version: created.version.clone(),
                is_enabled: true,
            },
        ))
        .await;
    assert_eq!(
        enable_before_verification.unwrap_err().code(),
        tonic::Code::InvalidArgument
    );

    env.api
        .machine_validation_test_verfied(tonic::Request::new(MachineValidationTestVerfiedRequest {
            test_id: created.test_id.clone(),
            version: created.version.clone(),
        }))
        .await?;

    // Legacy test-selection policy must not bypass plugin approval state.
    apply_config_on_startup(
        &env.api,
        &MachineValidationConfig {
            test_selection_mode: MachineValidationTestSelectionMode::EnableAll,
            approved_plugin_registries: vec!["registry.example.com".to_owned()],
            allow_privileged_plugins: true,
            allow_full_host_plugins: true,
            ..Default::default()
        },
    )
    .await?;
    let after_startup_policy = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some(created.test_id.clone()),
                version: Some(created.version.clone()),
                ..Default::default()
            },
        ))
        .await?
        .into_inner()
        .tests
        .pop()
        .expect("created plugin revision must be returned");
    assert!(!after_startup_policy.is_enabled);

    let enable_before_approval = env
        .api
        .machine_validation_test_enable_disable_test(tonic::Request::new(
            rpc::forge::MachineValidationTestEnableDisableTestRequest {
                test_id: created.test_id.clone(),
                version: created.version.clone(),
                is_enabled: true,
            },
        ))
        .await;
    assert_eq!(
        enable_before_approval.unwrap_err().code(),
        tonic::Code::InvalidArgument
    );

    env.api
        .machine_validation_test_approve_full_host(tonic::Request::new(
            MachineValidationTestFullHostApprovalRequest {
                test_id: created.test_id.clone(),
                version: created.version.clone(),
            },
        ))
        .await?;
    env.api
        .machine_validation_test_enable_disable_test(tonic::Request::new(
            rpc::forge::MachineValidationTestEnableDisableTestRequest {
                test_id: created.test_id.clone(),
                version: created.version.clone(),
                is_enabled: true,
            },
        ))
        .await?;

    let test = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some(created.test_id),
                version: Some(created.version),
                ..Default::default()
            },
        ))
        .await?
        .into_inner()
        .tests
        .pop()
        .expect("created plugin revision must be returned");
    assert!(test.verified);
    assert!(test.full_host_approved);
    assert!(test.is_enabled);
    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_create_clones(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let existing_test_list = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some("forge_dcgm_long_test".to_string()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    let next_version = env
        .api
        .machine_validation_test_next_version(tonic::Request::new(
            MachineValidationTestNextVersionRequest {
                test_id: existing_test_list[0].test_id.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        ConfigVersion::from_str(&next_version.version)?.version_nr(),
        2
    );
    let next_version = env
        .api
        .machine_validation_test_next_version(tonic::Request::new(
            MachineValidationTestNextVersionRequest {
                test_id: existing_test_list[0].test_id.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        ConfigVersion::from_str(&next_version.version)?.version_nr(),
        3
    );
    let tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some(existing_test_list[0].test_id.clone()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    assert_eq!(tests.len(), 3);
    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_test_disabled(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let existing_test_list = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    assert_eq!(existing_test_list.len(), 13);

    let _ = env
        .api
        .machine_validation_test_enable_disable_test(tonic::Request::new(
            rpc::forge::MachineValidationTestEnableDisableTestRequest {
                test_id: existing_test_list[0].test_id.clone(),
                version: existing_test_list[0].version.clone(),
                is_enabled: false,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let _ = env
        .api
        .machine_validation_test_enable_disable_test(tonic::Request::new(
            rpc::forge::MachineValidationTestEnableDisableTestRequest {
                test_id: existing_test_list[1].test_id.clone(),
                version: existing_test_list[1].version.clone(),
                is_enabled: false,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                is_enabled: Some(true),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    assert_eq!(updated_tests.len(), 2);

    Ok(())
}

#[crate::sqlx_test]
async fn test_on_demant_un_verified_machine_validation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let machine_validation_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Discovery".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };

    let mh =
        create_host_with_machine_validation(&env, Some(machine_validation_result.clone()), None)
            .await;

    let mut txn = env.pool.begin().await?;
    let machine = mh.dpu().db_machine(&mut txn).await;
    match machine.current_state() {
        ManagedHostState::Ready => {}
        s => {
            panic!("Incorrect state: {s}");
        }
    }

    let machine = mh.host().rpc_machine().await;
    assert!(
        machine
            .status
            .as_ref()
            .unwrap()
            .health
            .as_ref()
            .unwrap()
            .alerts
            .is_empty()
    );
    let allowed_tests = vec!["test1".to_string(), "test2".to_string()];
    let on_demand_response = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        allowed_tests.clone(),
        true,
        Vec::new(),
    )
    .await;
    let validation_id = on_demand_response.validation_id.unwrap();
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost { validation_id },
            },
        },
    )
    .await;
    let _ = mh.host().reboot_completed().await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "OnDemand".to_string(),
                    id: validation_id,
                    completed: 1,
                    total: 1,
                    is_enabled: env.config.machine_validation_config.enabled,
                },
            },
        },
    )
    .await;
    let response = mh.host().forge_agent_control().await;

    for item in response.data.unwrap().pair {
        if item.key == "MachineValidationFilter" {
            let machine_validation_filter: MachineValidationFilter =
                serde_json::from_str(&item.value)?;
            assert!(
                machine_validation_filter
                    .run_unverfied_tests
                    .unwrap_or_default()
            );
        }
    }

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
#[ignore = "RBAC (secure_mv): depends on AddMachineValidationTest (denied until MV execution path is hardened)"]
async fn test_machine_validation_get_unverified_tests(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let request = rpc::forge::MachineValidationTestAddRequest {
        name: "dcgm_short_test".to_string(),
        description: Some("Run run level 3 test cases".to_string()),
        contexts: vec![
            "Discovery".to_string(),
            "CleanUp".to_string(),
            "OnDemand".to_string(),
        ],
        img_name: Some("".to_string()),
        execute_in_host: Some(false),
        container_arg: Some("".to_string()),
        command: "dcgmi".to_string(),
        args: "diag -r 2".to_string(),
        extra_output_file: Some("/tmp/output".to_string()),
        extra_err_file: Some("/tmp/error".to_string()),
        external_config_file: Some("".to_string()),
        pre_condition: Some("nvdia-smi".to_string()),
        timeout: Some(10),
        supported_platforms: vec![
            "sku_090e_modelname_poweredge_r750".to_string(),
            "7z73cto1ww".to_string(),
        ],
        read_only: None,
        custom_tags: vec!["dgxcloud".to_string()],
        components: vec!["GPU".to_string()],
        is_enabled: Some(true),
        plugin: None,
    };
    let add_update_response = env
        .api
        .add_machine_validation_test(tonic::Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();

    let test_list = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                verified: Some(false),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .tests;
    assert_eq!(test_list.len(), 6);
    assert_eq!(add_update_response.clone().test_id, test_list[0].test_id);
    assert!(!test_list[0].verified);
    assert!(!test_list[1].verified);
    assert!(!test_list[2].verified);
    assert!(!test_list[3].verified);
    assert!(!test_list[4].verified);
    assert!(!test_list[5].verified);

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_m1_persists_selected_test_and_idempotent_result(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let initial_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Discovery".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };
    let mh = create_host_with_machine_validation(&env, Some(initial_result), None).await;
    let machine = mh.host().rpc_machine().await;

    let selected_test = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some("forge_dcgm_long_test".to_string()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await?
        .into_inner()
        .tests
        .into_iter()
        .next()
        .expect("machine validation fixture should include forge_dcgm_long_test");

    let on_demand_response = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        vec![selected_test.test_id.clone()],
        true,
        vec!["OnDemand".to_string()],
    )
    .await;
    let validation_id = on_demand_response.validation_id.unwrap();

    let mismatch = env
        .api
        .update_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationRunRequest {
                validation_id: Some(validation_id),
                duration_to_complete: Some(rpc::Duration::from(std::time::Duration::from_secs(
                    selected_test.timeout.unwrap_or(7200).try_into().unwrap(),
                ))),
                total: 2,
                selected_tests: vec![selected_test.clone()],
            },
        ))
        .await;
    let Err(status) = mismatch else {
        panic!("update_machine_validation_run should reject mismatched total");
    };
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("selected_tests"));

    env.api
        .update_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationRunRequest {
                validation_id: Some(validation_id),
                duration_to_complete: Some(rpc::Duration::from(std::time::Duration::from_secs(
                    selected_test.timeout.unwrap_or(7200).try_into().unwrap(),
                ))),
                total: 1,
                selected_tests: vec![selected_test.clone()],
            },
        ))
        .await?;

    let run_item_ids = env
        .api
        .find_machine_validation_run_item_ids(tonic::Request::new(
            rpc::forge::MachineValidationRunItemSearchFilter {
                validation_id: Some(validation_id),
            },
        ))
        .await?
        .into_inner()
        .run_item_ids;
    assert_eq!(run_item_ids.len(), 1);

    let run_items = env
        .api
        .find_machine_validation_run_items_by_ids(tonic::Request::new(
            rpc::forge::MachineValidationRunItemsByIdsRequest { run_item_ids },
        ))
        .await?
        .into_inner()
        .run_items;
    assert_eq!(run_items.len(), 1);
    assert_eq!(run_items[0].test_id, selected_test.test_id);
    assert_eq!(run_items[0].state, "Pending");
    assert!(run_items[0].current_attempt_id.is_some());

    let run_item_id = MachineValidationRunItemId::from(uuid::Uuid::try_from(
        run_items[0].run_item_id.as_ref().unwrap(),
    )?);
    let pending_attempts =
        db::machine_validation_execution::find_attempts_by_run_item_id(&env.pool, &run_item_id)
            .await?;
    assert_eq!(pending_attempts.len(), 1);
    assert_eq!(pending_attempts[0].state.to_string(), "Pending");

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost { validation_id },
            },
        },
    )
    .await;
    let _ = mh.host().reboot_completed().await;
    env.run_machine_state_controller_iteration_until_state_condition(&mh.host().id, 1, |machine| {
        match machine.current_state() {
            ManagedHostState::Validation {
                validation_state:
                    ValidationState::MachineValidation {
                        machine_validation: MachineValidatingState::MachineValidating { id, .. },
                    },
            } => *id == validation_id,
            _ => false,
        }
    })
    .await;

    let invalid_heartbeat = env
        .api
        .heartbeat_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationHeartbeatRequest {
                validation_id: Some(validation_id),
                target: Some(
                    rpc::forge::machine_validation_heartbeat_request::Target::TestId(
                        "unknown_machine_validation_test".to_string(),
                    ),
                ),
            },
        ))
        .await?
        .into_inner();
    assert!(!invalid_heartbeat.accepted);

    let heartbeat = env
        .api
        .heartbeat_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationHeartbeatRequest {
                validation_id: Some(validation_id),
                target: Some(
                    rpc::forge::machine_validation_heartbeat_request::Target::TestId(
                        selected_test.test_id.clone(),
                    ),
                ),
            },
        ))
        .await?
        .into_inner();
    assert!(heartbeat.accepted);

    let running_run_items = env
        .api
        .find_machine_validation_run_items_by_ids(tonic::Request::new(
            rpc::forge::MachineValidationRunItemsByIdsRequest {
                run_item_ids: vec![run_items[0].run_item_id.clone().unwrap()],
            },
        ))
        .await?
        .into_inner()
        .run_items;
    assert_eq!(running_run_items[0].state, "Running");
    assert!(running_run_items[0].last_heartbeat_at.is_some());

    let running_attempts =
        db::machine_validation_execution::find_attempts_by_run_item_id(&env.pool, &run_item_id)
            .await?;
    assert_eq!(running_attempts[0].state.to_string(), "Running");
    assert!(running_attempts[0].last_heartbeat_at.is_some());

    let terminal_result = rpc::forge::MachineValidationResult {
        validation_id: Some(validation_id),
        name: selected_test.name.clone(),
        description: selected_test.description.clone().unwrap_or_default(),
        command: selected_test.command.clone(),
        args: selected_test.args.clone(),
        std_out: "ok".to_string(),
        std_err: String::new(),
        context: "OnDemand".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some(selected_test.test_id.clone()),
    };
    env.api
        .persist_validation_result(tonic::Request::new(
            rpc::forge::MachineValidationResultPostRequest {
                result: Some(terminal_result.clone()),
            },
        ))
        .await?;

    let previous_run_heartbeat =
        db::machine_validation::find_by_id(&env.pool, &validation_id).await?;
    let rejected_heartbeat = env
        .api
        .heartbeat_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationHeartbeatRequest {
                validation_id: Some(validation_id),
                target: Some(
                    rpc::forge::machine_validation_heartbeat_request::Target::TestId(
                        selected_test.test_id.clone(),
                    ),
                ),
            },
        ))
        .await?
        .into_inner();
    assert!(!rejected_heartbeat.accepted);
    let run_after_rejected_heartbeat =
        db::machine_validation::find_by_id(&env.pool, &validation_id).await?;
    assert_eq!(
        run_after_rejected_heartbeat.last_heartbeat_at,
        previous_run_heartbeat.last_heartbeat_at
    );

    let replayed_result = rpc::forge::MachineValidationResult {
        name: "changed replay name".to_string(),
        std_out: "changed replay stdout".to_string(),
        context: "Replay".to_string(),
        ..terminal_result.clone()
    };
    env.api
        .persist_validation_result(tonic::Request::new(
            rpc::forge::MachineValidationResultPostRequest {
                result: Some(replayed_result),
            },
        ))
        .await?;

    let legacy_results =
        db::machine_validation_result::find_by_validation_id(&env.pool, &validation_id).await?;
    assert_eq!(
        legacy_results
            .iter()
            .filter(|result| result.test_id == Some(selected_test.test_id.clone()))
            .count(),
        1
    );

    let terminal_attempts =
        db::machine_validation_execution::find_attempts_by_run_item_id(&env.pool, &run_item_id)
            .await?;
    assert_eq!(terminal_attempts.len(), 1);
    assert_eq!(terminal_attempts[0].state.to_string(), "Success");
    assert_eq!(terminal_attempts[0].exit_code, Some(0));
    assert_eq!(terminal_attempts[0].stdout_summary, Some("ok".to_string()));

    let attempt = env
        .api
        .get_machine_validation_attempt(tonic::Request::new(
            rpc::forge::MachineValidationAttemptGetRequest {
                attempt_id: run_items[0].current_attempt_id.clone(),
            },
        ))
        .await?
        .into_inner();
    assert_eq!(attempt.state, "Success");

    let terminal_run_items = env
        .api
        .find_machine_validation_run_items_by_ids(tonic::Request::new(
            rpc::forge::MachineValidationRunItemsByIdsRequest {
                run_item_ids: vec![run_items[0].run_item_id.clone().unwrap()],
            },
        ))
        .await?
        .into_inner()
        .run_items;
    assert_eq!(terminal_run_items[0].state, "Success");
    assert_eq!(terminal_run_items[0].attempt, 1);
    assert_eq!(terminal_run_items[0].display_name, selected_test.name);
    assert_eq!(terminal_run_items[0].context, "OnDemand");

    let runs = get_machine_validation_runs(&env, &mh.host().id, true).await;
    let run = runs
        .runs
        .into_iter()
        .find(|run| run.validation_id == Some(validation_id))
        .expect("on-demand validation run should be listed");
    assert_eq!(run.status.as_ref().unwrap().completed_tests, 1);
    assert!(run.last_heartbeat_at.is_some());

    let mut config = env.config.machine_validation_config.clone();
    config.stale_run_timeout = std::time::Duration::from_secs(1);
    crate::machine_validation::MachineValidationManager::new(
        env.pool.clone(),
        config,
        opentelemetry::global::meter(
            "test_machine_validation_m1_persists_selected_test_and_idempotent_result",
        ),
    )
    .run_single_iteration()
    .await?;

    let runs = get_machine_validation_runs(&env, &mh.host().id, true).await;
    let success_state_int =
        rpc::forge::machine_validation_status::MachineValidationState::Completed(
            rpc::forge::machine_validation_status::MachineValidationCompleted::Success.into(),
        );
    let run = runs
        .runs
        .into_iter()
        .find(|run| run.validation_id == Some(validation_id))
        .expect("on-demand validation run should be listed");
    assert_eq!(
        run.status
            .unwrap_or_default()
            .machine_validation_state
            .unwrap_or(success_state_int),
        success_state_int
    );

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_manager_reconciles_stale_active_attempt(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let initial_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Discovery".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };
    let mh = create_host_with_machine_validation(&env, Some(initial_result), None).await;
    let machine = mh.host().rpc_machine().await;

    let selected_test = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest {
                test_id: Some("forge_dcgm_long_test".to_string()),
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            },
        ))
        .await?
        .into_inner()
        .tests
        .into_iter()
        .next()
        .expect("machine validation fixture should include forge_dcgm_long_test");

    let on_demand_response = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        vec![selected_test.test_id.clone()],
        true,
        vec!["OnDemand".to_string()],
    )
    .await;
    let validation_id = on_demand_response.validation_id.unwrap();

    env.api
        .update_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationRunRequest {
                validation_id: Some(validation_id),
                duration_to_complete: Some(rpc::Duration::from(std::time::Duration::from_secs(
                    selected_test.timeout.unwrap_or(7200).try_into().unwrap(),
                ))),
                total: 1,
                selected_tests: vec![selected_test.clone()],
            },
        ))
        .await?;

    let run_item_ids = env
        .api
        .find_machine_validation_run_item_ids(tonic::Request::new(
            rpc::forge::MachineValidationRunItemSearchFilter {
                validation_id: Some(validation_id),
            },
        ))
        .await?
        .into_inner()
        .run_item_ids;
    assert_eq!(run_item_ids.len(), 1);
    let run_item_id = MachineValidationRunItemId::from(uuid::Uuid::try_from(&run_item_ids[0])?);

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost { validation_id },
            },
        },
    )
    .await;
    let _ = mh.host().reboot_completed().await;
    env.run_machine_state_controller_iteration_until_state_condition(&mh.host().id, 1, |machine| {
        match machine.current_state() {
            ManagedHostState::Validation {
                validation_state:
                    ValidationState::MachineValidation {
                        machine_validation: MachineValidatingState::MachineValidating { id, .. },
                    },
            } => *id == validation_id,
            _ => false,
        }
    })
    .await;

    let heartbeat = env
        .api
        .heartbeat_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationHeartbeatRequest {
                validation_id: Some(validation_id),
                target: Some(
                    rpc::forge::machine_validation_heartbeat_request::Target::TestId(
                        selected_test.test_id.clone(),
                    ),
                ),
            },
        ))
        .await?
        .into_inner();
    assert!(heartbeat.accepted);

    let running_attempts =
        db::machine_validation_execution::find_attempts_by_run_item_id(&env.pool, &run_item_id)
            .await?;
    let attempt_id = running_attempts[0].id;
    assert_eq!(running_attempts[0].state.to_string(), "Running");

    sqlx::query(
        "UPDATE machine_validation SET last_heartbeat_at = NOW() - INTERVAL '2 days' WHERE id = $1",
    )
    .bind(validation_id)
    .execute(&env.pool)
    .await?;
    sqlx::query(
        "UPDATE machine_validation_run_items SET started_at = NOW() - INTERVAL '2 days', last_heartbeat_at = NOW() - INTERVAL '2 days' WHERE id = $1",
    )
    .bind(run_item_id)
    .execute(&env.pool)
    .await?;
    sqlx::query(
        "UPDATE machine_validation_attempts SET started_at = NOW() - INTERVAL '2 days', last_heartbeat_at = NOW() - INTERVAL '2 days' WHERE id = $1",
    )
    .bind(attempt_id)
    .execute(&env.pool)
    .await?;

    let mut config = env.config.machine_validation_config.clone();
    config.stale_run_timeout = std::time::Duration::from_secs(1);
    crate::machine_validation::MachineValidationManager::new(
        env.pool.clone(),
        config,
        opentelemetry::global::meter(
            "test_machine_validation_manager_reconciles_stale_active_attempt",
        ),
    )
    .run_single_iteration()
    .await?;

    let stale_attempts =
        db::machine_validation_execution::find_attempts_by_run_item_id(&env.pool, &run_item_id)
            .await?;
    assert_eq!(stale_attempts[0].state.to_string(), "Failed");
    assert_eq!(
        stale_attempts[0].failure_classification.as_deref(),
        Some("StaleHeartbeat")
    );
    assert!(
        stale_attempts[0]
            .stderr_summary
            .as_deref()
            .unwrap_or_default()
            .contains("stopped heartbeating")
    );

    let run_items = env
        .api
        .find_machine_validation_run_items_by_ids(tonic::Request::new(
            rpc::forge::MachineValidationRunItemsByIdsRequest { run_item_ids },
        ))
        .await?
        .into_inner()
        .run_items;
    assert_eq!(run_items[0].state, "Failed");
    assert!(
        run_items[0]
            .failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("stopped heartbeating")
    );

    env.api
        .persist_validation_result(tonic::Request::new(
            rpc::forge::MachineValidationResultPostRequest {
                result: Some(rpc::forge::MachineValidationResult {
                    validation_id: Some(validation_id),
                    name: selected_test.name.clone(),
                    description: selected_test.description.clone().unwrap_or_default(),
                    command: selected_test.command.clone(),
                    args: selected_test.args.clone(),
                    std_out: "late ok".to_string(),
                    std_err: String::new(),
                    context: "OnDemand".to_string(),
                    exit_code: 0,
                    start_time: Some(Timestamp::from(SystemTime::now())),
                    end_time: Some(Timestamp::from(SystemTime::now())),
                    test_id: Some(selected_test.test_id.clone()),
                }),
            },
        ))
        .await?;
    let legacy_results =
        db::machine_validation_result::find_by_validation_id(&env.pool, &validation_id).await?;
    assert!(
        !legacy_results
            .iter()
            .any(|result| result.test_id == Some(selected_test.test_id.clone()))
    );

    let runs = get_machine_validation_runs(&env, &mh.host().id, true).await;
    let failed_state_int = rpc::forge::machine_validation_status::MachineValidationState::Completed(
        rpc::forge::machine_validation_status::MachineValidationCompleted::Failed.into(),
    );
    let stale_run = runs
        .runs
        .into_iter()
        .find(|run| run.validation_id == Some(validation_id))
        .expect("stale attempt run should be listed");
    assert_eq!(
        stale_run
            .status
            .unwrap_or_default()
            .machine_validation_state
            .unwrap_or(failed_state_int),
        failed_state_int
    );

    env.run_machine_state_controller_iteration_until_state_condition(&mh.host().id, 1, |machine| {
        matches!(
            machine.current_state(),
            ManagedHostState::Failed {
                retry_count: 0,
                details: FailureDetails {
                    cause: FailureCause::MachineValidation { err },
                    source: FailureSource::Scout,
                    ..
                },
                ..
            } if err.contains("stopped heartbeating")
        )
    })
    .await;

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_validation_manager_reconciles_stale_run(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let machine_validation_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "test1".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Discovery".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };

    let mh = create_host_with_machine_validation(&env, Some(machine_validation_result), None).await;
    let machine = mh.host().rpc_machine().await;
    let on_demand_response = on_demand_machine_validation(
        &env,
        machine.id.unwrap_or_default(),
        Vec::new(),
        Vec::new(),
        false,
        Vec::new(),
    )
    .await;
    let validation_id = on_demand_response.validation_id.unwrap();

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost { validation_id },
            },
        },
    )
    .await;
    let _ = mh.host().reboot_completed().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "OnDemand".to_string(),
                    id: validation_id,
                    completed: 1,
                    total: 1,
                    is_enabled: env.config.machine_validation_config.enabled,
                },
            },
        },
    )
    .await;

    sqlx::query(
        "UPDATE machine_validation SET start_time = NOW() - INTERVAL '2 days' WHERE id = $1",
    )
    .bind(validation_id)
    .execute(&env.pool)
    .await?;

    let mut config = env.config.machine_validation_config.clone();
    config.stale_run_timeout = std::time::Duration::from_secs(1);
    // Reconciling the stale run is the run's one completion, so the outcome
    // counter must move exactly once under the stale-run cause. Only this
    // test drives that cause through the product funnel, and the emit-level
    // test serializes behind the same capture lock, so the exact delta is
    // race-free.
    let metrics = carbide_instrument::testing::MetricsCapture::start();
    crate::machine_validation::MachineValidationManager::new(
        env.pool.clone(),
        config,
        opentelemetry::global::meter("test_machine_validation_manager_reconciles_stale_run"),
    )
    .run_single_iteration()
    .await?;
    assert_eq!(
        metrics.counter_delta(
            "carbide_machine_validation_outcomes_total",
            &[
                ("outcome", "failed"),
                ("cause", "stale_machine_validation_run"),
            ],
        ),
        1.0
    );
    drop(metrics);

    let late_result_name = "late-stale-result".to_string();
    env.api
        .persist_validation_result(tonic::Request::new(
            rpc::forge::MachineValidationResultPostRequest {
                result: Some(rpc::forge::MachineValidationResult {
                    validation_id: Some(validation_id),
                    name: late_result_name.clone(),
                    description: "desc".to_string(),
                    command: "echo".to_string(),
                    args: "test".to_string(),
                    std_out: "".to_string(),
                    std_err: "".to_string(),
                    context: "OnDemand".to_string(),
                    exit_code: 0,
                    start_time: Some(Timestamp::from(SystemTime::now())),
                    end_time: Some(Timestamp::from(SystemTime::now())),
                    test_id: Some(late_result_name.clone()),
                }),
            },
        ))
        .await?;
    let results =
        db::machine_validation_result::find_by_validation_id(&env.pool, &validation_id).await?;
    assert!(!results.iter().any(|result| result.name == late_result_name));

    env.api
        .machine_validation_completed(tonic::Request::new(
            rpc::forge::MachineValidationCompletedRequest {
                machine_id: Some(mh.host().id.into()),
                machine_validation_error: None,
                validation_id: Some(validation_id),
            },
        ))
        .await?;

    let update_completed_run = env
        .api
        .update_machine_validation_run(tonic::Request::new(
            rpc::forge::MachineValidationRunRequest {
                validation_id: Some(validation_id),
                duration_to_complete: Some(rpc::Duration::from(std::time::Duration::from_secs(1))),
                total: 1,
                selected_tests: Vec::new(),
            },
        ))
        .await;
    assert!(update_completed_run.is_err());

    let runs = get_machine_validation_runs(&env, &mh.host().id, true).await;
    let failed_state_int = rpc::forge::machine_validation_status::MachineValidationState::Completed(
        rpc::forge::machine_validation_status::MachineValidationCompleted::Failed.into(),
    );
    let stale_run = runs
        .runs
        .into_iter()
        .find(|run| run.validation_id == Some(validation_id))
        .expect("stale run should be listed");
    assert_eq!(
        stale_run
            .status
            .unwrap_or_default()
            .machine_validation_state
            .unwrap_or(failed_state_int),
        failed_state_int
    );

    env.run_machine_state_controller_iteration_until_state_condition(
        &mh.host().id,
        1,
        |machine| {
            matches!(
                machine.current_state(),
                ManagedHostState::Failed {
                    retry_count: 0,
                    details: FailureDetails {
                        cause: FailureCause::MachineValidation { err },
                        source: FailureSource::Scout,
                        ..
                    },
                    ..
                } if err == &format!(
                    "Machine validation run {validation_id} exceeded its expected duration plus stale timeout"
                )
            )
        },
    )
    .await;

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_tests_on_startup_default_mode(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Get initial state of tests
    let initial_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // Create config with Default mode
    let config = MachineValidationConfig {
        enabled: true,
        test_selection_mode: MachineValidationTestSelectionMode::Default,
        run_interval: std::time::Duration::from_secs(60),
        stale_run_timeout: std::time::Duration::from_secs(24 * 60 * 60),
        tests: vec![
            MachineValidationTestConfig {
                id: initial_tests[0].test_id.clone(),
                enable: false,
            },
            MachineValidationTestConfig {
                id: initial_tests[1].test_id.clone(),
                enable: true,
            },
        ],
        approved_plugin_registries: vec![],
        allow_privileged_plugins: false,
        allow_full_host_plugins: false,
    };

    // Apply config
    apply_config_on_startup(&env.api, &config).await?;

    // Verify results
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // First test should be disabled
    assert!(
        !updated_tests
            .iter()
            .find(|t| t.test_id == initial_tests[0].test_id)
            .unwrap()
            .is_enabled
    );

    // Second test should be enabled
    assert!(
        updated_tests
            .iter()
            .find(|t| t.test_id == initial_tests[1].test_id)
            .unwrap()
            .is_enabled
    );

    // Other tests should remain unchanged
    for test in updated_tests.iter().skip(2) {
        assert_eq!(
            test.is_enabled,
            initial_tests
                .iter()
                .find(|t| t.test_id == test.test_id)
                .unwrap()
                .is_enabled,
            "Test {} state should not change",
            test.test_id
        );
    }

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_tests_enable_all_mode(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Get initial state of tests
    let initial_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // Create config with EnableAll mode and one override
    let config = MachineValidationConfig {
        enabled: true,
        test_selection_mode: MachineValidationTestSelectionMode::EnableAll,
        run_interval: std::time::Duration::from_secs(60),
        stale_run_timeout: std::time::Duration::from_secs(24 * 60 * 60),
        tests: vec![MachineValidationTestConfig {
            id: initial_tests[0].test_id.clone(),
            enable: false, // Override first test to be disabled
        }],
        approved_plugin_registries: vec![],
        allow_privileged_plugins: false,
        allow_full_host_plugins: false,
    };

    // Apply config
    apply_config_on_startup(&env.api, &config).await?;

    // Verify results
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // First test should be disabled (due to override)
    assert!(
        !updated_tests
            .iter()
            .find(|t| t.test_id == initial_tests[0].test_id)
            .unwrap()
            .is_enabled
    );

    // All other tests should be enabled
    for test in updated_tests.iter().skip(1) {
        assert!(
            test.is_enabled,
            "Test {} should be enabled in EnableAll mode",
            test.test_id
        );
    }

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_tests_on_startup_disable_all_mode(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Get initial state of tests
    let initial_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // Create config with DisableAll mode and one override
    let config = MachineValidationConfig {
        enabled: true,
        test_selection_mode: MachineValidationTestSelectionMode::DisableAll,
        run_interval: std::time::Duration::from_secs(60),
        stale_run_timeout: std::time::Duration::from_secs(24 * 60 * 60),
        tests: vec![MachineValidationTestConfig {
            id: initial_tests[0].test_id.clone(),
            enable: true, // Override first test to be enabled
        }],
        approved_plugin_registries: vec![],
        allow_privileged_plugins: false,
        allow_full_host_plugins: false,
    };

    // Apply config
    apply_config_on_startup(&env.api, &config).await?;

    // Verify results
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // First test should be enabled (due to override)
    assert!(
        updated_tests
            .iter()
            .find(|t| t.test_id == initial_tests[0].test_id)
            .unwrap()
            .is_enabled
    );

    // All other tests should be disabled
    for test in updated_tests.iter().skip(1) {
        assert!(
            !test.is_enabled,
            "Test {} should be disabled in DisableAll mode",
            test.test_id
        );
    }

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_tests_on_startup_missing_test_selection_mode(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Get initial state of tests
    let initial_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // Create minimal config without test_selection_mode (should default to Default mode)
    let config = MachineValidationConfig {
        enabled: true,
        run_interval: std::time::Duration::from_secs(60),
        tests: vec![MachineValidationTestConfig {
            id: initial_tests[0].test_id.clone(),
            enable: false,
        }],
        ..Default::default() // This will use the default test_selection_mode
    };

    // Apply config
    apply_config_on_startup(&env.api, &config).await?;

    // Verify results
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // First test should be disabled as specified in config
    assert!(
        !updated_tests
            .iter()
            .find(|t| t.test_id == initial_tests[0].test_id)
            .unwrap()
            .is_enabled
    );

    // Other tests should remain unchanged (Default mode behavior)
    for test in updated_tests.iter().skip(1) {
        assert_eq!(
            test.is_enabled,
            initial_tests
                .iter()
                .find(|t| t.test_id == test.test_id)
                .unwrap()
                .is_enabled,
            "Test {} state should not change when test_selection_mode is missing",
            test.test_id
        );
    }

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_tests_on_startup_missing_tests_config(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Create config without any test configurations
    let config = MachineValidationConfig {
        enabled: true,
        test_selection_mode: MachineValidationTestSelectionMode::EnableAll,
        run_interval: std::time::Duration::from_secs(60),
        stale_run_timeout: std::time::Duration::from_secs(24 * 60 * 60),
        tests: vec![], // Empty test configuration
        approved_plugin_registries: vec![],
        allow_privileged_plugins: false,
        allow_full_host_plugins: false,
    };

    // Apply config
    apply_config_on_startup(&env.api, &config).await?;

    // Verify results
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // All tests should be enabled (EnableAll mode with no overrides)
    for test in &updated_tests {
        assert!(
            test.is_enabled,
            "Test {} should be enabled when no test configs are provided in EnableAll mode",
            test.test_id
        );
    }

    // Test with DisableAll mode
    let config = MachineValidationConfig {
        enabled: true,
        test_selection_mode: MachineValidationTestSelectionMode::DisableAll,
        run_interval: std::time::Duration::from_secs(60),
        stale_run_timeout: std::time::Duration::from_secs(24 * 60 * 60),
        tests: vec![], // Empty test configuration
        approved_plugin_registries: vec![],
        allow_privileged_plugins: false,
        allow_full_host_plugins: false,
    };

    // Apply config
    apply_config_on_startup(&env.api, &config).await?;

    // Verify results
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // All tests should be disabled (DisableAll mode with no overrides)
    for test in &updated_tests {
        assert!(
            !test.is_enabled,
            "Test {} should be disabled when no test configs are provided in DisableAll mode",
            test.test_id
        );
    }

    Ok(())
}

#[crate::sqlx_test(fixtures("create_machine_validation_tests",))]
async fn test_machine_validation_tests_on_startup_missing_both_fields(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Get initial state of tests
    let initial_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // Create minimal config without both test_selection_mode and tests
    let config = MachineValidationConfig {
        enabled: true,
        run_interval: std::time::Duration::from_secs(60),
        ..Default::default() // This will use defaults for both test_selection_mode and tests
    };

    // Apply config
    apply_config_on_startup(&env.api, &config).await?;

    // Verify results
    let updated_tests = env
        .api
        .get_machine_validation_tests(tonic::Request::new(
            rpc::forge::MachineValidationTestsGetRequest::default(),
        ))
        .await?
        .into_inner()
        .tests;

    // All tests should remain unchanged (Default mode with no test configs)
    for test in &updated_tests {
        assert_eq!(
            test.is_enabled,
            initial_tests
                .iter()
                .find(|t| t.test_id == test.test_id)
                .unwrap()
                .is_enabled,
            "Test {} state should not change when both fields are missing",
            test.test_id
        );
    }

    Ok(())
}
