/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! The compiled driver catalogue: every driver NICo carries, keyed by a typed
//! [`Driver`], and the dispatch from a persisted selection to the driver.
//!
//! Adding a driver is one line in [`drivers!`] below plus its own file under
//! the capability module; each line's path is the file that implements it.
//! The macro derives the wire id, the capability, and the dispatch arm, so a
//! driver named in a rule for the wrong capability, or missing from the list,
//! fails to compile or fails table validation.

use std::fmt;
use std::str::FromStr;

use bmc_platform::{
    Accounts, Attestation, Bios, BmcControl, BootOrder, Capability, Console, Dpu, Firmware,
    Lockdown, PlatformIdentity, Power, SecureBoot, Storage, SuperNic,
};
use nv_redfish::core::{ActionError, Bmc};
use thiserror::Error;

use crate::selection::{CapabilitySelection, DriverMap, ResolvedSelection, Rules, SelectionError};
use crate::{
    accounts, attestation, bios, bmc_control, boot_order, console, dpu, firmware, lockdown, power,
    secure_boot, storage, super_nic,
};

/// Declares the catalogue: one block per capability naming its
/// [`SelectedDrivers`] accessor and override, its trait, optionally its
/// standard driver, and every named driver as `Variant = "wire-id" => path::Type`.
macro_rules! drivers {
    ($(
        $capability:ident as $accessor:ident, $with:ident : $trait:ident $(= $standard:path)? {
            $( $variant:ident = $id:literal => $driver:path, )*
        }
    )*) => {
        /// A compiled capability driver, or a plugin this binary does not carry.
        ///
        /// Known variants serialize as their kebab-case wire id, which is what
        /// selection rules and persisted driver maps store.
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub enum Driver {
            $($(
                #[doc = concat!("`", $id, "`")]
                $variant,
            )*)*
            /// A well-formed driver id this binary does not compile in.
            Plugin(PluginId),
        }

        impl Driver {
            /// Every compiled driver.
            pub const ALL: &'static [Self] = &[$($( Self::$variant, )*)*];

            /// The capability this driver implements; `None` for a plugin.
            pub const fn capability(&self) -> Option<Capability> {
                match self {
                    $($( Self::$variant => Some(Capability::$capability), )*)*
                    Self::Plugin(_) => None,
                }
            }

            /// The id as written in rules and driver maps.
            pub fn as_str(&self) -> &str {
                match self {
                    $($( Self::$variant => $id, )*)*
                    Self::Plugin(id) => id.as_str(),
                }
            }
        }

        impl FromStr for Driver {
            type Err = PluginIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($( $id => Ok(Self::$variant), )*)*
                    other => other.parse().map(Self::Plugin),
                }
            }
        }

        impl Drivers {
            /// The map a BMC gets when no rule matches: `Standard` for every
            /// capability with a compiled standard driver, `Unsupported` otherwise.
            pub fn default_map() -> DriverMap {
                let mut map = DriverMap::filled(CapabilitySelection::Unsupported);
                $(
                    if has_standard!($($standard)?) {
                        map.set(Capability::$capability, CapabilitySelection::Standard);
                    }
                )*
                map
            }
        }

        /// The driver serving each capability on one BMC, resolved once from its
        /// persisted [`DriverMap`] so a selection this binary cannot serve fails
        /// before any I/O.
        pub struct SelectedDrivers<B: Bmc + 'static> {
            $( $accessor: Result<&'static dyn $trait<B>, CatalogError>, )*
        }

        impl<B: Bmc + 'static> SelectedDrivers<B>
        where
            B::Error: ActionError,
        {
            /// Resolves every selection in `map` against the compiled catalogue.
            pub fn resolve(map: &DriverMap) -> Self {
                Self {
                    $(
                        $accessor: {
                            let capability = Capability::$capability;
                            match map.get(capability) {
                                CapabilitySelection::Unsupported => {
                                    Err(CatalogError::Unsupported { capability })
                                }
                                CapabilitySelection::Standard => {
                                    standard_driver!(capability, $($standard)?)
                                }
                                $( CapabilitySelection::Driver(Driver::$variant) => Ok(&$driver), )*
                                CapabilitySelection::Driver(Driver::Plugin(id)) => {
                                    Err(CatalogError::UnknownPlugin { capability, id: id.clone() })
                                }
                                CapabilitySelection::Driver(other) => {
                                    Err(CatalogError::WrongCapability {
                                        capability,
                                        driver: other.clone(),
                                    })
                                }
                            }
                        },
                    )*
                }
            }
        }

        impl<B: Bmc + 'static> SelectedDrivers<B> {
            /// Every capability unsupported; test doubles override what they script.
            pub fn unsupported() -> Self {
                Self {
                    $(
                        $accessor: Err(CatalogError::Unsupported {
                            capability: Capability::$capability,
                        }),
                    )*
                }
            }

            $(
                #[doc = concat!("The ", stringify!($trait), " driver selected for this BMC.")]
                pub fn $accessor(&self) -> Result<&dyn $trait<B>, CatalogError> {
                    self.$accessor.clone()
                }

                #[doc = concat!("Serves ", stringify!($trait), " with `driver`, for test doubles.")]
                pub fn $with(mut self, driver: &'static dyn $trait<B>) -> Self {
                    self.$accessor = Ok(driver);
                    self
                }
            )*
        }
    };
}

