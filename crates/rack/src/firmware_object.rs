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

use carbide_secrets::credentials::CredentialKey;
use carbide_uuid::rack::RackId;
use model::rack_type::{RackHardwareType, RackProfile};

pub const ANY_RACK_HARDWARE_TYPE: &str = "any";
pub const RMS_NOAUTH_ACCESS_TOKEN: &str = "NOAUTH";

pub fn hardware_type_wire_value(value: Option<&RackHardwareType>) -> String {
    value.map(|value| value.0.clone()).unwrap_or_default()
}

pub fn profile_hardware_type_wire_value(profile: &RackProfile) -> String {
    hardware_type_wire_value(profile.rack_hardware_type.as_ref())
}

pub fn rms_access_token_or_noauth(access_token: Option<&str>) -> String {
    access_token
        .filter(|token| !token.trim().is_empty())
        .unwrap_or(RMS_NOAUTH_ACCESS_TOKEN)
        .to_string()
}

pub fn rack_maintenance_access_token_key(rack_id: &RackId) -> CredentialKey {
    CredentialKey::RackMaintenanceAccessToken {
        rack_id: rack_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_rack_hardware_type_serializes_empty() {
        assert_eq!(hardware_type_wire_value(None), "");
    }

    #[test]
    fn rms_access_token_defaults_to_noauth() {
        assert_eq!(rms_access_token_or_noauth(None), RMS_NOAUTH_ACCESS_TOKEN);
        assert_eq!(
            rms_access_token_or_noauth(Some("")),
            RMS_NOAUTH_ACCESS_TOKEN
        );
        assert_eq!(
            rms_access_token_or_noauth(Some("   ")),
            RMS_NOAUTH_ACCESS_TOKEN
        );
        assert_eq!(rms_access_token_or_noauth(Some("token")), "token");
    }
}
