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

/// Marker type for NetworkSegmentId
pub struct NetworkSegmentIdMarker;

impl UuidSubtype for NetworkSegmentIdMarker {
    const TYPE_NAME: &'static str = "NetworkSegmentId";
}

/// NetworkSegmentId is a strongly typed UUID specific to a network
/// segment ID, with trait implementations allowing it to be passed
/// around as a UUID, an RPC UUID, bound to sqlx queries, etc.
pub type NetworkSegmentId = TypedUuid<NetworkSegmentIdMarker>;

/// Marker type for NetworkPrefixId
pub struct NetworkPrefixIdMarker;

impl UuidSubtype for NetworkPrefixIdMarker {
    const TYPE_NAME: &'static str = "NetworkPrefixId";
}

/// NetworkPrefixId is a strongly typed UUID for network prefixes.
pub type NetworkPrefixId = TypedUuid<NetworkPrefixIdMarker>;

#[cfg(test)]
mod network_segment_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(NetworkSegmentId, "NetworkSegmentId", "id");
}

#[cfg(test)]
mod network_prefix_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(NetworkPrefixId, "NetworkPrefixId", "id");
}
