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
/// For forward queries, `NoData` (the name exists but not the requested record
/// type) and `NxDomain` (the name does not exist) carry the held zone's SOA so
/// resolvers can cache these authoritative negative answers.
///
/// A positive PTR sets the authoritative-answer (AA) bit for the known address.
/// Its `Records::zone` identifies the forward domain containing the target
/// hostname; it is internal ownership metadata, not an advertised reverse zone
/// or a reverse SOA. Knowing one address's PTR does not establish authority over
/// neighbouring addresses. Reverse queries without a supported PTR answer yield
/// `NotAuthoritative`, not a reverse SOA or an authoritative negative answer.
#[derive(Clone, Debug)]
pub enum Answer {
    /// The name has records of the requested type. An empty list is still a
    /// positive answer (NOERROR with no RRs), never a negative; a negative is
    /// always `NoData` or `NxDomain`.
    Records {
        /// The owning forward domain for PTRs, otherwise the answering zone.
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
    /// No supported authority supplies an answer. Includes reverse queries for
    /// types other than PTR and PTR queries without an unambiguous published
    /// record. Never NXDOMAIN, because the name may exist elsewhere.
    NotAuthoritative,
}

impl Answer {
    /// Whether the AA bit is set: true for published records and held-zone negatives.
    pub fn is_authoritative(&self) -> bool {
        !matches!(self, Self::NotAuthoritative)
    }
}
