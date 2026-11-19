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

use carbide_tls::client_config::ClientCert;
use rpc::carbide_tls_client::CarbideClientConfig;

use crate::CONFIG;

pub(super) fn build_carbide_client_config() -> CarbideClientConfig {
    let forge_root_ca_path = &CONFIG
        .read()
        .unwrap() // safety: the only way this will panic is if the lock is poisoned,
        // which happens when another holder panics. we're already done at that point.
        .forge_root_ca_path;
    let forge_client_key_path = &CONFIG
        .read()
        .unwrap() // safety: the only way this will panic is if the lock is poisoned,
        // which happens when another holder panics. we're already done at that point.
        .forge_client_key_path;
    let forge_client_cert_path = &CONFIG
        .read()
        .unwrap() // safety: the only way this will panic is if the lock is poisoned,
        // which happens when another holder panics. we're already done at that point.
        .forge_client_cert_path;

    let client_cert = ClientCert {
        cert_path: forge_client_cert_path.clone(),
        key_path: forge_client_key_path.clone(),
    };

    CarbideClientConfig::new(forge_root_ca_path.clone(), Some(client_cert))
}
