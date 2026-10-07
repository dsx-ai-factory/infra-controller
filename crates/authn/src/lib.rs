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
pub mod config;
pub mod middleware;
pub mod spiffe_id;

#[derive(thiserror::Error, Debug, Clone)]
pub enum SpiffeError {
    #[error("SPIFFE validation error: {0}")]
    Validation(#[from] crate::SpiffeValidationError),

    #[error("unrecognized SPIFFE ID: {0}")]
    Recognition(#[from] crate::SpiffeContextError),
}

use std::collections::HashSet;

use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

// Validate an X.509 DER certificate against the SPIFFE requirements, and
// return a SPIFFE ID.
//
// https://github.com/spiffe/spiffe/blob/main/standards/X509-SVID.md#5-validation
//
// Note that this only implements the SPIFFE-specific validation steps. We
// assume the X.509 certificate has already been validated to a trusted root.
pub fn validate_x509_certificate(
    der_certificate: &[u8],
) -> Result<spiffe_id::SpiffeId, SpiffeValidationError> {
    use SpiffeValidationError::ValidationError;

    let (_remainder, x509_cert) = X509Certificate::from_der(der_certificate)
        .map_err(|e| ValidationError(format!("X.509 parse error: {e}")))?;

    // Verify that this is a leaf certificate (i.e. it is not a CA certificate)
    let is_ca_cert = match x509_cert.basic_constraints() {
        Ok(None) => Ok(false),
        Ok(Some(basic_constraints)) => Ok(basic_constraints.value.ca),
        Err(_) => Err(ValidationError(
            "More than one X.509 Basic Constraints extension was found".into(),
        )),
    }?;
    if is_ca_cert {
        return Err(ValidationError(
            "The X.509 certificate must be a leaf certificate (it must \
                not have CA=true in the Basic Constraints extension)"
                .into(),
        ));
    };

    // Verify that keyCertSign and cRLSign are not set in the Key Usage
    // extension (if any).
    if let Some(key_usage) = x509_cert
        .key_usage()
        .map_err(|_e| ValidationError("More than one X.509 Key Usage extension was found".into()))?
    {
        if key_usage.value.key_cert_sign() {
            return Err(ValidationError(
                "keyCertSign must not be set in the X.509 Key Usage extension".into(),
            ));
        }
        if key_usage.value.crl_sign() {
            return Err(ValidationError(
                "cRLSign must not be set in the X.509 Key Usage extension".into(),
            ));
        }
    };

    let subj_alt_name = x509_cert.subject_alternative_name().map_err(|_e| {
        ValidationError("Multiple X.509 Subject Alternative Name extensions found".into())
    })?;
    let subj_alt_name = subj_alt_name.ok_or_else(|| {
        ValidationError("No X.509 Subject Alternative Name extension found".into())
    })?;

    // Verify there is exactly one SAN URI
    let uris = subj_alt_name
        .value
        .general_names
        .iter()
        .cloned()
        .filter_map(|n| match n {
            GeneralName::URI(uri) => Some(uri),
            _ => None,
        })
        .collect::<Vec<_>>();
    let uri = match (uris.len(), uris.first()) {
        (1, Some(uri)) => Ok(uri),
        (n, _) => Err(ValidationError(format!(
            "The X.509 Subject Alternative Name extension must contain exactly \
                1 URI (found {n})"
        ))),
    }?;

    let spiffe_id = spiffe_id::SpiffeId::new(uri)
        .map_err(|e| ValidationError(format!("Couldn't parse SPIFFE ID: {e}")))?;
    Ok(spiffe_id)
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum SpiffeValidationError {
    #[error("SPIFFE validation error: {0}")]
    ValidationError(String),
}

#[derive(Debug)]
pub enum SpiffeIdClass {
    Service(String),
    Machine(String),
}

impl SpiffeIdClass {
    fn identifier(&self) -> &str {
        let identifier = match self {
            SpiffeIdClass::Service(identifier) => identifier,
            SpiffeIdClass::Machine(identifier) => identifier,
        };
        identifier.as_str()
    }
}

pub struct SpiffeContext {
    pub trust_domain: spiffe_id::TrustDomain,
    pub service_base_paths: Vec<String>,
    pub machine_base_path: String,
    pub additional_issuer_cns: HashSet<String>,
}

impl SpiffeContext {
    pub fn extract_service_identifier(
        &self,
        spiffe_id: &spiffe_id::SpiffeId,
    ) -> Result<SpiffeIdClass, SpiffeContextError> {
        use SpiffeContextError::*;

        if !spiffe_id.is_member_of(&self.trust_domain) {
            let id_trust_domain = spiffe_id.trust_domain().id_string();
            let expected_trust_domain = self.trust_domain.id_string();
            return Err(ContextError(format!(
                "Found a trust domain {id_trust_domain} which is not a \
                    member of the configured trust domain \
                    {expected_trust_domain}"
            )));
        };
        let spiffe_id_path = spiffe_id.path();
        let maybe_service = self
            .service_base_paths
            .iter()
            .find_map(|service_base_path| {
                spiffe_id_path
                    .strip_prefix(service_base_path.as_str())
                    .map(|i| SpiffeIdClass::Service(i.into()))
            });
        let maybe_machine = spiffe_id_path
            .strip_prefix(self.machine_base_path.as_str())
            .map(|i| SpiffeIdClass::Machine(i.into()));
        let maybe_identifier = maybe_service.or(maybe_machine);
        match maybe_identifier {
            Some(identifier) if !identifier.identifier().is_empty() => Ok(identifier),
            Some(_empty_identifier) => Err(ContextError(
                "The service identifier was empty after removing the base prefix".into(),
            )),
            None => Err(ContextError(format!(
                "The SPIFFE ID path \"{spiffe_id_path}\" does not begin \
                        with a recognized prefix (one of {:?} or {})",
                self.service_base_paths, self.machine_base_path,
            ))),
        }
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum SpiffeContextError {
    #[error("{0}")]
    ContextError(String),
}
