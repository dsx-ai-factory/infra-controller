// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_secrets::credentials::Credentials;
use mac_address::MacAddress;

use crate::error::ComponentManagerError;

/// Device credential overrides for a direct component-manager operation.
/// Omitted pairs retain the existing Vault and expected-inventory lookup.
pub struct ComponentCredentials {
    /// Compute or switch BMC credentials, also used for power-shelf PMCs.
    pub bmc: Option<Credentials>,
    /// Switch NVOS credentials.
    pub nvos: Option<Credentials>,
}

#[derive(Clone, Default)]
pub struct FirmwareUpdateOptions {
    pub access_token: Option<String>,
    pub force_update: bool,
}

impl std::fmt::Debug for FirmwareUpdateOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirmwareUpdateOptions")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .field("force_update", &self.force_update)
            .finish()
    }
}

pub fn parse_mac(s: &str) -> Result<MacAddress, ComponentManagerError> {
    s.parse::<MacAddress>()
        .map_err(|e| ComponentManagerError::Internal(format!("invalid MAC from backend: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power_shelf_manager::PowerShelfVendor;

    #[test]
    fn firmware_update_options_debug_redacts_access_token() {
        let debug = format!(
            "{:?}",
            FirmwareUpdateOptions {
                access_token: Some("secret-token".to_string()),
                force_update: true,
            }
        );

        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("force_update: true"));
        assert!(!debug.contains("secret-token"));
    }

    #[test]
    fn parse_mac_valid_colon_separated() {
        let mac = parse_mac("AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(mac.to_string(), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn parse_mac_valid_lowercase() {
        assert!(parse_mac("aa:bb:cc:dd:ee:ff").is_ok());
    }

    #[test]
    fn parse_mac_invalid_string() {
        let err = parse_mac("not-a-mac").unwrap_err();
        assert!(matches!(err, ComponentManagerError::Internal(msg) if msg.contains("invalid MAC")));
    }

    #[test]
    fn parse_mac_empty_string() {
        assert!(parse_mac("").is_err());
    }

    #[test]
    fn parse_mac_too_short() {
        assert!(parse_mac("AA:BB:CC").is_err());
    }

    #[test]
    fn power_shelf_vendor_default_is_liteon() {
        assert_eq!(PowerShelfVendor::DEFAULT, PowerShelfVendor::Liteon);
    }
}