macro_rules! has_standard {
    ($standard:path) => {
        true
    };
    () => {
        false
    };
}

macro_rules! standard_driver {
    ($capability:expr, $standard:path) => {
        Ok(&$standard)
    };
    ($capability:expr,) => {
        Err(CatalogError::StandardNotImplemented {
            capability: $capability,
        })
    };
}

drivers! {
    Power as power, with_power: Power = power::standard::StandardPower {
        DellIdracPower = "dell-idrac-power" => power::dell::idrac::IdracPower,
        DeltaPowerShelfPower = "delta-power-shelf-power" => power::delta::power_shelf::DeltaPowerShelfPower,
        HpeIloPower = "hpe-ilo-power" => power::hpe::ilo::IloPower,
        LenovoXccPower = "lenovo-xcc-power" => power::lenovo::xcc::XccPower,
        LiteOnPowerShelfPower = "liteon-power-shelf-power" => power::liteon::power_shelf::LiteOnPowerShelfPower,
        NvidiaBlueField4Power = "nvidia-bluefield4-power" => power::nvidia::bluefield4::BlueField4Power,
        NvidiaGh200Power = "nvidia-gh200-power" => power::nvidia::openbmc::Gh200Power,
        NvidiaOpenBmcPower = "nvidia-openbmc-power" => power::nvidia::openbmc::OpenBmcPower,
        SupermicroSmcPower = "supermicro-smc-power" => power::supermicro::smc::SmcPower,
    }
    BmcControl as bmc_control, with_bmc_control: BmcControl = bmc_control::standard::StandardBmcControl {
        AmiMegaRacBmcControl = "ami-megarac-bmc-control" => bmc_control::ami::megarac::MegaRacBmcControl,
        DellIdracBmcControl = "dell-idrac-bmc-control" => bmc_control::dell::idrac::IdracBmcControl,
        HpeIloBmcControl = "hpe-ilo-bmc-control" => bmc_control::hpe::ilo::IloBmcControl,
        NvidiaGb200BmcControl = "nvidia-gb200-bmc-control" => bmc_control::nvidia::gb200::Gb200BmcControl,
        NvidiaVeraRubinBmcControl = "nvidia-vera-rubin-bmc-control" => bmc_control::nvidia::vera_rubin::VeraRubinBmcControl,
        SupermicroSmcBmcControl = "supermicro-smc-bmc-control" => bmc_control::supermicro::smc::SmcBmcControl,
    }
    Bios as bios, with_bios: Bios = bios::standard::StandardBios {
        AmiMegaRacBios = "ami-megarac-bios" => bios::ami::megarac::MegaRacBios,
        DellIdracBios = "dell-idrac-bios" => bios::dell::idrac::IdracBios,
        GigaComputingAmiBios = "giga-computing-ami-bios" => bios::giga_computing::ami::GigaComputingAmiBios,
        HpeIloBios = "hpe-ilo-bios" => bios::hpe::ilo::IloBios,
        LenovoGb300Bios = "lenovo-gb300-bios" => bios::lenovo::gb300::Gb300Bios,
        LenovoXccBios = "lenovo-xcc-bios" => bios::lenovo::xcc::XccBios,
        LiteOnPowerShelfBios = "liteon-power-shelf-bios" => bios::liteon::power_shelf::LiteOnPowerShelfBios,
        NvidiaBlueFieldBios = "nvidia-bluefield-bios" => bios::nvidia::bluefield::BlueFieldBios,
        NvidiaBlueField4Bios = "nvidia-bluefield4-bios" => bios::nvidia::bluefield4::BlueField4Bios,
        NvidiaGbx00Bios = "nvidia-gbx00-bios" => bios::nvidia::gbx00::Gbx00Bios,
        NvidiaGh200Bios = "nvidia-gh200-bios" => bios::nvidia::gh200::Gh200Bios,
        NvidiaSwitchBios = "nvidia-switch-bios" => bios::nvidia::switch::SwitchBios,
        NvidiaVeraRubinBios = "nvidia-vera-rubin-bios" => bios::nvidia::vera_rubin::VeraRubinBios,
        NvidiaVikingBios = "nvidia-viking-bios" => bios::nvidia::viking::VikingBios,
        SupermicroGb300Bios = "supermicro-gb300-bios" => bios::supermicro::gb300::Gb300Bios,
        SupermicroSmcBios = "supermicro-smc-bios" => bios::supermicro::smc::SmcBios,
    }
    BootOrder as boot_order, with_boot_order: BootOrder = boot_order::standard::StandardBootOrder {
        AmiMegaRacBootOrder = "ami-megarac-boot-order" => boot_order::ami::megarac::MegaRacBootOrder,
        DellIdracBootOrder = "dell-idrac-boot-order" => boot_order::dell::idrac::IdracBootOrder,
        HpeIloBootOrder = "hpe-ilo-boot-order" => boot_order::hpe::ilo::IloBootOrder,
        LenovoGb300BootOrder = "lenovo-gb300-boot-order" => boot_order::lenovo::gb300::Gb300BootOrder,
        LenovoXccBootOrder = "lenovo-xcc-boot-order" => boot_order::lenovo::xcc::XccBootOrder,
        LenovoXcc3BootOrder = "lenovo-xcc3-boot-order" => boot_order::lenovo::xcc3::Xcc3BootOrder,
        NvidiaBlueFieldBootOrder = "nvidia-bluefield-boot-order" => boot_order::nvidia::bluefield::BlueFieldBootOrder,
        NvidiaGbx00BootOrder = "nvidia-gbx00-boot-order" => boot_order::nvidia::gbx00::Gbx00BootOrder,
        NvidiaGh200BootOrder = "nvidia-gh200-boot-order" => boot_order::nvidia::gh200::Gh200BootOrder,
        NvidiaSwitchBootOrder = "nvidia-switch-boot-order" => boot_order::nvidia::switch::SwitchBootOrder,
        NvidiaVeraRubinBootOrder = "nvidia-vera-rubin-boot-order" => boot_order::nvidia::vera_rubin::VeraRubinBootOrder,
        NvidiaVikingBootOrder = "nvidia-viking-boot-order" => boot_order::nvidia::viking::VikingBootOrder,
        SupermicroSmcBootOrder = "supermicro-smc-boot-order" => boot_order::supermicro::smc::SmcBootOrder,
    }
    SecureBoot as secure_boot, with_secure_boot: SecureBoot = secure_boot::standard::StandardSecureBoot {
    }
    Lockdown as lockdown, with_lockdown: Lockdown {
        AmiMegaRacLockdown = "ami-megarac-lockdown" => lockdown::ami::megarac::MegaRacLockdown,
        DellIdracLockdown = "dell-idrac-lockdown" => lockdown::dell::idrac::IdracLockdown,
        GigaComputingAmiLockdown = "giga-computing-ami-lockdown" => lockdown::giga_computing::ami::GigaComputingAmiLockdown,
        HpeIloLockdown = "hpe-ilo-lockdown" => lockdown::hpe::ilo::IloLockdown,
        LenovoAmiLockdown = "lenovo-ami-lockdown" => lockdown::lenovo::ami::LenovoAmiLockdown,
        LenovoGb300Lockdown = "lenovo-gb300-lockdown" => lockdown::lenovo::gb300::Gb300Lockdown,
        LenovoXccLockdown = "lenovo-xcc-lockdown" => lockdown::lenovo::xcc::XccLockdown,
        NvidiaVikingLockdown = "nvidia-viking-lockdown" => lockdown::nvidia::viking::VikingLockdown,
        SupermicroSmcLockdown = "supermicro-smc-lockdown" => lockdown::supermicro::smc::SmcLockdown,
    }
    Accounts as accounts, with_accounts: Accounts = accounts::standard::StandardAccounts {
        AmiMegaRacAccounts = "ami-megarac-accounts" => accounts::ami::megarac::MegaRacAccounts,
        DellIdracAccounts = "dell-idrac-accounts" => accounts::dell::idrac::IdracAccounts,
        DeltaPowerShelfAccounts = "delta-power-shelf-accounts" => accounts::delta::power_shelf::DeltaPowerShelfAccounts,
        HpeIloAccounts = "hpe-ilo-accounts" => accounts::hpe::ilo::IloAccounts,
        LenovoXccAccounts = "lenovo-xcc-accounts" => accounts::lenovo::xcc::XccAccounts,
        LiteOnPowerShelfAccounts = "liteon-power-shelf-accounts" => accounts::liteon::power_shelf::LiteOnPowerShelfAccounts,
        NvidiaBlueFieldAccounts = "nvidia-bluefield-accounts" => accounts::nvidia::bluefield::BlueFieldAccounts,
        NvidiaGh200Accounts = "nvidia-gh200-accounts" => accounts::nvidia::gh200::Gh200Accounts,
        NvidiaOpenBmcAccounts = "nvidia-openbmc-accounts" => accounts::nvidia::openbmc::OpenBmcAccounts,
        NvidiaSwitchAccounts = "nvidia-switch-accounts" => accounts::nvidia::switch::SwitchAccounts,
        NvidiaVikingAccounts = "nvidia-viking-accounts" => accounts::nvidia::viking::VikingAccounts,
    }
    Firmware as firmware, with_firmware: Firmware = firmware::standard::StandardFirmware {
        AmiMegaRacFirmware = "ami-megarac-firmware" => firmware::ami::megarac::MegaRacFirmware,
        DellIdracFirmware = "dell-idrac-firmware" => firmware::dell::idrac::IdracFirmware,
        LenovoXccFirmware = "lenovo-xcc-firmware" => firmware::lenovo::xcc::XccFirmware,
        NvidiaOpenBmcFirmware = "nvidia-openbmc-firmware" => firmware::nvidia::openbmc::OpenBmcFirmware,
        NvidiaVikingFirmware = "nvidia-viking-firmware" => firmware::nvidia::viking::VikingFirmware,
        SupermicroSmcFirmware = "supermicro-smc-firmware" => firmware::supermicro::smc::SmcFirmware,
    }
    Storage as storage, with_storage: Storage {
        DellIdracBossStorage = "dell-idrac-boss-storage" => storage::dell::idrac::IdracBossStorage,
    }
    Dpu as dpu, with_dpu: Dpu {
        NvidiaBlueField2Dpu = "nvidia-bluefield2-dpu" => dpu::nvidia::bluefield2::BlueField2Dpu,
        NvidiaBlueFieldDpu = "nvidia-bluefield-dpu" => dpu::nvidia::bluefield3::BlueField3Dpu,
        NvidiaBlueField4Dpu = "nvidia-bluefield4-dpu" => dpu::nvidia::bluefield4::BlueField4Dpu,
    }
    Attestation as attestation, with_attestation: Attestation = attestation::standard::StandardAttestation {
        NvidiaHgxAttestation = "nvidia-hgx-attestation" => attestation::nvidia::hgx::HgxAttestation,
    }
    Console as console, with_console: Console {
        AmiMegaRacConsole = "ami-megarac-console" => console::ami::megarac::MegaRacConsole,
        DellIdracConsole = "dell-idrac-console" => console::dell::idrac::IdracConsole,
        GigaComputingAmiConsole = "giga-computing-ami-console" => console::giga_computing::ami::GigaComputingAmiConsole,
        HpeIloConsole = "hpe-ilo-console" => console::hpe::ilo::IloConsole,
        LenovoAmiConsole = "lenovo-ami-console" => console::lenovo::ami::LenovoAmiConsole,
        LenovoGb300Console = "lenovo-gb300-console" => console::lenovo::gb300::Gb300Console,
        LenovoXccConsole = "lenovo-xcc-console" => console::lenovo::xcc::XccConsole,
        NvidiaBlueFieldConsole = "nvidia-bluefield-console" => console::nvidia::bluefield::BlueFieldConsole,
        NvidiaOpenBmcConsole = "nvidia-openbmc-console" => console::nvidia::openbmc::OpenBmcConsole,
        NvidiaVikingConsole = "nvidia-viking-console" => console::nvidia::viking::VikingConsole,
        SupermicroBmcConsole = "supermicro-bmc-console" => console::supermicro::bmc::SupermicroBmcConsole,
    }
    SuperNic as super_nic, with_super_nic: SuperNic {
        NvidiaVeraRubinSuperNic = "nvidia-vera-rubin-super-nic" => super_nic::nvidia::vera_rubin::VeraRubinSuperNic,
    }
}

