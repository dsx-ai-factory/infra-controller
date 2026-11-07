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

use std::collections as stdcol;

use jsonwebtoken as jst;
use serde_json as sj;

use crate::{NrasError, ProcessedAttestationOutcome, RawAttestationOutcome};

const OVERALL_ATT_RESULT_CLAIM: &str = "x-nvidia-overall-att-result";

pub struct Parser {
    pub validate_jwt_expiry: bool,
}

impl Parser {
    pub fn new_with_config(config: &crate::Config) -> Parser {
        Parser {
            validate_jwt_expiry: config.validate_jwt_expiry,
        }
    }

    pub fn parse_attestation_outcome(
        &self,
        raw_outcome: &RawAttestationOutcome,
        decoding_key_store: &impl crate::keystore::KeyStore,
    ) -> Result<ProcessedAttestationOutcome, NrasError> {
        // parse the "JWT" and extract submods and overall attestation result
        // for every submod, turn it into a hash map
        let (attestation_passed, submods) =
            self.extract_overall_result(&raw_outcome.overall_outcome.1, decoding_key_store)?;

        let mut processed_devices_outcome =
            stdcol::HashMap::<String, stdcol::HashMap<String, String>>::new();

        for submod in submods {
            if let Some((device_name, jwt)) =
                raw_outcome.devices_outcome.iter().find(|e| e.0 == &submod)
            {
                let entry = self.extract_device_result(jwt, decoding_key_store)?;
                processed_devices_outcome.insert(device_name.clone(), entry);
            } else {
                return Err(NrasError::ParsingVerifierResponse(format!(
                    "submod for device {} not found",
                    submod
                )));
            }
        }

        Ok(ProcessedAttestationOutcome {
            attestation_passed,
            devices: processed_devices_outcome,
        })
    }

    fn extract_overall_result(
        &self,
        jwt_token: &str,
        decoding_key_store: &impl crate::keystore::KeyStore,
    ) -> Result<(bool, stdcol::HashSet<String>), NrasError> {
        // parse into serde_json::Value representation of claims
        let parsed_json = self.parse_and_validate_jwt(jwt_token, decoding_key_store)?;

        // now extract the overall attestation result
        let att_result = match parsed_json.get(OVERALL_ATT_RESULT_CLAIM) {
            Some(claim) => match claim {
                sj::Value::Bool(v) => v,
                _ => {
                    return Err(NrasError::Jwt(
                        OVERALL_ATT_RESULT_CLAIM.to_owned() + " claim is not bool",
                    ));
                }
            },
            None => {
                return Err(NrasError::Jwt(
                    OVERALL_ATT_RESULT_CLAIM.to_owned() + " claim not found",
                ));
            }
        };

        // now extract the list of submods (devices)
        let submods: stdcol::HashSet<String> = match parsed_json.get("submods") {
            Some(claim) => match claim {
                sj::Value::Object(map) => map.iter().map(|elem| elem.0.clone()).collect(),
                _ => return Err(NrasError::Jwt("submods claim is not map".to_string())),
            },
            None => return Err(NrasError::Jwt("submods claim not found".to_string())),
        };

        Ok((*att_result, submods))
    }

    fn extract_device_result(
        &self,
        jwt_token: &str,
        decoding_key_store: &impl crate::keystore::KeyStore,
    ) -> Result<stdcol::HashMap<String, String>, NrasError> {
        // parse into serde_json::Value representation of claims
        let parsed_json = self.parse_and_validate_jwt(jwt_token, decoding_key_store)?;

        // now extract the attestation claims for this specific device
        let devices_claims = match parsed_json {
            sj::Value::Object(map) => map
                .iter()
                .map(|claim| {
                    (
                        claim.0.clone(),
                        match claim.1 {
                            sj::Value::String(s) => s.clone(),
                            other => other.to_string(), // convert value to string regardless
                        },
                    )
                })
                .collect(),
            _ => return Err(NrasError::Jwt("devices claims is not map".to_string())),
        };

        Ok(devices_claims)
    }

    fn parse_and_validate_jwt(
        &self,
        jwt_token: &str,
        decoding_key_store: &impl crate::keystore::KeyStore,
    ) -> Result<sj::Value, NrasError> {
        // decode header and obtain validation algorithm
        let jwt_header = jst::decode_header(jwt_token)
            .map_err(|e| NrasError::Jwt(format!("Error decoding header: {}", e)))?;

        let kid = jwt_header
            .kid
            .ok_or(NrasError::Jwt("No kid found in JWT header".to_string()))?;

        let decoding_key =
            decoding_key_store
                .find_key(&kid)
                .ok_or(NrasError::DecodingKeyNotFound(format!(
                    "DecodingKey not found for kid - {}",
                    kid
                )))?;

        let mut validation = jst::Validation::new(jwt_header.alg);
        validation.validate_exp = self.validate_jwt_expiry;

        // now decode the payload and perform the validation using the jwt_pub_key
        let jwt_token_data: jst::TokenData<sj::Value> =
            jst::decode(jwt_token, &decoding_key, &validation)
                .map_err(|e| NrasError::Jwt(format!("Error decoding JWT token: {}", e)))?;

        Ok(jwt_token_data.claims)
    }
}
