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

use std::num::TryFromIntError;
use std::str::FromStr;

use carbide_network::deserialize_input_mac_to_address;
use model::site_explorer::{
    EthernetInterface as ModelEthernetInterface, Manager as ModelManager, UefiDevicePath,
};
use nv_redfish::ethernet_interface::EthernetInterface;
use nv_redfish::host_interface::HostInterface;
use nv_redfish::manager::Manager;
use nv_redfish::oem::ami::config_bmc::ConfigBmc;
use nv_redfish::oem::dell::attributes::DellAttributes;
use nv_redfish::oem::lenovo::security_service::LenovoSecurityService;
use nv_redfish::oem::supermicro::{KcsInterface, SysLockdown};
use nv_redfish::{Bmc, Resource};

use crate::Error;

fn enabled_ipmi_port(
    protocol_enabled: Option<Option<bool>>,
    port: Option<Option<i64>>,
) -> Result<Option<u16>, TryFromIntError> {
    if protocol_enabled != Some(Some(true)) {
        return Ok(None);
    }

    port.flatten().map(u16::try_from).transpose()
}

#[derive(Default)]
pub(crate) struct Config {
    pub(crate) need_host_interfaces: bool,
    pub(crate) need_oem_dell_attributes: bool,
    pub(crate) need_oem_lenovo_security_service: bool,
    pub(crate) need_oem_supermicro_kcs_interface: bool,
    pub(crate) need_oem_supermicro_sys_lockdown: bool,
    pub(crate) need_oem_ami_config_bmc: bool,
}

pub(crate) struct ExploredManager<B: Bmc> {
    pub(crate) manager: Manager<B>,
    pub(crate) eth_interfaces: Vec<EthernetInterface<B>>,
    ipmi_port: Option<u16>,
    pub(crate) host_interfaces: Option<Vec<HostInterface<B>>>,
    pub(crate) oem_dell_attributes: Option<DellAttributes<B>>,
    pub(crate) oem_lenovo_security_service: Option<LenovoSecurityService<B>>,
    pub(crate) oem_supermicro_kcs_interface: Option<KcsInterface<B>>,
    pub(crate) oem_supermicro_sys_lockdown: Option<SysLockdown<B>>,
    pub(crate) oem_ami_config_bmc: Option<ConfigBmc<B>>,
}

impl<B: Bmc> ExploredManager<B> {
    pub(crate) async fn explore(manager: Manager<B>, config: &Config) -> Result<Self, Error<B>> {
        let eth_interfaces = manager
            .ethernet_interfaces()
            .await
            .map_err(Error::nv_redfish("manager ethernet interfaces"))?
            .ok_or_else(Error::bmc_not_provided("manager ethernet interfaces"))?
            .members()
            .await
            .map_err(Error::nv_redfish("manager ethernet interfaces members"))?;

        let host_interfaces = if config.need_host_interfaces {
            if let Some(collection) = manager
                .host_interfaces()
                .await
                .map_err(Error::nv_redfish("host interfaces collection"))?
            {
                Some(
                    collection
                        .members()
                        .await
                        .map_err(Error::nv_redfish("host interfaces collection members"))?,
                )
            } else {
                None
            }
        } else {
            None
        };

        let oem_dell_attributes = if config.need_oem_dell_attributes {
            manager
                .oem_dell_attributes()
                .await
                .map_err(Error::nv_redfish("Dell OEM Attributes"))?
        } else {
            None
        };

        let oem_lenovo_security_service = if config.need_oem_lenovo_security_service
            && let Some(oem_lenovo) = manager
                .oem_lenovo()
                .map_err(Error::nv_redfish("Lenovo manager OEM"))?
        {
            oem_lenovo
                .security()
                .await
                .map_err(Error::nv_redfish("Lenovo OEM security service"))?
        } else {
            None
        };

        let mut oem_supermicro_kcs_interface = None;
        let mut oem_supermicro_sys_lockdown = None;
        if (config.need_oem_supermicro_kcs_interface || config.need_oem_supermicro_sys_lockdown)
            && let Some(oem_supermicro) = manager
                .oem_supermicro()
                .map_err(Error::nv_redfish("Supermicro OEM"))?
        {
            if config.need_oem_supermicro_kcs_interface {
                oem_supermicro_kcs_interface = oem_supermicro
                    .kcs_interface()
                    .await
                    .map_err(Error::nv_redfish("Supermicro KCS Interface"))?
            };

            if config.need_oem_supermicro_sys_lockdown {
                oem_supermicro_sys_lockdown = oem_supermicro
                    .sys_lockdown()
                    .await
                    .map_err(Error::nv_redfish("Supermicro SysLockdown"))?
            }
        }

        let oem_ami_config_bmc = if config.need_oem_ami_config_bmc {
            manager
                .oem_ami_config_bmc()
                .await
                .map_err(Error::nv_redfish("AMI manager ConfigBMC OEM"))?
        } else {
            None
        };

        let ipmi_port = manager
            .network_protocol()
            .await
            .map_err(Error::nv_redfish("manager network protocol"))?
            .and_then(|network_protocol| {
                let raw = network_protocol.raw();
                raw.ipmi.as_ref().and_then(|ipmi| {
                    let reported_port = ipmi.port.flatten();
                    match enabled_ipmi_port(ipmi.protocol_enabled, ipmi.port) {
                        Ok(port) => port,
                        Err(error) => {
                            tracing::warn!(
                                manager_id = %manager.id(),
                                ipmi_port = ?reported_port,
                                error = %error,
                                "Ignoring invalid IPMI port reported by Redfish",
                            );
                            None
                        }
                    }
                })
            });

        Ok(Self {
            manager,
            eth_interfaces,
            ipmi_port,
            host_interfaces,
            oem_dell_attributes,
            oem_lenovo_security_service,
            oem_supermicro_kcs_interface,
            oem_supermicro_sys_lockdown,
            oem_ami_config_bmc,
        })
    }

