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
use std::fs;
use std::os::unix::fs::PermissionsExt;

use model::firmware::{Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry};
use regex::Regex;
use temp_dir::TempDir;

pub fn script_setup() -> (TempDir, HashMap<String, Firmware>) {
    let tmpdir = TempDir::with_prefix("test_script_upgrade").unwrap();
    let mut filename = tmpdir.path().to_path_buf();
    filename.push("testscript_delete_me.sh");
    fs::write(
        &filename,
        r#"#!/bin/bash

echo BMC_IP $BMC_IP
echo BMC_USERNAME $BMC_USERNAME
echo BMC_PASSWORD $BMC_PASSWORD
if ! echo $BMC_IP | grep -q ^192; then
    echo "Wrong BMC IP"
    exit 1
fi
sleep 2
cat /proc/self/stat
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(&filename, fs::Permissions::from_mode(0o755)).unwrap();

    let config = HashMap::from([(
        "1".to_string(),
        Firmware {
            vendor: bmc_vendor::BMCVendor::Dell,
            model: "PowerEdge R750".to_string(),
            explicit_start_needed: false,
            components: HashMap::from([(
                FirmwareComponentType::Bmc,
                FirmwareComponent {
                    current_version_reported_as: Some(Regex::new("^Installed-.*__iDRAC.").unwrap()),
                    preingest_upgrade_when_below: Some("1234".to_string()),
                    known_firmware: vec![FirmwareEntry::standard_script(
                        "1234",
                        filename.to_str().unwrap(),
                    )],
                },
            )]),
            ordering: vec![FirmwareComponentType::Uefi, FirmwareComponentType::Bmc],
        },
    )]);

    (tmpdir, config)
}
