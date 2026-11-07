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

// Define the sqlx_fixture_from_str function, which is used by the `#[crate::sqlx_test]` proc-macro.
// Use a macro to avoid having to paste every filename in 3 places.
#[macro_export]
macro_rules! define_sqlx_fixtures {
    ( $($file:expr),* $(,)? ) => {
        pub(crate) fn sqlx_fixture_from_str(s: &str) -> sqlx::testing::TestFixture {
            match s {
                $(
                    $file => sqlx::testing::TestFixture {
                        path: concat!("../fixtures/", $file, ".sql"),
                        contents: include_str!(concat!("../fixtures/", $file, ".sql")),
                    },
                )*
                _ => panic!("Invalid fixture name"),
            }
        }
    };
}

define_sqlx_fixtures!(
    "create_cred_pub_key",
    "create_cred_pub_key_invalid",
    "create_dpu_remediation",
    "create_machine_validation_tests",
    "create_tpm_ca_cert_invalid",
    "create_tpm_ca_wrong_cert",
    "create_sku",
);