    pub(crate) fn to_model(&self) -> Result<ModelManager, Error<B>> {
        let ethernet_interfaces = self
            .eth_interfaces
            .iter()
            .map(|iface| {
                let mac_address = iface
                    .mac_address()
                    .map(|addr| {
                        deserialize_input_mac_to_address(addr.as_str()).map_err(|e| {
                            Error::InvalidValue(format!("MAC address not valid: {addr} (err: {e})"))
                        })
                    })
                    .transpose()
                    .or_else(|err| {
                        if iface
                            .interface_enabled()
                            .is_some_and(|is_enabled| !is_enabled)
                        {
                            // disabled interfaces sometimes populate the MAC address with junk,
                            // ignore this error and create the interface with an empty mac address
                            // in the exploration report
                            tracing::debug!(
                                interface_id = %iface.id(),
                                link_status = ?iface.link_status(),
                                error = %err,
                                "could not parse MAC address for a disabled interface"
                            );
                            Ok(None)
                        } else {
                            Err(err)
                        }
                    })?;

                let uefi_device_path = iface
                    .uefi_device_path()
                    .map(|v| v.into_inner())
                    .map(UefiDevicePath::from_str)
                    .transpose()
                    .map_err(|err| Error::InvalidValue(format!("UefiDevicePath: {err}")))?;

                Ok(ModelEthernetInterface {
                    description: iface.description().map(|v| v.to_string()),
                    id: Some(iface.id().to_string()),
                    interface_enabled: iface.interface_enabled(),
                    mac_address,
                    link_status: iface.link_status().map(|s| format!("{s:?}")),
                    uefi_device_path,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ModelManager {
            id: self.manager.id().inner().to_string(),
            ethernet_interfaces,
            ipmi_port: self.ipmi_port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::enabled_ipmi_port;

    #[test]
    fn extracts_only_enabled_valid_ipmi_ports() {
        let cases = [
            (
                "enabled",
                Some(Some(true)),
                Some(Some(1623)),
                Ok(Some(1623)),
            ),
            ("disabled", Some(Some(false)), Some(Some(1623)), Ok(None)),
            ("enabled state absent", None, Some(Some(1623)), Ok(None)),
            ("port absent", Some(Some(true)), None, Ok(None)),
            ("port null", Some(Some(true)), Some(None), Ok(None)),
            ("negative port", Some(Some(true)), Some(Some(-1)), Err(())),
            (
                "port above u16 range",
                Some(Some(true)),
                Some(Some(65_536)),
                Err(()),
            ),
        ];

        for (name, protocol_enabled, port, expected) in cases {
            assert_eq!(
                enabled_ipmi_port(protocol_enabled, port).map_err(drop),
                expected,
                "{name}",
            );
        }
    }
}
