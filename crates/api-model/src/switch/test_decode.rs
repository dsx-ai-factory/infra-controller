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

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
use sqlx::FromRow;

use super::Switch;

#[crate::sqlx_test]
async fn switch_preserves_decode_errors_and_nullable_fields(pool: sqlx::PgPool) {
    struct DecodeCase {
        column: &'static str,
        projection: &'static str,
        check: fn(sqlx::Error),
    }

    // `db::switch::find_by` uses `SWITCHES_WITH_BMC_INFO`. Synthetic rows
    // violate its projection or native column constraints to test missing
    // columns, invalid NULLs, type mismatches, and malformed BMC JSON.
    let cases = [
        DecodeCase {
            column: "status",
            projection: "NULL::jsonb AS another_column",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnNotFound(column) if column == "status"),
                    "{error:?}"
                );
            },
        },
        DecodeCase {
            column: "status",
            projection: "1::integer AS status",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"status\""),
                    "{error:?}"
                );
            },
        },
        DecodeCase {
            column: "bmc_info",
            projection: "'{\"ip\":\"not-an-ip\"}'::jsonb AS bmc_info",
            check: |error| {
                let sqlx::Error::ColumnDecode { index, source } = error else {
                    panic!("expected BMC ColumnDecode, got {error:?}");
                };
                assert_eq!(index, "\"bmc_info\"");
                assert!(
                    source
                        .downcast_ref::<serde_json::Error>()
                        .is_some_and(serde_json::Error::is_data),
                    "{source:?}"
                );
            },
        },
        DecodeCase {
            column: "bmc_mac_address",
            projection: "1::integer AS bmc_mac_address",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"bmc_mac_address\""),
                    "{error:?}"
                );
            },
        },
        DecodeCase {
            column: "bmc_credential_rotation_requested",
            projection: "NULL::boolean AS bmc_credential_rotation_requested",
            check: |error| {
                let sqlx::Error::ColumnDecode { index, source } = error else {
                    panic!("expected rotation request ColumnDecode, got {error:?}");
                };
                assert_eq!(index, "\"bmc_credential_rotation_requested\"");
                assert!(
                    source.is::<sqlx::error::UnexpectedNullError>(),
                    "{source:?}"
                );
            },
        },
        DecodeCase {
            column: "decommission_requested",
            projection: "NULL::boolean AS decommission_requested",
            check: |error| {
                let sqlx::Error::ColumnDecode { index, source } = error else {
                    panic!("expected decommission request ColumnDecode, got {error:?}");
                };
                assert_eq!(index, "\"decommission_requested\"");
                assert!(
                    source.is::<sqlx::error::UnexpectedNullError>(),
                    "{source:?}"
                );
            },
        },
        DecodeCase {
            column: "nvlink_domain_uuid",
            projection: "NULL::uuid AS another_column",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnNotFound(column) if column == "nvlink_domain_uuid"),
                    "{error:?}"
                );
            },
        },
        DecodeCase {
            column: "is_primary",
            projection: "NULL::boolean AS is_primary",
            check: |error| {
                let sqlx::Error::ColumnDecode { index, source } = error else {
                    panic!("expected primary flag ColumnDecode, got {error:?}");
                };
                assert_eq!(index, "\"is_primary\"");
                assert!(
                    source.is::<sqlx::error::UnexpectedNullError>(),
                    "{source:?}"
                );
            },
        },
        DecodeCase {
            column: "rack_id",
            projection: "1::integer AS rack_id",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"rack_id\""),
                    "{error:?}"
                );
            },
        },
        DecodeCase {
            column: "slot_number",
            projection: "'invalid'::text AS slot_number",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"slot_number\""),
                    "{error:?}"
                );
            },
        },
        DecodeCase {
            column: "tray_index",
            projection: "'invalid'::text AS tray_index",
            check: |error| {
                assert!(
                    matches!(&error, sqlx::Error::ColumnDecode { index, .. } if index == "\"tray_index\""),
                    "{error:?}"
                );
            },
        },
    ];
    let columns = [
        ("status", "NULL::jsonb"),
        ("bmc_info", "NULL::jsonb"),
        ("bmc_mac_address", "NULL::macaddr"),
        ("bmc_credential_rotation_requested", "false"),
        ("decommission_requested", "false"),
        ("nvlink_domain_uuid", "NULL::uuid"),
        ("is_primary", "false"),
        ("rack_id", "NULL::text"),
        ("slot_number", "NULL::integer"),
        ("tray_index", "NULL::integer"),
    ];
    let switch_id = SwitchId::new(
        SwitchIdSource::ProductBoardChassisSerial,
        [0x43; 32],
        SwitchType::NvLink,
    );

    // Decode the unchanged projection first, then replace one column per case.
    for case in std::iter::once(None).chain(cases.iter().map(Some)) {
        let selected_columns = columns
            .iter()
            .map(|(column, value)| match case {
                Some(case) if *column == case.column => case.projection.to_string(),
                _ => format!("{value} AS {column}"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT $1::text AS id,
                '{{\"state\":\"ready\"}}'::jsonb AS controller_state,
                'V1-T0'::text AS controller_state_version,
                '{{\"name\":\"projection-switch\",\"enable_nmxc\":false}}'::jsonb AS config,
                NULL::timestamptz AS deleted,
                NULL::jsonb AS controller_state_outcome,
                NULL::jsonb AS switch_maintenance_requested,
                NULL::jsonb AS switch_reprovisioning_requested,
                NULL::jsonb AS firmware_upgrade_status, NULL::jsonb AS nvos_update_status,
                NULL::jsonb AS fabric_manager_status,
                NULL::jsonb AS health_reports,
                '{{}}'::jsonb AS labels, ''::text AS name, ''::text AS description,
                'V1-T0'::text AS version, {selected_columns}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(switch_id)
            .fetch_one(&pool)
            .await
            .expect("fetch switch projection");
        let decoded = Switch::from_row(&row);
        match case {
            Some(case) => (case.check)(decoded.expect_err(case.column)),
            None => {
                let switch = decoded.expect("decode nullable switch fields");
                assert_eq!(switch.bmc_mac_address, None);
                assert_eq!(switch.rack_id, None);
                assert_eq!(switch.nvlink_domain_uuid, None);
                assert_eq!(switch.slot_number, None);
                assert_eq!(switch.tray_index, None);
                assert!(!switch.bmc_credential_rotation_requested);
                assert!(!switch.decommission_requested);
                assert!(!switch.is_primary);
                assert_eq!(switch.status, None);
                assert_eq!(switch.controller_state_outcome, None);
                assert_eq!(switch.switch_maintenance_requested, None);
                assert_eq!(switch.switch_reprovisioning_requested, None);
                assert_eq!(switch.firmware_upgrade_status, None);
                assert_eq!(switch.nvos_update_status, None);
                assert_eq!(switch.fabric_manager_status, None);
                assert_eq!(switch.health_reports, Default::default());
            }
        }
    }
}

#[crate::sqlx_test]
async fn switch_preserves_lifecycle_json_decode_errors(pool: sqlx::PgPool) {
    let columns = [
        "status",
        "controller_state_outcome",
        "switch_maintenance_requested",
        "switch_reprovisioning_requested",
        "firmware_upgrade_status",
        "nvos_update_status",
        "fabric_manager_status",
        "health_reports",
    ];

    // Only `status` supports JSON null. The other optional lifecycle fields
    // and health reports use SQL NULL, so test those decoders independently.
    let cases = [
        ("status=42", "status", "42"),
        (
            "controller_state_outcome=42",
            "controller_state_outcome",
            "42",
        ),
        (
            "switch_maintenance_requested=42",
            "switch_maintenance_requested",
            "42",
        ),
        (
            "switch_reprovisioning_requested=42",
            "switch_reprovisioning_requested",
            "42",
        ),
        (
            "firmware_upgrade_status=42",
            "firmware_upgrade_status",
            "42",
        ),
        ("nvos_update_status=42", "nvos_update_status", "42"),
        ("fabric_manager_status=42", "fabric_manager_status", "42"),
        ("health_reports=42", "health_reports", "42"),
        (
            "controller_state_outcome=null",
            "controller_state_outcome",
            "null",
        ),
        (
            "switch_maintenance_requested=null",
            "switch_maintenance_requested",
            "null",
        ),
        (
            "switch_reprovisioning_requested=null",
            "switch_reprovisioning_requested",
            "null",
        ),
        (
            "firmware_upgrade_status=null",
            "firmware_upgrade_status",
            "null",
        ),
        ("nvos_update_status=null", "nvos_update_status", "null"),
        (
            "fabric_manager_status=null",
            "fabric_manager_status",
            "null",
        ),
        ("health_reports=null", "health_reports", "null"),
    ];
    let pool = &pool;

    check_cases_async(
        cases.into_iter().map(|(scenario, column, json)| Case {
            scenario,
            input: (column, json),
            expect: Yields((format!("\"{column}\""), true)),
        }),
        |(invalid_column, invalid_json)| async move {
            let projection = columns
                .iter()
                .map(|column| {
                    if *column == invalid_column {
                        format!("'{invalid_json}'::jsonb AS {column}")
                    } else {
                        format!("NULL::jsonb AS {column}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT '{{\"state\":\"ready\"}}'::jsonb AS controller_state,
                    '{{\"name\":\"projection-switch\",\"enable_nmxc\":false}}'::jsonb AS config,
                    {projection}"
            );
            let row = sqlx::query(sqlx::AssertSqlSafe(query))
                .fetch_one(pool)
                .await
                .map_err(|error| error.to_string())?;

            // Later columns are unnecessary: the error must name this column,
            // not a missing column read after it.
            match Switch::from_row(&row) {
                Err(sqlx::Error::ColumnDecode { index, source }) => Ok((
                    index,
                    source
                        .downcast_ref::<serde_json::Error>()
                        .is_some_and(serde_json::Error::is_data),
                )),
                other => Err(format!("expected JSON ColumnDecode, got {other:?}")),
            }
        },
    )
    .await;
}
