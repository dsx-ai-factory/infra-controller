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

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::SecretsError;
use crate::certificates::{Certificate, CertificateProvider};

#[derive(Debug, Default)]
pub struct TestCertificateProvider {
    pub certificates: Mutex<HashMap<String, Certificate>>,
}

impl TestCertificateProvider {
    pub fn new() -> Self {
        Self {
            certificates: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl CertificateProvider for TestCertificateProvider {
    async fn get_certificate(
        &self,
        unique_identifier: &str,
        _alt_names: Option<String>,
        _ttl: Option<String>,
    ) -> Result<Certificate, SecretsError> {
        let mut certificates = self.certificates.lock().await;
        let certificate = certificates
            .entry(unique_identifier.to_string())
            .or_insert(Certificate::default());

        Ok(certificate.clone())
    }
}
