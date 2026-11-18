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

use carbide_macros::sqlx_test;
use sqlx::FromRow;

use crate::FirmwareConfig;

#[derive(FromRow)]
struct AsStrings {
    pub versions: String,
}

#[sqlx_test]
pub async fn test_build_versions(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
    use std::ops::DerefMut;

    use db::desired_firmware;
    let mut config: FirmwareConfig = Default::default();

    // Source config is hacky, but we just need to have 3 different components in unsorted order
    let src_cfg_str = r#"
    model = "PowerEdge R750"
    vendor = "Dell"
    explicit_start_needed = false

    [components.uefi]
    current_version_reported_as = "^Installed-.*__BIOS.Setup."
    preingest_upgrade_when_below = "1.13.3"

    [[components.uefi.known_firmware]]
    version = "1.13.3"
    url = "https://artifactory.nvidia.com/artifactory/sw-ngc-forge-cargo-local/misc/BIOS_T3H20_WN64_1.13.2.EXE"
    default = true

    [components.bmc]
    current_version_reported_as = "^Installed-.*__iDRAC."

    [[components.bmc.known_firmware]]
    version = "7.10.30.00"
    url = "https://artifactory.nvidia.com/artifactory/sw-ngc-forge-cargo-local/misc/iDRAC-with-Lifecycle-Controller_Firmware_HV310_WN64_7.10.30.00_A00.EXE"
    default = true

    [components.cx7]
    current_version_reported_as = "^CX7_[0-9]+$"

    [[components.cx7.known_firmware]]
    version = "28.47.2682"
    url = "https://artifactory.nvidia.com/artifactory/sw-ngc-forge-cargo-local/misc/cx7.bin"
    default = true
        "#;
    config.add_test_override(src_cfg_str.to_string());

    let src_cfg_str_part2 = r#"
    model = "PowerEdge R750"
    vendor = "Dell"
    explicit_start_needed = true

    [components.cec]
    current_version_reported_as = "^Installed-.*__CEC."

    [[components.cec.known_firmware]]
    version = "8.10.30.00"
    url = "https://artifactory.nvidia.com/artifactory/sw-ngc-forge-cargo-local/misc/iDRAC-with-Lifecycle-Controller_Firmware_HV310_WN64_7.10.30.00_A00.EXE"
    default = true
        "#;
    config.add_test_override(src_cfg_str_part2.to_string());

    // And empty ones to test that we don't add these to desired firmware
    let src_cfg_str = r#"
vendor = "Hpe"
model = "ProLiant DL385 Gen10 Plus v2"
[components.bmc]
current_version_reported_as = "^1$"
[components.uefi]
current_version_reported_as = "^2$"
"#;
    config.add_test_override(src_cfg_str.to_string());
    let src_cfg_str = r#"
vendor = "Hpe"
model = "ProLiant DL380a Gen11"
[components.bmc]
current_version_reported_as = "^1$"
[components.uefi]
current_version_reported_as = "^2$"
"#;
    config.add_test_override(src_cfg_str.to_string());

    let mut txn = pool.begin().await?;
    desired_firmware::snapshot_desired_firmware(
        &mut txn,
        config
            .create_snapshot()
            .into_values()
            .collect::<Vec<_>>()
            .as_slice(),
    )
    .await?;
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    let query =
        r#"SELECT versions->>'Versions' AS versions FROM desired_firmware WHERE vendor = 'Dell';"#;

    let versions_all: Vec<AsStrings> = sqlx::query_as(query).fetch_all(txn.deref_mut()).await?;
    let versions = versions_all.first().unwrap().versions.clone();

    let expected =
        r#"{"bmc": "7.10.30.00", "cec": "8.10.30.00", "cx7": "28.47.2682", "uefi": "1.13.3"}"#;

    assert_eq!(expected, versions);

    let query = r#"SELECT COUNT(1) FROM desired_firmware;"#;
    let count: (i64,) = sqlx::query_as(query).fetch_one(txn.deref_mut()).await?;
    assert_eq!(count, (1,));

    let query = r#"SELECT explicit_update_start_needed FROM desired_firmware;"#;
    let explicit_update_needed: (bool,) = sqlx::query_as(query).fetch_one(txn.deref_mut()).await?;
    assert_eq!(explicit_update_needed, (true,));
    txn.commit().await?;

    Ok(())
}
