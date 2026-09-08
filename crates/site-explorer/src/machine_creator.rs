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
use std::sync::Arc;

use carbide_instrument::emit;
use carbide_rack::rms_node_type::compute_node_identity_for_profile;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials,
};
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSubtype, PredictedHostMachineId};
use db::Transaction;
use itertools::Itertools;
use librms::RmsApi;
use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use model::bmc_info::BmcInfo;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::hardware_info::HardwareInfo;
use model::machine::machine_id::host_id_from_dpu_hardware_info;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    CURRENT_STATE_MODEL_VERSION, ConfigureAstraState, Machine, MachineInterfaceSnapshot,
    ManagedHostState, pick_boot_interface, pick_boot_prediction,
};
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
    canonical_redfish_boot_interface_id,
};
use model::machine_interface::InterfaceType;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentType;
use model::predicted_machine_interface::{NewPredictedMachineInterface, PredictedMachineInterface};
use model::rack_type::RackProfileConfig;
use model::resource_pool::common::CommonPools;
use model::site_explorer::{
    EndpointExplorationReport, ExploredDpu, ExploredManagedHost, HostPrimaryInterfaceSelection,
};
use sqlx::{PgConnection, PgPool};

use crate::errors::{SiteExplorerError, SiteExplorerResult};
use crate::explored_endpoint_index::ExploredEndpointIndex;
use crate::managed_host::ManagedHost;
use crate::metrics::{SiteExplorationMetrics, SiteExplorerMachineSlotTrayPersistenceFailed};
use crate::{IdentifiedManagedHost, SiteExplorerConfig};

const DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE: i64 = 100;

/// Creates machines from site-explorer managed-host reports.
pub struct MachineCreator {
    database_connection: PgPool,
    config: SiteExplorerConfig,
    common_pools: Arc<CommonPools>,
    rack_profiles: Arc<RackProfileConfig>,
    rms_client: Option<Arc<dyn RmsApi>>,
    credential_manager: Arc<dyn CredentialManager>,
}

impl MachineCreator {
    /// Creates a machine creator with site configuration and optional RMS integration.
    pub fn new(
        database_connection: PgPool,
        config: SiteExplorerConfig,
        common_pools: Arc<CommonPools>,
        rack_profiles: Arc<RackProfileConfig>,
        rms_client: Option<Arc<dyn RmsApi>>,
        credential_manager: Arc<dyn CredentialManager>,
    ) -> Self {
        Self {
            database_connection,
            config,
            common_pools,
            rack_profiles,
            rms_client,
            credential_manager,
        }
    }

    /// Creates a new ManagedHost (Host `Machine` and DPU `Machine` pair)
    /// for each ManagedHost that was identified and that doesn't have a corresponding `Machine` yet
    pub(crate) async fn create_machines(
        &self,
        metrics: &mut SiteExplorationMetrics,
        explored_managed_hosts: &mut [IdentifiedManagedHost],
        expected_explored_endpoint_index: &ExploredEndpointIndex,
    ) -> SiteExplorerResult<()> {
        // TODO: Improve the efficiency of this method. Right now we perform 3 database transactions
        // for every identified ManagedHost even if we don't create any objects.
        // We can perform a single query upfront to identify which ManagedHosts don't yet have Machines
        for identified in explored_managed_hosts {
            let host = &identified.explored_host;
            let expected_machine =
                expected_explored_endpoint_index.matched_expected_machine(&host.host_bmc_ip);

            match self
                .create_managed_host_with_selection(
                    host,
                    &mut identified.report,
                    expected_machine,
                    identified.primary_interface_selection,
                    &self.database_connection,
                )
                .await
            {
                Ok(true) => {
                    metrics.created_machines += 1;
                    if metrics.created_machines as u64 == self.config.machines_created_per_run {
                        break;
                    }
                }
                Ok(false) => {}
                Err(error) => tracing::error!(
                    %error,
                    host = ?host,
                    "Failed to create managed host"
                ),
            }
        }

        Ok(())
    }

