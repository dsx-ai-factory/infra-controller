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

//! Strongly typed identities for external-validation holds, cycles, and requests.

use crate::typed_uuids::{TypedUuid, UuidSubtype};

/// Marker for a strongly typed external-validation hold UUID.
pub struct ExternalValidationHoldIdMarker;

impl UuidSubtype for ExternalValidationHoldIdMarker {
    const TYPE_NAME: &'static str = "ExternalValidationHoldId";
}

/// Identifies one external-validation hold; distinct from other external-validation identities.
pub type ExternalValidationHoldId = TypedUuid<ExternalValidationHoldIdMarker>;

/// Marker for a strongly typed external-validation lifecycle cycle UUID.
pub struct ExternalValidationCycleIdMarker;

impl UuidSubtype for ExternalValidationCycleIdMarker {
    const TYPE_NAME: &'static str = "ExternalValidationCycleId";
}

/// Identifies one external-validation lifecycle cycle; distinct from other external-validation identities.
pub type ExternalValidationCycleId = TypedUuid<ExternalValidationCycleIdMarker>;

/// Marker for a strongly typed external-validation attempt request UUID.
pub struct ExternalValidationRequestIdMarker;

impl UuidSubtype for ExternalValidationRequestIdMarker {
    const TYPE_NAME: &'static str = "ExternalValidationRequestId";
}

/// Identifies one external-validation attempt request; distinct from other external-validation identities.
pub type ExternalValidationRequestId = TypedUuid<ExternalValidationRequestIdMarker>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed_uuid_tests;

    mod external_validation_hold_id {
        use super::*;
        typed_uuid_tests!(ExternalValidationHoldId, "ExternalValidationHoldId", "id");
    }

    mod external_validation_cycle_id {
        use super::*;
        typed_uuid_tests!(ExternalValidationCycleId, "ExternalValidationCycleId", "id");
    }

    mod external_validation_request_id {
        use super::*;
        typed_uuid_tests!(
            ExternalValidationRequestId,
            "ExternalValidationRequestId",
            "id"
        );
    }
}
