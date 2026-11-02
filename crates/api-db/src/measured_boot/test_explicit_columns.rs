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

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::measured_boot::{
    MeasurementBundleId, MeasurementBundleValueId, MeasurementSystemProfileAttrId,
    MeasurementSystemProfileId, TrustedMachineId,
};
use chrono::{DateTime, Utc};
use measured_boot::pcr::PcrRegisterValue;
use measured_boot::records::{
    MeasurementApprovedType, MeasurementBundleRecord, MeasurementBundleState,
    MeasurementBundleValueRecord, MeasurementJournalRecord, MeasurementMachineState,
    MeasurementReportRecord, MeasurementReportValueRecord, MeasurementSystemProfileAttrRecord,
    MeasurementSystemProfileRecord,
};
use measured_boot::site::SiteModel;
use model::hardware_info::{DmiData, HardwareInfo};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use serde::Serialize;
use sqlx::{Connection, PgConnection, PgPool};

use super::interface::{bundle, journal, machine, profile, report, site};

fn assert_record(actual: &impl Serialize, expected: &impl Serialize) {
    assert_eq!(
        serde_json::to_value(actual).expect("serialize actual record"),
        serde_json::to_value(expected).expect("serialize expected record"),
    );
}

#[crate::sqlx_test]
async fn profile_and_bundle_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = pool.acquire().await?;
    exercise_profile_and_bundle_queries(&mut connection).await?;
    assert!(connection.cached_statements_size() > 0);

    // Keep the API connection's prepared statements while another
    // connection commits an unrelated schema change.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE measurement_system_profiles ADD COLUMN test_added_column text;
         ALTER TABLE measurement_system_profiles_attrs ADD COLUMN test_added_column text;
         ALTER TABLE measurement_bundles ADD COLUMN test_added_column text;
         ALTER TABLE measurement_bundles_values ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_profile_and_bundle_queries(&mut connection).await?;
    Ok(())
}

