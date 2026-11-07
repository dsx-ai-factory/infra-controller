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

/*!
 *  Code for working the measurement_system_profiles and measurement_system_profiles_attrs
 *  tables in the database, leveraging the profile-specific record types.
 */

use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementSystemProfileId;
use measured_boot::profile::MeasurementSystemProfile;
use measured_boot::records::{MeasurementSystemProfileAttrRecord, MeasurementSystemProfileRecord};
use sqlx::{PgConnection, PgTransaction};

use crate::db_read::DbReader;
use crate::measured_boot::interface::common;
use crate::measured_boot::interface::common::acquire_advisory_txn_lock;
use crate::measured_boot::interface::profile::{
    delete_profile_attr_records_for_id, delete_profile_record_for_id,
    get_all_measurement_profile_records, get_machines_for_profile_id,
    get_measurement_profile_attrs_for_profile_id, get_measurement_profile_record_by_attrs,
    get_measurement_profile_record_by_id, get_measurement_profile_record_by_name,
    insert_measurement_profile_attr_records, insert_measurement_profile_record,
    rename_profile_for_profile_id, rename_profile_for_profile_name,
};
use crate::{DatabaseError, DatabaseResult};

pub async fn new(
    txn: &mut PgTransaction<'_>,
    name: Option<String>,
    attrs: &HashMap<String, String>,
) -> DatabaseResult<MeasurementSystemProfile> {
    let profile_name = match name {
        Some(name) => name,
        None => common::generate_name()?,
    };
    create_measurement_profile(txn, profile_name, attrs).await
}

/// from_info_and_attrs creates a new system profile
/// from the base record and its values.
pub fn from_info_and_attrs(
    info: MeasurementSystemProfileRecord,
    attrs: Vec<MeasurementSystemProfileAttrRecord>,
) -> DatabaseResult<MeasurementSystemProfile> {
    Ok(MeasurementSystemProfile {
        profile_id: info.profile_id,
        name: info.name,
        ts: info.ts,
        attrs,
    })
}

pub async fn match_from_attrs_or_new(
    txn: &mut PgTransaction<'_>,
    attrs: &HashMap<String, String>,
) -> DatabaseResult<MeasurementSystemProfile> {
    match match_profile(txn.as_mut(), attrs).await? {
        Some(profile_id) => Ok(load_from_id(txn, profile_id).await?),
        None => Ok(new(txn, None, attrs).await?),
    }
}

pub async fn load_from_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> DatabaseResult<MeasurementSystemProfile> {
    get_measurement_profile_by_id(txn, profile_id).await
}

////////////////////////////////////////////////////////////////
/// load_from_name loads an existing measurement profile (and its
/// attributes), returning a MeasurementSystemProfile instance.
////////////////////////////////////////////////////////////////
pub async fn load_from_name(
    txn: &mut PgConnection,
    name: String,
) -> DatabaseResult<MeasurementSystemProfile> {
    get_measurement_profile_by_name(txn, name).await
}

////////////////////////////////////////////////////////////////
/// load_from_attrs loads an existing measurement profile (and
/// its attributes), returning a MeasurementSystemProfile instance.
////////////////////////////////////////////////////////////////
pub async fn load_from_attrs(
    txn: &mut PgConnection,
    attrs: &HashMap<String, String>,
) -> DatabaseResult<Option<MeasurementSystemProfile>> {
    let info = get_measurement_profile_record_by_attrs(txn, attrs).await?;
    match info {
        Some(info) => {
            let attrs = get_measurement_profile_attrs_for_profile_id(txn, info.profile_id).await?;
            Ok(Some(MeasurementSystemProfile {
                profile_id: info.profile_id,
                name: info.name,
                ts: info.ts,
                attrs,
            }))
        }
        None => Ok(None),
    }
}

////////////////////////////////////////////////////////////////
/// intersects_with is used to check if the current
/// MeasurementSystemProfile intersects with the provided attrs.
////////////////////////////////////////////////////////////////
pub fn intersects_with(
    profile: &MeasurementSystemProfile,
    machine_attrs: &HashMap<String, String>,
) -> DatabaseResult<bool> {
    if profile.attrs.len() > machine_attrs.len() {
        return Ok(false);
    }
    Ok(profile.attrs.iter().all(|record| {
        if let Some(machine_attr_value) = machine_attrs.get(&record.key) {
            machine_attr_value == &record.value
        } else {
            false
        }
    }))
}

