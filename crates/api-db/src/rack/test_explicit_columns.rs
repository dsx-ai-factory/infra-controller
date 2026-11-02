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

use carbide_uuid::rack::RackGroupId;
use model::expected_rack::ExpectedRack;
use model::rack::{FirmwareProgressState, MaintenanceScope, RackMaintenanceState};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn rack_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_rack_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE racks ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_rack_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_rack_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let inventory = ExpectedRack {
        rack_id: RackId::new("projection-rack"),
        rack_profile_id: RackProfileId::new("NVL72"),
        rack_group_id: Some(RackGroupId::new("projection-group")),
        metadata: Metadata {
            name: "projection rack".to_string(),
            description: "populated rack projection".to_string(),
            labels: [("location".to_string(), "row-1".to_string())].into(),
        },
    };
    let mut expected = create_from_expected(&mut txn, &inventory, &RackConfig::default()).await?;
    assert_eq!(expected.id, inventory.rack_id);
    assert_eq!(expected.rack_profile_id, Some(inventory.rack_profile_id));
    assert_eq!(expected.rack_group_id, inventory.rack_group_id);
    assert_eq!(expected.metadata, inventory.metadata);
    assert_eq!(expected.controller_state.value, RackState::Created);
    assert_eq!(expected.controller_state.version, expected.version);
    assert_eq!(expected.version.version_nr(), 1);

    let timestamp = "2026-09-01T12:00:00Z".parse()?;
    expected.firmware_upgrade_job = Some(FirmwareUpgradeJob {
        job_id: Some("firmware-job".to_string()),
        firmware_id: Some("firmware-image".to_string()),
        status: Some(FirmwareProgressState::InProgress),
        started_at: Some(timestamp),
        ..Default::default()
    });
    expected.nvos_update_job = Some(NvosUpdateJob {
        job_id: Some("nvos-job".to_string()),
        firmware_id: "nvos-image".to_string(),
        image_filename: "nvos.img".to_string(),
        local_file_path: "/tmp/nvos.img".to_string(),
        version: Some("1.0".to_string()),
        status: Some("in_progress".to_string()),
        started_at: Some(timestamp),
        ..Default::default()
    });
    let outcome = PersistentStateHandlerOutcome::Wait {
        reason: "waiting for firmware".to_string(),
        source_ref: None,
    };
    update_controller_state_outcome(&mut txn, &expected.id, outcome.clone()).await?;
    expected.controller_state_outcome = Some(outcome);
    update_firmware_upgrade_job(
        &mut txn,
        &expected.id,
        expected.firmware_upgrade_job.as_ref(),
    )
    .await?;
    update_nvos_update_job(&mut txn, &expected.id, expected.nvos_update_job.as_ref()).await?;
    let report = HealthReport::empty("projection-health".to_string());
    insert_health_report(
        &mut txn,
        &expected.id,
        HealthReportApplyMode::Replace,
        &report,
    )
    .await?;
    expected.health_reports.replace = Some(report);

    let new_version = expected.controller_state.version.increment();
    let maintenance = RackState::Maintenance {
        maintenance_state: RackMaintenanceState::Completed,
    };
    assert!(matches!(
        try_update_controller_state(
            &mut txn,
            &expected.id,
            expected.controller_state.version,
            new_version,
            &maintenance,
        )
        .await?,
        ConditionalWrite::Applied(())
    ));
    expected.controller_state.value = maintenance;
    expected.controller_state.version = new_version;
    let found = find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &expected.id),
    )
    .await?;
    assert_eq!(found.len(), 1);
    assert_rack(&found[0], &expected)?;

    expected.config = RackConfig {
        reprovision_requested: true,
        topology_changed: true,
        maintenance_requested: Some(MaintenanceScope {
            requested_at: Some(timestamp),
            ..Default::default()
        }),
        maintenance_termination_requested: true,
    };
    assert_rack(
        &update(&mut txn, &expected.id, &expected.config).await?,
        &expected,
    )?;

    expected.config.maintenance_requested = None;
    expected.config.maintenance_termination_requested = false;
    assert_rack(
        &consume_maintenance_termination_request(&mut txn, &expected.id).await?,
        &expected,
    )?;

    expected.deleted = Some(expected.updated);
    assert_rack(&mark_as_deleted(&expected.id, &mut txn).await?, &expected)?;
    txn.rollback().await?;
    Ok(())
}

fn assert_rack(actual: &Rack, expected: &Rack) -> Result<(), serde_json::Error> {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.rack_profile_id, expected.rack_profile_id);
    assert_eq!(actual.rack_group_id, expected.rack_group_id);
    assert_eq!(
        serde_json::to_value(&actual.config)?,
        serde_json::to_value(&expected.config)?
    );
    assert_eq!(
        actual.controller_state.value,
        expected.controller_state.value
    );
    assert_eq!(
        actual.controller_state.version,
        expected.controller_state.version
    );
    assert_eq!(
        actual.controller_state_outcome,
        expected.controller_state_outcome
    );
    assert_eq!(
        serde_json::to_value(&actual.firmware_upgrade_job)?,
        serde_json::to_value(&expected.firmware_upgrade_job)?
    );
    assert_eq!(
        serde_json::to_value(&actual.nvos_update_job)?,
        serde_json::to_value(&expected.nvos_update_job)?
    );
    assert_eq!(actual.health_reports, expected.health_reports);
    assert_eq!(actual.metadata, expected.metadata);
    assert_eq!(actual.version, expected.version);
    assert_eq!(actual.created, expected.created);
    assert_eq!(actual.updated, expected.updated);
    assert_eq!(actual.deleted, expected.deleted);
    Ok(())
}
