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

use std::fmt::Debug;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use rpc::forge;
use rpc::forge_api_client::ForgeApiClient;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::bmc::client_pool::BmcPoolMetrics;
use crate::bmc::connection_impl;
use crate::bmc::connection_impl::{ipmi, ssh};
use crate::bmc::message_proxy::{ToBmcMessage, ToFrontendMessage};
use crate::bmc::vendor::{BmcVendor, BmcVendorDetectionError, SshBmcVendor};
use crate::config::{Config, ConfigError};
use crate::shutdown_handle::ShutdownHandle;

pub(super) async fn spawn(
    connection_details: ConnectionDetails,
    broadcast_to_frontend_tx: broadcast::Sender<ToFrontendMessage>,
    metrics: Arc<BmcPoolMetrics>,
    config: Arc<Config>,
) -> Result<Handle, SpawnError> {
    let handle = match connection_details {
        ConnectionDetails::Ssh(ssh_connection_details) => {
            ssh::spawn(ssh_connection_details, broadcast_to_frontend_tx, metrics)
                .await?
                .into()
        }
        ConnectionDetails::Ipmi(ipmi_connection_details) => ipmi::spawn(
            ipmi_connection_details,
            broadcast_to_frontend_tx,
            config,
            metrics,
        )
        .await?
        .into(),
    };

    Ok(handle)
}

