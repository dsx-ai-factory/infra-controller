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

use crate::hw::BiosAttr;

pub const EXPECTED_BIOS_ATTRS: [BiosAttr; 14] = [
    BiosAttr::new_str("InBandManageabilityInterface", "Disabled"),
    BiosAttr::new_str("UefiVariableAccess", "Standard"),
    BiosAttr::new_any_str("SerialComm", &["OnConRedirAuto", "OnConRedir"]), // Second is legacy
    BiosAttr::new_any_str("SerialPortAddress", &["Serial1Com2Serial2Com1", "Com1"]), // Second is legacy
    BiosAttr::new_str("FailSafeBaud", "115200"),
    BiosAttr::new_str("ConTermType", "Vt100Vt220"),
    BiosAttr::new_str("RedirAfterBoot", "Enabled"),
    BiosAttr::new_str("SriovGlobalEnable", "Enabled"),
    BiosAttr::new_str("TpmSecurity", "On"),
    BiosAttr::new_str("Tpm2Hierarchy", "Enabled"), // Setup puts "Clear" here.
    BiosAttr::new_str("Tpm2Algorithm", "SHA256"),
    BiosAttr::new_str("HttpDev1EnDis", "Enabled"),
    BiosAttr::new_str("HttpDev1TlsMode", "None"),
    BiosAttr::new_str("PxeDev1EnDis", "Disabled"),
];