async fn exercise_profile_and_bundle_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let created =
        profile::insert_measurement_profile_record(&mut txn, "projection-profile".to_string())
            .await?;
    assert_eq!(created.name, "projection-profile");
    let attributes = profile::insert_measurement_profile_attr_records(
        &mut txn,
        created.profile_id,
        &HashMap::from([("bios_version".to_string(), "1.2.3".to_string())]),
    )
    .await?;
    assert_eq!(attributes.len(), 1);
    assert_eq!(attributes[0].profile_id, created.profile_id);
    assert_eq!(attributes[0].key, "bios_version");
    assert_eq!(attributes[0].value, "1.2.3");

    assert_record(
        &profile::get_measurement_profile_record_by_name(&mut txn, created.name.clone())
            .await?
            .expect("profile by name"),
        &created,
    );
    assert_record(
        &profile::get_all_measurement_profile_records(&mut *txn).await?,
        &[&created],
    );
    assert_record(
        &profile::get_measurement_profile_attrs_for_profile_id(&mut *txn, created.profile_id)
            .await?,
        &attributes,
    );

    let renamed = profile::rename_profile_for_profile_id(
        &mut txn,
        created.profile_id,
        "renamed-profile".to_string(),
    )
    .await?;
    assert_record(
        &renamed,
        &MeasurementSystemProfileRecord {
            name: "renamed-profile".to_string(),
            ..created.clone()
        },
    );
    let final_profile = profile::rename_profile_for_profile_name(
        &mut txn,
        renamed.name.clone(),
        "final-profile".to_string(),
    )
    .await?;
    assert_record(
        &final_profile,
        &MeasurementSystemProfileRecord {
            name: "final-profile".to_string(),
            ..created.clone()
        },
    );
    assert_record(
        &profile::get_measurement_profile_record_by_id(&mut txn, created.profile_id)
            .await?
            .expect("persisted renamed profile"),
        &final_profile,
    );

    let pending = bundle::insert_measurement_bundle_record(
        &mut txn,
        created.profile_id,
        "default-state".to_string(),
        None,
    )
    .await?;
    assert_eq!(pending.profile_id, created.profile_id);
    assert_eq!(pending.name, "default-state");
    assert_eq!(pending.state, MeasurementBundleState::Pending);
    let active = bundle::insert_measurement_bundle_record(
        &mut txn,
        created.profile_id,
        "explicit-state".to_string(),
        Some(MeasurementBundleState::Active),
    )
    .await?;
    assert_eq!(active.profile_id, created.profile_id);
    assert_eq!(active.name, "explicit-state");
    assert_eq!(active.state, MeasurementBundleState::Active);
    for expected in [&pending, &active] {
        assert_record(
            &bundle::get_measurement_bundle_by_id(&mut *txn, expected.bundle_id)
                .await?
                .expect("persisted bundle"),
            expected,
        );
    }
    let value = bundle::insert_measurement_bundle_value_record(
        &mut txn,
        active.bundle_id,
        7,
        &"aabbcc".to_string(),
    )
    .await?;
    assert_eq!(value.bundle_id, active.bundle_id);
    assert_eq!(value.pcr_register, 7);
    assert_eq!(value.sha_any, "aabbcc");
    assert_record(
        &bundle::get_measurement_bundle_values_for_bundle_id(&mut *txn, active.bundle_id).await?,
        &[&value],
    );

    let renamed = bundle::rename_bundle_for_bundle_id(
        &mut txn,
        active.bundle_id,
        "renamed-bundle".to_string(),
    )
    .await?
    .expect("rename bundle by ID");
    assert_record(
        &renamed,
        &MeasurementBundleRecord {
            name: "renamed-bundle".to_string(),
            ..active.clone()
        },
    );
    let renamed_again = bundle::rename_bundle_for_bundle_name(
        &mut txn,
        renamed.name.clone(),
        "final-bundle".to_string(),
    )
    .await?
    .expect("rename bundle by name");
    assert_record(
        &renamed_again,
        &MeasurementBundleRecord {
            name: "final-bundle".to_string(),
            ..active.clone()
        },
    );

    let mut expected = renamed_again;
    for (allow_from_revoked, state) in [
        (true, MeasurementBundleState::Obsolete),
        (false, MeasurementBundleState::Retired),
    ] {
        let updated = bundle::update_state_for_bundle_id(
            &mut txn,
            active.bundle_id,
            state,
            allow_from_revoked,
        )
        .await?
        .expect("update bundle state");
        expected.state = state;
        assert_record(&updated, &expected);
        assert_record(
            &bundle::get_measurement_bundle_by_id(&mut *txn, active.bundle_id)
                .await?
                .expect("persisted bundle state"),
            &expected,
        );
    }

    // Delete through the real wrappers so both generic RETURNING builders
    // keep their prepared statements, including a returned child record.
    assert_record(
        &bundle::delete_bundle_values_for_id(&mut txn, active.bundle_id).await?,
        &[value],
    );
    assert!(
        bundle::get_measurement_bundle_values_for_bundle_id(&mut *txn, active.bundle_id)
            .await?
            .is_empty()
    );
    for expected in [expected, pending] {
        assert_record(
            &bundle::delete_bundle_for_id(&mut txn, expected.bundle_id)
                .await?
                .expect("deleted bundle"),
            &expected,
        );
    }
    assert!(
        bundle::get_measurement_bundle_records(&mut *txn)
            .await?
            .is_empty()
    );
    assert_record(
        &profile::delete_profile_attr_records_for_id(&mut txn, created.profile_id).await?,
        &attributes,
    );
    assert_record(
        &profile::delete_profile_record_for_id(&mut txn, created.profile_id)
            .await?
            .expect("deleted profile"),
        &final_profile,
    );
    assert!(
        profile::get_measurement_profile_record_by_id(&mut txn, created.profile_id)
            .await?
            .is_none()
    );
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn report_and_journal_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = pool.acquire().await?;
    exercise_report_and_journal_queries(&mut connection).await?;
    assert!(connection.cached_statements_size() > 0);

    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE measurement_reports ADD COLUMN test_added_column text;
         ALTER TABLE measurement_reports_values ADD COLUMN test_added_column text;
         ALTER TABLE measurement_journal ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_report_and_journal_queries(&mut connection).await?;
    Ok(())
}

