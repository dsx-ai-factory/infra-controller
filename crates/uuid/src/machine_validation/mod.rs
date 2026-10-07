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

use crate::typed_uuids::{TypedUuid, UuidSubtype};

/// Marker type for MachineValidationId
pub struct MachineValidationIdMarker;

impl UuidSubtype for MachineValidationIdMarker {
    const TYPE_NAME: &'static str = "MachineValidationId";
}

/// MachineValidationId is a strongly typed UUID for MachineValidations.
pub type MachineValidationId = TypedUuid<MachineValidationIdMarker>;

/// Marker type for MachineValidationRunItemId
pub struct MachineValidationRunItemIdMarker;

impl UuidSubtype for MachineValidationRunItemIdMarker {
    const TYPE_NAME: &'static str = "MachineValidationRunItemId";
}

/// MachineValidationRunItemId is a strongly typed UUID for validation run items.
pub type MachineValidationRunItemId = TypedUuid<MachineValidationRunItemIdMarker>;

/// Marker type for MachineValidationAttemptId
pub struct MachineValidationAttemptIdMarker;

impl UuidSubtype for MachineValidationAttemptIdMarker {
    const TYPE_NAME: &'static str = "MachineValidationAttemptId";
}

/// MachineValidationAttemptId is a strongly typed UUID for validation attempts.
pub type MachineValidationAttemptId = TypedUuid<MachineValidationAttemptIdMarker>;

#[cfg(test)]
mod machine_validation_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    // Run all boilerplate TypedUuid tests for this type, also
    // ensuring TYPE_NAME and DB_COLUMN_NAME test correctly.
    typed_uuid_tests!(MachineValidationId, "MachineValidationId", "id");
}

#[cfg(test)]
mod machine_validation_run_item_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(
        MachineValidationRunItemId,
        "MachineValidationRunItemId",
        "id"
    );
}

#[cfg(test)]
mod machine_validation_attempt_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(
        MachineValidationAttemptId,
        "MachineValidationAttemptId",
        "id"
    );
}
