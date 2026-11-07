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

//! tests/report.rs
//!
//! Reports:
//! [x] test_report_crudl: Make sure basic CRUDL works as expected.
//! [x] test_report_journal: Make sure journal is updated on reports.
//! [x] test_report_to_active_bundle: Make sure active bundle promotion works.
//! [x] test_report_to_revoked_bundle: Ensure revoked bundle promotion works.

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use carbide_uuid::measured_boot::MeasurementReportId;
    use db::db_read::PgPoolReader;
    use db::measured_boot::interface::common::pcr_register_values_to_map;
    use db::measured_boot::interface::report::{
        get_all_measurement_report_records, get_all_measurement_report_value_records,
    };
    use measured_boot::pcr::{PcrRegisterValue, parse_pcr_index_input};
    use measured_boot::records::{MeasurementBundleState, MeasurementMachineState};
    use rand::prelude::*;
    use rpc::forge::forge_server::Forge;

    use crate::measured_boot::tests::common::{create_test_machine, load_topology_json};
    use crate::tests::create_test_env;

    // test_profile_crudl creates a new profile with 3 attributes,
    // another new profile with 4 attributes.
    //
    // It makes sure each profile results in the correct number
    // of records being inserted into the database, and also makes
    // sure the records themselves are correct.
    #[crate::sqlx_test]
    async fn test_report_crudl(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine = create_test_machine(
            &mut txn,
            "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530",
            &load_topology_json("dell_r750.json"),
        )
        .await?;

        let values: Vec<PcrRegisterValue> = vec![
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "aa".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 1,
                sha_any: "bb".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 2,
                sha_any: "cc".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 3,
                sha_any: "dd".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 4,
                sha_any: "ee".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 5,
                sha_any: "ff".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 6,
                sha_any: "gg".to_string(),
            },
        ];
        let input_value_map = pcr_register_values_to_map(&values)?;
        assert_eq!(input_value_map.len(), 7);

        // Make a report and make sure it looks good.
        let report = db::measured_boot::report::new(&mut txn, machine.machine_id, &values).await?;
        assert_eq!(report.machine_id, machine.machine_id);
        assert_eq!(report.pcr_values().len(), 7);
        let report_value_map = pcr_register_values_to_map(&report.pcr_values())?;

        // Get the same report and make sure its the same as the report we made.
        let same_report = db::measured_boot::report::from_id(&mut txn, report.report_id).await?;
        assert_eq!(report.report_id, same_report.report_id);
        assert_eq!(report.ts, same_report.ts);
        assert_eq!(same_report.machine_id, machine.machine_id);
        assert_eq!(same_report.pcr_values().len(), 7);
        let same_report_value_map = pcr_register_values_to_map(&same_report.pcr_values())?;

        // Now make another report with the same values. This will simply update the timestamp of
        // the previous report.
        // Verify that the timestamp has been updated.

        // get journal first
        let journal =
            db::measured_boot::journal::get_journal_for_report_id(&mut txn, report.report_id)
                .await?;

        let report2 = db::measured_boot::report::new(&mut txn, machine.machine_id, &values).await?;
        assert_eq!(report2.machine_id, machine.machine_id);
        assert_eq!(report2.report_id, report.report_id);
        assert_eq!(report2.pcr_values().len(), 7);

        // check journal too
        let journal2 =
            db::measured_boot::journal::get_journal_for_report_id(&mut txn, report.report_id)
                .await?;
        assert_eq!(journal.journal_id, journal2.journal_id);
        assert_eq!(journal.report_id, journal2.report_id);
        assert_eq!(journal.machine_id, journal2.machine_id);
        assert_eq!(journal.profile_id, journal2.profile_id);
        assert_ne!(journal.ts, journal2.ts);

        assert_eq!(report2.report_id, report.report_id);
        assert_ne!(report.ts, report2.ts);
        let report2_value_map = pcr_register_values_to_map(&report2.pcr_values())?;

        // Double check values against the input values.
        for (pcr_register, input_value) in input_value_map.iter() {
            let report_value = report_value_map.get(pcr_register);
            assert!(report_value.is_some());
            assert_eq!(report_value.unwrap().sha_any, input_value.sha_any);
            let same_report_value = same_report_value_map.get(pcr_register);
            assert!(same_report_value.is_some());
            assert_eq!(same_report_value.unwrap().sha_any, input_value.sha_any);
            let report2_value = report2_value_map.get(pcr_register);
            assert!(report2_value.is_some());
            assert_eq!(report2_value.unwrap().sha_any, input_value.sha_any);
        }
        assert_eq!(
            serde_json::to_string_pretty(&report).unwrap(),
            serde_json::to_string_pretty(&same_report).unwrap()
        );

        // And then a third report.
        let values3: Vec<PcrRegisterValue> = vec![
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "xx".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 1,
                sha_any: "yy".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 2,
                sha_any: "zz".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 3,
                sha_any: "dd".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 4,
                sha_any: "ee".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 5,
                sha_any: "ff".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 6,
                sha_any: "pp".to_string(),
            },
        ];
        let input3_value_map = pcr_register_values_to_map(&values3)?;
        assert_eq!(input3_value_map.len(), 7);

        // Make a report and make sure it looks good.
        let report3 =
            db::measured_boot::report::new(&mut txn, machine.machine_id, &values3).await?;
        assert_eq!(report3.machine_id, machine.machine_id);
        assert_eq!(report3.pcr_values().len(), 7);
        let report3_value_map = pcr_register_values_to_map(&report3.pcr_values())?;

        // Get the same report and make sure its the same as the report we made.
        let same_report3 = db::measured_boot::report::from_id(&mut txn, report3.report_id).await?;
        assert_eq!(report3.report_id, same_report3.report_id);
        assert_eq!(report3.ts, same_report3.ts);
        assert_eq!(same_report3.machine_id, machine.machine_id);
        assert_eq!(same_report3.pcr_values().len(), 7);
        let same_report3_value_map = pcr_register_values_to_map(&same_report3.pcr_values())?;

        // Double check values against the input values.
        for (pcr_register, input_value) in input3_value_map.iter() {
            let report3_value = report3_value_map.get(pcr_register);
            assert!(report3_value.is_some());
            assert_eq!(report3_value.unwrap().sha_any, input_value.sha_any);
            let same_report3_value = same_report3_value_map.get(pcr_register);
            assert!(same_report3_value.is_some());
            assert_eq!(same_report3_value.unwrap().sha_any, input_value.sha_any);
        }

        // And then get everything thus far.
        let reports = db::measured_boot::report::get_all(txn.as_mut()).await?;
        assert_eq!(reports.len(), 2);

        // And now lets do some database record checks.
        let report_records = get_all_measurement_report_records(&mut txn).await?;
        assert_eq!(report_records.len(), 2);

        let report_value_records = get_all_measurement_report_value_records(&mut txn).await?;
        assert_eq!(report_value_records.len(), 14);

        Ok(())
    }

    // test_report_journal creates a new profile and makes
    // sure the journal gets updated.
    #[crate::sqlx_test]
    async fn test_report_journal(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine = create_test_machine(
            &mut txn,
            "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530",
            &load_topology_json("dell_r750.json"),
        )
        .await?;

        // There should be no profiles yet, so just check
        // to make sure.
        let attrs: HashMap<String, String> = vec![
            ("sys_vendor".to_string(), "Dell, Inc.".to_string()),
            ("product_name".to_string(), "PowerEdge R750".to_string()),
            ("bios_version".to_string(), "1.8.2".to_string()),
        ]
        .into_iter()
        .collect();

        let optional_profile =
            db::measured_boot::profile::load_from_attrs(&mut txn, &attrs).await?;
        assert!(optional_profile.is_none());

        let values: Vec<PcrRegisterValue> = vec![
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "aa".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 1,
                sha_any: "bb".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 2,
                sha_any: "cc".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 3,
                sha_any: "dd".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 4,
                sha_any: "ee".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 5,
                sha_any: "ff".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 6,
                sha_any: "gg".to_string(),
            },
        ];
        let input_value_map = pcr_register_values_to_map(&values)?;
        assert_eq!(input_value_map.len(), 7);

        // Make a report and make sure the records themselves are correct.
        let report = db::measured_boot::report::new(&mut txn, machine.machine_id, &values).await?;

        txn.commit().await?;
        let mut txn = pool.begin().await?;

        // And NOW there should be a profile.
        let optional_profile =
            db::measured_boot::profile::load_from_attrs(&mut txn, &attrs).await?;
        assert!(optional_profile.is_some());
        let profile = optional_profile.unwrap();

        let journals =
            db::measured_boot::journal::get_all_for_machine_id(&mut txn, report.machine_id).await?;
        assert_eq!(journals.len(), 1);

        let journal = &journals[0];
        assert_eq!(journal.machine_id, report.machine_id);
        assert_eq!(journal.report_id, report.report_id);
        assert_eq!(journal.profile_id, Some(profile.profile_id));
        assert_eq!(journal.bundle_id, None);
        assert_eq!(journal.state, MeasurementMachineState::PendingBundle);

        Ok(())
    }

    // test_report_to_active_bundle promotes a report to an active
    // bundle and makes sure all is well.
    #[crate::sqlx_test]
    async fn test_report_to_active_bundle(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine = create_test_machine(
            &mut txn,
            "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530",
            &load_topology_json("dell_r750.json"),
        )
        .await?;

        let values: Vec<PcrRegisterValue> = vec![
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "aa".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 1,
                sha_any: "bb".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 2,
                sha_any: "cc".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 3,
                sha_any: "dd".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 4,
                sha_any: "ee".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 5,
                sha_any: "ff".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 6,
                sha_any: "gg".to_string(),
            },
        ];

        // And then make a report. At this point, we should have a:
        // - machine_id
        // - report_id
        // - profile_id
        //
        // ...which means we have what we need to promote a new bundle,
        // which will result in a second journal entry.
        let report = db::measured_boot::report::new(&mut txn, machine.machine_id, &values).await?;
        let journals =
            db::measured_boot::journal::get_all_for_machine_id(&mut txn, report.machine_id).await?;
        assert_eq!(journals.len(), 1);
        let is_latest_journal =
            db::measured_boot::journal::get_latest_for_machine_id(&mut txn, report.machine_id)
                .await?;
        assert!(is_latest_journal.is_some());
        let latest_journal = is_latest_journal.unwrap();
        assert_eq!(latest_journal.state, MeasurementMachineState::PendingBundle);
        assert_eq!(latest_journal.bundle_id, None);

        // Make a bundle with all of the values from the report,
        // and make sure everything gets updated accordingly.
        let bundle_full =
            db::measured_boot::report::create_active_bundle(&mut txn, &report, &None).await?;
        let bundles = db::measured_boot::bundle::get_all(txn.as_mut()).await?;
        assert_eq!(bundles.len(), 1);
        let journals =
            db::measured_boot::journal::get_all_for_machine_id(&mut txn, report.machine_id).await?;
        assert_eq!(journals.len(), 2);
        let first_bundle =
            db::measured_boot::bundle::from_id(&mut txn, bundle_full.bundle_id).await?;
        assert_eq!(first_bundle.pcr_values().len(), 7);
        let is_latest_journal =
            db::measured_boot::journal::get_latest_for_machine_id(&mut txn, report.machine_id)
                .await?;
        assert!(is_latest_journal.is_some());
        let latest_journal = is_latest_journal.unwrap();
        assert_eq!(latest_journal.bundle_id, Some(first_bundle.bundle_id));
        assert_eq!(latest_journal.state, MeasurementMachineState::Measured);

        // Create a new bundle with SOME of the bundles from the report,
        // and make sure it all looks good.
        let pcr_set = parse_pcr_index_input("0-3")?;
        let bundle_partial =
            db::measured_boot::report::create_active_bundle(&mut txn, &report, &Some(pcr_set))
                .await?;
        let bundles = db::measured_boot::bundle::get_all(txn.as_mut()).await?;
        assert_eq!(bundles.len(), 2);
        let journals =
            db::measured_boot::journal::get_all_for_machine_id(&mut txn, report.machine_id).await?;
        assert_eq!(journals.len(), 3);
        let second_bundle =
            db::measured_boot::bundle::from_id(&mut txn, bundle_partial.bundle_id).await?;
        assert_eq!(second_bundle.pcr_values().len(), 4);

        let is_latest_journal =
            db::measured_boot::journal::get_latest_for_machine_id(&mut txn, report.machine_id)
                .await?;
        assert!(is_latest_journal.is_some());
        let latest_journal = is_latest_journal.unwrap();
        assert_eq!(latest_journal.bundle_id, Some(second_bundle.bundle_id));
        assert_eq!(latest_journal.state, MeasurementMachineState::Measured);

        // And then create a bundle that won't match, and make
        // sure that it all works.

        let values: Vec<PcrRegisterValue> = vec![
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "aa".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 1,
                sha_any: "bb".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 2,
                sha_any: "cc".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 3,
                sha_any: "dd".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 4,
                sha_any: "ee".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 5,
                sha_any: "ff".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 6,
                sha_any: "gg".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 7,
                sha_any: "hh".to_string(),
            },
        ];

        let other_bundle = db::measured_boot::bundle::new(
            &mut txn,
            bundle_partial.profile_id,
            None,
            &values,
            Some(MeasurementBundleState::Active),
        )
        .await?;

        let bundles = db::measured_boot::bundle::get_all(txn.as_mut()).await?;
        assert_eq!(bundles.len(), 3);
        let journals =
            db::measured_boot::journal::get_all_for_machine_id(&mut txn, report.machine_id).await?;
        assert_eq!(journals.len(), 3);
        let third_bundle =
            db::measured_boot::bundle::from_id(&mut txn, other_bundle.bundle_id).await?;
        assert_eq!(third_bundle.pcr_values().len(), 8);

        let is_latest_journal =
            db::measured_boot::journal::get_latest_for_machine_id(&mut txn, report.machine_id)
                .await?;
        assert!(is_latest_journal.is_some());
        let latest_journal = is_latest_journal.unwrap();
        assert_eq!(latest_journal.bundle_id, Some(second_bundle.bundle_id));
        assert_eq!(latest_journal.state, MeasurementMachineState::Measured);

        Ok(())
    }

    // test_report_to_revoked_bundle "promotes" a report to a
    // revoked bundle and makes sure all is well.
    #[crate::sqlx_test]
    async fn test_report_to_revoked_bundle(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine = create_test_machine(
            &mut txn,
            "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530",
            &load_topology_json("dell_r750.json"),
        )
        .await?;

        let values: Vec<PcrRegisterValue> = vec![
            PcrRegisterValue {
                pcr_register: 0,
                sha_any: "aa".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 1,
                sha_any: "bb".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 2,
                sha_any: "cc".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 3,
                sha_any: "dd".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 4,
                sha_any: "ee".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 5,
                sha_any: "ff".to_string(),
            },
            PcrRegisterValue {
                pcr_register: 6,
                sha_any: "gg".to_string(),
            },
        ];

        // And then make a report. At this point, we should have a:
        // - machine_id
        // - report_id
        // - profile_id
        //
        // ...which means we have what we need to promote a new bundle,
        // which will result in a second journal entry.
        let report = db::measured_boot::report::new(&mut txn, machine.machine_id, &values).await?;
        let journals =
            db::measured_boot::journal::get_all_for_machine_id(&mut txn, report.machine_id).await?;
        assert_eq!(journals.len(), 1);
        let is_latest_journal =
            db::measured_boot::journal::get_latest_for_machine_id(&mut txn, report.machine_id)
                .await?;
        assert!(is_latest_journal.is_some());
        let latest_journal = is_latest_journal.unwrap();
        assert_eq!(latest_journal.state, MeasurementMachineState::PendingBundle);
        assert_eq!(latest_journal.bundle_id, None);

        // Make a bundle with some of the values from the report,
        // and make sure everything gets updated accordingly.
        let pcr_set = parse_pcr_index_input("0-3")?;
        let bundle_partial =
            db::measured_boot::report::create_revoked_bundle(&mut txn, &report, &Some(pcr_set))
                .await?;
        let bundles = db::measured_boot::bundle::get_all(txn.as_mut()).await?;
        assert_eq!(bundles.len(), 1);
        let journals =
            db::measured_boot::journal::get_all_for_machine_id(&mut txn, report.machine_id).await?;
        assert_eq!(journals.len(), 2);
        let first_bundle =
            db::measured_boot::bundle::from_id(&mut txn, bundle_partial.bundle_id).await?;
        assert_eq!(first_bundle.pcr_values().len(), 4);
        let is_latest_journal =
            db::measured_boot::journal::get_latest_for_machine_id(&mut txn, report.machine_id)
                .await?;
        assert!(is_latest_journal.is_some());
        let latest_journal = is_latest_journal.unwrap();
        assert_eq!(latest_journal.bundle_id, Some(first_bundle.bundle_id));
        assert_eq!(
            latest_journal.state,
            MeasurementMachineState::MeasuringFailed
        );
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_max_reports_limit(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let env = create_test_env(pool.clone()).await;
        let machine_1 = create_test_machine(
            &mut txn,
            "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530",
            &load_topology_json("dell_r750.json"),
        )
        .await?;

        let machine_2 = create_test_machine(
            &mut txn,
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30",
            &load_topology_json("dell_r750.json"),
        )
        .await?;

        let mut inserted_report_ids = Vec::<MeasurementReportId>::new();
        // create 260*2 reports, saving their report_ids in a vector
        for i in 0..260 {
            // generate random values to count as separate reports
            let rng = rand::rng();

            let mut random_str: String = rng
                .sample_iter(&rand::distr::Alphanumeric)
                .take(7)
                .map(char::from)
                .collect();

            random_str.push_str(&i.to_string());

            let values: Vec<PcrRegisterValue> = vec![
                PcrRegisterValue {
                    pcr_register: 0,
                    sha_any: random_str,
                },
                PcrRegisterValue {
                    pcr_register: 1,
                    sha_any: "bb".to_string(),
                },
                PcrRegisterValue {
                    pcr_register: 2,
                    sha_any: "cc".to_string(),
                },
                PcrRegisterValue {
                    pcr_register: 3,
                    sha_any: "dd".to_string(),
                },
                PcrRegisterValue {
                    pcr_register: 4,
                    sha_any: "ee".to_string(),
                },
                PcrRegisterValue {
                    pcr_register: 5,
                    sha_any: "ff".to_string(),
                },
                PcrRegisterValue {
                    pcr_register: 6,
                    sha_any: "gg".to_string(),
                },
            ];

            let report =
                db::measured_boot::report::new(&mut txn, machine_1.machine_id, &values).await?;
            inserted_report_ids.push(report.report_id);
            let report =
                db::measured_boot::report::new(&mut txn, machine_2.machine_id, &values).await?;
            inserted_report_ids.push(report.report_id);
        }

        // make sure 520 reports have been stored
        let inserted_reports = db::measured_boot::report::get_all(txn.as_mut()).await?;
        assert_eq!(inserted_reports.len(), 520);

        // since the trim operation happens in a separate transaction, we need to commit
        // the current transaction
        let _ = txn.commit().await;

        // now trim the table and verify that it has been trimmed down to 500
        let request = tonic::Request::new(rpc::forge::TrimTableRequest {
            target: rpc::forge::TrimTableTarget::MeasuredBoot as i32,
            keep_entries: 250,
        });

        let response = env.api.trim_table(request).await?;
        assert_eq!(response.into_inner().total_deleted, 20.to_string());

        // make sure 500 are remaining
        let remaining_reports =
            db::measured_boot::report::get_all(&mut PgPoolReader::from(pool)).await?;
        assert_eq!(remaining_reports.len(), 500);

        // now make sure the first 20 reports are already gone and the rest are still in
        let remaining_reports_as_set: HashSet<MeasurementReportId> =
            remaining_reports.iter().map(|e| e.report_id).collect();
        // the first 20 report_ids will not be in the all_reports, the rest will be
        for report_id in inserted_report_ids.iter().take(20) {
            assert!(!remaining_reports_as_set.contains(report_id));
        }

        for report_id in inserted_report_ids.iter().skip(20) {
            assert!(remaining_reports_as_set.contains(report_id));
        }

        Ok(())
    }
}
