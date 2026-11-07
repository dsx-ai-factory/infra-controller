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
use carbide_uuid::domain::DomainId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainInfo {
    pub id: DomainId,
    pub zone: String,
    pub kind: String,
    pub serial: u32,
    pub last_check: Option<u32>,
    pub notified_serial: Option<u32>,
    pub masters: Vec<String>,
}

impl From<super::Domain> for DomainInfo {
    fn from(domain: super::Domain) -> Self {
        let soa = domain
            .soa
            .unwrap_or_else(|| super::SoaSnapshot::new(&domain.name));

        DomainInfo {
            id: domain.id,
            zone: domain.name + ".",
            kind: "native".to_string(),
            serial: soa.0.serial,
            last_check: None,
            notified_serial: None,
            masters: vec![],
        }
    }
}
