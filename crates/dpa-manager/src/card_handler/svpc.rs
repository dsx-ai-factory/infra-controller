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

use async_trait::async_trait;
use carbide_dpa::DpaInfo;
use carbide_uuid::dpa_interface::DpaInterfaceId;
use carbide_uuid::spx::{NULL_SPX_PARTITION_ID, SpxPartitionId};
use chrono::TimeDelta;
use db::{self, ObjectColumnFilter};
use mac_address::MacAddress;
use model::dpa_interface::DpaLockMode::{Locked, Unlocked};
use model::dpa_interface::{DpaInterface, DpaInterfaceControllerState};
use model::instance::snapshot::InstanceSnapshot;
use model::machine::{HostMachine, ManagedHostStateSnapshot};
use mqttea::client::MqtteaClient;
use sqlx::{PgConnection, PgTransaction};

use super::DpaInterfaceStateHandler;
use crate::errors::{DpaManagerError, DpaManagerResult};
use crate::metrics::DpaMonitorMetrics;
use crate::{DpaMonitor, HandlerResult};

pub(super) struct SvpcInterfaceHandler;

enum ReconcileAction {
    Noop,
    Heartbeat,
    Create,
    Delete,
}

impl SvpcInterfaceHandler {
    // Given an iterator, return the first item if there is only one, otherwise return an error.
    fn at_most_one<I: Iterator>(mut iter: I, ctx: &str) -> DpaManagerResult<Option<I::Item>> {
        let first = iter.next();
        if first.is_some() && iter.next().is_some() {
            tracing::error!(
                context = %ctx,
                "Multiple matches found",
            );
            return Err(DpaManagerError::InvalidArgument(format!(
                "{ctx}: more than one match"
            )));
        }
        Ok(first)
    }

    // Given an SPX Partitioin ID, find the DPA VNI associated with it.
    async fn get_partition_vni(
        monitor: &mut DpaMonitor,
        partition_id: SpxPartitionId,
    ) -> DpaManagerResult<u32> {
        let db_pool = monitor.db_services.db_pool.clone();
        let mut txn = db_pool
            .begin()
            .await
            .map_err(|e| db::AnnotatedSqlxError::new("get_partition_vni begin txn", e))?;
        let partition = db::spx_partition::find_by(
            txn.as_mut(),
            ObjectColumnFilter::List(db::spx_partition::IdColumn, &[partition_id]),
        )
        .await?;
        txn.commit()
            .await
            .map_err(|e| db::AnnotatedSqlxError::new("get_partition_vni commit txn", e))?;
        Ok(partition
            .into_iter()
            .next()
            .and_then(|p| p.vni)
            .unwrap_or(0) as u32)
    }

    async fn reconcile_assigned_state<'a>(
        monitor: &mut DpaMonitor,
        dpa_interface: &DpaInterface,
        machine: &HostMachine,
        instance: &InstanceSnapshot,
        client: Arc<MqtteaClient>,
        dpa_info: &Arc<DpaInfo>,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<Option<PgTransaction<'a>>> {
        let this_mac = dpa_interface.mac_address;

        let instance_version = instance.spx_config_version;
        let nic_version = dpa_interface.network_config.version.to_string();

        // We expect to find only one configured attachment for the MAC address of this DPA interface
        // TODO: This has to be changed when we support multiple VFs per NIC
        let configured = Self::at_most_one(
            instance
                .config
                .spxconfig
                .spx_attachments
                .iter()
                .filter(|a| a.mac_address.as_deref() == Some(this_mac.to_string().as_str())),
            "reconcile_assigned_state configured attachments",
        )?;

        // We expect to find only one observation for the MAC address of this DPA interface
        // TODO: This has to be changed when we support multiple VFs per NIC
        let observed = Self::at_most_one(
            machine
                .status
                .spx_status_observation
                .iter()
                .flat_map(|o| &o.spx_attachments)
                .filter(|a| a.mac_address == this_mac),
            "reconcile_assigned_state observed attachments",
        )?;

        let configured_partition_id = configured
            .as_ref()
            .map(|attachment| attachment.spx_partition_id);

        let action = match configured {
            Some(configured_attachment) => match observed {
                Some(observed_attachment) => {
                    if observed_attachment.partition_id
                        != Some(configured_attachment.spx_partition_id)
                        || observed_attachment.config_version != Some(instance_version)
                    {
                        ReconcileAction::Create
                    } else {
                        ReconcileAction::Heartbeat
                    }
                }
                None => ReconcileAction::Create,
            },
            None => match observed {
                Some(_observed_attachment) => ReconcileAction::Delete,
                None => ReconcileAction::Noop,
            },
        };

