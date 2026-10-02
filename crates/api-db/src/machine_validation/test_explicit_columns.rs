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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use chrono::{DateTime, Duration, Utc};
use config_version::ConfigVersion;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::machine_validation::{
    MachineValidationAttemptState, MachineValidationPlugin, MachineValidationResult,
    MachineValidationRunItemState, MachineValidationTest, MachineValidationTestAddRequest,
    MachineValidationTestsGetRequest,
};
use sqlx::{Connection, PgPool};

use super::*;
use crate::{
    machine_validation_config, machine_validation_execution, machine_validation_result,
    machine_validation_suites,
};

#[crate::sqlx_test]
async fn validation_run_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_run_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // The migration uses another connection while the API keeps its cached statements.
    let mut migration_connection = pool.acquire().await?;
    let mut migration = migration_connection.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE machine_validation ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_run_queries(&mut api_connection).await?;
    Ok(())
}

async fn create_host(connection: &mut PgConnection) -> DatabaseResult<MachineId> {
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x67; 32],
        MachineType::Host,
    );
    crate::machine::create(
        connection,
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    Ok(machine_id)
}

async fn exercise_run_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let machine_id = create_host(txn.as_mut()).await?;
    let filter = MachineValidationFilter {
        tags: vec!["diagnostic".to_string()],
        allowed_tests: vec!["forge_projection".to_string()],
        run_unverfied_tests: Some(true),
        contexts: Some(vec!["OnDemand".to_string()]),
    };
    let mut expected = create_new_run(
        txn.as_mut(),
        &machine_id,
        MachineValidationContext::OnDemand,
        filter.clone(),
    )
    .await?;
    assert_eq!(expected.machine_id, machine_id);
    assert_eq!(expected.name, format!("Test_{machine_id}"));
    assert_eq!(expected.filter.as_ref(), Some(&filter));
    assert_eq!(expected.context.as_deref(), Some("OnDemand"));
    assert!(expected.start_time.is_some());
    assert_eq!(expected.end_time, None);
    assert_eq!(
        expected.status,
        Some(MachineValidationStatus {
            state: MachineValidationState::Started,
            total: 0,
            completed: 0,
        })
    );

    expected.status.as_mut().unwrap().state = MachineValidationState::InProgress;
    let in_progress = mark_in_progress_if_active(txn.as_mut(), &expected.id)
        .await?
        .expect("the new run is active");
    assert_run(&in_progress, &expected);

    update_run(txn.as_mut(), &expected.id, 3, 90).await?;
    expected.duration_to_complete = 90;
    expected.status.as_mut().unwrap().total = 3;
    let heartbeat: DateTime<Utc> = "2026-09-01T12:00:00Z".parse()?;
    assert_eq!(
        machine_validation_execution::record_heartbeat(
            txn.as_mut(),
            &expected.id,
            None,
            None,
            None,
            heartbeat,
        )
        .await?,
        ConditionalWrite::Applied(())
    );
    expected.last_heartbeat_at = Some(heartbeat);

    let active = find_active(txn.as_mut()).await?;
    assert_eq!(active.len(), 1);
    assert_run(&active[0], &expected);
    let found = find_by_id(txn.as_mut(), &expected.id).await?;
    assert_run(&found, &expected);
    let locked = lock_by_id_no_key_update(txn.as_mut(), &expected.id)
        .await?
        .expect("the run exists");
    assert_run(&locked, &expected);

    let failed = MachineValidationStatus {
        state: MachineValidationState::Failed,
        total: 3,
        completed: 0,
    };
    update_end_time(txn.as_mut(), &expected.id, &failed).await?;
    let ended = find_by_id(txn.as_mut(), &expected.id).await?;
    assert!(ended.end_time.is_some());
    expected.end_time = ended.end_time;
    expected.status = Some(failed.clone());
    assert_run(&ended, &expected);

    let mut completed = create_new_run(
        txn.as_mut(),
        &machine_id,
        MachineValidationContext::Cleanup,
        filter.clone(),
    )
    .await?;
    let ConditionalWrite::Applied(updated) =
        update_end_time_if_active(txn.as_mut(), &completed.id, &failed).await?
    else {
        panic!("the new run should complete");
    };
    assert!(updated.end_time.is_some());
    completed.end_time = updated.end_time;
    completed.status.as_mut().unwrap().state = MachineValidationState::Failed;
    assert_run(&updated, &completed);

    let mut stale = create_new_run(
        txn.as_mut(),
        &machine_id,
        MachineValidationContext::Discovery,
        filter,
    )
    .await?;
    assert_eq!(
        machine_validation_execution::record_heartbeat(
            txn.as_mut(),
            &stale.id,
            None,
            None,
            None,
            heartbeat,
        )
        .await?,
        ConditionalWrite::Applied(())
    );
    let timed_out = mark_stale_if_active(
        txn.as_mut(),
        &stale.id,
        std::time::Duration::from_secs(60),
        heartbeat + Duration::seconds(61),
        &failed,
    )
    .await?
    .expect("the recorded heartbeat is stale");
    assert!(timed_out.end_time.is_some());
    stale.end_time = timed_out.end_time;
    stale.last_heartbeat_at = Some(heartbeat);
    stale.status.as_mut().unwrap().state = MachineValidationState::Failed;
    assert_run(&timed_out, &stale);

    // Roll back the fixtures without discarding the connection's prepared statements.
    txn.rollback().await?;
    Ok(())
}

