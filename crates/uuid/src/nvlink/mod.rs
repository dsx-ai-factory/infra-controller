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

/// Marker type for NvLinkPartitionId
pub struct NvLinkPartitionIdMarker;

impl UuidSubtype for NvLinkPartitionIdMarker {
    const TYPE_NAME: &'static str = "NvLinkPartitionId";
}

/// NvLinkPartitionId is a strongly typed UUID specific to an NvLink partition.
pub type NvLinkPartitionId = TypedUuid<NvLinkPartitionIdMarker>;

/// Marker type for NvLinkLogicalPartitionId
pub struct NvLinkLogicalPartitionIdMarker;

impl UuidSubtype for NvLinkLogicalPartitionIdMarker {
    const TYPE_NAME: &'static str = "NvLinkLogicalPartitionId";
}

/// NvLinkLogicalPartitionId is a strongly typed UUID for NvLink logical partitions.
pub type NvLinkLogicalPartitionId = TypedUuid<NvLinkLogicalPartitionIdMarker>;

/// Marker type for NvLinkDomainId
pub struct NvLinkDomainIdMarker;

impl UuidSubtype for NvLinkDomainIdMarker {
    const TYPE_NAME: &'static str = "NvLinkDomainId";
}

/// NvLinkDomainId is a strongly typed UUID for NvLink domains.
pub type NvLinkDomainId = TypedUuid<NvLinkDomainIdMarker>;

#[cfg(test)]
mod nvlink_partition_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(NvLinkPartitionId, "NvLinkPartitionId", "id");
}

#[cfg(test)]
mod nvlink_logical_partition_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(NvLinkLogicalPartitionId, "NvLinkLogicalPartitionId", "id");
}

#[cfg(test)]
mod nvlink_domain_id_tests {
    use super::*;
    use crate::typed_uuid_tests;
    typed_uuid_tests!(NvLinkDomainId, "NvLinkDomainId", "id");
}
