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

use carbide_secrets::credentials::{BmcCredentialType, CredentialKey};
use mac_address::MacAddress;

pub enum RedfishAuth {
    Anonymous,
    Key(CredentialKey),
    Direct(String, String), // username, password
}

impl RedfishAuth {
    pub fn for_bmc_mac(bmc_mac_address: MacAddress) -> Self {
        RedfishAuth::Key(CredentialKey::BmcCredentials {
            // TODO(ajf): Change this to Forge Admin user once site explorer
            // ensures it exist, credentials are done by mac address
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
        })
    }
}