    /// Best-effort reconciles every host whose desired boot interface is still
    /// incomplete.
    ///
    /// This runs independently of `create_machines`: operators may disable
    /// creation, and a host may no longer have an `ExpectedMachine` or a fresh
    /// observation. Creation-time calls remain in place so a new machine and
    /// its initial desired target still commit atomically. Keyset pages prevent
    /// permanently MAC-only hosts from starving later IDs, while one
    /// transaction per machine bounds locks and preserves completed progress.
    pub(crate) async fn reconcile_desired_boot_interfaces(&self) -> SiteExplorerResult<()> {
        let mut after_id = None;
        loop {
            let machine_ids = db::machine_desired_boot_interface::find_incomplete_machine_ids(
                &self.database_connection,
                after_id.as_ref(),
                DESIRED_BOOT_INTERFACE_RECONCILE_PAGE_SIZE,
            )
            .await?;
            let Some(last_id) = machine_ids.last() else {
                break;
            };
            after_id = Some(*last_id);

            for machine_id in machine_ids {
                let mut txn = Transaction::begin(&self.database_connection).await?;
                match reconcile_desired_boot_interface(txn.as_pgconn(), &machine_id, None, None)
                    .await
                {
                    Ok(()) => {
                        if let Err(error) = txn.commit().await {
                            tracing::warn!(
                                %error,
                                %machine_id,
                                "Desired boot-interface reconciliation commit failed; a later Site Explorer run will retry"
                            );
                        }
                    }
                    Err(error) => {
                        txn.rollback_or_log(
                            "site-explorer desired boot-interface reconciliation failure",
                        )
                        .await;
                        tracing::warn!(
                            %error,
                            %machine_id,
                            "Desired boot-interface reconciliation failed; a later Site Explorer run will retry"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Creates a `Machine` objects for an identified `ManagedHost` with initial states
    ///
    /// Returns `true` if new `Machine` objects have been created or `false` otherwise.
    ///
    /// Refuses to create a Managed Host when `expected_machine` is `None`: only hosts
    /// listed in the `expected_machines` table are allowed to become Managed Hosts.
    /// Already ingested hosts are not affected.
    pub async fn create_managed_host(
        &self,
        explored_host: &ExploredManagedHost,
        report: &mut EndpointExplorationReport,
        expected_machine: Option<&ExpectedMachine>,
        pool: &PgPool,
    ) -> SiteExplorerResult<bool> {
        self.create_managed_host_with_selection(explored_host, report, expected_machine, None, pool)
            .await
    }

    /// Creates a managed host with its transient report selection available for
    /// the initial desired boot interface metadata.
    ///
    /// The source is recorded only when its selected MAC becomes the durable
    /// target; target resolution may legitimately choose another interface.
    async fn create_managed_host_with_selection(
        &self,
        explored_host: &ExploredManagedHost,
        report: &mut EndpointExplorationReport,
        expected_machine: Option<&ExpectedMachine>,
        primary_interface_selection: Option<HostPrimaryInterfaceSelection>,
        pool: &PgPool,
    ) -> SiteExplorerResult<bool> {
        let Some(expected_machine) = expected_machine else {
            tracing::warn!(
                host_bmc_ip_address = %explored_host.host_bmc_ip,
                    "Refusing to create managed host, expected machines entry not found"
            );
            return Ok(false);
        };
        let machine_data = Some(&expected_machine.data);
        let mut managed_host = ManagedHost::init(explored_host);

        let bmc_credentials =
            if expected_machine.data.rack_id.is_some() && self.rms_client.is_some() {
                let key = CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: expected_machine.bmc_mac_address,
                    },
                };
                match self.credential_manager.get_credentials(&key).await {
                    Ok(Some(Credentials::UsernamePassword { username, password })) => {
                        Some((username, password))
                    }
                    _ => None,
                }
            } else {
                None
            };

        // Admission permit BEFORE the transaction: waiters on the admin-segment
        // advisory lock must queue in memory, not on open pool connections.
        let _admin_admission = db::machine_interface::admin_lock_admission().await;
        let mut txn = Transaction::begin(pool).await?;

        // Advisory-lock the admin segments before any machine-interface row
        // writes (`attach_dpu_to_host` / `configure_dpu_interface`), so this
        // transaction holds locks in the allocator order (segment advisory
        // lock first, then interface rows) all the way to the reconcile
        // pass -- which re-acquires the same locks as a no-op.
        db::machine_interface::lock_all_admin_segments(txn.as_pgconn()).await?;

        // Zero-dpu case: If the explored host had no DPUs, we can create the machine now
        if managed_host.explored_host.dpus.is_empty() {
            if let Some(machine_id) = self
                .create_zero_dpu_machine(
                    &mut txn,
                    &managed_host,
                    report,
                    expected_machine.bmc_mac_address,
                    machine_data,
                )
                .await?
            {
                managed_host.machine_id = Some(machine_id);
            } else {
                // An existing predicted host may have gained interfaces and a
                // desired boot target. Keep those restart-safe writes even
                // though there is no new machine to count this iteration.
                txn.commit().await?;
                return Ok(false);
            }
            tracing::info!("Created managed_host with zero DPUs");
        }

        let mut dpu_ids = vec![];
        for dpu_report in managed_host.explored_host.dpus.iter() {
            // machine_id_if_valid_report makes sure that all optional fields on dpu_report are
            // actually set (like the machine-id etc) and returns the machine_id if everything
            // is valid.
            let dpu_machine_id = dpu_report.machine_id_if_valid_report()?;
            dpu_ids.push(dpu_machine_id);
        }

        let existing_hosts_by_dpu_id =
            db::machine::lookup_host_machine_ids_by_dpu_ids(&mut txn, &dpu_ids).await?;

        if !existing_hosts_by_dpu_id.is_empty() {
            // TODO: We run this code for every endpoint on every site explorer run, and it is slow.
            // The call to reconcile_host_admin_addresses below is particularly slow and locks all
            // network segments. We need to find a good way to know when to skip reconciliation in
            // the common case when nothing has changed.

            // Steady state case: DPU's already exist, so site explorer must have already created
            // this managed host (since only site explorer would have created them.) Ensure they're
            // associated with this machine, then return early.

            let existing_dpu_ids = existing_hosts_by_dpu_id
                .keys()
                .copied()
                .sorted()
                .dedup()
                .collect::<Vec<_>>();
            let existing_managed_host_ids = existing_hosts_by_dpu_id
                .values()
                .copied()
                .sorted()
                .dedup()
                .collect::<Vec<_>>();

            if existing_dpu_ids != dpu_ids.iter().copied().sorted().dedup().collect::<Vec<_>>() {
                // This would only happen if somehow a host endpoint gains/loses a DPU from its endpoint report before we
                // get a chance to create a managed host for it.
                let msg = "explored endpoint has a partial number of DPU's already created";
                tracing::error!(
                    dpu_ids = dpu_ids.iter().join(", "),
                    existing_dpu_ids = existing_dpu_ids.iter().join(", "),
                    "{msg}",
                );
                return Err(SiteExplorerError::internal(msg.to_string()));
            }

            let host_machine_id = match existing_managed_host_ids.as_slice() {
                [host_machine_id] => *host_machine_id,
                host_machine_ids => {
                    let existing_dpu_ids = existing_dpu_ids.iter().join(", ");
                    let existing_host_ids = host_machine_ids.iter().join(", ");
                    let msg = "DPU's from exploration report exist but are members of different managed hosts. exploration results are inconsistent";
                    tracing::error!(%existing_dpu_ids, %existing_host_ids, "BUG: {msg}");
                    return Err(SiteExplorerError::internal(msg.to_string()));
                }
            };

            for dpu_report in managed_host.explored_host.dpus.iter() {
                self.configure_dpu_interface(&mut txn, dpu_report).await?;
            }

            self.reconcile_host_admin_addresses(&mut txn, &host_machine_id)
                .await?;
            // The primary on an existing host may predate source
            // tracking. Do not infer its historical selector from today's
            // exploration or ExpectedMachine data.
            reconcile_desired_boot_interface(&mut txn, &host_machine_id, None, None).await?;

            txn.commit().await?;
            return Ok(false);
        }

        for (dpu_report, dpu_machine_id) in
            managed_host.explored_host.dpus.iter().zip(dpu_ids.iter())
        {
            let dpu_machine = self
                .create_dpu(&mut txn, dpu_report)
                .await?
                .ok_or_else(|| {
                    SiteExplorerError::internal(format!(
                        "BUG: DPU machine {} was already found, but we already verified that it did not exist?",
                        dpu_machine_id,
                    ))
                })?;

            let host_machine_id = self
                .attach_dpu_to_host(&mut txn, &managed_host, dpu_report, machine_data)
                .await?;
            managed_host.machine_id = Some(host_machine_id);

            // Now that the host link exists in machine_interfaces, the
            // machine group syncing in try_update_network_config keeps this
            // DPU verison bump in sync with the host-level version (and any
            // sibling DPUs already linked) network_config_version.
            self.update_dpu_network_config(&mut txn, &dpu_machine)
                .await?;
        }

        // Now since all DPUs are created, update host and DPUs state correctly.
        let host_machine_id =
            managed_host
                .machine_id
                .ok_or(SiteExplorerError::internal(format!(
                    "Failed to get machine ID for host: {managed_host:#?}"
                )))?;

        db::machine::update_state(
            &mut txn,
            &host_machine_id,
            &ManagedHostState::ConfigureAstra {
                configure_astra_state: ConfigureAstraState::EnableNics,
            },
        )
        .await?;

        let mut rack_profile_id = None;
        if let Some(rack_id) = machine_data.and_then(|d| d.rack_id.as_ref()) {
            tracing::info!(%rack_id, %host_machine_id, "Ensuring rack exists for host machine");
            if let Some(rack) = crate::ensure_rack_exists(&mut txn, rack_id).await? {
                tracing::info!(
                    %rack_id,
                    %host_machine_id,
                    rack = ?rack,
                    "Rack exists"
                );
                rack_profile_id = rack.rack_profile_id;
            }
        }

        // Own a declared integrated boot NIC so a managed-DPU host can boot from it
        // while its DPUs stay managed: the NIC becomes the host's HostInband
        // primary and the DPU admin links go dormant in the reconcile below.
        // Only for hosts with explored DPUs -- a zero-DPU host's NICs (including
        // a declared primary) are already owned by `create_zero_dpu_machine`.
        if !managed_host.explored_host.dpus.is_empty() {
            self.own_declared_host_boot_nic(&mut txn, &host_machine_id, report, machine_data)
                .await?;
        }

        // Normalize host admin address ownership after all DPU-backed host
        // interfaces have been attached and primary flags are final.
        self.reconcile_host_admin_addresses(&mut txn, &host_machine_id)
            .await?;

        reconcile_desired_boot_interface(
            &mut txn,
            // TODO: ManagedHost will eventually have a HostMachineId as its machine_id and
            // conversion will become unnecessary.
            &host_machine_id,
            primary_interface_selection,
            machine_data.and_then(ExpectedMachineData::declared_primary_mac),
        )
        .await?;

        let rms_node_identity = if let (Some(rack_id), Some(_)) =
            (&expected_machine.data.rack_id, &self.rms_client)
        {
            let Some(rack_profile_id) = rack_profile_id.as_ref() else {
                return Err(SiteExplorerError::InvalidArgument(format!(
                    "rack {rack_id} has no rack_profile_id for RMS slot and tray lookup for host machine {host_machine_id}"
                )));
            };

            let Some(rack_profile) = self.rack_profiles.get(rack_profile_id.as_str()) else {
                return Err(SiteExplorerError::InvalidArgument(format!(
                    "rack profile {rack_profile_id} is not configured for RMS slot and tray lookup for host machine {host_machine_id}"
                )));
            };

            Some(
                compute_node_identity_for_profile(rack_profile)
                    .map_err(|error| SiteExplorerError::InvalidArgument(error.to_string()))?,
            )
        } else {
            None
        };

        txn.commit().await?;

        if let (Some(rack_id), Some(rms_client), Some(node_identity)) = (
            &expected_machine.data.rack_id,
            &self.rms_client,
            rms_node_identity,
        ) {
            let mut node = rms::NodeInfo {
                node_id: host_machine_id.to_string(),
                rack_id: rack_id.to_string(),
                r#type: None,
                node_descriptor: None,
                bmc_endpoint: Some(rms::Endpoint {
                    interface: Some(rms::NetworkInterface {
                        ip_address: explored_host.host_bmc_ip.to_string(),
                        mac_address: expected_machine.bmc_mac_address.to_string(),
                        host_name: None,
                    }),
                    port: 443,
                    credentials: bmc_credentials.map(|(username, password)| rms::Credentials {
                        auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
                            username,
                            password,
                        })),
                    }),
                }),
                ..Default::default()
            };

            node_identity.apply_to_node_info(&mut node);

            let request = rms::BatchGetNodeDeviceInfoRequest {
                nodes: Some(rms::NodeSet { nodes: vec![node] }),
            };
            let (slot_number, tray_index) =
                crate::fetch_slot_and_tray(rms_client.as_ref(), request).await;
            let mut update_txn = Transaction::begin(pool).await?;
            if let Err(e) = db::machine::update_slot_and_tray(
                &mut update_txn,
                &host_machine_id,
                slot_number,
                tray_index,
            )
            .await
            {
                emit(SiteExplorerMachineSlotTrayPersistenceFailed::new(
                    e.to_string(),
                    host_machine_id.to_string(),
                ));
                update_txn
                    .rollback_or_log("site-explorer slot and tray update after operation failure")
                    .await;
            } else {
                update_txn.commit().await?;
            }
        }

        Ok(true)
    }

    // Returns MachineId if machine was created.
    async fn create_zero_dpu_machine(
        &self,
        txn: &mut PgConnection,
        managed_host: &ManagedHost<'_>,
        report: &mut EndpointExplorationReport,
        bmc_mac_address: MacAddress,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<Option<HostMachineId>> {
        // If there's already a machine with the same MAC address as this endpoint, return false. We
        // can't rely on matching the machine_id, as it may have migrated to a stable MachineID
        // already.
        let mac_addresses = host_mac_addresses_for_predicted_machine(report, machine_data);

        // Resolve each MAC's Redfish interface id from the live report up
        // front (`generate_machine_id` below takes a mutable borrow of the
        // report that lives for the rest of this function).
        let report_boot_interface_ids: Vec<(MacAddress, String)> = mac_addresses
            .iter()
            .filter_map(|mac| {
                report
                    .find_interface_id_for_mac(*mac)
                    .map(|id| (*mac, id.to_string()))
            })
            .collect();
        let endpoint_machine_id =
            db::machine_topology::find_machine_id_by_bmc_mac(&mut *txn, bmc_mac_address).await?;

        let mut existing_predicted_host_machine_id: Option<PredictedHostMachineId> = None;
        for mac_address in &mac_addresses {
            if let Some(machine) = db::machine::find_by_mac_address(txn, mac_address).await? {
                match machine.id.machine_id_subtype() {
                    MachineIdSubtype::StableHost(stable_machine_id) => {
                        // ExpectedMachine is ingestion policy, not a way to
                        // rewrite interfaces on an already managed host.
                        reconcile_desired_boot_interface(txn, &stable_machine_id, None, None)
                            .await?;
                        return Ok(None);
                    }
                    MachineIdSubtype::PredictedHost(predicted_machine_id) => {
                        if endpoint_machine_id.as_ref() != Some(&predicted_machine_id) {
                            tracing::warn!(
                                %mac_address,
                                predicted_machine_id = %machine.id,
                                ?endpoint_machine_id,
                                %bmc_mac_address,
                                "Could not confirm that the predicted interface belongs to the explored BMC; skipping refresh",
                            );
                            return Ok(None);
                        }
                        existing_predicted_host_machine_id = Some(predicted_machine_id);
                    }
                    MachineIdSubtype::Dpu(_) => {
                        // Preserve the existing collision behavior for a MAC
                        // already owned by a non-host machine.
                        return Ok(None);
                    }
                }
                continue;
            }

            // If we already minted this machine and it hasn't DHCP'd yet, a
            // prediction identifies the host that this refreshed report needs
            // to reconcile.
            if let Some(prediction) =
                db::predicted_machine_interface::find_by_mac_address(&mut *txn, *mac_address)
                    .await?
            {
                match prediction.machine_id.machine_id_subtype() {
                    MachineIdSubtype::StableHost(stable_machine_id) => {
                        reconcile_desired_boot_interface(
                            txn,
                            stable_machine_id.as_host_machine_id(),
                            None,
                            None,
                        )
                        .await?;
                        return Ok(None);
                    }
                    MachineIdSubtype::PredictedHost(predicted_machine_id) => {
                        if endpoint_machine_id.as_ref() != Some(&predicted_machine_id) {
                            tracing::warn!(
                                %mac_address,
                                predicted_machine_id = %prediction.machine_id,
                                ?endpoint_machine_id,
                                %bmc_mac_address,
                                "Could not confirm that the predicted interface belongs to the explored BMC; skipping refresh",
                            );
                            return Ok(None);
                        }
                        existing_predicted_host_machine_id = Some(predicted_machine_id);
                    }
                    _ => return Ok(None),
                }
            }
        }

        // A declared primary remains authoritative while the host is awaiting
        // its first lease. Without one, retained boot metadata is only a
        // fallback when the predicted host has no primary row or prediction;
        // an unrelated retained interface must not replace a settled primary
        // during an ordinary refresh.
        let declared_primary = machine_data.and_then(|data| data.declared_primary_mac());
        let has_existing_primary = if declared_primary.is_none()
            && let Some(machine_id) = existing_predicted_host_machine_id
        {
            db::machine_interface::machine_has_primary_interface(&machine_id, &mut *txn).await?
                || db::predicted_machine_interface::find_by_machine_id(&mut *txn, &machine_id)
                    .await?
                    .iter()
                    .any(|prediction| prediction.primary_interface)
        } else {
            false
        };
        let primary_mac = match declared_primary {
            Some(declared) => Some(declared),
            None if !has_existing_primary => {
                let mut recovered = None;
                for mac_address in &mac_addresses {
                    if db::retained_boot_interface::find_by_mac(
                        &mut *txn,
                        *mac_address,
                        self.config.retained_boot_interface_window,
                    )
                    .await?
                    .is_some()
                    {
                        recovered = Some(*mac_address);
                        break;
                    }
                }
                recovered
            }
            None => None,
        };

        if let Some(machine_id) = existing_predicted_host_machine_id {
            Self::reconcile_zero_dpu_host_interfaces(
                txn,
                &machine_id,
                &mac_addresses,
                &report_boot_interface_ids,
                primary_mac,
            )
            .await?;
            reconcile_desired_boot_interface(
                txn,
                machine_id.as_host_machine_id(),
                None,
                declared_primary,
            )
            .await?;
            return Ok(None);
        }

        let machine_id = match managed_host.machine_id.as_ref() {
            Some(machine_id) => *machine_id,
            None => {
                // Mint a predicted-host machine_id from the exploration report
                PredictedHostMachineId::try_from(*report.generate_machine_id(true)?.unwrap())?
                    .into()
            }
        };

        tracing::info!(%machine_id, "Minted predicted host ID for zero-DPU machine");

        let existing_machine = db::machine::find_one(
            &mut *txn,
            &machine_id,
            MachineSearchConfig {
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?;

        if let Some(existing_machine) = existing_machine {
            // There's already a machine with this ID, but we already looked above for machines with
            // the same MAC address as this one, so something's weird here. Log this host's mac
            // addresses and the ones from the colliding hosts to help in diagnosis.
            let existing_macs = existing_machine
                .status
                .hardware_info
                .as_ref()
                .map(|hw| hw.all_mac_addresses())
                .unwrap_or_default();
            tracing::warn!(
                %machine_id,
                existing_mac_addresses = ?existing_macs,
                predicted_host_mac_addresses = ?mac_addresses,
                "Predicted host already exists, with different mac addresses from this one. Potentially multiple machines with same serial number?"
            );
            reconcile_desired_boot_interface(txn, &existing_machine.id.try_into()?, None, None)
                .await?;
            return Ok(None);
        }

        self.create_machine_from_explored_managed_host(
            txn,
            managed_host,
            &machine_id,
            machine_data,
        )
        .await?;

        Self::reconcile_zero_dpu_host_interfaces(
            txn,
            &machine_id,
            &mac_addresses,
            &report_boot_interface_ids,
            primary_mac,
        )
        .await?;

        Ok(Some(machine_id))
    }

    /// Reconciles every discovered Host candidate with a zero-DPU host.
    ///
    /// This runs for both a newly minted host and a refreshed report for an
    /// existing predicted host. That second path matters when the BMC learns a
    /// declared NIC through supplemental adapter-Port inventory after it has
    /// already reported other System interfaces.
    async fn reconcile_zero_dpu_host_interfaces(
        txn: &mut PgConnection,
        machine_id: &MachineId,
        mac_addresses: &[MacAddress],
        report_boot_interface_ids: &[(MacAddress, String)],
        primary_mac: Option<MacAddress>,
    ) -> SiteExplorerResult<()> {
        // If the selected primary has already DHCP'd, settle the host's real
        // rows before adopting it so the partial unique index never sees two
        // primary interfaces at once. A pending prediction does not require
        // demoting the currently leased primary; DHCP promotion handles that
        // transition when the selected NIC actually arrives.
        let selected_primary = primary_mac.filter(|mac| mac_addresses.contains(mac));
        let selected_primary_has_real_row = if let Some(primary_mac) = selected_primary
            && let Some(primary_interface) =
                db::machine_interface::find_by_mac_address(&mut *txn, primary_mac)
                    .await?
                    .into_iter()
                    .min_by_key(|interface| interface.interface_type == InterfaceType::Bmc)
            && primary_interface.interface_type != InterfaceType::Bmc
            && primary_interface
                .machine_id
                .is_none_or(|existing_machine_id| existing_machine_id == *machine_id)
        {
            if primary_interface.machine_id.is_none() || !primary_interface.primary_interface {
                db::machine_interface::demote_primary_interfaces_for_machine(machine_id, txn)
                    .await?;
            }
            true
        } else {
            false
        };
        if let Some(primary_mac) = selected_primary {
            db::predicted_machine_interface::demote_other_primary_interfaces_for_machine(
                machine_id,
                primary_mac,
                txn,
            )
            .await?;
        }

        // Route already-leased rows and pending predictions through the same
        // declaration so a refresh can add inventory without minting another
        // host or depending on DHCP arrival order.
        for mac_address in mac_addresses.iter().copied() {
            let is_primary = selected_primary == Some(mac_address);
            if let Some(machine_interface) =
                db::machine_interface::find_by_mac_address(&mut *txn, mac_address)
                    .await?
                    .into_iter()
                    .min_by_key(|interface| interface.interface_type == InterfaceType::Bmc)
            {
                // There's already a machine_interface with this MAC...
                if let Some(existing_machine_id) = machine_interface.machine_id {
                    // Same machine_id means the preallocated BMC interface row we
                    // just attached via update_machine_topology(), not a contradiction.
                    if existing_machine_id == *machine_id {
                        // Preserve a settled primary when no declaration (or
                        // retained fallback) selects a replacement. Likewise,
                        // a primary that exists only as a prediction takes over
                        // at DHCP promotion, not while it is still pending.
                        // Once the selected primary is an owned row, reconcile
                        // every owned row to the machine-wide choice.
                        let want_primary =
                            is_primary && machine_interface.interface_type != InterfaceType::Bmc;
                        if (selected_primary_has_real_row
                            || machine_interface.interface_type == InterfaceType::Bmc)
                            && machine_interface.primary_interface != want_primary
                        {
                            db::machine_interface::set_primary_interface(
                                &machine_interface.id,
                                want_primary,
                                txn,
                            )
                            .await?;
                        }
                        continue;
                    }
                    // Different machine_id contradicts the find_by_mac() above.
                    tracing::error!(
                        %mac_address,
                        %machine_id,
                        %existing_machine_id,
                        "BUG! Found existing machine_interface with this MAC address, we should not have gotten here!"
                    );
                    return Err(SiteExplorerError::AlreadyFoundError {
                        kind: "MachineInterface",
                        id: mac_address.to_string(),
                    });
                } else {
                    // ...If it has no MachineId, the host must have DHCP'd before site-explorer ran.
                    // Reconcile its primary flag to the declaration before adopting it: an anonymous
                    // DHCP row defaults to primary=true, so without this two pre-ingestion leases
                    // would both arrive primary and collide on association.
                    if machine_interface.primary_interface != is_primary {
                        db::machine_interface::set_primary_interface(
                            &machine_interface.id,
                            is_primary,
                            txn,
                        )
                        .await?;
                    }
                    tracing::info!(%mac_address, %machine_id, "Migrating unowned machine_interface to new managed host");
                    db::machine_interface::associate_interface_with_machine(
                        &machine_interface.id,
                        MachineInterfaceAssociation::Machine(*machine_id),
                        txn,
                    )
                    .await?;
                }
                continue;
            }

            let boot_interface_id = report_boot_interface_ids
                .iter()
                .find(|(mac, _)| *mac == mac_address)
                .map(|(_, id)| id.clone());

            if let Some(prediction) =
                db::predicted_machine_interface::find_by_mac_address(&mut *txn, mac_address).await?
            {
                if prediction.machine_id != *machine_id {
                    tracing::error!(
                        %mac_address,
                        %machine_id,
                        existing_machine_id = %prediction.machine_id,
                        "BUG! Found existing predicted_machine_interface with this MAC address, we should not have gotten here!",
                    );
                    return Err(SiteExplorerError::AlreadyFoundError {
                        kind: "PredictedMachineInterface",
                        id: mac_address.to_string(),
                    });
                }
                // An absent declaration is not an instruction to clear a
                // retained or previously settled prediction. A real primary
                // selection is authoritative across every pending candidate.
                if is_primary && !prediction.primary_interface {
                    db::predicted_machine_interface::set_primary_interface(
                        prediction.id,
                        true,
                        txn,
                    )
                    .await?;
                }
                if let Some(boot_interface_id) = boot_interface_id.as_deref()
                    && prediction.boot_interface_id.as_deref() != Some(boot_interface_id)
                {
                    db::predicted_machine_interface::set_boot_interface_id(
                        txn,
                        mac_address,
                        boot_interface_id,
                    )
                    .await?;
                }
            } else {
                // Give the predicted interface its boot interface id when
                // the live report resolves one, so the promoted row starts
                // with the full boot pair. Retained ids are deliberately
                // NOT copied here: a prediction has no recorded_at, so a
                // copy would dodge the `retained_boot_interface_window`
                // check. The retained pair instead lands on the row at
                // creation (see `create_with_type`), where the window is
                // checked at DHCP time.
                db::predicted_machine_interface::create(
                    NewPredictedMachineInterface {
                        machine_id,
                        mac_address,
                        expected_network_segment_type: NetworkSegmentType::HostInband,
                        boot_interface_id,
                        primary_interface: is_primary,
                    },
                    txn,
                )
                .await?;
            }
        }

        Ok(())
    }

    /// Owns a declared integrated (non-DPU) host NIC as a managed-DPU host's
    /// HostInband boot interface, so a host with managed DPUs can still boot from
    /// an integrated NIC. The NIC carries `primary` into `machine_interfaces` on
    /// its first DHCP; the DPUs stay explored and linked, and their admin links
    /// go dormant in `reconcile_admin_addresses_for_host` once this NIC is the
    /// primary.
    ///
    /// Mirrors the host-NIC ownership in `create_zero_dpu_machine`, but for the
    /// one declared NIC reached from the managed-DPU path. No-op when nothing is
    /// declared, or when the declared NIC is already owned (e.g. a declared DPU
    /// host-PF, which `attach_dpu_to_host` already owns).
    async fn own_declared_host_boot_nic(
        &self,
        txn: &mut PgConnection,
        host_machine_id: &HostMachineId,
        report: &EndpointExplorationReport,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<()> {
        let Some(declared_mac) = machine_data.and_then(|data| data.declared_primary_mac()) else {
            return Ok(());
        };

        if let Some(existing) = db::machine_interface::find_by_mac_address(&mut *txn, declared_mac)
            .await?
            .into_iter()
            .next()
        {
            if let Some(existing_machine_id) = existing.machine_id {
                // Owned by THIS host already (e.g. a declared DPU host-PF): its
                // primary flag is settled by the DPU attach / promotion paths.
                // Owned by a DIFFERENT machine: the declaration names a MAC that
                // already belongs elsewhere -- surface it rather than silently
                // dropping the declared boot NIC (mirrors create_zero_dpu_machine).
                if existing_machine_id != host_machine_id.into() {
                    return Err(SiteExplorerError::AlreadyFoundError {
                        kind: "MachineInterface",
                        id: declared_mac.to_string(),
                    });
                }
                return Ok(());
            }
            // The integrated NIC leased before ingestion: adopt its anonymous row
            // as the host's sole primary, demoting the interim DPU primary so the
            // two never collide on `one_primary_interface_per_machine`.
            db::machine_interface::demote_primary_interfaces_for_machine(host_machine_id, txn)
                .await?;
            if !existing.primary_interface {
                db::machine_interface::set_primary_interface(&existing.id, true, txn).await?;
            }
            db::machine_interface::associate_interface_with_machine(
                &existing.id,
                MachineInterfaceAssociation::Machine((*host_machine_id).into()),
                txn,
            )
            .await?;
            tracing::info!(
                declared_mac_address = %declared_mac, %host_machine_id,
                "Adopted declared integrated boot NIC as the managed-DPU host's primary",
            );
            return Ok(());
        }

        // A prediction may already exist from a prior ingestion of this host --
        // don't mint a second one. One owned by a different machine is the same
        // contradiction as the machine_interface case above, so surface it rather
        // than silently dropping the declaration.
        if let Some(existing_prediction) =
            db::predicted_machine_interface::find_by_mac_address(&mut *txn, declared_mac).await?
        {
            if existing_prediction.machine_id != (*host_machine_id).into() {
                return Err(SiteExplorerError::AlreadyFoundError {
                    kind: "PredictedMachineInterface",
                    id: declared_mac.to_string(),
                });
            }
            return Ok(());
        }

        // Not yet leased: mint a HostInband prediction carrying primary, so the
        // NIC is adopted and made primary on its first DHCP (the promotion demotes
        // the interim DPU primary).
        let boot_interface_id = report
            .find_interface_id_for_mac(declared_mac)
            .map(|id| id.to_string());
        db::predicted_machine_interface::create(
            NewPredictedMachineInterface {
                machine_id: host_machine_id,
                mac_address: declared_mac,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id,
                primary_interface: true,
            },
            txn,
        )
        .await?;
        tracing::info!(
            declared_mac_address = %declared_mac, %host_machine_id,
            "Minted HostInband boot-NIC prediction for managed-DPU host's declared integrated primary",
        );
        Ok(())
    }

    // create_dpu does everything needed to create a DPU as part of a newly discovered managed host.
    // If the DPU does not exist in the machines table, the function creates a new DPU machine and
    // configures it appropriately, returning the new `Machine`.
    // If the DPU already exists in the machines table, this is a no-op and returns `None`.
    //
    // `db::machine::create` can persist pool-backed defaults on the new row.
    // The full `network_config` reconciliation and version bump wait until
    // `attach_dpu_to_host` wires the host link in `machine_interfaces`; only
    // then can `try_update_network_config` observe both siblings and keep their
    // versions equal.
    async fn create_dpu(
        &self,
        txn: &mut PgConnection,
        explored_dpu: &ExploredDpu,
    ) -> SiteExplorerResult<Option<Machine>> {
        if let Some(dpu_machine) = self.create_dpu_machine(txn, explored_dpu).await? {
            self.configure_dpu_interface(txn, explored_dpu).await?;
            let dpu_machine_id: &MachineId = explored_dpu.report.machine_id.as_ref().unwrap();
            let dpu_bmc_info = explored_dpu.bmc_info();
            let dpu_hw_info = explored_dpu.hardware_info()?;
            self.update_machine_topology(txn, dpu_machine_id, dpu_bmc_info, dpu_hw_info)
                .await?;
            return Ok(Some(dpu_machine));
        }
        Ok(None)
    }

    // 1) Create a machine for this host using the passed machine_id
    // 2) Update the "machine_topologies" table with the bmc info for this host
    async fn create_machine_from_explored_managed_host(
        &self,
        txn: &mut PgConnection,
        managed_host: &ManagedHost<'_>,
        predicted_machine_id: &MachineId,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<()> {
        _ = db::machine::create(
            txn,
            Some(&self.common_pools),
            predicted_machine_id,
            ManagedHostState::Created,
            machine_data,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        let hardware_info = HardwareInfo::default();
        self.update_machine_topology(
            txn,
            predicted_machine_id,
            managed_host.explored_host.bmc_info(),
            hardware_info,
        )
        .await
    }

    // Ensure the DPU's OOB interface is owned as soon as it appears. If DHCP has already created
    // the interface, associate it directly. Otherwise record a trusted prediction that DHCP can
    // promote atomically. DiscoverMachine requires this ownership before returning credentials, so
    // deferring the association to a later Site Explorer sweep would strand the DPU in discovery.
    async fn configure_dpu_interface(
        &self,
        txn: &mut PgConnection,
        explored_dpu: &ExploredDpu,
    ) -> SiteExplorerResult<bool> {
        let dpu_machine_id: &MachineId = explored_dpu.report.machine_id.as_ref().unwrap();
        let oob_net0_mac = explored_dpu.report.systems.iter().find_map(|x| {
            x.ethernet_interfaces.iter().find_map(|x| {
                if x.id
                    .as_ref()
                    .is_some_and(|id| id.to_lowercase().contains("oob"))
                {
                    x.mac_address
                } else {
                    None
                }
            })
        });

        // If machine_interface exists for the DPU and machine_id is not updated, do it now.
        if let Some(oob_net0_mac) = oob_net0_mac {
            let mi = db::machine_interface::find_by_mac_address(&mut *txn, oob_net0_mac).await?;

            if let Some(interface) = mi.first()
                && interface.machine_id.is_none()
            {
                tracing::info!(
                    machine_interface_id = %interface.id,
                    machine_id = %dpu_machine_id,
                    "Associating machine interface with machine"
                );
                db::machine_interface::associate_interface_with_machine(
                    &interface.id,
                    MachineInterfaceAssociation::Machine(*dpu_machine_id),
                    txn,
                )
                .await?;
                db::machine_interface::associate_interface_with_dpu_machine(
                    &interface.id,
                    dpu_machine_id,
                    txn,
                )
                .await?;
                return Ok(true);
            }

            if mi.is_empty() {
                if let Some(prediction) =
                    db::predicted_machine_interface::find_by_mac_address(&mut *txn, oob_net0_mac)
                        .await?
                {
                    if prediction.machine_id != *dpu_machine_id {
                        return Err(SiteExplorerError::AlreadyFoundError {
                            kind: "PredictedMachineInterface",
                            id: oob_net0_mac.to_string(),
                        });
                    }
                    return Ok(false);
                }

                db::predicted_machine_interface::create(
                    NewPredictedMachineInterface {
                        machine_id: dpu_machine_id,
                        mac_address: oob_net0_mac,
                        expected_network_segment_type: NetworkSegmentType::Underlay,
                        boot_interface_id: None,
                        primary_interface: true,
                    },
                    txn,
                )
                .await?;
                tracing::info!(
                    machine_id = %dpu_machine_id,
                    mac_address = %oob_net0_mac,
                    "Created predicted DPU OOB interface"
                );
                return Ok(true);
            }
        }

        Ok(false)
    }

    // create_dpu_machine creates a machine for the DPU as specified by dpu_machine_id. Returns an Optional Machine indicating whether the function created a new machine (returns None if a machine already existed for this DPU).
    // if an entry exists in the machines table with a machine ID which matches dpu_machine_id, a machine has already been created for this DPU. Returns None.
    // if an entry doesnt exist in the machine table, the site explorer will add an entry in the machines table for the DPU and update its network config appropriately (allocating a loop ip address etc). Return the newly created machine.
    async fn create_dpu_machine(
        &self,
        txn: &mut PgConnection,
        explored_dpu: &ExploredDpu,
    ) -> SiteExplorerResult<Option<Machine>> {
        let dpu_machine_id = explored_dpu.report.machine_id.as_ref().unwrap();
        match db::machine::find_one(&mut *txn, dpu_machine_id, MachineSearchConfig::default())
            .await?
        {
            // Do nothing if machine exists. It'll be reprovisioned via redfish
            Some(_existing_machine) => Ok(None),
            None => match db::machine::create(
                txn,
                Some(&self.common_pools),
                dpu_machine_id,
                ManagedHostState::Created,
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await
            {
                Ok(machine) => {
                    tracing::info!(machine_id = %dpu_machine_id, "Created DPU machine");
                    Ok(Some(machine))
                }
                Err(e) => {
                    tracing::error!(error = %e, "Can't create DPU machine");
                    Err(e.into())
                }
            },
        }
    }

    async fn attach_dpu_to_host(
        &self,
        txn: &mut PgConnection,
        explored_host: &ManagedHost<'_>,
        explored_dpu: &ExploredDpu,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<HostMachineId> {
        let dpu_hw_info = explored_dpu.hardware_info()?;
        // Create Host proactively.
        // In case host interface is created, this method will return existing one, instead
        // creating new everytime.
        let host_machine_interface =
            db::machine_interface::create_host_machine_dpu_interface_proactively(
                txn,
                Some(&dpu_hw_info),
                explored_dpu.report.machine_id.as_ref().unwrap(),
                self.config.retained_boot_interface_window,
            )
            .await?;

        if host_machine_interface.machine_id.is_some() {
            return Err(SiteExplorerError::internal(format!(
                "The host's machine interface for DPU {} already has the machine ID set--something is wrong: {:#?}",
                explored_dpu.report.machine_id.as_ref().unwrap(),
                host_machine_interface
            )));
        }

        let host_machine_id = self
            .configure_host_machine(
                txn,
                explored_host,
                &host_machine_interface,
                explored_dpu,
                machine_data,
            )
            .await?;

        db::machine_interface::associate_interface_with_machine(
            &host_machine_interface.id,
            MachineInterfaceAssociation::Machine(host_machine_id.into()),
            txn,
        )
        .await?;

        Ok(host_machine_id)
    }

    async fn update_machine_topology(
        &self,
        txn: &mut PgConnection,
        machine_id: &MachineId,
        mut bmc_info: BmcInfo,
        hardware_info: HardwareInfo,
    ) -> SiteExplorerResult<()> {
        let _topology =
            db::machine_topology::create_or_update(txn, machine_id, &hardware_info).await?;

        // Forge scout will update this topology with a full information.
        db::machine_topology::set_topology_update_needed(txn, machine_id, true).await?;

        // call enrich_mac_address to fill the MAC address info from the machine_interfaces table
        db::bmc_metadata::enrich_mac_address(
            &mut bmc_info,
            "SiteExplorer::update_machine_topology".to_string(),
            txn,
            machine_id,
            true,
        )
        .await?;

        db::bmc_metadata::update_bmc_network_into_machine_interfaces(
            txn,
            machine_id,
            &mut bmc_info,
        )
        .await?;

        Ok(())
    }

    async fn update_dpu_network_config(
        &self,
        txn: &mut PgConnection,
        dpu_machine: &Machine,
    ) -> SiteExplorerResult<()> {
        let (mut network_config, version) = dpu_machine.network_config.clone().take();
        if network_config.loopback_ip.is_none() {
            let loopback_ip = db::machine::allocate_loopback_ip(
                &self.common_pools,
                txn,
                &dpu_machine.id.to_string(),
            )
            .await?;
            network_config.loopback_ip = Some(loopback_ip);
        }

        if network_config.loopback_ip_v6.is_none() {
            network_config.loopback_ip_v6 = db::machine::allocate_loopback_ip_v6(
                &self.common_pools,
                txn,
                &dpu_machine.id.to_string(),
            )
            .await?;
        }

        // A stale version must fail the whole transaction so any addresses
        // allocated above return to their pools.
        if !db::machine::try_update_network_config(txn, &dpu_machine.id, version, &network_config)
            .await?
        {
            return Err(db::DatabaseError::ConcurrentModificationError(
                "machine",
                version.to_string(),
            )
            .into());
        }

        Ok(())
    }

    /// Reconciles host admin addresses and bumps visible host network config when needed.
    ///
    /// Returns whether the active admin config changed.
    async fn reconcile_host_admin_addresses(
        &self,
        txn: &mut PgConnection,
        host_machine_id: &HostMachineId,
    ) -> SiteExplorerResult<bool> {
        let active_config_changed =
            db::machine_interface::reconcile_admin_addresses_for_host(txn, host_machine_id).await?;
        if active_config_changed {
            let (network_config, network_config_version) =
                db::machine::get_network_config(&mut *txn, host_machine_id)
                    .await?
                    .take();
            db::machine::try_update_network_config(
                txn,
                host_machine_id,
                network_config_version,
                &network_config,
            )
            .await?;
        }
        Ok(active_config_changed)
    }

    // configure_host_machine configures the host's machine with the specific interface. It returns the host's machine ID.
    //
    // Normally, a host will have a single machine interface because the majority of hosts (for now) have a single DPU.
    // If a host has multiple DPUs, the host machine will have a machine interface for each DPU.
    // However, all of the host machine interfaces must be attached to the same host machine (and host machine-id).
    // Until this point, all of these interfaces will be marked as the "primary" interface by default.
    //
    // configure_host_machine handles two cases:
    // 1) host_machine_interface is the primary interface for this host: generate the machine ID for this host and use it to actually create the machine for the host.
    // 2) host_machine_interface is *not* the primary interface for this host: set "primary_interface" to false for this machine interface. Return the host ID generated from (1)
    //
    // The first DPU that we attach to the host is designated as the primary DPU; the associate host machine interface is designated is the primary interface.
    // Therefore, the primary interface is guaranteed to be configured prior to any secondary interface.
    #[allow(clippy::too_many_arguments)]
    async fn configure_host_machine(
        &self,
        txn: &mut PgConnection,
        explored_host: &ManagedHost<'_>,
        host_machine_interface: &MachineInterfaceSnapshot,
        explored_dpu: &ExploredDpu,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<HostMachineId> {
        match &explored_host.machine_id {
            Some(host_machine_id) => {
                // This is not the primary interface for this host
                // The primary interface *must* have already been created for this host (otherwise something very bad has happened)
                db::machine_interface::set_primary_interface(
                    &host_machine_interface.id,
                    false,
                    txn,
                )
                .await?;
                Ok(*host_machine_id)
            }
            None => {
                // This is the primary interface for the host.
                // 1. Generate the ID for the host from *this* DPU's hw info
                // 2. Add an entry for this host in the machines table (with a machine-id from (1)).
                let host_machine_id = self
                    .create_host_from_dpu_hw_info(
                        txn,
                        explored_host.explored_host,
                        explored_dpu,
                        machine_data,
                    )
                    .await?;

                tracing::info!(
                    ?host_machine_interface.id,
                    machine_id = %host_machine_id,
                    "Created host machine proactively in site-explorer",
                );

                db::machine_interface::set_primary_interface(&host_machine_interface.id, true, txn)
                    .await?;
                Ok(host_machine_id.into())
            }
        }
    }

    // 1) Generate the host's machine ID from the DPU's hardware info
    // 2) Create a machine for this host using the machine ID from (1)
    // 3) Update the "machine_topologies" table with the bmc info for this host
    async fn create_host_from_dpu_hw_info(
        &self,
        txn: &mut PgConnection,
        explored_host: &ExploredManagedHost,
        explored_dpu: &ExploredDpu,
        machine_data: Option<&ExpectedMachineData>,
    ) -> SiteExplorerResult<PredictedHostMachineId> {
        let dpu_hw_info = explored_dpu.hardware_info()?;
        let predicted_machine_id = host_id_from_dpu_hardware_info(&dpu_hw_info).map_err(|e| {
            SiteExplorerError::InvalidArgument(format!("hardware info missing: {e}"))
        })?;

        let _host_machine = db::machine::create(
            txn,
            Some(&self.common_pools),
            &predicted_machine_id,
            ManagedHostState::Created,
            machine_data,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;

        let host_bmc_info = explored_host.bmc_info();
        let host_hardware_info = HardwareInfo::default();
        self.update_machine_topology(
            txn,
            &predicted_machine_id,
            host_bmc_info,
            host_hardware_info,
        )
        .await?;

        Ok(PredictedHostMachineId::try_from(predicted_machine_id)?)
    }
}

/// Initializes the durable desired boot interface from Site Explorer's settled
/// interface ownership, or enriches a persisted target that has only a MAC
/// with a newly observed Redfish interface ID.
///
/// The initial read does not lock, so a completed pair does not serialize
/// unrelated machine updates on every exploration. The mutation helpers lock
/// and recheck the two allowed transitions: unset to initialized, and a
/// `MacOnly` target to a `Pair` for the same MAC.
async fn reconcile_desired_boot_interface(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    primary_interface_selection: Option<HostPrimaryInterfaceSelection>,
    declared_primary: Option<MacAddress>,
) -> SiteExplorerResult<()> {
    let desired = db::machine_desired_boot_interface::get(&mut *txn, machine_id).await?;

    if matches!(
        desired.as_ref().map(|desired| &desired.value),
        Some(MachineBootInterfaceTarget::Pair(_))
    ) {
        return Ok(());
    }

    // `find_by_machine_ids` removes BMC rows. Keeping that boundary in the
    // database query also lets `pick_boot_interface` borrow this list directly.
    let mut interfaces_by_machine =
        db::machine_interface::find_by_machine_ids(txn, &[*machine_id]).await?;
    let interfaces = interfaces_by_machine.remove(machine_id).unwrap_or_default();
    let predictions = db::predicted_machine_interface::find_by_machine_id(txn, machine_id).await?;
    let update = desired_boot_interface_update(
        desired.as_ref().map(|desired| &desired.value),
        &interfaces,
        &predictions,
    );

    match (desired.as_ref().map(|desired| &desired.value), update) {
        (None, Some(target)) => {
            let selection_source =
                initial_selection_source(&target, primary_interface_selection, declared_primary);
            db::machine_desired_boot_interface::initialize_if_unset(
                txn,
                machine_id,
                &target,
                selection_source,
            )
            .await?;
        }
        (
            Some(MachineBootInterfaceTarget::MacOnly(mac_address)),
            Some(MachineBootInterfaceTarget::Pair(target)),
        ) => {
            debug_assert_eq!(*mac_address, target.mac_address);
            db::machine_desired_boot_interface::enrich_interface_id(
                txn,
                machine_id,
                *mac_address,
                &target.interface_id,
            )
            .await?;
        }
        _ => {}
    }

    Ok(())
}

/// Returns `ExpectedMachine` when the declared MAC became durable, otherwise
/// returns the report source only when its MAC matches the durable target.
/// `LegacyUnknown` covers every other target, including one chosen by a report
/// fallback when Redfish could not resolve the declared interface and the
/// fallback selected a different MAC.
fn initial_selection_source(
    target: &MachineBootInterfaceTarget,
    report_selection: Option<HostPrimaryInterfaceSelection>,
    declared_primary: Option<MacAddress>,
) -> BootInterfaceSelectionSource {
    let target_mac = target.mac_address();
    if declared_primary == Some(target_mac) {
        return BootInterfaceSelectionSource::ExpectedMachine;
    }

    report_selection
        .filter(|selection| selection.mac_address == target_mac)
        .map_or(BootInterfaceSelectionSource::LegacyUnknown, |selection| {
            selection.source
        })
}

/// Returns the only desired-target transition Site Explorer should attempt.
///
/// An existing pair is already complete. An existing MAC-only value is
/// already designated, so only a unique id for that same MAC may enrich it.
/// With no value, an explicitly primary prediction outranks the interim owned
/// interface (the integrated-NIC-before-first-DHCP case); otherwise owned
/// interfaces win, with an unambiguous prediction as the final fallback.
/// Once the MAC is selected, all same-MAC records resolve its id so a current
/// owned id can complete an id-less prediction.
fn desired_boot_interface_update(
    desired: Option<&MachineBootInterfaceTarget>,
    interfaces: &[MachineInterfaceSnapshot],
    predictions: &[PredictedMachineInterface],
) -> Option<MachineBootInterfaceTarget> {
    match desired {
        Some(MachineBootInterfaceTarget::Pair(_)) => None,
        Some(MachineBootInterfaceTarget::MacOnly(mac_address)) => {
            resolve_interface_id(*mac_address, interfaces, predictions).map(|interface_id| {
                MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address: *mac_address,
                    interface_id,
                })
            })
        }
        None => {
            let prediction = pick_boot_prediction(predictions);
            let selected_mac = prediction
                .filter(|prediction| prediction.primary_interface)
                .map(|prediction| prediction.mac_address)
                .or_else(|| pick_boot_interface(interfaces).map(|interface| interface.mac_address))
                .or_else(|| prediction.map(|prediction| prediction.mac_address));
            selected_mac.map(|mac_address| {
                target_with_resolved_interface_id(mac_address, interfaces, predictions)
            })
        }
    }
}

fn target_with_resolved_interface_id(
    mac_address: MacAddress,
    interfaces: &[MachineInterfaceSnapshot],
    predictions: &[PredictedMachineInterface],
) -> MachineBootInterfaceTarget {
    match resolve_interface_id(mac_address, interfaces, predictions) {
        Some(interface_id) => MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id,
        }),
        None => MachineBootInterfaceTarget::MacOnly(mac_address),
    }
}

enum InterfaceIdResolution<'a> {
    Missing,
    Unique(&'a str),
    Ambiguous,
}

fn unique_interface_id<'a>(
    interface_ids: impl Iterator<Item = &'a str>,
) -> InterfaceIdResolution<'a> {
    let mut interface_ids = interface_ids
        .filter_map(canonical_redfish_boot_interface_id)
        .unique();
    match (interface_ids.next(), interface_ids.next()) {
        (None, _) => InterfaceIdResolution::Missing,
        (Some(interface_id), None) => InterfaceIdResolution::Unique(interface_id),
        (Some(_), Some(_)) => InterfaceIdResolution::Ambiguous,
    }
}

fn resolve_interface_id(
    mac_address: MacAddress,
    interfaces: &[MachineInterfaceSnapshot],
    predictions: &[PredictedMachineInterface],
) -> Option<String> {
    let owned = unique_interface_id(
        interfaces
            .iter()
            .filter(|interface| interface.mac_address == mac_address)
            .filter_map(|interface| interface.boot_interface_id.as_deref()),
    );
    match owned {
        InterfaceIdResolution::Unique(interface_id) => return Some(interface_id.to_string()),
        InterfaceIdResolution::Ambiguous => return None,
        InterfaceIdResolution::Missing => {}
    }

    match unique_interface_id(
        predictions
            .iter()
            .filter(|prediction| prediction.mac_address == mac_address)
            .filter_map(|prediction| prediction.boot_interface_id.as_deref()),
    ) {
        InterfaceIdResolution::Unique(interface_id) => Some(interface_id.to_string()),
        InterfaceIdResolution::Missing | InterfaceIdResolution::Ambiguous => None,
    }
}

/// `host_mac_addresses_for_predicted_machine` finds the Host interfaces used to
/// mint `predicted_machine_interface` rows for a zero-DPU host.
///
/// System EthernetInterfaces remain Host candidates. Adapter Ports are broader
/// chassis inventory, so when at least one usable System interface exists we
/// include only discovered Port MACs that ExpectedMachine also declares as
/// Host interfaces. When no usable System MAC exists, discovered Ports preserve
/// the existing hardware fallback alongside any System MACs. ExpectedMachine
/// supplies MACs by itself only when Redfish reports neither collection.
fn host_mac_addresses_for_predicted_machine(
    report: &EndpointExplorationReport,
    machine_data: Option<&ExpectedMachineData>,
) -> Vec<MacAddress> {
    let system_mac_addresses = report
        .systems
        .iter()
        .flat_map(|system| &system.ethernet_interfaces)
        .filter_map(|interface| interface.mac_address)
        .unique()
        .collect::<Vec<_>>();
    let has_usable_system_mac_address = report
        .systems
        .iter()
        .flat_map(|system| &system.ethernet_interfaces)
        .any(|interface| {
            interface.interface_enabled != Some(false) && interface.mac_address.is_some()
        });
    let port_mac_addresses = report
        .chassis
        .iter()
        .flat_map(|chassis| &chassis.network_adapters)
        .flat_map(|adapter| adapter.port_mac_addresses.iter().copied())
        .unique()
        .collect::<Vec<_>>();
    let is_host_report = !(report.is_dpu() || report.is_switch() || report.is_power_shelf());

    if has_usable_system_mac_address {
        let declared_host_mac_addresses = machine_data
            .into_iter()
            .flat_map(|data| &data.interfaces)
            .filter(|interface| interface.role.is_host())
            .map(|interface| interface.mac_address)
            .collect::<Vec<_>>();

        return system_mac_addresses
            .into_iter()
            .chain(port_mac_addresses.into_iter().filter(|mac_address| {
                is_host_report && declared_host_mac_addresses.contains(mac_address)
            }))
            .unique()
            .collect();
    }

    if !port_mac_addresses.is_empty() && is_host_report {
        return system_mac_addresses
            .into_iter()
            .chain(port_mac_addresses)
            .unique()
            .collect();
    }
    if !system_mac_addresses.is_empty() {
        return system_mac_addresses;
    }

    machine_data
        .filter(|_| is_host_report)
        .map(|data| {
            data.interfaces
                .iter()
                .filter(|interface| interface.role.is_host())
                .map(|interface| interface.mac_address)
                .unique()
                .collect::<Vec<_>>()
        })
        .none_if_empty()
        .inspect(|host_mac_addresses| {
            tracing::info!(
                host_nic_count = host_mac_addresses.len(),
                "Redfish host MAC inventory missing; using ExpectedMachine.interfaces for predicted machine interfaces"
            );
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values};
    use model::expected_machine::{ExpectedInterface, ExpectedInterfaceRole};
    use model::site_explorer::{Chassis, ComputerSystem, EthernetInterface, NetworkAdapter};

    use super::*;

    /// Redfish inventory stays authoritative, ExpectedMachine narrows
    /// supplemental Ports, and the zero-DPU fallback uses only Host
    /// declarations from a host ExpectedMachine.
    #[test]
    fn host_mac_addresses_for_predicted_machine_cases() {
        let host_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let dpu_os_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        let dpu_bmc_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x03]);
        let host_bmc_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x04]);
        let redfish_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x05]);
        let declared_port_mac = MacAddress::new([0x94, 0x6d, 0xae, 0x53, 0xcb, 0x9b]);
        let unrelated_port_mac = MacAddress::new([0x94, 0x6d, 0xae, 0x53, 0xcb, 0x9a]);
        let system_macs = [
            MacAddress::new([0x0a, 0x8f, 0xc3, 0xa5, 0x8a, 0x41]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xa8]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xa9]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xaa]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xab]),
        ];
        let interface = |mac_address, role| ExpectedInterface {
            mac_address,
            role,
            ..Default::default()
        };
        let machine_data = |interfaces| ExpectedMachineData {
            interfaces,
            ..Default::default()
        };
        let all_roles = || {
            machine_data(vec![
                interface(host_mac, ExpectedInterfaceRole::Host),
                interface(dpu_os_mac, ExpectedInterfaceRole::DpuOs),
                interface(dpu_bmc_mac, ExpectedInterfaceRole::DpuBmc),
                interface(host_bmc_mac, ExpectedInterfaceRole::HostBmc),
            ])
        };
        let report_with_ports = |port_mac_addresses| EndpointExplorationReport {
            chassis: vec![Chassis {
                network_adapters: vec![NetworkAdapter {
                    id: "slot-15".to_string(),
                    port_mac_addresses,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let excluded_report = |chassis_id: &str| EndpointExplorationReport {
            chassis: vec![Chassis {
                id: chassis_id.to_string(),
                network_adapters: vec![NetworkAdapter {
                    id: "slot-15".to_string(),
                    port_mac_addresses: vec![declared_port_mac],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        check_values(
            [
                Check {
                    scenario: "Redfish EthernetInterfaces are authoritative",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        Some(all_roles()),
                    ),
                    expect: vec![redfish_mac],
                },
                Check {
                    scenario: "declared discovered Port supplements partial System inventory",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: system_macs
                                    .into_iter()
                                    .map(|mac_address| EthernetInterface {
                                        mac_address: Some(mac_address),
                                        ..Default::default()
                                    })
                                    .collect(),
                                ..Default::default()
                            }],
                            chassis: report_with_ports(vec![
                                declared_port_mac,
                                unrelated_port_mac,
                                dpu_os_mac,
                                system_macs[0],
                            ])
                            .chassis,
                            ..Default::default()
                        },
                        Some(machine_data(vec![
                            ExpectedInterface {
                                primary: Some(true),
                                ..interface(declared_port_mac, ExpectedInterfaceRole::Host)
                            },
                            interface(system_macs[0], ExpectedInterfaceRole::Host),
                            interface(dpu_os_mac, ExpectedInterfaceRole::DpuOs),
                        ])),
                    ),
                    expect: system_macs.into_iter().chain([declared_port_mac]).collect(),
                },
                Check {
                    scenario: "undeclared Ports do not supplement usable System inventory",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            chassis: report_with_ports(vec![declared_port_mac]).chassis,
                            ..Default::default()
                        },
                        None,
                    ),
                    expect: vec![redfish_mac],
                },
                Check {
                    scenario: "disabled System interfaces do not suppress the adapter-Port fallback",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    interface_enabled: Some(false),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            chassis: report_with_ports(vec![declared_port_mac, unrelated_port_mac])
                                .chassis,
                            ..Default::default()
                        },
                        None,
                    ),
                    expect: vec![redfish_mac, declared_port_mac, unrelated_port_mac],
                },
                Check {
                    scenario: "adapter Ports remain the hardware fallback without System inventory",
                    input: (
                        report_with_ports(vec![declared_port_mac, unrelated_port_mac]),
                        None,
                    ),
                    expect: vec![declared_port_mac, unrelated_port_mac],
                },
                Check {
                    scenario: "Host declarations provide the zero-DPU fallback",
                    input: (EndpointExplorationReport::default(), Some(all_roles())),
                    expect: vec![host_mac],
                },
                Check {
                    scenario: "DPU-only declarations do not become Host predictions",
                    input: (
                        EndpointExplorationReport::default(),
                        Some(machine_data(vec![
                            interface(dpu_os_mac, ExpectedInterfaceRole::DpuOs),
                            interface(dpu_bmc_mac, ExpectedInterfaceRole::DpuBmc),
                            interface(host_bmc_mac, ExpectedInterfaceRole::HostBmc),
                        ])),
                    ),
                    expect: vec![],
                },
                Check {
                    scenario: "missing ExpectedMachine data has no fallback",
                    input: (EndpointExplorationReport::default(), None),
                    expect: vec![],
                },
                Check {
                    scenario: "DPU reports do not use Host declarations",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                id: "Bluefield".to_string(),
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            chassis: vec![Chassis {
                                id: "Card1".to_string(),
                                model: Some("BlueField 3 DPU".to_string()),
                                network_adapters: vec![NetworkAdapter {
                                    id: "slot-15".to_string(),
                                    port_mac_addresses: vec![declared_port_mac],
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        Some(machine_data(vec![interface(
                            declared_port_mac,
                            ExpectedInterfaceRole::Host,
                        )])),
                    ),
                    expect: vec![redfish_mac],
                },
                Check {
                    scenario: "switch reports do not use Host declarations",
                    input: (excluded_report("MGX_NVSwitch_0"), Some(all_roles())),
                    expect: vec![],
                },
                Check {
                    scenario: "power-shelf reports do not use Host declarations",
                    input: (excluded_report("PowerShelf"), Some(all_roles())),
                    expect: vec![],
                },
            ],
            |(report, machine_data)| {
                host_mac_addresses_for_predicted_machine(&report, machine_data.as_ref())
            },
        );
    }

    /// `desired_boot_interface_update` only initializes a missing target or
    /// completes the same designated MAC. These cases keep the
    /// prediction/owned precedence and the ambiguity rules together.
    #[test]
    fn desired_boot_interface_update_cases() {
        let mac = |last| MacAddress::new([0x02, 0, 0, 0, 0, last]);
        let interface =
            |mac_address, interface_type, primary_interface, interface_id: Option<&str>| {
                MachineInterfaceSnapshot {
                    interface_type,
                    primary_interface,
                    boot_interface_id: interface_id.map(str::to_string),
                    network_segment_type: Some(NetworkSegmentType::HostInband),
                    ..MachineInterfaceSnapshot::mock_with_mac(mac_address)
                }
            };
        let prediction = |mac_address, primary_interface, interface_id: Option<&str>| {
            PredictedMachineInterface {
                id: uuid::Uuid::nil(),
                machine_id: MachineId::from_str(
                    "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng",
                )
                .unwrap(),
                mac_address,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id: interface_id.map(str::to_string),
                primary_interface,
            }
        };
        let pair = |mac_address, interface_id: &str| {
            MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address,
                interface_id: interface_id.to_string(),
            })
        };

        let owned_mac = mac(1);
        let predicted_mac = mac(2);
        let desired_mac = mac(3);

        check_values(
            [
                Check {
                    scenario: "explicit primary prediction outranks an owned primary",
                    input: (
                        None,
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(predicted_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "selected prediction uses a same-MAC owned id",
                    input: (
                        None,
                        vec![interface(
                            predicted_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, None)],
                    ),
                    expect: Some(pair(predicted_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "owned candidate outranks a non-primary prediction",
                    input: (
                        None,
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(owned_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "a sole prediction initializes a MAC-only target",
                    input: (None, vec![], vec![prediction(predicted_mac, false, None)]),
                    expect: Some(MachineBootInterfaceTarget::MacOnly(predicted_mac)),
                },
                Check {
                    scenario: "a whitespace-only id initializes a MAC-only target",
                    input: (
                        None,
                        vec![],
                        vec![prediction(predicted_mac, false, Some("\t\n"))],
                    ),
                    expect: Some(MachineBootInterfaceTarget::MacOnly(predicted_mac)),
                },
                Check {
                    scenario: "several non-primary predictions do not initialize a target",
                    input: (
                        None,
                        vec![],
                        vec![
                            prediction(predicted_mac, false, Some("NIC.Predicted")),
                            prediction(desired_mac, false, Some("NIC.Other")),
                        ],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "an existing pair is never rewritten",
                    input: (
                        Some(pair(desired_mac, "NIC.Remembered")),
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "a MAC-only target uses a unique owned id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            desired_mac,
                            InterfaceType::Data,
                            false,
                            Some("NIC.Remembered"),
                        )],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Remembered")),
                },
                Check {
                    scenario: "a MAC-only target falls back to a predicted id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(desired_mac, InterfaceType::Data, false, None)],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "a whitespace-only owned id falls back to a predicted id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            desired_mac,
                            InterfaceType::Data,
                            false,
                            Some("\t\n"),
                        )],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "ambiguous owned ids stop enrichment",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned.1")),
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned.2")),
                        ],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "duplicate owned ids are still one unique id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![
                            interface(
                                desired_mac,
                                InterfaceType::Data,
                                false,
                                Some(" \tNIC.Owned\n "),
                            ),
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned")),
                        ],
                        vec![],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "ambiguous predicted ids stop enrichment",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![],
                        vec![
                            prediction(desired_mac, false, Some("NIC.Predicted.1")),
                            prediction(desired_mac, false, Some("NIC.Predicted.2")),
                        ],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "a different designated MAC is preserved",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
            ],
            |(desired, interfaces, predictions)| {
                desired_boot_interface_update(desired.as_ref(), &interfaces, &predictions)
            },
        );
    }

    #[test]
    fn initial_selection_source_must_match_the_durable_target() {
        let target_mac = MacAddress::new([0x02, 0, 0, 0, 0, 1]);
        let other_mac = MacAddress::new([0x02, 0, 0, 0, 0, 2]);
        let target = MachineBootInterfaceTarget::MacOnly(target_mac);
        let selection = |mac_address, source| HostPrimaryInterfaceSelection {
            mac_address,
            source,
        };

        check_values(
            [
                Check {
                    scenario: "matching ExpectedMachine declaration wins over report fallback",
                    input: (
                        Some(selection(
                            other_mac,
                            BootInterfaceSelectionSource::RedfishSerialNumber,
                        )),
                        Some(target_mac),
                    ),
                    expect: BootInterfaceSelectionSource::ExpectedMachine,
                },
                Check {
                    scenario: "matching UEFI selection is retained",
                    input: (
                        Some(selection(
                            target_mac,
                            BootInterfaceSelectionSource::RedfishUefiPci,
                        )),
                        None,
                    ),
                    expect: BootInterfaceSelectionSource::RedfishUefiPci,
                },
                Check {
                    scenario: "matching serial fallback is retained",
                    input: (
                        Some(selection(
                            target_mac,
                            BootInterfaceSelectionSource::RedfishSerialNumber,
                        )),
                        None,
                    ),
                    expect: BootInterfaceSelectionSource::RedfishSerialNumber,
                },
                Check {
                    scenario: "different report selection is not attributed to target",
                    input: (
                        Some(selection(
                            other_mac,
                            BootInterfaceSelectionSource::RedfishUefiPci,
                        )),
                        None,
                    ),
                    expect: BootInterfaceSelectionSource::LegacyUnknown,
                },
                Check {
                    scenario: "unexplained target stays explicitly unknown",
                    input: (None, None),
                    expect: BootInterfaceSelectionSource::LegacyUnknown,
                },
            ],
            |(report_selection, declared_primary)| {
                initial_selection_source(&target, report_selection, declared_primary)
            },
        );
    }
}
