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

/// Where we bake the root CA in our containers
pub const ROOT_CA: &str = "/opt/forge/forge_root.pem";

pub fn default_root_ca() -> &'static str {
    ROOT_CA
}

/// Where we write the client cert in our clients
pub const CLIENT_CERT: &str = "/opt/forge/machine_cert.pem";

pub fn default_client_cert() -> &'static str {
    CLIENT_CERT
}

/// Where we write the client key in our clients
pub const CLIENT_KEY: &str = "/opt/forge/machine_cert.key";

pub fn default_client_key() -> &'static str {
    CLIENT_KEY
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn returns_baked_in_default_paths() {
        value_scenarios!(
            run = |getter: fn() -> &'static str| getter();
            "defaults" {
                default_root_ca as fn() -> &'static str => ROOT_CA,
                default_client_cert as fn() -> &'static str => CLIENT_CERT,
                default_client_key as fn() -> &'static str => CLIENT_KEY,
            }
        );
    }
}
