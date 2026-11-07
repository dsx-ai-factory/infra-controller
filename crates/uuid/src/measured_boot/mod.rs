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
 *  Code for defining primary/foreign keys used by the measured boot
 *  database tables.
 *
 *  The idea here is to make it very obvious which type of UUID is being
 *  worked with, since it would be otherwise easy to pass the wrong UUID
 *  to the wrong part of a query. Being able to type the specific ID ends
 *  up catching a lot of potential bugs.
 */

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
#[cfg(feature = "sqlx")]
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    postgres::PgTypeInfo,
    {Database, Postgres},
};

use crate::UuidConversionError;
use crate::machine::MachineId;
use crate::typed_uuids::{TypedUuid, UuidSubtype};

// ============================================================================
// TrustedMachineId
//
// TODO(chet): Consider having a HardwareUuid type that things like MachineId,
// TrustedMachineId, RackId, etc, can all use).
// ============================================================================

/// TrustedMachineId is a special adaptation of a
/// Carbide MachineId, which has support for being
/// expressed as a machine ID, or "*", for the purpose
/// of doing trusted machine approvals for measured
/// boot.
///
/// This makes it so you can provide "*" as an input,
/// as well as read it back into a bound instance, for
/// the admin CLI, API calls, and backend.
///
/// It includes all of the necessary trait implementations
/// to allow it to be used as a clap argument, sqlx binding,
/// etc.
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub enum TrustedMachineId {
    MachineId(MachineId),
    Any,
}

impl FromStr for TrustedMachineId {
    type Err = UuidConversionError;

    fn from_str(input: &str) -> Result<Self, UuidConversionError> {
        if input == "*" {
            Ok(Self::Any)
        } else {
            Ok(Self::MachineId(MachineId::from_str(input).map_err(
                |_| UuidConversionError::InvalidMachineId(input.to_string()),
            )?))
        }
    }
}

impl fmt::Display for TrustedMachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            Self::Any => write!(f, "*"),
            Self::MachineId(machine_id) => write!(f, "{machine_id}"),
        }
    }
}

// Make TrustedMachineId bindable directly into a sqlx query.
// Similar code exists for other IDs, including MachineId.
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for TrustedMachineId {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for TrustedMachineId {
    fn type_info() -> PgTypeInfo {
        <&str as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <&str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl crate::DbPrimaryUuid for TrustedMachineId {
    fn db_primary_uuid_name() -> &'static str {
        "machine_id"
    }
}

#[cfg(test)]
mod trusted_machine_id_tests {
    use std::str::FromStr;

    use super::*;
    use crate::DbPrimaryUuid;

    // TrustedMachineId is a special enum type, not a TypedUuid.
    #[test]
    fn test_trusted_machine_id_any() {
        let id = TrustedMachineId::from_str("*").expect("failed to parse");
        assert_eq!(id, TrustedMachineId::Any);
        assert_eq!(id.to_string(), "*");
    }

    #[test]
    fn test_trusted_machine_id_db_column_name() {
        assert_eq!(TrustedMachineId::db_primary_uuid_name(), "machine_id");
    }
}
// ============================================================================
// MeasurementSystemProfileId
// ============================================================================

/// Marker type for MeasurementSystemProfileId.
pub struct MeasurementSystemProfileIdMarker;

impl UuidSubtype for MeasurementSystemProfileIdMarker {
    const TYPE_NAME: &'static str = "MeasurementSystemProfileId";
    const DB_COLUMN_NAME: &'static str = "profile_id";
}

/// Primary key for a measurement_system_profiles table entry, which is the table
/// containing general metadata about a machine profile.
pub type MeasurementSystemProfileId = TypedUuid<MeasurementSystemProfileIdMarker>;

#[cfg(test)]
mod system_profile_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(
        MeasurementSystemProfileId,
        "MeasurementSystemProfileId",
        "profile_id"
    );
}

// ============================================================================
// MeasurementSystemProfileAttrId
// ============================================================================

/// Marker type for MeasurementSystemProfileAttrId.
pub struct MeasurementSystemProfileAttrIdMarker;

impl UuidSubtype for MeasurementSystemProfileAttrIdMarker {
    const TYPE_NAME: &'static str = "MeasurementSystemProfileAttrId";
}

/// Primary key for a measurement_system_profiles_attrs table entry, which is
/// the table containing the attributes used to map machines to profiles.
pub type MeasurementSystemProfileAttrId = TypedUuid<MeasurementSystemProfileAttrIdMarker>;

#[cfg(test)]
mod system_profile_attr_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(
        MeasurementSystemProfileAttrId,
        "MeasurementSystemProfileAttrId",
        "id"
    );
}

// ============================================================================
// MeasurementBundleId
// ============================================================================

/// Marker type for MeasurementBundleId.
pub struct MeasurementBundleIdMarker;

