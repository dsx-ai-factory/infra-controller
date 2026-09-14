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

use std::fmt;

use itertools::Itertools;

pub mod bluefield;
pub mod dell;
pub mod gb200;
pub mod hpe;
pub mod lenovo;
pub mod lenovo_ami;
pub mod lenovo_gb300;
pub mod supermicro;
pub mod supermicro_gb300;
pub mod vera_rubin;
pub mod viking;

/// Re-exported so the many `hw::HwType` paths in this crate keep reading as
/// explorer vocabulary. The enum itself lives in the model because its rendered
/// variant names are the hardware classes an attestation profile is keyed to.
pub use model::site_explorer::HwType;

/// The BIOS attribute, and the value it has to hold, for this hardware to retry
/// booting indefinitely. `None` where the platform has no such attribute or its
/// polarity is not yet characterized.
///
/// A free function rather than a method on `HwType`, because `BiosAttr` is this
/// crate's vocabulary and the enum is declared in the model.
pub const fn infinite_boot_enabled_attr(hw_type: HwType) -> Option<BiosAttr<'static>> {
    match hw_type {
        HwType::Ami => Some(BiosAttr::new_str("EndlessBoot", "Enabled")),
        HwType::Bluefield => None,
        HwType::Dell => Some(BiosAttr::new_str("BootSeqRetry", "Enabled")),
        HwType::Gb200 => Some(BiosAttr::new_str("EmbeddedUefiShell", "Disabled")),
        // The DGX GB300 BIOS exposes EmbeddedUefiShell, but the value that means
        // infinite-boot-enabled is not yet characterized on hardware (GB200's polarity
        // is not assumed to carry over). Left None until confirmed on a tray.
        // TODO(dgx-gb300): set the infinite-boot attribute from the DGX GB300 BIOS.
        HwType::DgxGb300 => None,
        HwType::Hpe => None,
        HwType::Lenovo => Some(BiosAttr::new_str("BootModes_InfiniteBootRetry", "Enabled")),
        HwType::LenovoAmi => Some(BiosAttr::new_str("EndlessBoot", "Enabled")),
        HwType::LenovoGb300 => Some(BiosAttr::new_int("LEM0003", 50)),
        // TODO(smc): confirm the SMC GB300 infinite-boot BIOS attribute from the tray BIOS.
        HwType::SupermicroGb300 => None,
        HwType::LiteonPowerShelf => None,
        HwType::DeltaPowerShelf => None,
        HwType::NvSwitch => None,
        HwType::Supermicro => None,
        HwType::Viking => Some(BiosAttr::new_str("NvidiaInfiniteboot", "Enable")),
        // Same EmbeddedUefiShell polarity as GB200 / libredfish NvidiaGBx00.
        HwType::VeraRubin => Some(BiosAttr::new_str("EmbeddedUefiShell", "Disabled")),
    }
}

#[derive(Clone, Copy)]
pub struct BiosAttr<'a> {
    pub key: &'a str,
    pub value: BiosAttrValue<'a>,
}

impl BiosAttr<'_> {
    pub const fn new_bool(key: &'static str, value: bool) -> BiosAttr<'static> {
        BiosAttr {
            key,
            value: BiosAttrValue::Bool(value),
        }
    }
    pub const fn new_str(key: &'static str, value: &'static str) -> BiosAttr<'static> {
        BiosAttr {
            key,
            value: BiosAttrValue::Str(value),
        }
    }
    pub const fn new_any_str(
        key: &'static str,
        value: &'static [&'static str],
    ) -> BiosAttr<'static> {
        BiosAttr {
            key,
            value: BiosAttrValue::AnyStr(value),
        }
    }
    pub const fn new_int(key: &'static str, value: i64) -> BiosAttr<'static> {
        BiosAttr {
            key,
            value: BiosAttrValue::Int(value),
        }
    }
}

#[derive(Clone, Copy)]
pub enum BiosAttrValue<'a> {
    Str(&'a str),
    AnyStr(&'a [&'a str]),
    Bool(bool),
    Int(i64),
}

impl fmt::Display for BiosAttrValue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BiosAttrValue::Str(v) => v.fmt(f),
            BiosAttrValue::Bool(v) => v.fmt(f),
            BiosAttrValue::Int(v) => v.fmt(f),
            BiosAttrValue::AnyStr(v) => write!(f, "any({})", v.iter().join(",")),
        }
    }
}