pub async fn delete_for_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> DatabaseResult<Option<MeasurementSystemProfile>> {
    delete_profile_for_id(txn, profile_id).await
}

pub async fn delete_for_name(
    txn: &mut PgConnection,
    name: String,
) -> DatabaseResult<Option<MeasurementSystemProfile>> {
    delete_profile_for_name(txn, name).await
}

/// rename_for_id renames a MeasurementSystemProfile based on its ID.
pub async fn rename_for_id(
    txn: &mut PgConnection,
    system_profile_id: MeasurementSystemProfileId,
    new_system_profile_name: String,
) -> DatabaseResult<MeasurementSystemProfile> {
    from_info_and_attrs(
        rename_profile_for_profile_id(txn, system_profile_id, new_system_profile_name.clone())
            .await?,
        get_measurement_profile_attrs_for_profile_id(txn, system_profile_id).await?,
    )
}

/// rename_for_name renames a MeasurementSystemProfile based on its name.
pub async fn rename_for_name(
    txn: &mut PgConnection,
    system_profile_name: String,
    new_system_profile_name: String,
) -> DatabaseResult<MeasurementSystemProfile> {
    let info = rename_profile_for_profile_name(
        txn,
        system_profile_name.clone(),
        new_system_profile_name.clone(),
    )
    .await?;
    let attrs = get_measurement_profile_attrs_for_profile_id(txn, info.profile_id).await?;
    from_info_and_attrs(info, attrs)
}

pub async fn get_all<DB>(txn: &mut DB) -> DatabaseResult<Vec<MeasurementSystemProfile>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    get_all_profiles(txn).await
}

/// get_machines gets a list of all MachineIds for a given
/// system measurement profile.
pub async fn get_machines(
    profile: &MeasurementSystemProfile,
    txn: &mut PgConnection,
) -> DatabaseResult<Vec<MachineId>> {
    get_machines_for_profile_id(txn, profile.profile_id).await
}

/// match_profile takes a map of k/v pairs and returns a singular matching
/// profile based on the exact k/v pairs and the number of pairs, should
/// one exist.
///
/// The code is written as such to only allow one profile with the same set
/// of attributes, so if two matching profiles end up existing, it's because
/// someone was messing around in the tables (or there's a bug).
async fn match_profile<DB>(
    txn: &mut DB,
    attrs: &HashMap<String, String>,
) -> DatabaseResult<Option<MeasurementSystemProfileId>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    // Get all profiles, and figure out which one intersects
    // with the provided attrs. After that, we'll attempt to find the
    // most specific match (if there are multiple matches).
    let mut all_profiles = get_all_profiles(txn).await?;

    let match_attempts: DatabaseResult<Vec<MeasurementSystemProfile>> = all_profiles
        .drain(..)
        .filter_map(|profile| match intersects_with(&profile, attrs) {
            Ok(true) => Some(Ok(profile)),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        })
        .collect();

    let mut matching = match match_attempts {
        Ok(matched) => matched,
        Err(e) => return Err(e),
    };

    // If there are no matching bundles, or a single matching
    // bundle, it's simple to handle here.
    if matching.is_empty() {
        return Ok(None);
    } else if matching.len() == 1 {
        return Ok(Some(matching[0].profile_id));
    }

    // Otherwise, sort by the number of bundle values
    // in the bundle, and return the most specific bundle
    // match (as in, the most unique values, if there is
    // one). If there's a conflict, then return an error.
    matching.sort_by_key(|b| std::cmp::Reverse(b.attrs.len()));
    if matching[0].attrs.len() == matching[1].attrs.len() {
        return Err(DatabaseError::internal(String::from(
            "cannot determine most specific profile match",
        )));
    }

    Ok(Some(matching[0].profile_id))
}

