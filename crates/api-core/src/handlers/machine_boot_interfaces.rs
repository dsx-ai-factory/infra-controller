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

//! One machine's boot-interface view, gathered from every store that records
//! it. This is a read-only troubleshooting projection: it reports the four
//! places a host's boot interface can live -- owned `machine_interfaces` rows,
//! `predicted_machine_interfaces`, the `explored_endpoints` default, and the
//! post-deletion `retained_boot_interfaces` pairs -- alongside the effective
//! boot interface the system would select via `pick_boot_interface`, and a
//! divergence flag for when the stores disagree about which NIC boots. The
//! same response also reports whether the machine controller has converged the
//! persisted desired target.

use std::collections::BTreeSet;

use ::rpc::forge as rpc;
use ::rpc::forge::BootInterfaceSelectionSource as RpcBootInterfaceSelectionSource;
use ::rpc::forge::get_machine_boot_interfaces_response::Reconciliation as BootInterfaceReconciliationStatus;
use ::rpc::forge::get_machine_boot_interfaces_response::reconciliation::State as BootInterfaceReconciliationState;
use config_version::{ConfigVersion, Versioned};
use mac_address::MacAddress;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{ManagedHostState, ReadyBootConfigState};
use model::machine_boot_interface::{
    BootInterfaceSelection, BootInterfaceStatusObservation, MachineBootInterfaceTarget,
};
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

