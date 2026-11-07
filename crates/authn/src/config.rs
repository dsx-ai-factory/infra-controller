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

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::SpiffeContext;
use crate::spiffe_id::{SpiffeIdError, TrustDomain};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    /// The SPIFFE trust domain which client certs must adhere to
    pub spiffe_trust_domain: String,
    /// Allowed base paths for valid client cert spiffe:// URIs for services
    pub spiffe_service_base_paths: Vec<String>,
    /// Allowed base path for client cert spiffe:// URIs for machines
    pub spiffe_machine_base_path: String,
    /// Additional issuer CN's to trust other than the SPIFFE issuer, useful for external user certs.
    pub additional_issuer_cns: Vec<String>,
}

impl TryFrom<TrustConfig> for SpiffeContext {
    type Error = SpiffeIdError;

    fn try_from(value: TrustConfig) -> Result<Self, Self::Error> {
        Ok(crate::SpiffeContext {
            trust_domain: TrustDomain::new(&value.spiffe_trust_domain)?,
            service_base_paths: value.spiffe_service_base_paths,
            machine_base_path: value.spiffe_machine_base_path,
            additional_issuer_cns: value.additional_issuer_cns.into_iter().collect(),
        })
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Deserialize, Serialize)]
pub enum CertComponent {
    IssuerO,
    IssuerOU,
    IssuerCN,
    SubjectO,
    SubjectOU,
    SubjectCN,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AllowedCertCriteria {
    /// These components of the cert must equal the given values to be approved
    pub required_equals: HashMap<CertComponent, String>,
    /// Use this cert component to specify the group it should be reported as
    pub group_from: Option<CertComponent>,
    /// Use this cert component to pick the username
    pub username_from: Option<CertComponent>,
    /// If not using username_from, specify the username used for all certs of this type
    pub username: Option<String>,
}