#[derive(thiserror::Error, Debug)]
pub(super) enum SpawnError {
    #[error(transparent)]
    Ssh(#[from] connection_impl::ssh::SpawnError),
    #[error(transparent)]
    Ipmi(#[from] connection_impl::ipmi::SpawnError),
}

impl SpawnError {
    pub(crate) fn retry_immediately(&self) -> bool {
        matches!(self, Self::Ipmi(error) if error.retry_immediately())
    }
}

/// Get the address and auth details to use for a connection to a given machine or instance ID.
///
/// This information is normally gotten by calling GetBMCMetadData on carbide-api, but it can
/// also obey overridden information from ssh-console's config.
pub(super) async fn lookup(
    machine_or_instance_id: &str,
    config: &Config,
    forge_api_client: &ForgeApiClient,
) -> Result<ConnectionDetails, LookupError> {
    if let Some(override_bmc) = config.override_bmcs.as_ref().and_then(|override_bmcs| {
        override_bmcs
            .iter()
            .find(|bmc| {
                bmc.machine_id == machine_or_instance_id
                    || bmc
                        .instance_id
                        .as_ref()
                        .is_some_and(|i| i.as_str() == machine_or_instance_id)
            })
            .cloned()
    }) {
        let machine_id = MachineId::from_str(&override_bmc.machine_id)
            .map_err(|e| LookupError::Config(ConfigError::InvalidBmcOverrideMachineId(e)))?;
        let connection_details = match override_bmc.bmc_vendor {
            BmcVendor::Ssh(ssh_bmc_vendor) => {
                ConnectionDetails::Ssh(Arc::new(ssh::ConnectionDetails {
                    machine_id,
                    addr: config
                        .override_bmc_ssh_addr(override_bmc.addr().port())
                        .await?
                        .unwrap_or(override_bmc.addr()),
                    user: override_bmc.user,
                    password: override_bmc.password,
                    ssh_key_path: override_bmc.ssh_key_path,
                    bmc_vendor: ssh_bmc_vendor,
                }))
            }
            BmcVendor::Ipmi(_) => ConnectionDetails::Ipmi(Arc::new(ipmi::ConnectionDetails {
                machine_id,
                addr: override_bmc.addr(),
                user: override_bmc.user,
                password: override_bmc.password,
            })),
        };
        tracing::info!(
            %machine_or_instance_id,
            bmc_address = %connection_details.addr(),
            bmc_machine_id = %connection_details.machine_id(),
            "Overriding BMC connection"
        );
        return Ok(connection_details);
    }

    let machine_id: MachineId = if let Ok(id) = machine_or_instance_id.parse() {
        id
    } else if let Ok(instance_id) = InstanceId::from_str(machine_or_instance_id) {
        forge_api_client
            .find_instances_by_ids(forge::InstancesByIdsRequest {
                instance_ids: vec![instance_id],
            })
            .await
            .map_err(|e| LookupError::InstanceIdLookup {
                instance_id,
                tonic_status: e,
            })?
            .instances
            .into_iter()
            .next()
            .ok_or_else(|| LookupError::CouldNotFindInstance { instance_id })?
            .machine_id
            .ok_or_else(|| LookupError::InstanceHasNoMachineId { instance_id })?
            .into()
    } else {
        return Err(LookupError::CouldNotParseId {
            machine_or_instance_id: machine_or_instance_id.to_owned(),
        });
    };

    let forge::BmcMetaDataGetResponse {
        ip,
        user,
        password,
        mac: _,
        port: _,
        ssh_port: _,
        serial_console_ssh_port,
        ipmi_port,
        vendor,
    } = forge_api_client
        .get_bmc_meta_data(forge::BmcMetaDataGetRequest {
            machine_id: Some(machine_id),
            role: 0,
            request_type: forge::BmcRequestType::Ipmi.into(),
            bmc_endpoint_request: None,
        })
        .await
        .map_err(|tonic_status| LookupError::BmcMetaDataLookup {
            machine_id: machine_id.to_string(),
            tonic_status,
        })?;

    let bmc_vendor =
        BmcVendor::detect_from_api_vendor(vendor.as_deref().unwrap_or_default(), &machine_id)
            .map_err(|error| LookupError::BmcVendorDetection { machine_id, error })?;

    let ip: IpAddr = ip.parse().map_err(|e| LookupError::InvalidBmcMetadata {
        reason: format!("Error parsing IP address {ip:?}: {e:?}"),
    })?;

    let port = match &bmc_vendor {
        BmcVendor::Ssh(ssh_bmc_vendor) => nonzero_serial_console_ssh_port(serial_console_ssh_port)
            .map_err(|e| LookupError::InvalidBmcMetadata {
                reason: format!("invalid SSH serial-console port: {e:?}"),
            })?
            .or(config.override_bmc_ssh_port)
            .unwrap_or(match ssh_bmc_vendor {
                SshBmcVendor::Dpu => 2200,
                _ => 22,
            }),
        BmcVendor::Ipmi(_) => ipmi_port
            .map(u16::try_from)
            .transpose()
            .map_err(|e| LookupError::InvalidBmcMetadata {
                reason: format!("invalid ipmi port: {e:?}"),
            })?
            .or(config.override_ipmi_port)
            .unwrap_or(623),
    };

    let addr = if let Some(override_ssh_addr) = config.override_bmc_ssh_addr(port).await? {
        tracing::info!(
            bmc_ip_address = %ip,
            override_bmc_ssh_address = %override_ssh_addr,
            "Overriding BMC connection per configuration"
        );
        override_ssh_addr
    } else {
        SocketAddr::new(ip, port)
    };

    let connection_details = match bmc_vendor {
        BmcVendor::Ssh(ssh_bmc_vendor) => {
            ConnectionDetails::Ssh(Arc::new(ssh::ConnectionDetails {
                machine_id,
                addr,
                user,
                password,
                ssh_key_path: None,
                bmc_vendor: ssh_bmc_vendor,
            }))
        }
        BmcVendor::Ipmi(_) => ConnectionDetails::Ipmi(Arc::new(ipmi::ConnectionDetails {
            machine_id,
            addr,
            user,
            password,
        })),
    };

    Ok(connection_details)
}

fn nonzero_serial_console_ssh_port(
    port: Option<u32>,
) -> Result<Option<u16>, std::num::TryFromIntError> {
    port.filter(|port| *port != 0)
        .map(u16::try_from)
        .transpose()
}

#[derive(thiserror::Error, Debug)]
pub(super) enum LookupError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("error looking up instance ID {instance_id}: {tonic_status}")]
    InstanceIdLookup {
        instance_id: InstanceId,
        tonic_status: tonic::Status,
    },
    #[error("could not find instance with id {instance_id}")]
    CouldNotFindInstance { instance_id: InstanceId },
    #[error("instance {instance_id} has no machine_id")]
    InstanceHasNoMachineId { instance_id: InstanceId },
    #[error("could not parse {machine_or_instance_id} into a machine ID or instance ID")]
    CouldNotParseId { machine_or_instance_id: String },
    #[error("cannot detect BMC vendor for machine: {machine_id}: {error}")]
    BmcVendorDetection {
        machine_id: MachineId,
        error: BmcVendorDetectionError,
    },
    #[error("error calling forge.GetBmcMetaData for {machine_id}: {tonic_status}")]
    BmcMetaDataLookup {
        machine_id: String,
        tonic_status: tonic::Status,
    },
    #[error("BMC metadata is invalid: {reason}")]
    InvalidBmcMetadata { reason: String },
}