impl UuidSubtype for MeasurementBundleIdMarker {
    const TYPE_NAME: &'static str = "MeasurementBundleId";
    const DB_COLUMN_NAME: &'static str = "bundle_id";
}

/// Primary key for a measurement_bundles table entry, where a bundle is
/// a collection of measurements that come from the measurement_bundles table.
pub type MeasurementBundleId = TypedUuid<MeasurementBundleIdMarker>;

#[cfg(test)]
mod bundle_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(MeasurementBundleId, "MeasurementBundleId", "bundle_id");
}

// ============================================================================
// MeasurementBundleValueId
// ============================================================================

/// Marker type for MeasurementBundleValueId.
pub struct MeasurementBundleValueIdMarker;

impl UuidSubtype for MeasurementBundleValueIdMarker {
    const TYPE_NAME: &'static str = "MeasurementBundleValueId";
}

/// Primary key for a measurement_bundles_values table entry, where a value is
/// a single measurement that is part of a measurement bundle.
pub type MeasurementBundleValueId = TypedUuid<MeasurementBundleValueIdMarker>;

#[cfg(test)]
mod bundle_value_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(MeasurementBundleValueId, "MeasurementBundleValueId", "id");
}

// ============================================================================
// MeasurementReportId
// ============================================================================

/// Marker type for MeasurementReportId.
pub struct MeasurementReportIdMarker;

impl UuidSubtype for MeasurementReportIdMarker {
    const TYPE_NAME: &'static str = "MeasurementReportId";
    const DB_COLUMN_NAME: &'static str = "report_id";
}

/// Primary key for a measurement_reports table entry, which contains reports
/// of all reported measurement bundles for a given machine.
pub type MeasurementReportId = TypedUuid<MeasurementReportIdMarker>;

#[cfg(test)]
mod report_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(MeasurementReportId, "MeasurementReportId", "report_id");
}

// ============================================================================
// MeasurementReportValueId
// ============================================================================

/// Marker type for MeasurementReportValueId.
pub struct MeasurementReportValueIdMarker;

impl UuidSubtype for MeasurementReportValueIdMarker {
    const TYPE_NAME: &'static str = "MeasurementReportValueId";
}

/// Primary key for a measurement_reports_values table entry, which is the
/// backing values reported for each report into measurement_reports.
pub type MeasurementReportValueId = TypedUuid<MeasurementReportValueIdMarker>;

#[cfg(test)]
mod report_value_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(MeasurementReportValueId, "MeasurementReportValueId", "id");
}

// ============================================================================
// MeasurementJournalId
// ============================================================================

/// Marker type for MeasurementJournalId.
pub struct MeasurementJournalIdMarker;

impl UuidSubtype for MeasurementJournalIdMarker {
    const TYPE_NAME: &'static str = "MeasurementJournalId";
    const DB_COLUMN_NAME: &'static str = "journal_id";
}

/// Primary key for a measurement_journal table entry, which is the journal
/// of all reported measurement bundles for a given machine.
pub type MeasurementJournalId = TypedUuid<MeasurementJournalIdMarker>;

#[cfg(test)]
mod journal_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(MeasurementJournalId, "MeasurementJournalId", "journal_id");
}

// ============================================================================
// MeasurementApprovedMachineId
// ============================================================================

/// Marker type for MeasurementApprovedMachineId.
pub struct MeasurementApprovedMachineIdMarker;

impl UuidSubtype for MeasurementApprovedMachineIdMarker {
    const TYPE_NAME: &'static str = "MeasurementApprovedMachineId";
    const DB_COLUMN_NAME: &'static str = "approval_id";
}

/// Primary key for a measurement_approved_machines table entry, which is how
/// control is enabled at the site-level for auto-approving machine reports
/// into golden measurement bundles.
pub type MeasurementApprovedMachineId = TypedUuid<MeasurementApprovedMachineIdMarker>;

#[cfg(test)]
mod approved_machine_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(
        MeasurementApprovedMachineId,
        "MeasurementApprovedMachineId",
        "approval_id"
    );
}

// ============================================================================
// MeasurementApprovedProfileId
// ============================================================================

/// Marker type for MeasurementApprovedProfileId.
pub struct MeasurementApprovedProfileIdMarker;

impl UuidSubtype for MeasurementApprovedProfileIdMarker {
    const TYPE_NAME: &'static str = "MeasurementApprovedProfileId";
    const DB_COLUMN_NAME: &'static str = "approval_id";
}

/// Primary key for a measurement_approved_profiles table entry, which is how
/// control is enabled at the site-level for auto-approving machine reports
/// for a specific profile into golden measurement bundles.
pub type MeasurementApprovedProfileId = TypedUuid<MeasurementApprovedProfileIdMarker>;

#[cfg(test)]
mod approved_profile_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(
        MeasurementApprovedProfileId,
        "MeasurementApprovedProfileId",
        "approval_id"
    );
}