fn assert_run(actual: &MachineValidation, expected: &MachineValidation) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.machine_id, expected.machine_id);
    assert_eq!(actual.name, expected.name);
    assert_eq!(actual.start_time, expected.start_time);
    assert_eq!(actual.end_time, expected.end_time);
    assert_eq!(actual.filter, expected.filter);
    assert_eq!(actual.context, expected.context);
    assert_eq!(actual.status, expected.status);
    assert_eq!(actual.duration_to_complete, expected.duration_to_complete);
    assert_eq!(actual.last_heartbeat_at, expected.last_heartbeat_at);
}

#[crate::sqlx_test]
async fn external_config_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_config_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    let mut migration_connection = pool.acquire().await?;
    let mut migration = migration_connection.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE machine_validation_external_config ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_config_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_config_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let name = "projection-config";
    let description = "validation parameters";
    let config = b"{\"iterations\":7}".to_vec();
    machine_validation_config::save(txn.as_mut(), name, description, &config).await?;
    let expected = machine_validation_config::find_config_by_name(txn.as_mut(), name).await?;
    assert_eq!(expected.name, name);
    assert_eq!(expected.description, description);
    assert_eq!(expected.config, config);
    assert_eq!(expected.version.version_nr(), 1);
    let configs = machine_validation_config::find_configs(txn.as_mut()).await?;
    assert_eq!(configs.len(), 1);
    assert_eq!(
        serde_json::to_value(&configs[0])?,
        serde_json::to_value(&expected)?
    );
    let removed = machine_validation_config::remove_config(txn.as_mut(), name).await?;
    assert_eq!(
        serde_json::to_value(&removed)?,
        serde_json::to_value(&expected)?
    );
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn validation_execution_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_execution_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    let mut migration_connection = pool.acquire().await?;
    let mut migration = migration_connection.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE machine_validation_tests ADD COLUMN test_added_column text;
         ALTER TABLE machine_validation_run_items ADD COLUMN test_added_column text;
         ALTER TABLE machine_validation_attempts ADD COLUMN test_added_column text;
         ALTER TABLE machine_validation_results ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_execution_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_execution_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let machine_id = create_host(txn.as_mut()).await?;
    let run = create_new_run(
        txn.as_mut(),
        &machine_id,
        MachineValidationContext::OnDemand,
        MachineValidationFilter::default(),
    )
    .await?;
    let request = MachineValidationTestAddRequest {
        name: "Projection".to_string(),
        description: Some("accelerator diagnostic".to_string()),
        contexts: vec!["OnDemand".to_string()],
        img_name: Some("validation-image".to_string()),
        execute_in_host: Some(true),
        container_arg: Some("--network=none".to_string()),
        command: "diagnose".to_string(),
        args: "--iterations=7".to_string(),
        extra_err_file: Some("diagnostic.err".to_string()),
        external_config_file: Some("diagnostic.json".to_string()),
        pre_condition: Some("test -e /dev/nvidia0".to_string()),
        timeout: Some(123),
        extra_output_file: Some("diagnostic.out".to_string()),
        supported_platforms: vec!["test_platform".to_string()],
        read_only: Some(true),
        custom_tags: vec!["projection".to_string()],
        components: vec!["Accelerator".to_string()],
        is_enabled: Some(false),
        plugin: Some(MachineValidationPlugin {
            plugin_type: MachineValidationPlugin::CONTAINER_TYPE.to_string(),
            image: format!("example.com/diagnostic@sha256:{}", "a".repeat(64)),
            entrypoint: vec!["/diagnose".to_string()],
            parameters_json: "{\"iterations\":7}".to_string(),
            privileged: true,
            host_access_full: true,
        }),
    };
    let version = ConfigVersion::initial();
    let test_id = machine_validation_suites::save(txn.as_mut(), request.clone(), version).await?;
    machine_validation_suites::mark_verified(txn.as_mut(), test_id.clone(), version).await?;
    machine_validation_suites::approve_full_host(txn.as_mut(), test_id.clone(), version).await?;
    machine_validation_suites::enable_disable(
        txn.as_mut(),
        test_id.clone(),
        version,
        true,
        true,
        true,
    )
    .await?;
    let selected = machine_validation_suites::find(
        txn.as_mut(),
        MachineValidationTestsGetRequest {
            test_id: Some(test_id.clone()),
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(selected.len(), 1);
    let expected_test = MachineValidationTest {
        test_id: test_id.clone(),
        name: request.name.clone(),
        description: request.description.clone(),
        contexts: request.contexts.clone(),
        img_name: request.img_name.clone(),
        execute_in_host: request.execute_in_host,
        container_arg: request.container_arg.clone(),
        command: request.command.clone(),
        args: request.args.clone(),
        extra_output_file: request.extra_output_file.clone(),
        extra_err_file: request.extra_err_file.clone(),
        external_config_file: request.external_config_file.clone(),
        pre_condition: request.pre_condition.clone(),
        timeout: request.timeout,
        version,
        supported_platforms: request.supported_platforms.clone(),
        modified_by: "User".to_string(),
        verified: true,
        read_only: true,
        custom_tags: Some(request.custom_tags.clone()),
        components: request.components.clone(),
        last_modified_at: selected[0].last_modified_at,
        is_enabled: true,
        plugin: request.plugin.clone(),
        full_host_approved: true,
    };
    assert_eq!(
        serde_json::to_value(&selected[0])?,
        serde_json::to_value(&expected_test)?
    );
    machine_validation_execution::materialize_run_plan(
        txn.as_mut(),
        &run.id,
        "OnDemand",
        &selected,
    )
    .await?;
    let pending =
        machine_validation_execution::find_run_items_by_run_id(txn.as_mut(), &run.id).await?;
    assert_eq!(pending.len(), 1);
    let run_item_id = pending[0].id;
    let attempt_id = pending[0]
        .current_attempt_id
        .expect("the run plan created an attempt");

    let start_time: DateTime<Utc> = "2026-09-01T12:00:00Z".parse()?;
    let result = MachineValidationResult {
        validation_id: run.id,
        name: request.name.clone(),
        description: "diagnostic failed".to_string(),
        stdout: "checked seven devices".to_string(),
        stderr: "device seven failed".to_string(),
        command: request.command.clone(),
        args: request.args.clone(),
        context: "OnDemand".to_string(),
        exit_code: 17,
        start_time,
        end_time: start_time + Duration::seconds(3),
        test_id: Some(test_id.clone()),
    };
    assert!(machine_validation_execution::record_result(txn.as_mut(), &result).await?);
    machine_validation_result::create(result.clone(), txn.as_mut()).await?;

    for (operation, items) in [
        (
            "by run",
            machine_validation_execution::find_run_items_by_run_id(txn.as_mut(), &run.id).await?,
        ),
        (
            "by IDs",
            machine_validation_execution::find_run_items_by_ids(txn.as_mut(), &[run_item_id])
                .await?,
        ),
    ] {
        assert_eq!(items.len(), 1, "{operation}");
        let item = &items[0];
        assert_eq!(item.id, run_item_id, "{operation}");
        assert_eq!(item.run_id, run.id, "{operation}");
        assert_eq!(item.current_attempt_id, Some(attempt_id), "{operation}");
        assert_eq!(item.test_id, test_id, "{operation}");
        assert_eq!(
            item.test_version,
            Some(version.version_string()),
            "{operation}"
        );
        assert_eq!(item.display_name, request.name, "{operation}");
        assert_eq!(item.context, result.context, "{operation}");
        assert_eq!(
            item.component.as_deref(),
            Some("Accelerator"),
            "{operation}"
        );
        assert_eq!(
            item.state,
            MachineValidationRunItemState::Failed,
            "{operation}"
        );
        assert_eq!(
            (item.order_index, item.attempt, item.max_attempts),
            (0, 1, 1),
            "{operation}"
        );
        assert_eq!(item.timeout_seconds, 123, "{operation}");
        assert_eq!(
            serde_json::to_value(&item.plugin)?,
            serde_json::to_value(&request.plugin)?,
            "{operation}"
        );
        assert!(item.plugin_full_host_approved, "{operation}");
        assert_eq!(item.started_at, Some(result.start_time), "{operation}");
        assert_eq!(item.ended_at, Some(result.end_time), "{operation}");
        assert_eq!(item.last_heartbeat_at, Some(result.end_time), "{operation}");
        assert_eq!(item.skip_reason, None, "{operation}");
        assert_eq!(
            item.failure_reason.as_deref(),
            Some(result.stderr.as_str()),
            "{operation}"
        );
    }

    let attempts =
        machine_validation_execution::find_attempts_by_run_item_id(txn.as_mut(), &run_item_id)
            .await?;
    assert_eq!(attempts.len(), 1);
    let attempt =
        machine_validation_execution::find_attempt_by_id(txn.as_mut(), &attempt_id).await?;
    for (operation, attempt) in [("by run item", &attempts[0]), ("by ID", &attempt)] {
        assert_eq!(attempt.id, attempt_id, "{operation}");
        assert_eq!(attempt.run_item_id, run_item_id, "{operation}");
        assert_eq!(attempt.attempt_number, 1, "{operation}");
        assert_eq!(
            attempt.state,
            MachineValidationAttemptState::Failed,
            "{operation}"
        );
        assert_eq!(
            attempt.command.as_deref(),
            Some(result.command.as_str()),
            "{operation}"
        );
        assert_eq!(
            attempt.args.as_deref(),
            Some(result.args.as_str()),
            "{operation}"
        );
        assert_eq!(attempt.container_image, request.img_name, "{operation}");
        assert_eq!(attempt.execute_in_host, Some(true), "{operation}");
        assert_eq!(attempt.exit_code, Some(17), "{operation}");
        assert_eq!(
            attempt.failure_classification.as_deref(),
            Some("CommandFailed"),
            "{operation}"
        );
        assert_eq!(attempt.started_at, Some(result.start_time), "{operation}");
        assert_eq!(attempt.ended_at, Some(result.end_time), "{operation}");
        assert_eq!(
            attempt.last_heartbeat_at,
            Some(result.end_time),
            "{operation}"
        );
        assert_eq!(
            attempt.stdout_summary.as_deref(),
            Some(result.stdout.as_str()),
            "{operation}"
        );
        assert_eq!(
            attempt.stderr_summary.as_deref(),
            Some(result.stderr.as_str()),
            "{operation}"
        );
    }
    let results = machine_validation_result::find_by_validation_id(txn.as_mut(), &run.id).await?;
    assert_eq!(results.len(), 1);
    let stored = &results[0];
    assert_eq!(stored.validation_id, result.validation_id);
    assert_eq!(stored.name, result.name);
    assert_eq!(stored.description, result.description);
    assert_eq!(stored.command, result.command);
    assert_eq!(stored.args, result.args);
    assert_eq!(stored.context, result.context);
    assert_eq!(stored.stdout, result.stdout);
    assert_eq!(stored.stderr, result.stderr);
    assert_eq!(stored.exit_code, result.exit_code);
    assert_eq!(stored.start_time, result.start_time);
    assert_eq!(stored.end_time, result.end_time);
    assert_eq!(stored.test_id, result.test_id);

    txn.rollback().await?;
    Ok(())
}