/// create_measurement_profile creates a new measurement profile
/// and corresponding measurement profile attributes. The transaction
/// is created here, and is used for corresponding insert statements
/// into both the measurement_system_profiles and measurement_system_profiles_attrs
/// tables.
pub async fn create_measurement_profile(
    txn: &mut PgTransaction<'_>,
    profile_name: String,
    attrs: &HashMap<String, String>,
) -> DatabaseResult<MeasurementSystemProfile> {
    // Acquire an advisory lock (automatically released at the end of the txn),
    // whose hash key is generated by the attrs, ensuring that a duplicate
    // profile cannot be created during this time.
    acquire_advisory_txn_lock(txn, &attr_map_to_string(attrs)).await?;

    if let Some(existing) = load_from_attrs(txn, attrs).await? {
        return Err(DatabaseError::AlreadyFoundError {
            kind: "MeasurementSystemProfile",
            id: existing.profile_id.to_string(),
        });
    }

    let info = insert_measurement_profile_record(txn, profile_name.clone())
        .await
        .map_err(|sqlx_err| {
            let is_db_err = sqlx_err.as_database_error();
            match is_db_err {
                Some(db_err) => match db_err.kind() {
                    sqlx::error::ErrorKind::UniqueViolation => DatabaseError::AlreadyFoundError {
                        kind: "MeasurementSystemProfile",
                        id: profile_name.clone(),
                    },
                    sqlx::error::ErrorKind::NotNullViolation => DatabaseError::internal(format!(
                        "system profile missing not null value: {} (msg: {})",
                        profile_name.clone(),
                        db_err
                    )),
                    _ => DatabaseError::new("MeasurementSystemProfile.new db_err", sqlx_err),
                },
                None => DatabaseError::new("MeasurementSystemProfile.new sqlx_err", sqlx_err),
            }
        })?;

    let attrs = insert_measurement_profile_attr_records(txn, info.profile_id, attrs).await?;
    Ok(MeasurementSystemProfile {
        profile_id: info.profile_id,
        name: info.name,
        ts: info.ts,
        attrs,
    })
}

/// get_measurement_profile_by_id returns a MeasurementSystemProfile
/// for the given MeasurementSystemProfileId.
pub async fn get_measurement_profile_by_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> DatabaseResult<MeasurementSystemProfile> {
    match get_measurement_profile_record_by_id(txn, profile_id).await? {
        Some(info) => {
            let attrs = get_measurement_profile_attrs_for_profile_id(txn, info.profile_id).await?;
            Ok(MeasurementSystemProfile {
                profile_id: info.profile_id,
                name: info.name,
                ts: info.ts,
                attrs,
            })
        }
        None => Err(DatabaseError::NotFoundError {
            kind: "MeasurementSystemProfile",
            id: profile_id.to_string(),
        }),
    }
}

/// get_measurement_profile_by_name returns a MeasurementSystemProfile
/// for the given name.
pub async fn get_measurement_profile_by_name(
    txn: &mut PgConnection,
    name: String,
) -> DatabaseResult<MeasurementSystemProfile> {
    match get_measurement_profile_record_by_name(txn, name.clone()).await? {
        Some(info) => {
            let attrs = get_measurement_profile_attrs_for_profile_id(txn, info.profile_id).await?;
            Ok(MeasurementSystemProfile {
                profile_id: info.profile_id,
                name: info.name,
                ts: info.ts,
                attrs,
            })
        }
        None => Err(DatabaseError::NotFoundError {
            kind: "MeasurementSystemProfile",
            id: name.clone(),
        }),
    }
}

/// delete_profile_for_id deletes a complete profile, including
/// its attributes, by ID. It returns the deleted profile for display.
pub async fn delete_profile_for_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> DatabaseResult<Option<MeasurementSystemProfile>> {
    let attrs = delete_profile_attr_records_for_id(txn, profile_id).await?;
    match delete_profile_record_for_id(txn, profile_id).await? {
        Some(info) => Ok(Some(MeasurementSystemProfile {
            name: info.name,
            profile_id: info.profile_id,
            ts: info.ts,
            attrs,
        })),
        None => Ok(None),
    }
}

/// delete_profile_for_name deletes a complete profile, including
/// its attributes, by name. It returns the deleted profile for display.
pub async fn delete_profile_for_name(
    txn: &mut PgConnection,
    name: String,
) -> DatabaseResult<Option<MeasurementSystemProfile>> {
    let profile = load_from_name(txn, name).await?;
    delete_profile_for_id(txn, profile.profile_id).await
}

