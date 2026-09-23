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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod answer;
pub mod arpa;
pub mod domain_info;
pub mod metadata;
pub mod resource_record;
pub mod snapshot;
pub mod zone;

pub use answer::Answer;
pub use arpa::{arpa_qname_to_ip, arpa_qname_to_prefix};
pub use domain_info::DomainInfo;
pub use metadata::DomainMetadata;
pub use resource_record::ResourceRecord;
pub use snapshot::SoaSnapshot;
pub use zone::{Fqdn, NameError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Domain {
    pub id: carbide_uuid::domain::DomainId,
    pub name: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub soa: Option<SoaSnapshot>,
    pub metadata: Option<DomainMetadata>,
}

impl Domain {
    /// Increments the SOA serial number for this domain.
    /// This should be called before updating the domain in the database
    /// to ensure DNS changes are properly versioned.
    pub fn increment_serial(&mut self) {
        if let Some(ref mut soa_snapshot) = self.soa {
            soa_snapshot.0.increment_serial();
        }
    }

    /// Creates a new SOA record if one doesn't exist, or increments the existing one.
    pub fn ensure_soa_and_increment(&mut self) {
        if self.soa.is_none() {
            self.soa = Some(SoaSnapshot::new(&self.name));
        }
        self.increment_serial();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewDomain {
    pub name: String,
    pub soa: Option<SoaSnapshot>,
}

impl NewDomain {
    /// Creates a new domain with a default SOA record.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            soa: Some(SoaSnapshot::new(&name)),
            name,
        }
    }
}