impl fmt::Display for Driver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A well-formed driver id that names no compiled driver.
///
/// Persisted maps and deployment overrides may carry one; validation
/// rejects it before any I/O so a pluggable driver mechanism can adopt the
/// same wire form later.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginId(String);

impl PluginId {
    /// Returns the id as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PluginId {
    type Err = PluginIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(PluginIdError::Empty);
        }
        if matches!(value, "standard" | "unsupported") {
            return Err(PluginIdError::Reserved);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(PluginIdError::InvalidCharacter);
        }
        if value.starts_with('-') || value.ends_with('-') || value.contains("--") {
            return Err(PluginIdError::InvalidSeparator);
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PluginIdError {
    #[error("driver id is empty")]
    Empty,
    #[error("driver id is reserved for a capability-selection sentinel")]
    Reserved,
    #[error("driver id must contain only lowercase ASCII letters, digits, and hyphens")]
    InvalidCharacter,
    #[error("driver id must use single hyphens between nonempty segments")]
    InvalidSeparator,
}

/// The catalogue compiled into this binary.
#[derive(Clone, Copy, Debug, Default)]
pub struct Drivers;

impl Drivers {
    /// Fails before any I/O when `selection` names no compiled driver for `capability`.
    pub fn check(
        capability: Capability,
        selection: &CapabilitySelection,
    ) -> Result<(), CatalogError> {
        match selection {
            CapabilitySelection::Unsupported => Ok(()),
            CapabilitySelection::Standard => {
                if Self::default_map().get(capability) == &CapabilitySelection::Standard {
                    Ok(())
                } else {
                    Err(CatalogError::StandardNotImplemented { capability })
                }
            }
            CapabilitySelection::Driver(Driver::Plugin(id)) => Err(CatalogError::UnknownPlugin {
                capability,
                id: id.clone(),
            }),
            CapabilitySelection::Driver(driver) if driver.capability() == Some(capability) => {
                Ok(())
            }
            CapabilitySelection::Driver(driver) => Err(CatalogError::WrongCapability {
                capability,
                driver: driver.clone(),
            }),
        }
    }

    /// Checks that every selection in a persisted map names a compiled driver.
    pub fn validate_map(map: &DriverMap) -> Result<(), CatalogError> {
        map.iter()
            .try_for_each(|(capability, selection)| Self::check(capability, selection))
    }

    /// Checks that every rule selects compiled drivers for their capabilities.
    pub fn validate_rules(rules: &Rules) -> Result<(), CatalogError> {
        rules.rules().iter().try_for_each(|rule| {
            rule.selections
                .iter()
                .try_for_each(|(capability, selection)| Self::check(*capability, selection))
        })
    }

    /// The selection `rules` make for `identity` over [`Self::default_map`],
    /// checked against the compiled catalogue.
    pub fn select(
        rules: &Rules,
        identity: &PlatformIdentity,
    ) -> Result<ResolvedSelection, SelectError> {
        let selection = rules.resolve(identity, &Self::default_map())?;
        Self::validate_map(&selection.drivers)?;
        Ok(selection)
    }
}

/// Failure to select drivers for an identity.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SelectError {
    /// Matching rules tie for a capability.
    #[error(transparent)]
    Rules(#[from] SelectionError),
    /// The selected map names a driver this binary cannot serve.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
}

/// Failure to resolve a selection against the compiled catalogue.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CatalogError {
    /// The persisted map marks this capability unsupported on this BMC.
    #[error("{capability} is unsupported on this BMC")]
    Unsupported { capability: Capability },
    /// No standard driver is compiled for this capability.
    #[error("no standard {capability} driver is compiled in")]
    StandardNotImplemented { capability: Capability },
    /// The selection names a driver of a different capability.
    #[error("{driver} is not a {capability} driver")]
    WrongCapability {
        capability: Capability,
        driver: Driver,
    },
    /// The selection names a driver this binary does not compile in.
    #[error("unknown {capability} driver {id}")]
    UnknownPlugin {
        capability: Capability,
        id: PluginId,
    },
}

