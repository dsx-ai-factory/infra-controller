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

/// Marker type for VpcId
pub struct VpcIdMarker;

impl UuidSubtype for VpcIdMarker {
    const TYPE_NAME: &'static str = "VpcId";
}

/// VpcId is a strongly typed UUID specific to a VPC ID, with
/// trait implementations allowing it to be passed around as
/// a UUID, an RPC UUID, bound to sqlx queries, etc.
pub type VpcId = TypedUuid<VpcIdMarker>;

/// Marker type for VpcPrefixId
pub struct VpcPrefixMarker;

impl UuidSubtype for VpcPrefixMarker {
    const TYPE_NAME: &'static str = "VpcPrefixId";
}

pub type VpcPrefixId = TypedUuid<VpcPrefixMarker>;

#[cfg(test)]
mod vpc_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(VpcId, "VpcId", "id");
}

#[cfg(test)]
mod vpc_prefix_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(VpcPrefixId, "VpcPrefixId", "id");
}