pub async fn get_all_profiles<DB>(txn: &mut DB) -> DatabaseResult<Vec<MeasurementSystemProfile>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let mut res: Vec<MeasurementSystemProfile> = Vec::new();
    let mut infos = get_all_measurement_profile_records(&mut *txn).await?;
    for info in infos.drain(..) {
        let attrs =
            get_measurement_profile_attrs_for_profile_id(&mut *txn, info.profile_id).await?;
        res.push(MeasurementSystemProfile {
            profile_id: info.profile_id,
            name: info.name,
            ts: info.ts,
            attrs,
        });
    }
    Ok(res)
}

/// attr_map_to_string takes a hashmap of attribute key/val
/// and turns them into a consistent string sorted by key,
/// with k:v, separated by commas.
fn attr_map_to_string(attr_map: &HashMap<String, String>) -> String {
    let mut attr_tuples = attr_map.iter().collect::<Vec<_>>();
    attr_tuples.sort_by_key(|&(key, _)| key);
    attr_tuples
        .into_iter()
        .map(|(key, val)| format!("{key}:{val}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// attr_records_to_string returns a consistent (sorted by key)
/// string of the attributes, paired by a colon, separated by a comma.
pub fn attr_records_to_string(profile: &MeasurementSystemProfile) -> String {
    let attr_map: HashMap<String, String> = profile
        .attrs
        .iter()
        .map(|attr_record| (attr_record.key.clone(), attr_record.value.clone()))
        .collect();
    attr_map_to_string(&attr_map)
}

pub async fn match_from_attrs(
    txn: &mut PgConnection,
    attrs: &HashMap<String, String>,
) -> DatabaseResult<Option<MeasurementSystemProfile>> {
    match match_profile(&mut *txn, attrs).await? {
        Some(info) => Ok(Some(load_from_id(txn, info).await?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::measured_boot::MeasurementSystemProfileAttrId;
    use chrono::Utc;

    use super::*;

    #[test]
    // test_attr_map_to_string makes sure attr_map_to_string works
    // as expected for creating a MeasurementSystemProfile.
    fn test_attr_map_to_string() {
        let mut attr_map = HashMap::new();
        attr_map.insert(String::from("dog"), String::from("woof"));
        attr_map.insert(String::from("emu"), String::from("boofboof"));
        attr_map.insert(String::from("cat"), String::from("meow"));
        let result = attr_map_to_string(&attr_map);
        assert_eq!(result, "cat:meow,dog:woof,emu:boofboof");
    }

    #[test]
    // test_attr_records_to_string makes sure attr_records_to_string works
    // as expected on a MeasurementSystemProfile.
    fn test_attr_records_to_string() {
        let profile = MeasurementSystemProfile {
            profile_id: MeasurementSystemProfileId::from_str(
                "45601234-abcd-1010-3210-2468ea0c0100",
            )
            .unwrap(),
            name: String::from("my_profile"),
            ts: Utc::now(),
            attrs: vec![
                MeasurementSystemProfileAttrRecord {
                    attribute_id: MeasurementSystemProfileAttrId::from_str(
                        "45601234-abcd-1010-3210-2468ea0c0101",
                    )
                    .unwrap(),
                    profile_id: MeasurementSystemProfileId::from_str(
                        "45601234-abcd-1010-3210-2468ea0c0100",
                    )
                    .unwrap(),
                    key: String::from("dog"),
                    value: String::from("woof"),
                    ts: Utc::now(),
                },
                MeasurementSystemProfileAttrRecord {
                    attribute_id: MeasurementSystemProfileAttrId::from_str(
                        "45601234-abcd-1010-3210-2468ea0c0102",
                    )
                    .unwrap(),
                    profile_id: MeasurementSystemProfileId::from_str(
                        "45601234-abcd-1010-3210-2468ea0c0100",
                    )
                    .unwrap(),
                    key: String::from("emu"),
                    value: String::from("boofboof"),
                    ts: Utc::now(),
                },
                MeasurementSystemProfileAttrRecord {
                    attribute_id: MeasurementSystemProfileAttrId::from_str(
                        "45601234-abcd-1010-3210-2468ea0c0103",
                    )
                    .unwrap(),
                    profile_id: MeasurementSystemProfileId::from_str(
                        "45601234-abcd-1010-3210-2468ea0c0100",
                    )
                    .unwrap(),
                    key: String::from("cat"),
                    value: String::from("meow"),
                    ts: Utc::now(),
                },
            ],
        };

        let result = attr_records_to_string(&profile);
        assert_eq!(result, "cat:meow,dog:woof,emu:boofboof");
    }
}