#[cfg(test)]
mod tests {
    use bmc_mock::test_support::TestBmc;

    use super::*;
    use crate::rules::built_in_rules;

    #[test]
    fn every_rule_names_compiled_drivers_of_the_right_capability() {
        Drivers::validate_rules(&built_in_rules()).expect("built-in rules are consistent");
        for driver in Driver::ALL {
            assert_eq!(
                driver.as_str().parse::<Driver>().as_ref(),
                Ok(driver),
                "{driver} round-trips through its wire id"
            );
        }
    }

    #[test]
    fn selections_resolve_or_fail_before_io() {
        let power = |selection| {
            SelectedDrivers::<TestBmc>::resolve(
                &DriverMap::filled(CapabilitySelection::Standard)
                    .with(Capability::Power, selection),
            )
            .power()
            .err()
        };
        assert_eq!(power(CapabilitySelection::Standard), None);
        assert_eq!(
            Drivers::check(Capability::Storage, &CapabilitySelection::Standard),
            Err(CatalogError::StandardNotImplemented {
                capability: Capability::Storage,
            })
        );
        assert_eq!(
            power(CapabilitySelection::Unsupported),
            Some(CatalogError::Unsupported {
                capability: Capability::Power,
            })
        );
        assert_eq!(
            power(CapabilitySelection::Driver(Driver::DellIdracBios)),
            Some(CatalogError::WrongCapability {
                capability: Capability::Power,
                driver: Driver::DellIdracBios,
            })
        );
        let plugin: Driver = "acme-power".parse().expect("well-formed plugin id");
        assert!(matches!(plugin, Driver::Plugin(_)));
        assert!(matches!(
            power(CapabilitySelection::Driver(plugin)),
            Some(CatalogError::UnknownPlugin { .. })
        ));
        assert!("Bad Id".parse::<Driver>().is_err());
    }
}