async fn exercise_report_and_journal_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x78; 32],
        MachineType::Host,
    );
    crate::machine::create(
        &mut txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let profile =
        profile::insert_measurement_profile_record(&mut txn, "journal-profile".to_string()).await?;
    let bundle = bundle::insert_measurement_bundle_record(
        &mut txn,
        profile.profile_id,
        "journal-bundle".to_string(),
        Some(MeasurementBundleState::Active),
    )
    .await?;

    let created = report::insert_measurement_report_record(&mut txn, machine_id).await?;
    assert_eq!(created.machine_id, machine_id);
    let values = report::insert_measurement_report_value_records(
        &mut txn,
        created.report_id,
        &[
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "aabb".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 7,
                sha_any: "ccdd".to_string(),
            },
        ],
    )
    .await?;
    assert_eq!(values.len(), 2);
    for (value, (pcr_register, sha_any)) in values.iter().zip([(0, "aabb"), (7, "ccdd")]) {
        assert_eq!(value.report_id, created.report_id);
        assert_eq!(value.pcr_register, pcr_register);
        assert_eq!(value.sha_any, sha_any);
    }
    assert_record(
        &report::get_measurement_report_record_by_id(&mut txn, created.report_id)
            .await?
            .expect("inserted report"),
        &created,
    );
    let mut persisted_values =
        report::get_measurement_report_values_for_report_id(&mut txn, created.report_id).await?;
    persisted_values.sort_by_key(|value| value.pcr_register);
    assert_record(&persisted_values, &values);

    let timestamp = DateTime::from_timestamp(1_700_000_000, 0).expect("fixture timestamp");
    let expected_report = MeasurementReportRecord {
        ts: timestamp,
        ..created.clone()
    };
    assert_record(
        &report::update_report_tstamp(&mut txn, created.report_id, timestamp).await?,
        &expected_report,
    );
    let expected_values: Vec<_> = values
        .iter()
        .map(|value| MeasurementReportValueRecord {
            ts: timestamp,
            ..value.clone()
        })
        .collect();
    let mut updated_values =
        report::update_report_values_tstamp(&mut txn, created.report_id, timestamp).await?;
    updated_values.sort_by_key(|value| value.pcr_register);
    assert_record(&updated_values, &expected_values);
    assert_record(
        &report::get_measurement_report_record_by_id(&mut txn, created.report_id)
            .await?
            .expect("report with updated timestamp"),
        &expected_report,
    );
    let mut persisted_values =
        report::get_measurement_report_values_for_report_id(&mut txn, created.report_id).await?;
    persisted_values.sort_by_key(|value| value.pcr_register);
    assert_record(&persisted_values, &expected_values);

    let initial = journal::insert_measurement_journal_record(
        &mut txn,
        machine_id,
        created.report_id,
        None,
        None,
        MeasurementMachineState::PendingBundle,
    )
    .await?;
    assert_eq!(initial.machine_id, machine_id);
    assert_eq!(initial.report_id, created.report_id);
    assert_eq!(initial.profile_id, None);
    assert_eq!(initial.bundle_id, None);
    assert_eq!(initial.state, MeasurementMachineState::PendingBundle);
    assert_record(
        &journal::get_measurement_journal_record_by_id(&mut txn, initial.journal_id)
            .await?
            .expect("journal with absent references"),
        &initial,
    );
    let updated = journal::update_measurement_journal_record(
        &mut txn,
        created.report_id,
        Some(profile.profile_id),
        Some(bundle.bundle_id),
        MeasurementMachineState::Measured,
    )
    .await?;
    assert!(updated.ts >= initial.ts);
    assert_record(
        &updated,
        &MeasurementJournalRecord {
            profile_id: Some(profile.profile_id),
            bundle_id: Some(bundle.bundle_id),
            state: MeasurementMachineState::Measured,
            ts: updated.ts,
            ..initial.clone()
        },
    );
    assert_record(
        &journal::get_measurement_journal_record_by_report_id(&mut txn, created.report_id)
            .await?
            .expect("updated journal"),
        &updated,
    );

    // Insert an older journal last so latest selection must use `ts`,
    // rather than insertion order or whichever row the database returns.
    let older_report = report::insert_measurement_report_record(&mut txn, machine_id).await?;
    let older_journal = journal::insert_measurement_journal_record(
        &mut txn,
        machine_id,
        older_report.report_id,
        None,
        None,
        MeasurementMachineState::Discovered,
    )
    .await?;
    sqlx::query("UPDATE measurement_journal SET ts = $1 WHERE journal_id = $2")
        .bind(timestamp)
        .bind(older_journal.journal_id)
        .execute(&mut *txn)
        .await?;
    assert_record(
        &machine::get_latest_journal_for_id(&mut *txn, machine_id)
            .await?
            .expect("latest journal record"),
        &updated,
    );
    assert_record(
        &super::journal::get_latest_journal_for_id(&mut *txn, machine_id)
            .await?
            .expect("latest journal model"),
        &updated,
    );

    assert_record(
        &journal::delete_journal_where_id(&mut txn, updated.journal_id)
            .await?
            .expect("deleted journal"),
        &updated,
    );
    assert!(
        journal::get_measurement_journal_record_by_id(&mut txn, updated.journal_id)
            .await?
            .is_none()
    );
    let mut removed_values =
        report::delete_report_values_for_id(&mut txn, created.report_id).await?;
    removed_values.sort_by_key(|value| value.pcr_register);
    assert_record(&removed_values, &expected_values);
    assert_record(
        &report::delete_report_for_id(&mut txn, created.report_id)
            .await?
            .expect("deleted report"),
        &expected_report,
    );
    assert!(
        report::get_measurement_report_record_by_id(&mut txn, created.report_id)
            .await?
            .is_none()
    );
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn candidate_machine_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = pool.acquire().await?;
    let mut txn = connection.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x79; 32],
        MachineType::Host,
    );
    crate::machine::create(
        &mut txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let hardware = HardwareInfo {
        dmi_data: Some(DmiData {
            sys_vendor: "NVIDIA".to_string(),
            product_name: "projection-host".to_string(),
            bios_version: "1.2.3".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let topology =
        crate::machine_topology::create_or_update(&mut txn, &machine_id, &hardware).await?;
    txn.commit().await?;
    let expected = super::machine::CandidateMachineRecord {
        machine_id,
        topology: sqlx::types::Json(topology.topology),
        created: topology.created,
        updated: topology.updated,
    };

    for after_column_addition in [false, true] {
        if after_column_addition {
            let mut migration = pool.begin().await?;
            sqlx::raw_sql(
                "SET LOCAL lock_timeout = '5s';
                 ALTER TABLE machine_topologies ADD COLUMN test_added_column text;",
            )
            .execute(&mut *migration)
            .await?;
            migration.commit().await?;
        }
        assert_record(
            &machine::get_candidate_machine_record_by_id(&mut connection, machine_id)
                .await?
                .expect("candidate topology"),
            &expected,
        );
        assert_record(
            &machine::get_candidate_machine_records(&mut *connection).await?,
            &[&expected],
        );
        assert!(connection.cached_statements_size() > 0);
    }
    Ok(())
}

#[crate::sqlx_test]
async fn approval_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = pool.acquire().await?;
    exercise_approval_queries(&mut connection).await?;
    assert!(connection.cached_statements_size() > 0);

    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE measurement_approved_machines ADD COLUMN test_added_column text;
         ALTER TABLE measurement_approved_profiles ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_approval_queries(&mut connection).await?;
    Ok(())
}

async fn exercise_approval_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let profile =
        profile::insert_measurement_profile_record(&mut txn, "approved-profile".to_string())
            .await?;

    for remove_by_approval_id in [true, false] {
        let approved_machine = site::insert_into_approved_machines(
            &mut txn,
            TrustedMachineId::Any,
            MeasurementApprovedType::Persist,
            Some("0,7".to_string()),
            Some("approved machine projection".to_string()),
        )
        .await?;
        assert_eq!(approved_machine.machine_id, TrustedMachineId::Any);
        assert_eq!(
            approved_machine.approval_type,
            MeasurementApprovedType::Persist
        );
        assert_eq!(approved_machine.pcr_registers.as_deref(), Some("0,7"));
        assert_eq!(
            approved_machine.comments.as_deref(),
            Some("approved machine projection")
        );
        assert_record(
            &site::get_approval_for_machine_id(&mut txn, TrustedMachineId::Any)
                .await?
                .expect("approved machine lookup"),
            &approved_machine,
        );
        assert_record(
            &site::get_approved_machines(&mut *txn).await?,
            &[&approved_machine],
        );
        let removed_machine = if remove_by_approval_id {
            site::remove_from_approved_machines_by_approval_id(
                &mut txn,
                approved_machine.approval_id,
            )
            .await?
        } else {
            site::remove_from_approved_machines_by_machine_id(&mut txn, TrustedMachineId::Any)
                .await?
        };
        assert_record(&removed_machine, &approved_machine);
        assert!(site::get_approved_machines(&mut *txn).await?.is_empty());

        let approved_profile = site::insert_into_approved_profiles(
            &mut txn,
            profile.profile_id,
            MeasurementApprovedType::Oneshot,
            Some("0-7".to_string()),
            Some("approved profile projection".to_string()),
        )
        .await?;
        assert_eq!(approved_profile.profile_id, profile.profile_id);
        assert_eq!(
            approved_profile.approval_type,
            MeasurementApprovedType::Oneshot
        );
        assert_eq!(approved_profile.pcr_registers.as_deref(), Some("0-7"));
        assert_eq!(
            approved_profile.comments.as_deref(),
            Some("approved profile projection")
        );
        assert_record(
            &site::get_approval_for_profile_id(&mut txn, profile.profile_id)
                .await?
                .expect("approved profile lookup"),
            &approved_profile,
        );
        assert_record(
            &site::get_approved_profiles(&mut *txn).await?,
            &[&approved_profile],
        );
        let removed_profile = if remove_by_approval_id {
            site::remove_from_approved_profiles_by_approval_id(
                &mut txn,
                approved_profile.approval_id,
            )
            .await?
        } else {
            site::remove_from_approved_profiles_by_profile_id(&mut txn, profile.profile_id).await?
        };
        assert_record(&removed_profile, &approved_profile);
        assert!(site::get_approved_profiles(&mut *txn).await?.is_empty());
    }
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn site_import_preserves_records_across_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let profile_id = MeasurementSystemProfileId::new();
    let bundle_id = MeasurementBundleId::new();
    let timestamp = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("fixture timestamp");
    let expected = SiteModel {
        measurement_system_profiles: vec![MeasurementSystemProfileRecord {
            profile_id,
            name: "imported-profile".to_string(),
            ts: timestamp,
        }],
        measurement_system_profiles_attrs: vec![MeasurementSystemProfileAttrRecord {
            attribute_id: MeasurementSystemProfileAttrId::new(),
            profile_id,
            key: "bios_version".to_string(),
            value: "3.2.1".to_string(),
            ts: timestamp + chrono::TimeDelta::seconds(1),
        }],
        measurement_bundles: vec![MeasurementBundleRecord {
            bundle_id,
            profile_id,
            name: "imported-bundle".to_string(),
            state: MeasurementBundleState::Obsolete,
            ts: timestamp + chrono::TimeDelta::seconds(2),
        }],
        measurement_bundles_values: vec![MeasurementBundleValueRecord {
            value_id: MeasurementBundleValueId::new(),
            bundle_id,
            pcr_register: 7,
            sha_any: "aabbccdd".to_string(),
            ts: timestamp + chrono::TimeDelta::seconds(3),
        }],
    };
    let mut connection = pool.acquire().await?;

    for after_column_addition in [false, true] {
        if after_column_addition {
            let mut migration = pool.begin().await?;
            sqlx::raw_sql(
                "SET LOCAL lock_timeout = '5s';
                 ALTER TABLE measurement_system_profiles ADD COLUMN test_added_column text;
                 ALTER TABLE measurement_system_profiles_attrs ADD COLUMN test_added_column text;
                 ALTER TABLE measurement_bundles ADD COLUMN test_added_column text;
                 ALTER TABLE measurement_bundles_values ADD COLUMN test_added_column text;",
            )
            .execute(&mut *migration)
            .await?;
            migration.commit().await?;
        }

        let mut txn = connection.begin().await?;
        super::site::import(&mut txn, &expected).await?;
        txn.commit().await?;
        assert!(connection.cached_statements_size() > 0);
        let exported = super::site::export(&mut *connection).await?;
        assert_record(&exported, &expected);

        // A separate reader proves that import committed every supplied
        // identifier, timestamp and value, not just its parent records.
        let mut persisted = pool.acquire().await?;
        assert_record(&super::site::export(&mut *persisted).await?, &expected);
        drop(persisted);

        if !after_column_addition {
            // Reuse the same import arguments and cached INSERT statements
            // after DDL without conflicting with the first import's IDs.
            let mut txn = connection.begin().await?;
            bundle::delete_bundle_values_for_id(&mut txn, bundle_id).await?;
            bundle::delete_bundle_for_id(&mut txn, bundle_id).await?;
            profile::delete_profile_attr_records_for_id(&mut txn, profile_id).await?;
            profile::delete_profile_record_for_id(&mut txn, profile_id).await?;
            txn.commit().await?;
        }
    }
    Ok(())
}