/// Gather the boot-interface view for one machine across all four stores.
///
/// All four stores are read within a single read transaction. The effective
/// boot interface is the same
/// `pick_boot_interface` selection every other flow acts on, applied to the
/// owned `machine_interfaces` rows. Reconciliation is derived from the
/// persisted desired target, its latest observation, and `ManagedHostState`.
pub(crate) async fn get_machine_boot_interfaces(
    api: &Api,
    request: Request<rpc::GetMachineBootInterfacesRequest>,
) -> Result<Response<rpc::GetMachineBootInterfacesResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;

    // Store 1: owned interface rows -- the authoritative store for a machine
    // that exists. `find_by_machine_ids` returns a per-machine map.
    let owned_interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[machine_id])
        .await?
        .remove(&machine_id)
        .unwrap_or_default();

    // Load the desired target, last observation, and controller state through
    // one machine snapshot. The reconciliation label only makes sense when
    // those three persisted values are interpreted together.
    let machine = db::machine::find_one(
        &mut txn,
        &machine_id,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;

    // Store 2: predictions -- the boot candidates a host offers before its
    // first DHCP lease creates an owned row.
    let predicted_interfaces =
        db::predicted_machine_interface::find_by_machine_id(txn.as_mut(), &machine_id).await?;

    // Store 3: the explored endpoint default. The machine's BMC IP(s) map it to
    // the explored endpoints site-explorer recorded a default against.
    let bmc_pairs =
        db::machine_topology::find_machine_bmc_pairs_by_machine_id(txn.as_mut(), vec![machine_id])
            .await?;
    let bmc_ips: Vec<std::net::IpAddr> = bmc_pairs
        .into_iter()
        .filter_map(|(_, ip)| ip)
        .filter_map(|ip| ip.parse().ok())
        .collect();
    let explored_endpoints = if bmc_ips.is_empty() {
        Vec::new()
    } else {
        // `find_by_ips` takes `impl DbReader`; the wrapping transaction
        // implements it directly (a bare `&mut PgConnection` would need a
        // coercion that generic bound can't perform).
        db::explored_endpoints::find_by_ips(&mut txn, bmc_ips).await?
    };

    // Store 4: the retained post-deletion pairs. Collect the MACs the machine
    // knows about and read their raw retained rows -- un-window-filtered, so
    // stale records show up in the troubleshooting view. Owned (store 1) and
    // predicted (store 2) MACs, plus each explored endpoint's recorded boot MAC
    // (store 3): a retained record keyed on the explored boot MAC is surfaced
    // too, even when no owned/predicted row carries that MAC.
    let macs: Vec<MacAddress> = owned_interfaces
        .iter()
        .map(|i| i.mac_address)
        .chain(predicted_interfaces.iter().map(|p| p.mac_address))
        .chain(
            explored_endpoints
                .iter()
                .filter_map(|e| e.boot_interface_mac),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let retained_records = if macs.is_empty() {
        Vec::new()
    } else {
        db::retained_boot_interface::find_records_by_macs(&mut txn, &macs).await?
    };

    txn.commit().await?;

    // The effective boot interface: `pick_boot_interface` over the owned rows
    // (primary wins, else the lowest-MAC non-underlay NIC). This is what the
    // controller and admin actions resolve.
    let effective = model::machine::pick_boot_interface(&owned_interfaces);
    let effective_mac = effective.map(|i| i.mac_address);
    let effective_boot_interface = effective.and_then(|i| i.boot_interface());

    // The default pick: the same selection with the primary flag masked --
    // what the automation would choose if nothing were declared. Reported so
    // the candidates view can show whether a primary designation is
    // overriding the automatic choice.
    let default_pick = model::machine::pick_default_boot_interface(&owned_interfaces);
    let default_boot_interface = boot_interface_message(
        default_pick.map(|i| i.mac_address),
        default_pick.and_then(|i| i.boot_interface()),
    );

    // The predicted pick: what `pick_boot_prediction` would boot from while
    // the machine is still waiting on its first DHCP lease. `None` also when
    // the pick refuses to guess among several undeclared predictions.
    let predicted_pick = model::machine::pick_boot_prediction(&predicted_interfaces);
    let predicted_boot_interface = boot_interface_message(
        predicted_pick.map(|p| p.mac_address),
        predicted_pick.and_then(|p| p.boot_interface()),
    );

    // Divergence: do the stores agree on which MAC boots this machine? We
    // compare the boot-MAC signals each store offers -- the effective owned
    // pick, every explored endpoint's recorded default, and any predicted NIC
    // flagged primary -- and flag a disagreement when more than one distinct
    // MAC turns up. (Retained rows are post-deletion history, shown for context
    // but not part of the agreement check.) A single signal, or none, is not a
    // divergence.
    let mut boot_macs: BTreeSet<MacAddress> = BTreeSet::new();
    if let Some(mac) = effective_mac {
        boot_macs.insert(mac);
    }
    for endpoint in &explored_endpoints {
        if let Some(mac) = endpoint.boot_interface_mac {
            boot_macs.insert(mac);
        }
    }
    for prediction in &predicted_interfaces {
        if prediction.primary_interface {
            boot_macs.insert(prediction.mac_address);
        }
    }
    let divergent = boot_macs.len() > 1;

    Ok(Response::new(rpc::GetMachineBootInterfacesResponse {
        machine_id: Some(machine_id),
        machine_interfaces: owned_interfaces
            .iter()
            .map(|i| rpc::MachineInterfaceBootInterface {
                mac_address: i.mac_address.to_string(),
                primary_interface: i.primary_interface,
                boot_interface_id: i.boot_interface_id.clone(),
                network_segment_type: i.network_segment_type.map(|t| t.to_string()),
                interface_id: Some(i.id),
            })
            .collect(),
        predicted_interfaces: predicted_interfaces
            .iter()
            .map(|p| rpc::PredictedBootInterface {
                mac_address: p.mac_address.to_string(),
                primary_interface: p.primary_interface,
                boot_interface_id: p.boot_interface_id.clone(),
                network_segment_type: Some(p.expected_network_segment_type.to_string()),
            })
            .collect(),
        explored_endpoints: explored_endpoints
            .iter()
            .map(|e| rpc::ExploredBootInterface {
                address: e.address.to_string(),
                boot_interface_mac: e.boot_interface_mac.map(|m| m.to_string()),
                boot_interface_id: e.boot_interface_id.clone(),
            })
            .collect(),
        retained_interfaces: retained_records
            .iter()
            .map(|r| rpc::RetainedBootInterface {
                mac_address: r.mac_address.to_string(),
                boot_interface_id: r.boot_interface_id.clone(),
                recorded_at: Some(r.recorded_at.into()),
            })
            .collect(),
        effective_boot_interface_mac: effective_mac.map(|m| m.to_string()),
        effective_boot_interface_id: effective_boot_interface.map(|b| b.interface_id),
        divergent,
        default_boot_interface,
        predicted_boot_interface,
        reconciliation: machine.as_ref().and_then(|machine| {
            boot_interface_reconciliation_status(
                machine.config.desired_boot_interface.as_ref(),
                machine.config.boot_interface_selection.as_ref(),
                machine.status.boot_interface_status_observation.as_ref(),
                machine.current_state(),
            )
        }),
    }))
}

/// `boot_interface_reconciliation_status` builds the operator-facing view of
/// one desired generation.
///
/// Stale observations and superseded `BootConfiguring` work remain visible so
/// an operator can tell what the controller last verified and what it is
/// finishing, even though neither can satisfy the current desired version.
fn boot_interface_reconciliation_status(
    desired_boot_interface: Option<&Versioned<MachineBootInterfaceTarget>>,
    boot_interface_selection: Option<&BootInterfaceSelection>,
    observation: Option<&BootInterfaceStatusObservation>,
    machine_state: &ManagedHostState,
) -> Option<BootInterfaceReconciliationStatus> {
    let desired_boot_interface = desired_boot_interface?;
    let active_reconciliation = match machine_state {
        ManagedHostState::BootConfiguring {
            desired_version,
            boot_config_state,
            ..
        } => Some((*desired_version, boot_config_state)),
        _ => None,
    };
    let reconciliation_state = boot_interface_reconciliation_state(
        desired_boot_interface.version,
        observation.map(|status| status.config_version),
        machine_state,
    );
    let failure = if reconciliation_state == BootInterfaceReconciliationState::Converged {
        None
    } else {
        active_reconciliation.and_then(|(_, state)| match state {
            ReadyBootConfigState::Failed { failure } => Some(failure.clone()),
            _ => None,
        })
    };

    Some(BootInterfaceReconciliationStatus {
        desired_boot_interface: Some(boot_interface_target_message(&desired_boot_interface.value)),
        desired_version: desired_boot_interface.version.version_string(),
        verified_version: observation.map(|status| status.config_version.version_string()),
        observed_at: observation.map(|status| status.observed_at.into()),
        is_compatibility_baseline: observation.is_some_and(|status| status.assumed),
        reconciliation_state: reconciliation_state as i32,
        machine_state: machine_state.to_string(),
        reconciling_version: active_reconciliation.map(|(version, _)| version.version_string()),
        failure,
        selection_source: boot_interface_selection.map_or(
            RpcBootInterfaceSelectionSource::Unspecified as i32,
            |selection| RpcBootInterfaceSelectionSource::from(selection.source) as i32,
        ),
        selection_updated_at: boot_interface_selection
            .and_then(|selection| selection.updated_at.map(Into::into)),
    })
}

/// `boot_interface_reconciliation_state` classifies only the current desired
/// generation. A matching observation wins; active and failed labels apply
/// only when `BootConfiguring` captured that same version.
fn boot_interface_reconciliation_state(
    desired_version: ConfigVersion,
    verified_version: Option<ConfigVersion>,
    machine_state: &ManagedHostState,
) -> BootInterfaceReconciliationState {
    if verified_version == Some(desired_version) {
        return BootInterfaceReconciliationState::Converged;
    }

    match machine_state {
        ManagedHostState::BootConfiguring {
            desired_version: reconciling_version,
            boot_config_state,
            ..
        } if *reconciling_version == desired_version => match boot_config_state {
            ReadyBootConfigState::Failed { .. } => BootInterfaceReconciliationState::Failed,
            _ => BootInterfaceReconciliationState::Converging,
        },
        _ => BootInterfaceReconciliationState::Pending,
    }
}

/// `boot_interface_target_message` preserves whichever target identifiers the
/// desired generation contains.
fn boot_interface_target_message(target: &MachineBootInterfaceTarget) -> rpc::MachineBootInterface {
    rpc::MachineBootInterface {
        mac_address: target.mac_address().to_string(),
        interface_id: target.interface_id().map(str::to_string),
    }
}

/// The wire form of a pick: the complete pair when captured, else the MAC
/// alone -- whatever halves exist travel.
fn boot_interface_message(
    mac: Option<MacAddress>,
    pair: Option<model::machine_boot_interface::MachineBootInterface>,
) -> Option<rpc::MachineBootInterface> {
    match (mac, pair) {
        (_, Some(pair)) => Some(pair.into()),
        (Some(mac), None) => Some(rpc::MachineBootInterface {
            mac_address: mac.to_string(),
            interface_id: None,
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use chrono::{DateTime, Utc};
    use model::machine_boot_interface::BootInterfaceSelectionSource;

    use super::*;

    #[test]
    fn reconciliation_status_is_scoped_to_the_current_desired_generation() {
        let desired_version = ConfigVersion::new(7);
        let stale_version = ConfigVersion::new(6);
        let target = MachineBootInterfaceTarget::MacOnly(
            "00:00:5e:00:53:01".parse().expect("test MAC is valid"),
        );
        let desired = Versioned::new(target.clone(), desired_version);
        let observed_at = DateTime::<Utc>::UNIX_EPOCH;
        let selection = BootInterfaceSelection {
            source: BootInterfaceSelectionSource::Operator,
            updated_at: Some(observed_at),
        };
        let boot_configuring =
            |reconciling_version, boot_config_state| ManagedHostState::BootConfiguring {
                desired_version: reconciling_version,
                desired_boot_interface: target.clone(),
                post_lock_verification_retry_count: 0,
                boot_config_state,
            };

        check_values(
            [
                Check {
                    scenario: "no observation or active work is pending",
                    input: (None, ManagedHostState::Ready),
                    expect: (BootInterfaceReconciliationState::Pending, None),
                },
                Check {
                    scenario: "a stale observation is pending",
                    input: (Some(stale_version), ManagedHostState::Ready),
                    expect: (BootInterfaceReconciliationState::Pending, None),
                },
                Check {
                    scenario: "a matching observation is converged",
                    input: (Some(desired_version), ManagedHostState::Ready),
                    expect: (BootInterfaceReconciliationState::Converged, None),
                },
                Check {
                    scenario: "a matching observation wins over leftover failed state",
                    input: (
                        Some(desired_version),
                        boot_configuring(
                            desired_version,
                            ReadyBootConfigState::Failed {
                                failure: "old failure".to_string(),
                            },
                        ),
                    ),
                    expect: (BootInterfaceReconciliationState::Converged, None),
                },
                Check {
                    scenario: "current active work is converging",
                    input: (
                        None,
                        boot_configuring(desired_version, ReadyBootConfigState::Prepare),
                    ),
                    expect: (BootInterfaceReconciliationState::Converging, None),
                },
                Check {
                    scenario: "current terminal failure is failed",
                    input: (
                        None,
                        boot_configuring(
                            desired_version,
                            ReadyBootConfigState::Failed {
                                failure: "BIOS job retries exhausted".to_string(),
                            },
                        ),
                    ),
                    expect: (
                        BootInterfaceReconciliationState::Failed,
                        Some("BIOS job retries exhausted".to_string()),
                    ),
                },
                Check {
                    scenario: "superseded active work is pending",
                    input: (
                        None,
                        boot_configuring(
                            stale_version,
                            ReadyBootConfigState::Failed {
                                failure: "failure for old generation".to_string(),
                            },
                        ),
                    ),
                    expect: (
                        BootInterfaceReconciliationState::Pending,
                        Some("failure for old generation".to_string()),
                    ),
                },
            ],
            |(verified_version, machine_state)| {
                let observation =
                    verified_version.map(|config_version| BootInterfaceStatusObservation {
                        config_version,
                        observed_at,
                        assumed: false,
                    });
                let status = boot_interface_reconciliation_status(
                    Some(&desired),
                    Some(&selection),
                    observation.as_ref(),
                    &machine_state,
                )
                .expect("the desired target should produce a reconciliation status");
                (
                    BootInterfaceReconciliationState::try_from(status.reconciliation_state)
                        .expect("the reconciliation state should be valid"),
                    status.failure,
                )
            },
        );
    }

    #[test]
    fn reconciliation_status_keeps_stale_observation_and_active_failure_details() {
        let desired_version = ConfigVersion::new(7);
        let stale_version = ConfigVersion::new(6);
        let target = MachineBootInterfaceTarget::MacOnly(
            "00:00:5e:00:53:01"
                .parse::<MacAddress>()
                .expect("test MAC is valid"),
        );
        let desired = Versioned::new(target.clone(), desired_version);
        let observed_at = DateTime::<Utc>::UNIX_EPOCH;
        let selection = BootInterfaceSelection {
            source: BootInterfaceSelectionSource::RedfishUefiPci,
            updated_at: Some(observed_at),
        };
        let observation = BootInterfaceStatusObservation {
            config_version: stale_version,
            observed_at,
            assumed: true,
        };
        let failure = "failure for old generation".to_string();
        let machine_state = ManagedHostState::BootConfiguring {
            desired_version: stale_version,
            desired_boot_interface: target,
            post_lock_verification_retry_count: 0,
            boot_config_state: ReadyBootConfigState::Failed {
                failure: failure.clone(),
            },
        };

        assert!(
            boot_interface_reconciliation_status(
                None,
                Some(&selection),
                Some(&observation),
                &machine_state,
            )
            .is_none(),
            "a machine without a desired target has no reconciliation view"
        );

        let status = boot_interface_reconciliation_status(
            Some(&desired),
            Some(&selection),
            Some(&observation),
            &machine_state,
        )
        .expect("a desired target has a reconciliation view");
        assert_eq!(
            status.desired_boot_interface,
            Some(boot_interface_target_message(&desired.value))
        );
        assert_eq!(status.desired_version, desired_version.version_string());
        assert_eq!(
            status.verified_version.as_deref(),
            Some(stale_version.version_string().as_str())
        );
        assert_eq!(status.observed_at, Some(observed_at.into()));
        assert!(status.is_compatibility_baseline);
        assert_eq!(
            status.reconciliation_state,
            BootInterfaceReconciliationState::Pending as i32
        );
        assert_eq!(status.machine_state, "BootConfiguring/Failed");
        assert_eq!(
            status.reconciling_version.as_deref(),
            Some(stale_version.version_string().as_str())
        );
        assert_eq!(status.failure.as_deref(), Some(failure.as_str()));
        assert_eq!(
            status.selection_source(),
            RpcBootInterfaceSelectionSource::RedfishUefiPci,
        );
        assert_eq!(status.selection_updated_at, Some(observed_at.into()));
    }
}
