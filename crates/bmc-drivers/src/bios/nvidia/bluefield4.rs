/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Bios, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::{ActionError, Bmc};

use crate::bios::standard::StandardBios;

/// BlueField-4's BIOS has a standard `ResetBios` action and no platform
/// settings. Its `ChangePassword` refuses an empty new password, so a UEFI
/// password, once set, cannot be cleared; the driver sets none.
pub(crate) struct BlueField4Bios;

#[async_trait]
impl<B: Bmc> Bios<B> for BlueField4Bios
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Bios<B> {
        &StandardBios
    }

    async fn change_uefi_password(
        &self,
        _cx: &OpCx<'_, B>,
        _current_password: &str,
        _new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/BlueField_0";
    const BIOS: &str = "/redfish/v1/Systems/BlueField_0/Bios";
    const RESET_BIOS: &str = "/redfish/v1/Systems/BlueField_0/Bios/Actions/Bios.ResetBios";

    /// Password changes and clears are refused without a request, and a BIOS
    /// reset uses the standard action rather than BlueField-3's
    /// `ResetEfiVars` attribute.
    #[tokio::test]
    async fn passwords_are_unsupported_and_reset_uses_reset_bios() {
        let bmc = Fixture::new("Nvidia", "BlueField-4", "BlueField_0", "Bluefield_BMC")
            .document(
                SYSTEM,
                json!({
                    "@odata.id": SYSTEM, "Id": "BlueField_0", "Name": "BlueField_0",
                    "Bios": {"@odata.id": BIOS},
                }),
            )
            .document(
                BIOS,
                json!({
                    "@odata.id": BIOS, "Id": "BIOS", "Name": "BIOS Configuration",
                    "Attributes": {"EmbeddedUefiShell": "Disabled"},
                    "Actions": {
                        "#Bios.ChangePassword": {
                            "target": "/redfish/v1/Systems/BlueField_0/Bios/Actions/Bios.ChangePassword",
                        },
                        "#Bios.ResetBios": {"target": RESET_BIOS},
                    },
                }),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        assert_eq!(
            BlueField4Bios.change_uefi_password(&cx, "old", "new").await,
            Err(PlatformError::Unsupported)
        );
        assert_eq!(
            BlueField4Bios.clear_uefi_password(&cx, "old").await,
            Err(PlatformError::Unsupported)
        );
        BlueField4Bios.reset(&cx).await.expect("reset succeeds");

        assert_eq!(
            bmc.writes()
                .iter()
                .map(|write| (path(write).to_string(), body(write)))
                .collect::<Vec<_>>(),
            [(RESET_BIOS.to_string(), json!({}))]
        );
    }
}