        match action {
            ReconcileAction::Noop => Ok(None),
            ReconcileAction::Delete => {
                metrics.num_deletes += 1;
                let txn = monitor
                    .send_set_vni_command(
                        dpa_interface,
                        client,
                        dpa_info,
                        0_u32,
                        false,
                        nic_version,
                    )
                    .await?;
                Ok(txn)
            }
            ReconcileAction::Heartbeat => {
                let vni =
                    Self::get_partition_vni(monitor, configured_partition_id.unwrap()).await?;
                let hb_interval = monitor.config.svpc.hb_interval;
                let txn = monitor
                    .do_heartbeat(dpa_interface, client, dpa_info, hb_interval, vni, metrics)
                    .await?;
                Ok(txn)
            }
            ReconcileAction::Create => {
                metrics.num_creates += 1;
                let vni =
                    Self::get_partition_vni(monitor, configured_partition_id.unwrap()).await?;
                let txn = monitor
                    .send_set_vni_command(
                        dpa_interface,
                        client,
                        dpa_info,
                        vni,
                        false,
                        instance_version.to_string(),
                    )
                    .await?;
                Ok(txn)
            }
        }
    }

    async fn reconcile_ready_state<'a>(
        monitor: &mut DpaMonitor,
        machine: &HostMachine,
        dpa_interface: &DpaInterface,
        client: Arc<MqtteaClient>,
        dpa_info: &Arc<DpaInfo>,
        hb_interval: TimeDelta,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<Option<PgTransaction<'a>>> {
        let nic_version = dpa_interface.network_config.version;
        let nic_version_str = nic_version.to_string();

        let this_mac = dpa_interface.mac_address;

        let this_nic_observed_attachments = machine
            .status
            .spx_status_observation
            .clone()
            .map(|observed| {
                observed
                    .spx_attachments
                    .into_iter()
                    .filter(|a| a.mac_address == this_mac)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if this_nic_observed_attachments.len() > 1 {
            tracing::error!(
                "reconcile_assigned_state this_nic_observed_attachments length is greater than 1"
            );
            return Err(DpaManagerError::InvalidArgument(
                "reconcile_assigned_state this_nic_observed_attachments length is greater than 1"
                    .to_string(),
            ));
        }

        let Some(observed_attachment) = this_nic_observed_attachments.first() else {
            return Ok(None);
        };

        let need_deletion = (observed_attachment.partition_id != Some(NULL_SPX_PARTITION_ID))
            || (observed_attachment.config_version != Some(nic_version));

        tracing::debug!(
            timestamp = %chrono::Utc::now(),
            need_deletion = need_deletion,
            heartbeat_needed = !need_deletion,
            "Reconciled ready-state action",
        );

        if need_deletion {
            metrics.num_deletes += 1;
            let txn = monitor
                .send_set_vni_command(
                    dpa_interface,
                    client,
                    dpa_info,
                    0_u32,
                    false,
                    nic_version_str,
                )
                .await?;
            return Ok(txn);
        }

        let txn = monitor
            .do_heartbeat(dpa_interface, client, dpa_info, hb_interval, 0_u32, metrics)
            .await?;
        Ok(txn)
    }
}

#[async_trait]
impl DpaInterfaceStateHandler for SvpcInterfaceHandler {
    #[allow(clippy::unused_async)]
    async fn handle_provisioning(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_provisioning index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let host_use_admin_network = dpa_interface.use_admin_network();
        if host_use_admin_network {
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let new_state = DpaInterfaceControllerState::Ready;
        tracing::info!(next_state = ?new_state, "Dpa Interface state transition");
        Ok(HandlerResult {
            new_state: Some(new_state),
            txn: None,
        })
    }

    async fn handle_ready(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_ready index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let host_use_admin_network = dpa_interface.use_admin_network();
        if !host_use_admin_network {
            let new_state = DpaInterfaceControllerState::Unlocking;
            tracing::info!(next_state = ?new_state, "Dpa Interface state transition");

            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: None,
            });
        }

        let dpa_info = monitor.dpa_info.clone();
        let hb_interval = monitor.config.svpc.hb_interval;
        let client = dpa_info
            .mqtt_client
            .clone()
            .ok_or_else(|| eyre::eyre!("missing mqtt_client"))?;

