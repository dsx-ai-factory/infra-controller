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

//! Shared configuration for the combined integration test executable.

use std::time::Duration;

use axum::http::StatusCode;
use bmc_explorer::ErrorClass;
use bmc_mock::test_support::TestBmc;
use bmc_mock::test_support::axum_http_client::Error as TestBmcError;

pub(crate) fn error_classifier(err: &<TestBmc as nv_redfish::Bmc>::Error) -> Option<ErrorClass> {
    match err {
        TestBmcError::InvalidResponse { status, .. } => match *status {
            StatusCode::NOT_FOUND => Some(ErrorClass::NotFound),
            StatusCode::INTERNAL_SERVER_ERROR => Some(ErrorClass::InternalServerError),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn explorer_config() -> bmc_explorer::Config<'static, TestBmc> {
    bmc_explorer::Config {
        boot_interface_mac: None,
        error_classifier: &error_classifier,
        retry_timeout: Duration::from_millis(0),
    }
}
