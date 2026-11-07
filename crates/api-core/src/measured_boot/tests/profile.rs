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

//! tests/profile.rs
//!
//! Bundles:
//! [ ] test_bundle_crudl: Ensure basic CRUDL works as expected.
//! [ ] test_bundle_duplicates: Ensure we can't make duplicates.
//! [ ] test_bundle_matching: Ensure matching logic works as expected.
//! [ ] test_bundle_set_state: Ensure updating bundle states works as expected.
//! [ ] test_bundle_journal: Ensure journal is updated on bundle changes.
//!
//! Profiles:
//! [x] test_profile_crudl: Make sure basic CRUDL works as expected.
//! [x] test_profile_duplicates: Make sure we can't make duplicates
//! [x] test_profile_matching: Make sure matching logic works as expected.
//!
//! Site:
//! [ ] test_site_import_export: Make sure an export/import looks good.
//! [ ] test_site_approved_machine: Make sure approved machine mgmt works.
//! [ ] test_site_approved_profile: Make sure approved profile mgmt works.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use db::measured_boot::interface::profile::{
        get_all_measurement_profile_attr_records, get_all_measurement_profile_records,
    };

    // test_profile_crudl creates a new profile with 3 attributes,
    // another new profile with 4 attributes.
    //
    // It makes sure each profile results in the correct number
    // of records being inserted into the database, and also makes
    // sure the records themselves are correct.
    #[crate::sqlx_test]
    async fn test_profile_crudl(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let vals = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
            (String::from("bios_version"), String::from("v1")),
        ]);

        // Make sure the profile itself is in tact.
        let profile1 =
            db::measured_boot::profile::new(&mut txn, Some(String::from("my-profile")), &vals)
                .await?;
        assert_eq!(profile1.name, String::from("my-profile"));
        assert_eq!(profile1.attrs.len(), 3);

        // And now get the profile in various ways to make sure the
        // various ways work.
        let profile_from_id =
            db::measured_boot::profile::load_from_id(&mut txn, profile1.profile_id).await?;
        assert_eq!(profile1.profile_id, profile_from_id.profile_id);
        assert_eq!(profile1.name, profile_from_id.name);
        assert_eq!(
            serde_json::to_string_pretty(&profile1).unwrap(),
            serde_json::to_string_pretty(&profile_from_id).unwrap()
        );

        let profile_from_name =
            db::measured_boot::profile::load_from_name(&mut txn, profile1.name.clone()).await?;
        assert_eq!(profile1.profile_id, profile_from_name.profile_id);
        assert_eq!(profile1.name, profile_from_name.name);
        assert_eq!(
            serde_json::to_string_pretty(&profile1).unwrap(),
            serde_json::to_string_pretty(&profile_from_name).unwrap()
        );

        let some_profile_from_attrs =
            db::measured_boot::profile::load_from_attrs(&mut txn, &vals).await?;
        assert!(some_profile_from_attrs.is_some());

        let profile_from_attrs = some_profile_from_attrs.unwrap();
        assert_eq!(profile1.profile_id, profile_from_attrs.profile_id);
        assert_eq!(profile1.name, profile_from_attrs.name);
        assert_eq!(
            serde_json::to_string_pretty(&profile1).unwrap(),
            serde_json::to_string_pretty(&profile_from_attrs).unwrap()
        );

        // Do a little bit of database recon to make
        // sure the expected number of rows are there.
        let profile1_records = get_all_measurement_profile_records(txn.as_mut()).await?;
        assert_eq!(profile1_records.len(), 1);

        let profile_attr_records = get_all_measurement_profile_attr_records(&mut txn).await?;
        assert_eq!(profile_attr_records.len(), 3);
        for attr_record in profile_attr_records.iter() {
            assert_eq!(profile1_records[0].profile_id, attr_record.profile_id);
        }

        let vals2 = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
            (String::from("bios_version"), String::from("v1")),
            (String::from("uefi_version"), String::from("2.10")),
        ]);

        let profile2 =
            db::measured_boot::profile::new(&mut txn, Some(String::from("my-profile2")), &vals2)
                .await?;
        assert_eq!(profile2.name, String::from("my-profile2"));
        assert_eq!(profile2.attrs.len(), 4);

        // And now get the profile in various ways to make sure the
        // various ways work.
        let profile2_from_id =
            db::measured_boot::profile::load_from_id(&mut txn, profile2.profile_id).await?;
        assert_eq!(profile2.profile_id, profile2_from_id.profile_id);
        assert_eq!(profile2.name, profile2_from_id.name);
        assert_eq!(
            serde_json::to_string_pretty(&profile2).unwrap(),
            serde_json::to_string_pretty(&profile2_from_id).unwrap()
        );

        let profile2_from_name =
            db::measured_boot::profile::load_from_name(&mut txn, profile2.name.clone()).await?;
        assert_eq!(profile2.profile_id, profile2_from_name.profile_id);
        assert_eq!(profile2.name, profile2_from_name.name);
        assert_eq!(
            serde_json::to_string_pretty(&profile2).unwrap(),
            serde_json::to_string_pretty(&profile2_from_name).unwrap()
        );

        let some_profile2_from_attrs =
            db::measured_boot::profile::load_from_attrs(&mut txn, &vals2).await?;
        assert!(some_profile2_from_attrs.is_some());

        let profile2_from_attrs = some_profile2_from_attrs.unwrap();
        assert_eq!(profile2.profile_id, profile2_from_attrs.profile_id);
        assert_eq!(profile2.name, profile2_from_attrs.name);
        assert_eq!(
            serde_json::to_string_pretty(&profile2).unwrap(),
            serde_json::to_string_pretty(&profile2_from_attrs).unwrap()
        );

        // Do a little bit of database recon to make
        // sure the expected number of rows are there.
        let profile_both_records = get_all_measurement_profile_records(txn.as_mut()).await?;
        assert_eq!(profile_both_records.len(), 2);

        let profile_all_attr_records = get_all_measurement_profile_attr_records(&mut txn).await?;
        assert_eq!(profile_all_attr_records.len(), 7);
        Ok(())
    }

    // test_profile_duplicates creates a new profile with 3 attributes,
    // and then tries to make profiles with:
    // - the same name
    // - the same attributes
    // - an actual unique one (for funsies)
    //
    // It makes sure both of those cases fail, and that
    // the actual unique one works.
    #[crate::sqlx_test]
    async fn test_profile_duplicates(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let vals = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
        ]);

        let vals2 = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
            (String::from("bios_version"), String::from("v1")),
        ]);

        // Create the first profile and commit it
        let mut txn = pool.begin().await?;
        db::measured_boot::profile::new(&mut txn, Some(String::from("my-profile")), &vals).await?;
        txn.commit().await?;

        // Try to create a duplicate by name (should fail) - use separate txn since failure aborts it
        let mut txn = pool.begin().await?;
        let dupe_by_name =
            db::measured_boot::profile::new(&mut txn, Some(String::from("my-profile")), &vals2)
                .await;
        assert!(dupe_by_name.is_err());
        // txn is aborted due to error, don't try to use it further

        // Try to create a duplicate by attrs (should fail) - use separate txn
        let mut txn = pool.begin().await?;
        let dupe_by_vals =
            db::measured_boot::profile::new(&mut txn, Some(String::from("my-profile2")), &vals)
                .await;
        assert!(dupe_by_vals.is_err());
        // txn is aborted due to error, don't try to use it further

        // Create a non-duplicate (should succeed)
        let mut txn = pool.begin().await?;
        let not_a_dupe =
            db::measured_boot::profile::new(&mut txn, Some(String::from("my-profile2")), &vals2)
                .await;
        assert!(not_a_dupe.is_ok());
        txn.commit().await?;

        Ok(())
    }

    // test_profile_matching creates a 5 profiles. one with
    // a completely different set of attributes, and four with
    // different (but overlapping) attributes, and then makes sure
    // the matching logic works as expected.
    #[crate::sqlx_test]
    async fn test_profile_matching(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let vals1 = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
        ]);

        let vals2 = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
            (String::from("bios_version"), String::from("v1")),
        ]);

        let vals3 = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
            (String::from("bios_version"), String::from("v1")),
            (String::from("uefi_version"), String::from("2.10")),
        ]);

        let vals4 = HashMap::from([
            (String::from("sys_vendor"), String::from("dell")),
            (String::from("product_name"), String::from("poweredge_r750")),
            (String::from("bios_version"), String::from("v1")),
            (String::from("uefi_version"), String::from("2.20")),
        ]);

        let vals5 = HashMap::from([
            (String::from("sys_vendor"), String::from("nvidia")),
            (String::from("product_name"), String::from("dgx_h100")),
        ]);

        match db::measured_boot::profile::new(&mut txn, None, &vals1).await {
            Ok(profile1) => {
                let match1_vals = HashMap::from([
                    (String::from("sys_vendor"), String::from("dell")),
                    (String::from("product_name"), String::from("poweredge_r750")),
                ]);
                let match1_result =
                    db::measured_boot::profile::match_from_attrs(&mut txn, &match1_vals).await;
                assert!(match1_result.is_ok());
                let match1 = match1_result.unwrap();
                assert_eq!(profile1.profile_id, match1.unwrap().profile_id);
            }
            Err(e) => return Err(eyre::eyre!("failed to create profile1: {}", e).into()),
        }

        match db::measured_boot::profile::new(&mut txn, None, &vals2).await {
            Ok(profile2) => {
                let match2_vals = HashMap::from([
                    (String::from("sys_vendor"), String::from("dell")),
                    (String::from("product_name"), String::from("poweredge_r750")),
                    (String::from("bios_version"), String::from("v1")),
                    (String::from("random_firmware_ver"), String::from("meowwww")),
                ]);
                let match2_result =
                    db::measured_boot::profile::match_from_attrs(&mut txn, &match2_vals).await;
                assert!(match2_result.is_ok());
                let match2 = match2_result.unwrap();
                assert_eq!(profile2.profile_id, match2.unwrap().profile_id);
            }
            Err(e) => return Err(eyre::eyre!("failed to create profile2: {}", e).into()),
        }

        match db::measured_boot::profile::new(&mut txn, None, &vals3).await {
            Ok(profile3) => {
                let match3_vals = HashMap::from([
                    (String::from("sys_vendor"), String::from("dell")),
                    (String::from("product_name"), String::from("poweredge_r750")),
                    (String::from("bios_version"), String::from("v1")),
                    (String::from("uefi_version"), String::from("2.10")),
                    (String::from("more_random_attr"), String::from("1.2.3.4")),
                    (String::from("another_thing"), String::from("v1-0.24")),
                ]);
                let match3_result =
                    db::measured_boot::profile::match_from_attrs(&mut txn, &match3_vals).await;
                assert!(match3_result.is_ok());
                let match3 = match3_result.unwrap();
                assert_eq!(profile3.profile_id, match3.unwrap().profile_id);
            }
            Err(e) => return Err(eyre::eyre!("failed to create profile3: {}", e).into()),
        }

        match db::measured_boot::profile::new(&mut txn, None, &vals4).await {
            Ok(profile4) => {
                let match4_vals = HashMap::from([
                    (String::from("sys_vendor"), String::from("dell")),
                    (String::from("product_name"), String::from("poweredge_r750")),
                    (String::from("bios_version"), String::from("v1")),
                    (String::from("uefi_version"), String::from("2.20")), // this is the significant value
                    (String::from("more_random_attr"), String::from("1.2.3.4")),
                    (String::from("another_thing"), String::from("v1-0.24")),
                ]);
                let match4_result =
                    db::measured_boot::profile::match_from_attrs(&mut txn, &match4_vals).await;
                assert!(match4_result.is_ok());
                let match4 = match4_result.unwrap();
                assert_eq!(profile4.profile_id, match4.unwrap().profile_id);
            }
            Err(e) => return Err(eyre::eyre!("failed to create profile4: {}", e).into()),
        }

        match db::measured_boot::profile::new(&mut txn, None, &vals5).await {
            Ok(profile5) => {
                let match5_vals = HashMap::from([
                    (String::from("sys_vendor"), String::from("nvidia")),
                    (String::from("product_name"), String::from("dgx_h100")),
                    (String::from("uefi_version"), String::from("2.20")),
                    (String::from("more_random_attr"), String::from("1.2.3.4")),
                    (String::from("another_thing"), String::from("v1-0.24")),
                ]);
                let match5_result =
                    db::measured_boot::profile::match_from_attrs(&mut txn, &match5_vals).await;
                assert!(match5_result.is_ok());
                let match5 = match5_result.unwrap();
                assert_eq!(profile5.profile_id, match5.unwrap().profile_id);
            }
            Err(e) => return Err(eyre::eyre!("failed to create profile5: {}", e).into()),
        }

        Ok(())
    }
}