        let txn = Self::reconcile_ready_state(
            monitor,
            &mh.host_snapshot,
            dpa_interface,
            client,
            &dpa_info,
            hb_interval,
            metrics,
        )
        .await?;

        Ok(HandlerResult {
            new_state: None,
            txn,
        })
    }

    #[allow(clippy::unused_async)]
    async fn handle_unlocking(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_unlocking index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let Some(ref cs) = dpa_interface.card_state else {
            tracing::error!(
                dpa_interface_id = %dpa_interface.id,
                "DPA interface has no card state",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        if cs.lockmode == Some(Unlocked) {
            let new_state = DpaInterfaceControllerState::ApplyFirmware;
            tracing::info!(next_state = ?new_state, "Interface unlocked. Transitioning to next state");
            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: None,
            });
        }

        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }

    #[allow(clippy::unused_async)]
    async fn handle_apply_firmware(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_apply_firmware index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let Some(ref card_state) = dpa_interface.card_state else {
            tracing::info!(
                dpa_interface_id = %dpa_interface.id,
                "DPA interface has no card state; waiting to retry firmware report",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        if let Some(ref firmware_report) = card_state.firmware_report {
            let reset_ok = firmware_report.reset.unwrap_or(true);
            if firmware_report.flashed && reset_ok {
                let new_state = DpaInterfaceControllerState::ApplyProfile;
                tracing::info!(
                    next_state = ?new_state,
                    observed_version = firmware_report.observed_version.as_deref().unwrap_or("none"),
                    "firmware report received and successfully applied, transitioning"
                );
                return Ok(HandlerResult {
                    new_state: Some(new_state),
                    txn: None,
                });
            }
            tracing::warn!(
                flashed = firmware_report.flashed,
                reset = ?firmware_report.reset,
                observed_version = firmware_report.observed_version.as_deref().unwrap_or("none"),
                "firmware report received but not successful, waiting for retry"
            );
        }

        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }

    #[allow(clippy::unused_async)]
    async fn handle_apply_profile(
        &self,
        _monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_apply_profile index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        apply_profile(dpa_interface)
    }

    async fn handle_locking(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_locking index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let Some(ref cs) = dpa_interface.card_state else {
            tracing::error!(
                dpa_interface_id = %dpa_interface.id,
                "DPA interface has no card state",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        if cs.lockmode == Some(Locked) {
            // The card is now locked on the device. Complete its lockdown_ikm
            // convergence, keyed by the card (NIC) MAC, so the rotation engine
            // tracks this card from the moment it is actually locked. The record
            // is committed atomically with the state transition below (the
            // monitor reuses this txn to persist the new controller state).
            //
            // api-core staged the *exact* IKM version the lock key was derived
            // from as the in-flight `rotating_to_version` when it issued the lock
            // command; we promote that value to `current_version` here. We must
            // not re-read the site-wide target instead: it can advance between
            // issuing the lock and observing it, which would record the card as
            // converged to a newer version than the IKM it is actually locked
            // under. A card with nothing staged (locked before this flow shipped,
            // already covered by the backfill at v0) falls back to the site-wide
            // target; that path is idempotent and warns.
            let mut txn = monitor
                .db_services
                .db_pool
                .begin()
                .await
                .map_err(|e| db::AnnotatedSqlxError::new("handle_locking begin txn", e))?;
            record_lock_convergence(txn.as_mut(), dpa_interface.id, dpa_interface.mac_address)
                .await?;

            let new_state = DpaInterfaceControllerState::Assigned;
            tracing::info!(next_state = ?new_state, "Dpa Interface state transition");
            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: Some(txn),
            });
        }

        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }

    async fn handle_assigned(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_assigned index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let host_use_admin_network = dpa_interface.use_admin_network();

        if host_use_admin_network {
            let new_state = DpaInterfaceControllerState::Ready;
            tracing::info!(next_state = ?new_state, "Dpa Interface state transition");
            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: None,
            });
        }

        let dpa_info = monitor.dpa_info.clone();
        let client = dpa_info
            .mqtt_client
            .clone()
            .ok_or_else(|| eyre::eyre!("missing mqtt_client"))?;

        let instance = mh.instance.as_ref().ok_or_else(|| {
            tracing::error!("reconcile_assigned_state instance is missing");
            eyre::eyre!("reconcile_assigned_state instance is missing")
        })?;
        let txn = Self::reconcile_assigned_state(
            monitor,
            dpa_interface,
            &mh.host_snapshot,
            instance,
            client,
            &dpa_info,
            metrics,
        )
        .await?;

        Ok(HandlerResult {
            new_state: None,
            txn,
        })
    }

    /// Tenant-free rekey, phase one: wait for the card to report `Unlocked`,
    /// record the unlock (NULLing `current_version` so the truth column reflects
    /// an unlocked card), then advance to `RotateKeyLocking` to relock at the
    /// staged site-wide target. The record commits atomically with the state
    /// transition via the returned txn, mirroring `handle_locking`.
    async fn handle_rotate_key_unlocking(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_rotate_key_unlocking index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let Some(ref cs) = dpa_interface.card_state else {
            tracing::error!(
                dpa_interface_id = %dpa_interface.id,
                "DPA interface has no card state",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        if cs.lockmode == Some(Unlocked) {
            let mut txn = monitor.db_services.db_pool.begin().await.map_err(|e| {
                db::AnnotatedSqlxError::new("handle_rotate_key_unlocking begin txn", e)
            })?;
            db::credential_rotation::record_device_unlocked(
                txn.as_mut(),
                dpa_interface.mac_address,
                db::credential_rotation::CredentialRotationType::LockdownIkm,
            )
            .await?;

            let new_state = DpaInterfaceControllerState::RotateKeyLocking;
            tracing::info!(next_state = ?new_state, "rekey: card unlocked, transitioning to relock");
            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: Some(txn),
            });
        }

        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }

    /// Tenant-free rekey, phase two: wait for the card to report `Locked` at the
    /// staged target, promote `rotating_to_version -> current_version`
    /// (`record_lock_convergence`), then return the card to `Ready`. This is the
    /// assignment `handle_locking` promote path with a `Ready` terminal instead
    /// of `Assigned`.
    async fn handle_rotate_key_locking(
        &self,
        monitor: &mut DpaMonitor,
        mh: &ManagedHostStateSnapshot,
        idx: usize,
        _metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        let Some(dpa_interface) = mh.dpa_interface_snapshots.get(idx) else {
            tracing::error!(
                index = idx,
                dpa_interface_snapshot_count = mh.dpa_interface_snapshots.len(),
                "handle_rotate_key_locking index out of bounds",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        let Some(ref cs) = dpa_interface.card_state else {
            tracing::error!(
                dpa_interface_id = %dpa_interface.id,
                "DPA interface has no card state",
            );
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        };

        if cs.lockmode == Some(Locked) {
            let mut txn = monitor.db_services.db_pool.begin().await.map_err(|e| {
                db::AnnotatedSqlxError::new("handle_rotate_key_locking begin txn", e)
            })?;
            record_lock_convergence(txn.as_mut(), dpa_interface.id, dpa_interface.mac_address)
                .await?;

            let new_state = DpaInterfaceControllerState::Ready;
            tracing::info!(next_state = ?new_state, "rekey: card relocked, returning to Ready");
            return Ok(HandlerResult {
                new_state: Some(new_state),
                txn: Some(txn),
            });
        }

        Ok(HandlerResult {
            new_state: None,
            txn: None,
        })
    }
}

fn apply_profile(state: &DpaInterface) -> DpaManagerResult<HandlerResult> {
    let Some(ref cs) = state.card_state else {
        tracing::info!(
            dpa_interface_id = %state.id,
            "DPA interface has no card state; waiting to retry profile report",
        );
        return Ok(HandlerResult {
            new_state: None,
            txn: None,
        });
    };
    if cs.profile_synced == Some(true) {
        let new_state = DpaInterfaceControllerState::Locking;
        tracing::info!(
            next_state = ?new_state,
            profile = cs.profile.as_deref().unwrap_or("none"),
            "profile applied successfully, transitioning"
        );
        return Ok(HandlerResult {
            new_state: Some(new_state),
            txn: None,
        });
    }
    Ok(HandlerResult {
        new_state: None,
        txn: None,
    })
}

/// Completes `lockdown_ikm` convergence for a card observed locked, on the
/// caller's `conn` (`handle_locking` passes its open transaction so the record
/// commits atomically with the state transition).
///
/// api-core staged the exact IKM version the lock key was derived from as the
/// in-flight `rotating_to_version` when it issued the lock command, so:
///
///   * if a rotation is staged, promote it to `current_version` verbatim. We
///     must NOT re-read the site-wide target: it can advance between issuing the
///     lock and observing it, which would record the card as converged to a
///     newer version than the IKM it is actually locked under.
///   * if nothing is staged (a card locked before this flow shipped, already
///     covered by the backfill at v0), fall back to the site-wide target and
///     warn; that path is idempotent.
async fn record_lock_convergence(
    conn: &mut PgConnection,
    dpa_interface_id: DpaInterfaceId,
    mac_address: MacAddress,
) -> DpaManagerResult<()> {
    let promoted = db::credential_rotation::promote_rotating_to_current(
        conn,
        mac_address,
        db::credential_rotation::CredentialRotationType::LockdownIkm,
    )
    .await?;

    if !promoted {
        tracing::warn!(
            %dpa_interface_id,
            %mac_address,
            "card locked without a staged lockdown IKM rotation; \
             recording convergence at the site-wide target"
        );
        db::credential_rotation::record_device_converged(
            conn,
            mac_address,
            db::credential_rotation::CredentialRotationType::LockdownIkm,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use mac_address::MacAddress;
    use sqlx::PgPool;

    use super::record_lock_convergence;

    // current_version for the lockdown_ikm row of `mac`, or None if no row exists.
    async fn lockdown_version_of(pool: &PgPool, mac: &str) -> Option<i32> {
        let row: Option<Option<i32>> = sqlx::query_scalar(
            "SELECT current_version FROM device_credential_rotation \
             WHERE device_mac = $1::macaddr AND credential_type = 'lockdown_ikm'",
        )
        .bind(mac)
        .fetch_optional(pool)
        .await
        .unwrap();
        row.flatten()
    }

    // rotating_to_version for the lockdown_ikm row of `mac`, or None.
    async fn lockdown_rotating_version_of(pool: &PgPool, mac: &str) -> Option<i32> {
        let row: Option<Option<i32>> = sqlx::query_scalar(
            "SELECT rotating_to_version FROM device_credential_rotation \
             WHERE device_mac = $1::macaddr AND credential_type = 'lockdown_ikm'",
        )
        .bind(mac)
        .fetch_optional(pool)
        .await
        .unwrap();
        row.flatten()
    }

    // Promote-path: a staged in-flight version is promoted verbatim, never
    // re-derived from the (mutable) site-wide target -- this is the TOCTOU the
    // two-phase lock flow guards against.
    #[crate::sqlx_test]
    async fn promotes_staged_version_not_sitewide_target(pool: PgPool) {
        let id = carbide_uuid::dpa_interface::DpaInterfaceId::new();
        let mac: MacAddress = "02:00:00:00:00:11".parse().unwrap();

        // api-core staged the lock at version 2; advance the site-wide target so
        // it differs: the promote-path must ignore it.
        sqlx::query(
            "UPDATE sitewide_credential_rotation SET target_version = 7 \
             WHERE credential_type = 'lockdown_ikm'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        db::credential_rotation::mark_device_rotating_to_version(
            &mut conn,
            mac,
            db::credential_rotation::CredentialRotationType::LockdownIkm,
            2,
        )
        .await
        .unwrap();

        record_lock_convergence(&mut conn, id, mac).await.unwrap();
        drop(conn);

        assert_eq!(
            lockdown_version_of(&pool, "02:00:00:00:00:11").await,
            Some(2),
            "must promote the staged version, not the site-wide target (7)"
        );
        assert_eq!(
            lockdown_rotating_version_of(&pool, "02:00:00:00:00:11").await,
            None,
            "the in-flight marker must be cleared on promotion"
        );
    }

    // Fallback (warn) path: nothing staged falls back to the site-wide target
    // (seeded at 0 by the backfill migration).
    #[crate::sqlx_test]
    async fn falls_back_to_sitewide_target_when_nothing_staged(pool: PgPool) {
        let id = carbide_uuid::dpa_interface::DpaInterfaceId::new();
        let mac: MacAddress = "02:00:00:00:00:12".parse().unwrap();

        let mut conn = pool.acquire().await.unwrap();
        record_lock_convergence(&mut conn, id, mac).await.unwrap();
        drop(conn);

        assert_eq!(
            lockdown_version_of(&pool, "02:00:00:00:00:12").await,
            Some(0),
            "nothing staged must fall back to the site-wide target (0)"
        );
    }
}
