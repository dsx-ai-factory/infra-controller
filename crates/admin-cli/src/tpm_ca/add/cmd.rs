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

use std::fs::File;
use std::io::Read;
use std::path::Path;

use x509_parser::certificate::X509Certificate;
use x509_parser::pem::parse_x509_pem;
use x509_parser::prelude::FromDer;
use x509_parser::validate::*;

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn add_filename(filename: &str, api_client: &ApiClient) -> CarbideCliResult<()> {
    let filepath = Path::new(filename);
    let is_pem = filepath.with_extension("pem").is_file();
    let is_der =
        filepath.with_extension("cer").is_file() || filepath.with_extension("der").is_file();

    if !is_der && !is_pem {
        return Err(CarbideCliError::GenericError(
            "The certificate must exist and be with PEM or CER or DER extension".to_string(),
        ));
    }

    add_individual(filepath, is_pem, api_client).await
}

pub(crate) async fn add_individual(
    filepath: &Path,
    is_pem: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    println!("Adding CA Certificate {0}", filepath.to_string_lossy());
    let mut ca_file = File::open(filepath).map_err(CarbideCliError::IOError)?;

    let mut ca_file_bytes: Vec<u8> = Vec::new();
    ca_file
        .read_to_end(&mut ca_file_bytes)
        .map_err(CarbideCliError::IOError)?;

    let ca_file_bytes_der;
    if is_pem {
        // convert pem to der to normalize
        let res = parse_x509_pem(&ca_file_bytes);
        match res {
            Ok((rem, pem)) => {
                if !rem.is_empty() && (pem.label != *"CERTIFICATE") {
                    return Err(CarbideCliError::GenericError(
                        "PEM certificate validation failed".to_string(),
                    ));
                }

                ca_file_bytes_der = pem.contents;
            }
            _ => {
                return Err(CarbideCliError::GenericError(
                    "Could not parse PEM certificate".to_string(),
                ));
            }
        }
    } else {
        ca_file_bytes_der = ca_file_bytes;
    }

    validate_ca_cert(&ca_file_bytes_der)?;

    let ca_cert_id_response = api_client.0.tpm_add_ca_cert(ca_file_bytes_der).await?;

    println!(
        "Successfully added CA Certificate {0} with id {1}. {2} EK certs have been matched",
        filepath.to_string_lossy(),
        ca_cert_id_response
            .id
            .map(|v| v.ca_cert_id.to_string())
            .unwrap_or("*CA ID has not been returned*".to_string()),
        ca_cert_id_response.matched_ek_certs
    );

    Ok(())
}

fn validate_ca_cert(ca_cert_bytes: &[u8]) -> CarbideCliResult<()> {
    let ca_cert = X509Certificate::from_der(ca_cert_bytes)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))?
        .1;

    let mut logger = VecLogger::default();

    if !X509StructureValidator.validate(&ca_cert, &mut logger) {
        return Err(CarbideCliError::GenericError(
            "Validation Error".to_string(),
        ));
    }

    Ok(())
}
