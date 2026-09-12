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

use dns_record::SoaRecord;

use super::resource_record::ResourceRecord;
use super::zone::Fqdn;

/// The classified result of one DNS question, before wire mapping.
///
/// The three authoritative variants name the zone they were answered from so
/// the zone SOA can go in the authority section.
#[derive(Clone, Debug)]
pub enum Answer {
    /// The name has records of the requested type. An empty list is still a
    /// positive answer (NOERROR with no RRs), never a negative; a negative is
    /// always `NoData` or `NxDomain`.
    Records {
        /// The zone the records were answered from.
        zone: Fqdn,
        /// Records of the requested type at the name.
        records: Vec<ResourceRecord>,
    },
    /// The name exists in a held zone but has no records of the requested type
    /// (RFC 2308 §2.2).
    NoData {
        /// The zone the name is in.
        zone: Fqdn,
        /// That zone's SOA, for the authority section.
        soa: SoaRecord,
    },
    /// The name is inside a held zone and nothing exists at or below it
    /// (RFC 2308 §2.1).
    NxDomain {
        /// The zone the name would be in.
        zone: Fqdn,
        /// That zone's SOA, for the authority section.
        soa: SoaRecord,
    },
    /// No held zone contains the name. Never NXDOMAIN, because the name may
    /// exist in a zone someone else serves.
    NotAuthoritative,
}

impl Answer {
    /// Whether the AA bit is set: true for every answer given from a held zone.
    pub fn is_authoritative(&self) -> bool {
        !matches!(self, Self::NotAuthoritative)
    }
}
