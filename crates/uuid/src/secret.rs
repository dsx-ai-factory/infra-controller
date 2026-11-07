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

/// SecretFlavor is the marker type for secret UUIDs.
pub struct SecretFlavor;

impl UuidSubtype for SecretFlavor {
    const TYPE_NAME: &'static str = "SecretId";
    const DB_COLUMN_NAME: &'static str = "secret_id";
}

/// SecretId uniquely identifies a row in the secrets table.
pub type SecretId = TypedUuid<SecretFlavor>;

#[cfg(test)]
mod tests {
    use super::*;

    crate::typed_uuid_tests!(SecretId, "SecretId", "secret_id");
}