/// A handle to a BMC connection, which will shut down when dropped.
pub(super) struct Handle {
    pub(super) to_bmc_msg_tx: mpsc::Sender<ToBmcMessage>,
    pub(super) shutdown_tx: oneshot::Sender<()>,
    pub(super) join_handle: JoinHandle<Result<(), SpawnError>>,
}

impl From<ipmi::Handle> for Handle {
    fn from(handle: ipmi::Handle) -> Self {
        Self {
            to_bmc_msg_tx: handle.to_bmc_msg_tx,
            shutdown_tx: handle.shutdown_tx,
            join_handle: tokio::spawn(async move {
                handle
                    .join_handle
                    .await
                    .expect("task panicked")
                    .map_err(Into::into)
            }),
        }
    }
}

impl From<ssh::Handle> for Handle {
    fn from(handle: ssh::Handle) -> Self {
        Self {
            to_bmc_msg_tx: handle.to_bmc_msg_tx,
            shutdown_tx: handle.shutdown_tx,
            join_handle: tokio::spawn(async move {
                handle
                    .join_handle
                    .await
                    .expect("task panicked")
                    .map_err(Into::into)
            }),
        }
    }
}

impl ShutdownHandle<Result<(), SpawnError>> for Handle {
    fn into_parts(self) -> (oneshot::Sender<()>, JoinHandle<Result<(), SpawnError>>) {
        (self.shutdown_tx, self.join_handle)
    }
}

#[derive(Debug, Clone)]
pub(super) enum ConnectionDetails {
    Ssh(Arc<ssh::ConnectionDetails>),
    Ipmi(Arc<ipmi::ConnectionDetails>),
}

impl ConnectionDetails {
    pub(super) fn addr(&self) -> SocketAddr {
        match self {
            ConnectionDetails::Ssh(s) => s.addr,
            ConnectionDetails::Ipmi(i) => i.addr,
        }
    }

    pub(super) fn machine_id(&self) -> MachineId {
        match self {
            ConnectionDetails::Ssh(s) => s.machine_id,
            ConnectionDetails::Ipmi(i) => i.machine_id,
        }
    }

    pub(super) fn kind(&self) -> Kind {
        match self {
            ConnectionDetails::Ssh(_) => Kind::Ssh,
            ConnectionDetails::Ipmi(_) => Kind::Ipmi,
        }
    }
}

#[derive(Copy, Clone)]
pub(crate) enum Kind {
    Ssh,
    Ipmi,
}

/// Represents the state of a connection to a BMC
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    Disconnected = 0,
    Connecting = 1,
    Connected = 2,
    ConnectionError = 3,
}

impl From<State> for u8 {
    fn from(state: State) -> u8 {
        state as u8
    }
}

impl TryFrom<u8> for State {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(State::Disconnected),
            1 => Ok(State::Connecting),
            2 => Ok(State::Connected),
            3 => Ok(State::ConnectionError),
            _ => Err(()),
        }
    }
}

/// Wrapper for an AtomicU8 representing a [`State`], so that the state can be shared
/// between threads.
#[derive(Debug)]
pub(super) struct AtomicConnectionState(AtomicU8);

impl AtomicConnectionState {
    #[inline]
    pub(super) fn new(state: State) -> Self {
        Self(AtomicU8::new(state.into()))
    }

    #[inline]
    pub(super) fn load(&self) -> State {
        State::try_from(self.0.load(Ordering::SeqCst)).expect("BUG: connection state corrupted")
    }

    #[inline]
    pub(super) fn store(&self, state: State) {
        self.0.store(state.into(), Ordering::SeqCst);
    }
}

impl Default for AtomicConnectionState {
    fn default() -> Self {
        AtomicConnectionState::new(State::Disconnected)
    }
}

#[cfg(test)]
mod tests {
    use super::nonzero_serial_console_ssh_port;

    #[test]
    fn validates_serial_console_ssh_port() {
        assert_eq!(nonzero_serial_console_ssh_port(None).unwrap(), None);
        assert_eq!(nonzero_serial_console_ssh_port(Some(0)).unwrap(), None);
        assert_eq!(nonzero_serial_console_ssh_port(Some(1)).unwrap(), Some(1));
        assert_eq!(
            nonzero_serial_console_ssh_port(Some(u16::MAX.into())).unwrap(),
            Some(u16::MAX)
        );
        assert!(nonzero_serial_console_ssh_port(Some(u32::from(u16::MAX) + 1)).is_err());
    }
}
