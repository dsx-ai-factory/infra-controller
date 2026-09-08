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

use carbide_uuid::machine::{HostMachineId, MachineId, MachineType};
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use mac_address::MacAddress;
use model::machine_boot_interface::{
    BootInterfaceSelection, BootInterfaceSelectionAuthority, BootInterfaceSelectionSource,
    MachineBootInterface, MachineBootInterfaceTarget, canonical_redfish_boot_interface_id,
};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

#[derive(Debug, sqlx::FromRow)]
struct DesiredBootInterfaceRow {
    machine_version: ConfigVersion,
    desired_mac_address: Option<MacAddress>,
    desired_interface_id: Option<String>,
    desired_version: Option<ConfigVersion>,
    selection_source: Option<BootInterfaceSelectionSource>,
    selection_updated_at: Option<DateTime<Utc>>,
    rollout_baseline_eligible: bool,
}

/// A desired target and its selection metadata decoded from one consistent row.
#[derive(Clone, Debug)]
struct DecodedDesiredBootInterface {
    desired: Versioned<MachineBootInterfaceTarget>,
    selection: BootInterfaceSelection,
}

impl DesiredBootInterfaceRow {
    /// Decodes the nullable child columns together, rejecting partial records.
    ///
    /// `None` means the child row has not been initialized. Any other
    /// incomplete combination is persisted corruption rather than an absent
    /// desired target.
    fn decode(
        self,
        machine_id: &HostMachineId,
    ) -> DatabaseResult<Option<DecodedDesiredBootInterface>> {
        match (
            self.desired_mac_address,
            self.desired_interface_id,
            self.desired_version,
            self.selection_source,
            self.selection_updated_at,
        ) {
            (None, None, None, None, None) => Ok(None),
            (Some(mac_address), interface_id, Some(version), Some(source), updated_at) => {
                if source != BootInterfaceSelectionSource::LegacyUnknown && updated_at.is_none() {
                    return Err(DatabaseError::Internal {
                        message: format!(
                            "machine {machine_id} has an attributed boot interface without a selection timestamp"
                        ),
                    });
                }
                if let Some(interface_id) = interface_id.as_deref()
                    && canonical_redfish_boot_interface_id(interface_id) != Some(interface_id)
                {
                    return Err(DatabaseError::Internal {
                        message: format!(
                            "machine {machine_id} has an empty or noncanonical desired boot interface id"
                        ),
                    });
                }
                let value = MachineBootInterfaceTarget::from_parts(Some(mac_address), interface_id)
                    .ok_or_else(|| DatabaseError::Internal {
                        message: format!(
                            "machine {machine_id} has an invalid desired boot interface"
                        ),
                    })?;
                Ok(Some(DecodedDesiredBootInterface {
                    desired: Versioned { value, version },
                    selection: BootInterfaceSelection { source, updated_at },
                }))
            }
            _ => Err(DatabaseError::Internal {
                message: format!(
                    "machine {machine_id} has an inconsistent desired boot interface row"
                ),
            }),
        }
    }
}

fn validate_target(target: &MachineBootInterfaceTarget) -> DatabaseResult<()> {
    if let Some(interface_id) = target.interface_id()
        && canonical_redfish_boot_interface_id(interface_id) != Some(interface_id)
    {
        Err(DatabaseError::InvalidArgument(
            "desired boot interface id must be nonempty and canonical".to_string(),
        ))
    } else {
        Ok(())
    }
}

async fn load(
    db: impl DbReader<'_>,
    machine_id: &HostMachineId,
) -> DatabaseResult<DesiredBootInterfaceRow> {
    let query = r#"
        SELECT
            machine.version AS machine_version,
            boot_interface.desired_mac_address,
            boot_interface.desired_interface_id,
            boot_interface.desired_version,
            boot_interface.selection_source,
            boot_interface.selection_updated_at,
            COALESCE(
                machine.controller_state->>'state' IN ('ready', 'assigned'),
                false
            ) AS rollout_baseline_eligible
        FROM machines machine
        LEFT JOIN machine_boot_interfaces boot_interface
            ON boot_interface.machine_id = machine.id
        WHERE machine.id = $1
    "#;

    sqlx::query_as(query)
        .bind(machine_id)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })
}

async fn load_for_update(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
) -> DatabaseResult<DesiredBootInterfaceRow> {
    let query = r#"
        SELECT
            machine.version AS machine_version,
            boot_interface.desired_mac_address,
            boot_interface.desired_interface_id,
            boot_interface.desired_version,
            boot_interface.selection_source,
            boot_interface.selection_updated_at,
            COALESCE(
                machine.controller_state->>'state' IN ('ready', 'assigned'),
                false
            ) AS rollout_baseline_eligible
        FROM machines machine
        LEFT JOIN machine_boot_interfaces boot_interface
            ON boot_interface.machine_id = machine.id
        WHERE machine.id = $1
        FOR UPDATE OF machine
    "#;

    sqlx::query_as(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })
}

/// `get` returns the host's desired boot interface, or `None` before Site
/// Explorer has initialized it.
pub async fn get(
    db: impl DbReader<'_>,
    machine_id: &HostMachineId,
) -> Result<Option<Versioned<MachineBootInterfaceTarget>>, DatabaseError> {
    Ok(load(db, machine_id)
        .await?
        .decode(machine_id)?
        .map(|decoded| decoded.desired))
}

/// `lock` reads the desired boot interface while locking the machine row for
/// the rest of the caller's transaction.
pub async fn lock(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
) -> Result<Option<Versioned<MachineBootInterfaceTarget>>, DatabaseError> {
    Ok(load_for_update(txn, machine_id)
        .await?
        .decode(machine_id)?
        .map(|decoded| decoded.desired))
}

/// Returns one keyset page of hosts whose desired target is unset or still
/// lacks a Redfish id.
pub async fn find_incomplete_machine_ids(
    db: impl DbReader<'_>,
    after_id: Option<&HostMachineId>,
    limit: i64,
) -> DatabaseResult<Vec<HostMachineId>> {
    let query = r#"
        SELECT machine.id
        FROM machines machine
        LEFT JOIN machine_boot_interfaces boot_interface
            ON boot_interface.machine_id = machine.id
        WHERE (
                starts_with(machine.id, $1)
                OR starts_with(machine.id, $2)
            )
          AND boot_interface.desired_interface_id IS NULL
          AND ($3::text IS NULL OR machine.id::text > $3)
        ORDER BY machine.id
        LIMIT $4
    "#;

    sqlx::query_scalar(query)
        .bind(MachineType::Host.id_prefix())
        .bind(MachineType::PredictedHost.id_prefix())
        .bind(after_id)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

fn request_is_satisfied(
    current: &MachineBootInterfaceTarget,
    requested: &MachineBootInterfaceTarget,
) -> bool {
    current == requested
        || matches!(
            (current, requested),
            (
                MachineBootInterfaceTarget::Pair(current),
                MachineBootInterfaceTarget::MacOnly(requested_mac),
            ) if current.mac_address == *requested_mac
        )
}

fn next_version(expected_version: Option<ConfigVersion>) -> ConfigVersion {
    expected_version
        .map(|version| version.increment())
        .unwrap_or_else(ConfigVersion::initial)
}

/// Controls how a desired generation write treats its verification status.
#[derive(Clone, Copy)]
enum VerificationPolicy {
    /// Leave the new desired generation pending verification.
    Pending,
    /// Record the new generation as an assumed rollout baseline.
    AssumeVerified,
    /// Advance verification only when it covered the previous generation.
    CarryCurrentForward,
}

/// Controls whether a desired generation write also changes its selection
/// source and decision time.
///
/// Adding a Redfish ID for the same MAC and retrying after drift preserve the
/// original decision. A new selected MAC or source records the supplied source,
/// while a rollout baseline uses `LegacyUnknown` without inventing a decision
/// time.
#[derive(Clone, Copy)]
enum SelectionSourceUpdate {
    /// Keep the source and decision timestamp attached to the current selection.
    Preserve,
    /// Store the supplied source and optionally record a new decision time.
    Set {
        source: BootInterfaceSelectionSource,
        update_timestamp: bool,
    },
}

impl SelectionSourceUpdate {
    /// Converts the policy into the source and timestamp parameters used by SQL.
    fn into_query_parameters(self) -> (Option<BootInterfaceSelectionSource>, bool) {
        match self {
            Self::Preserve => (None, false),
            Self::Set {
                source,
                update_timestamp,
            } => (Some(source), update_timestamp),
        }
    }
}

/// Result of a desired boot interface write.
#[derive(Clone, Debug)]
pub struct SetDesiredBootInterfaceOutcome {
    /// Persisted desired target and generation after the write.
    pub desired: Versioned<MachineBootInterfaceTarget>,
    /// Whether the write opened a new desired convergence generation.
    pub desired_changed: bool,
}

/// Advances the parent aggregate after changing its boot interface child row.
///
/// The caller holds the machine row lock and supplies the version read while
/// acquiring it, so a missing update indicates an internal locking violation.
async fn bump_machine_version(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    current_machine_version: ConfigVersion,
) -> DatabaseResult<()> {
    let machine_version = current_machine_version.increment();
    let query = r#"
        UPDATE machines
        SET version = $1
        WHERE id = $2
          AND version = $3
        RETURNING id
    "#;
    let bumped: Option<MachineId> = sqlx::query_scalar(query)
        .bind(machine_version)
        .bind(machine_id)
        .bind(current_machine_version)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if bumped.is_none() {
        return Err(DatabaseError::Internal {
            message: format!(
                "failed to bump the aggregate version for locked machine {machine_id}"
            ),
        });
    }

    Ok(())
}

/// Writes a desired generation and its selection metadata with one child row CAS.
///
/// A successful child write also advances the parent machine aggregate. `None`
/// means the expected child generation no longer matches.
async fn write_desired_generation(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    current_machine_version: ConfigVersion,
    expected_version: Option<ConfigVersion>,
    target: &MachineBootInterfaceTarget,
    verification_policy: VerificationPolicy,
    selection_source_update: SelectionSourceUpdate,
) -> DatabaseResult<Option<ConfigVersion>> {
    let (selection_source, update_selection_timestamp) =
        selection_source_update.into_query_parameters();
    let (assume_verified, carry_current_forward) = match verification_policy {
        VerificationPolicy::Pending => (false, false),
        VerificationPolicy::AssumeVerified => (true, false),
        VerificationPolicy::CarryCurrentForward => (false, true),
    };
    let desired_version = next_version(expected_version);
    let updated: Option<MachineId> = if let Some(expected_version) = expected_version {
        let query = r#"
            UPDATE machine_boot_interfaces
            SET desired_mac_address = $1,
                desired_interface_id = $2,
                desired_version = $3,
                verified_version = CASE
                    WHEN $6 AND verified_version = $5 THEN $3
                    ELSE verified_version
                END,
                selection_source = COALESCE($7, selection_source),
                selection_updated_at = CASE
                    WHEN $8 THEN GREATEST(
                        statement_timestamp(),
                        selection_updated_at + INTERVAL '1 microsecond'
                    )
                    ELSE selection_updated_at
                END
            WHERE machine_id = $4
              AND desired_version = $5
            RETURNING machine_id
        "#;
        sqlx::query_scalar(query)
            .bind(target.mac_address())
            .bind(target.interface_id())
            .bind(desired_version)
            .bind(machine_id)
            .bind(expected_version)
            .bind(carry_current_forward)
            .bind(selection_source)
            .bind(update_selection_timestamp)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?
    } else {
        // An assumed rollout baseline uses the transaction start because it
        // describes state that existed before this write. A decision that
        // records its source uses statement time so a writer delayed on the
        // machine lock cannot record a timestamp older than the decision it
        // replaces.
        let query = r#"
            INSERT INTO machine_boot_interfaces (
                machine_id,
                desired_mac_address,
                desired_interface_id,
                desired_version,
                verified_version,
                observed_at,
                assumed,
                selection_source,
                selection_updated_at
            )
            VALUES (
                $1,
                $2,
                $3,
                $4,
                CASE WHEN $5 THEN $4 END,
                CASE WHEN $5 THEN CURRENT_TIMESTAMP END,
                $5,
                $6,
                CASE WHEN $7 THEN statement_timestamp() END
            )
            ON CONFLICT (machine_id) DO NOTHING
            RETURNING machine_id
        "#;
        sqlx::query_scalar(query)
            .bind(machine_id)
            .bind(target.mac_address())
            .bind(target.interface_id())
            .bind(desired_version)
            .bind(assume_verified)
            .bind(selection_source.ok_or_else(|| DatabaseError::Internal {
                message: format!(
                    "cannot initialize desired boot interface for machine {machine_id} without a selection source"
                ),
            })?)
            .bind(update_selection_timestamp)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?
    };

    if updated.is_none() {
        return Ok(None);
    }

    bump_machine_version(txn, machine_id, current_machine_version).await?;

    Ok(Some(desired_version))
}

/// Changes the recorded selection source and decision time without opening a
/// new desired generation.
///
/// A write that changes only the source still advances the parent machine
/// aggregate.
/// `false` means the expected desired generation no longer matches.
async fn update_selection_source(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    current_machine_version: ConfigVersion,
    expected_version: ConfigVersion,
    source: BootInterfaceSelectionSource,
) -> DatabaseResult<bool> {
    let query = r#"
        UPDATE machine_boot_interfaces
        SET selection_source = $1,
            selection_updated_at = GREATEST(
                statement_timestamp(),
                selection_updated_at + INTERVAL '1 microsecond'
            )
        WHERE machine_id = $2
          AND desired_version = $3
        RETURNING machine_id
    "#;
    let updated: Option<MachineId> = sqlx::query_scalar(query)
        .bind(source)
        .bind(machine_id)
        .bind(expected_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if updated.is_none() {
        return Ok(false);
    }

    bump_machine_version(txn, machine_id, current_machine_version).await?;
    Ok(true)
}

/// `try_set` changes the target only when `expected_version` still matches.
///
/// It returns `true` for both an update and a request that is already satisfied.
/// A satisfied request preserves the current selection source even
/// when the requested source or expected version differs. `false` means
/// another writer changed the target first.
pub async fn try_set(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    expected_version: Option<ConfigVersion>,
    target: &MachineBootInterfaceTarget,
    source: BootInterfaceSelectionSource,
) -> Result<bool, DatabaseError> {
    validate_target(target)?;

    let row = load_for_update(txn, machine_id).await?;
    let current_machine_version = row.machine_version;
    let current = row.decode(machine_id)?;

    if current
        .as_ref()
        .is_some_and(|current| request_is_satisfied(&current.desired.value, target))
    {
        // A complete pair is stronger than the same MAC alone. Treat the
        // weaker retry as satisfied so it cannot discard the Redfish id.
        return Ok(true);
    }

    if current.as_ref().map(|current| current.desired.version) != expected_version {
        return Ok(false);
    }

    let selection_changed = current.as_ref().is_none_or(|current| {
        current.selection.source != source
            || current.desired.value.mac_address() != target.mac_address()
    });

    Ok(write_desired_generation(
        txn,
        machine_id,
        current_machine_version,
        expected_version,
        target,
        VerificationPolicy::Pending,
        SelectionSourceUpdate::Set {
            source,
            update_timestamp: selection_changed,
        },
    )
    .await?
    .is_some())
}

/// `set` stores an explicitly sourced target, serializing concurrent writers
/// on the machine row.
///
/// Repeating the current target with the same source changes nothing. A new
/// source for the same target updates the selection metadata without opening a
/// desired generation. A request with the same MAC but no interface ID also
/// keeps an existing pair so an operator retry cannot discard its Redfish ID.
pub async fn set(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    target: &MachineBootInterfaceTarget,
    source: BootInterfaceSelectionSource,
) -> Result<SetDesiredBootInterfaceOutcome, DatabaseError> {
    set_with_mode(txn, machine_id, target, source, SetMode::IfChanged).await
}

/// `force_set` stores an explicitly sourced target as a new pending generation,
/// even when the value is unchanged.
///
/// A request with the same MAC but no interface ID keeps an existing complete
/// pair, so forcing convergence cannot discard the Redfish ID we already
/// learned.
pub async fn force_set(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    target: &MachineBootInterfaceTarget,
    source: BootInterfaceSelectionSource,
) -> Result<SetDesiredBootInterfaceOutcome, DatabaseError> {
    set_with_mode(txn, machine_id, target, source, SetMode::Force).await
}

/// Opens a pending boot interface generation using the caller's selection
/// authority.
///
/// A request with operator authority records an operator selection. Reapplying an
/// existing selection accepts only its current MAC, preserves its source and
/// decision time, and may refresh the Redfish id for that MAC. If no selection
/// exists yet, a request with existing selection authority records
/// [`BootInterfaceSelectionSource::LegacyUnknown`].
pub async fn force_reconcile(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    target: &MachineBootInterfaceTarget,
    authority: BootInterfaceSelectionAuthority,
) -> Result<SetDesiredBootInterfaceOutcome, DatabaseError> {
    match authority {
        BootInterfaceSelectionAuthority::Operator => {
            force_set(
                txn,
                machine_id,
                target,
                BootInterfaceSelectionSource::Operator,
            )
            .await
        }
        BootInterfaceSelectionAuthority::Existing => {
            force_reconcile_existing(txn, machine_id, target).await
        }
    }
}

/// Opens a pending generation for the boot interface already selected without
/// claiming that the reconciliation request selected it.
///
/// An existing complete pair is retained when the caller supplies only its MAC.
/// A newly observed complete pair for the same MAC refreshes the Redfish
/// id without changing the selection source or decision time. A different MAC
/// is rejected instead of silently changing operator or discovery intent. If
/// no target exists, the request establishes an actively recorded
/// [`BootInterfaceSelectionSource::LegacyUnknown`] selection.
async fn force_reconcile_existing(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    target: &MachineBootInterfaceTarget,
) -> Result<SetDesiredBootInterfaceOutcome, DatabaseError> {
    validate_target(target)?;

    let row = load_for_update(txn, machine_id).await?;
    let current_machine_version = row.machine_version;
    let current = row.decode(machine_id)?;
    let (target, expected_version, selection) = if let Some(current) = current.as_ref() {
        let target = if request_is_satisfied(&current.desired.value, target) {
            &current.desired.value
        } else if matches!(target, MachineBootInterfaceTarget::Pair(_))
            && current.desired.value.mac_address() == target.mac_address()
        {
            // A resolved or refreshed Redfish id describes the same selected
            // physical target; it does not establish a new selection source
            // or decision time.
            target
        } else {
            return Err(DatabaseError::InvalidArgument(format!(
                "cannot reconcile boot interface MAC {} for machine {machine_id} because its selected MAC is {}",
                target.mac_address(),
                current.desired.value.mac_address(),
            )));
        };
        (
            target,
            Some(current.desired.version),
            SelectionSourceUpdate::Preserve,
        )
    } else {
        (
            target,
            None,
            SelectionSourceUpdate::Set {
                source: BootInterfaceSelectionSource::LegacyUnknown,
                update_timestamp: true,
            },
        )
    };

    let Some(version) = write_desired_generation(
        txn,
        machine_id,
        current_machine_version,
        expected_version,
        target,
        VerificationPolicy::Pending,
        selection,
    )
    .await?
    else {
        return Err(DatabaseError::Internal {
            message: format!(
                "failed to force boot interface reconciliation for locked machine {machine_id}"
            ),
        });
    };

    Ok(SetDesiredBootInterfaceOutcome {
        desired: Versioned {
            value: target.clone(),
            version,
        },
        desired_changed: true,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetMode {
    IfChanged,
    Force,
}

/// Serializes the desired-target write and applies the caller's generation
/// policy without weakening a complete interface pair.
async fn set_with_mode(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    target: &MachineBootInterfaceTarget,
    source: BootInterfaceSelectionSource,
    mode: SetMode,
) -> Result<SetDesiredBootInterfaceOutcome, DatabaseError> {
    validate_target(target)?;

    let row = load_for_update(txn, machine_id).await?;
    let current_machine_version = row.machine_version;
    let current = row.decode(machine_id)?;
    if mode == SetMode::IfChanged
        && let Some(current) = current.as_ref()
        && request_is_satisfied(&current.desired.value, target)
    {
        if current.selection.source == source {
            return Ok(SetDesiredBootInterfaceOutcome {
                desired: current.desired.clone(),
                desired_changed: false,
            });
        }

        if !update_selection_source(
            txn,
            machine_id,
            current_machine_version,
            current.desired.version,
            source,
        )
        .await?
        {
            return Err(DatabaseError::Internal {
                message: format!(
                    "failed to update boot interface selection source for locked machine {machine_id}"
                ),
            });
        }
        return Ok(SetDesiredBootInterfaceOutcome {
            desired: current.desired.clone(),
            desired_changed: false,
        });
    }

    let target = current
        .as_ref()
        .filter(|current| request_is_satisfied(&current.desired.value, target))
        .map_or(target, |current| &current.desired.value);
    let selection_changed = current.as_ref().is_none_or(|current| {
        current.selection.source != source
            || current.desired.value.mac_address() != target.mac_address()
    });
    let expected_version = current.as_ref().map(|current| current.desired.version);
    let Some(version) = write_desired_generation(
        txn,
        machine_id,
        current_machine_version,
        expected_version,
        target,
        VerificationPolicy::Pending,
        SelectionSourceUpdate::Set {
            source,
            update_timestamp: selection_changed,
        },
    )
    .await?
    else {
        return Err(DatabaseError::Internal {
            message: format!(
                "failed to set desired boot interface for locked machine {machine_id}"
            ),
        });
    };

    Ok(SetDesiredBootInterfaceOutcome {
        desired: Versioned {
            value: target.clone(),
            version,
        },
        desired_changed: true,
    })
}

/// `initialize_if_unset` stores Site Explorer's initial target without
/// replacing a target that is already present.
///
/// A stable host already in `Ready` or `Assigned` receives an assumed
/// compatibility observation and records `LegacyUnknown` without a decision
/// time. Current exploration cannot recover how that host originally selected
/// its target. This covers a missing row while old and new component versions
/// run together or during a later repair without scheduling remediation across
/// the fleet. Normal lifecycle initialization happens on a predicted host or in
/// `HostInit`, so newly provisioned hosts remain pending real verification.
pub async fn initialize_if_unset(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    target: &MachineBootInterfaceTarget,
    source: BootInterfaceSelectionSource,
) -> Result<Versioned<MachineBootInterfaceTarget>, DatabaseError> {
    validate_target(target)?;

    let row = load_for_update(txn, machine_id).await?;
    let current_machine_version = row.machine_version;
    let assume_verified = machine_id.is_stable_host() && row.rollout_baseline_eligible;
    let verification_policy = if assume_verified {
        VerificationPolicy::AssumeVerified
    } else {
        VerificationPolicy::Pending
    };
    if let Some(current) = row.decode(machine_id)? {
        return Ok(current.desired);
    }

    let selection = if assume_verified {
        SelectionSourceUpdate::Set {
            source: BootInterfaceSelectionSource::LegacyUnknown,
            update_timestamp: false,
        }
    } else {
        SelectionSourceUpdate::Set {
            source,
            update_timestamp: true,
        }
    };

    let Some(version) = write_desired_generation(
        txn,
        machine_id,
        current_machine_version,
        None,
        target,
        verification_policy,
        selection,
    )
    .await?
    else {
        return Err(DatabaseError::Internal {
            message: format!(
                "failed to initialize desired boot interface for locked machine {machine_id}"
            ),
        });
    };

    Ok(Versioned {
        value: target.clone(),
        version,
    })
}

/// `enrich_interface_id` adds a Redfish id to a matching MAC-only target.
///
/// Once a pair is stored, later observations do not replace its id.
/// Enrichment advances a current status because it strengthens the same
/// physical target's identity; stale or pending status remains unchanged.
pub async fn enrich_interface_id(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    mac_address: MacAddress,
    interface_id: &str,
) -> Result<Option<Versioned<MachineBootInterfaceTarget>>, DatabaseError> {
    let Some(interface_id) = canonical_redfish_boot_interface_id(interface_id) else {
        return Err(DatabaseError::InvalidArgument(
            "desired boot interface id must not be blank".to_string(),
        ));
    };

    let row = load_for_update(txn, machine_id).await?;
    let current_machine_version = row.machine_version;
    let current = row.decode(machine_id)?;
    let Some(current) = current else {
        return Ok(None);
    };
    let MachineBootInterfaceTarget::MacOnly(current_mac) = &current.desired.value else {
        return Ok(Some(current.desired));
    };
    if *current_mac != mac_address {
        return Ok(Some(current.desired));
    }

    let target = MachineBootInterfaceTarget::Pair(MachineBootInterface {
        mac_address,
        interface_id: interface_id.to_string(),
    });
    let expected_version = Some(current.desired.version);
    let Some(version) = write_desired_generation(
        txn,
        machine_id,
        current_machine_version,
        expected_version,
        &target,
        VerificationPolicy::CarryCurrentForward,
        SelectionSourceUpdate::Preserve,
    )
    .await?
    else {
        return Err(DatabaseError::Internal {
            message: format!(
                "failed to enrich desired boot interface for locked machine {machine_id}"
            ),
        });
    };

    Ok(Some(Versioned {
        value: target,
        version,
    }))
}

/// Tries to reopen an inspected target as a new pending generation after
/// Redfish drift.
///
/// The parent machine lock and exact desired generation check make this an
/// observation result, not a blind `force_set`: newer operator intent wins.
/// The verified-version check also makes replay a no-op once a generation is
/// already pending. `None` means either condition changed before this result
/// could be persisted.
pub async fn try_reopen_after_observed_drift(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    inspected_boot_interface: &Versioned<MachineBootInterfaceTarget>,
) -> Result<Option<Versioned<MachineBootInterfaceTarget>>, DatabaseError> {
    validate_target(&inspected_boot_interface.value)?;

    let desired_boot_interface_row = load_for_update(txn, machine_id).await?;
    let current_machine_version = desired_boot_interface_row.machine_version;
    let Some(current_desired_boot_interface) = desired_boot_interface_row.decode(machine_id)?
    else {
        return Ok(None);
    };
    if current_desired_boot_interface.desired.version != inspected_boot_interface.version
        || current_desired_boot_interface.desired.value != inspected_boot_interface.value
    {
        return Ok(None);
    }

    // Read and lock the child status only after `load_for_update` has acquired
    // the parent-machine lock. Every desired/status writer uses this same
    // parent-first order, so this is both a fresh status read and deadlock-safe.
    let verified_version_query = r#"
        SELECT verified_version
        FROM machine_boot_interfaces
        WHERE machine_id = $1
        FOR UPDATE
    "#;
    let verified_version: Option<ConfigVersion> = sqlx::query_scalar(verified_version_query)
        .bind(machine_id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(verified_version_query, error))?;
    if verified_version != Some(current_desired_boot_interface.desired.version) {
        return Ok(None);
    }

    let Some(reopened_version) = write_desired_generation(
        txn,
        machine_id,
        current_machine_version,
        Some(current_desired_boot_interface.desired.version),
        &current_desired_boot_interface.desired.value,
        VerificationPolicy::Pending,
        SelectionSourceUpdate::Preserve,
    )
    .await?
    else {
        return Ok(None);
    };

    Ok(Some(Versioned {
        value: current_desired_boot_interface.desired.value,
        version: reopened_version,
    }))
}

/// Records a Redfish observation only if the desired boot-interface version
/// still matches the version the caller observed.
///
/// A `false` return means the desired target was removed or replaced before
/// the observation could be committed. The caller must not treat that newer
/// target as verified.
pub async fn mark_verified(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    expected_desired_version: ConfigVersion,
    observed_at: DateTime<Utc>,
) -> Result<bool, DatabaseError> {
    // Desired-target writers lock the parent machine row before touching this
    // child row. Preserve that order so a concurrent operator write cannot
    // deadlock verification against the state-controller transition, which
    // also updates the parent before commit.
    load_for_update(txn, machine_id).await?;

    let query = r#"
        UPDATE machine_boot_interfaces
        SET verified_version = desired_version,
            observed_at = $1,
            assumed = false
        WHERE machine_id = $2
          AND desired_version = $3
        RETURNING machine_id
    "#;
    let updated: Option<MachineId> = sqlx::query_scalar(query)
        .bind(observed_at)
        .bind(machine_id)
        .bind(expected_desired_version)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    Ok(updated.is_some())
}

#[cfg(test)]
mod tests {
    use BootInterfaceSelectionAuthority::Existing;
    use BootInterfaceSelectionSource::{Operator, RedfishUefiPci};
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use model::machine::machine_search_config::MachineSearchConfig;
    use model::machine::{InstanceState, MachineState, ManagedHostState};
    use sqlx::PgPool;
    use sqlx::types::Json;

    use super::*;

    const MIGRATION: &str =
        include_str!("../migrations/20260728120000_machine_boot_interfaces.sql");
    const STATUS_MIGRATION: &str =
        include_str!("../migrations/20260730120000_machine_boot_interface_status.sql");
    const SELECTION_MIGRATION: &str =
        include_str!("../migrations/20260819221226_boot_interface_selection_source.sql");

    fn host_machine_id(marker: u8) -> HostMachineId {
        let mut hardware_id = [0u8; 32];
        hardware_id[0] = marker;
        MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            hardware_id,
            MachineType::Host,
        )
        .try_into()
        .expect("tests should only construct host and predicted hosts")
    }

    fn predicted_host_machine_id(marker: u8) -> HostMachineId {
        let mut hardware_id = [0u8; 32];
        hardware_id[0] = marker;
        MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            hardware_id,
            MachineType::PredictedHost,
        )
        .try_into()
        .expect("tests should only construct host and predicted hosts")
    }

    fn dpu_machine_id(marker: u8) -> MachineId {
        let mut hardware_id = [0u8; 32];
        hardware_id[0] = marker;
        MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            hardware_id,
            MachineType::Dpu,
        )
    }

    async fn seed_machine(
        txn: &mut PgConnection,
        machine_id: &MachineId,
    ) -> Result<ConfigVersion, sqlx::Error> {
        let query = r#"
            INSERT INTO machines (id, dpf)
            VALUES (
                $1,
                '{"enabled": false, "used_for_ingestion": false}'::jsonb
            )
            RETURNING version
        "#;
        sqlx::query_scalar(query)
            .bind(machine_id)
            .fetch_one(txn)
            .await
    }

    async fn versions(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
    ) -> Result<(ConfigVersion, Option<ConfigVersion>), sqlx::Error> {
        sqlx::query_as(
            "SELECT machine.version, boot_interface.desired_version
             FROM machines machine
             LEFT JOIN machine_boot_interfaces boot_interface
                 ON boot_interface.machine_id = machine.id
             WHERE machine.id = $1",
        )
        .bind(machine_id)
        .fetch_one(txn)
        .await
    }

    async fn set_controller_state(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
        state: ManagedHostState,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE machines SET controller_state = $1 WHERE id = $2")
            .bind(Json(state))
            .bind(machine_id)
            .execute(txn)
            .await?;
        Ok(())
    }

    async fn status_observation(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
    ) -> Result<(Option<ConfigVersion>, Option<DateTime<Utc>>, bool), sqlx::Error> {
        sqlx::query_as(
            "SELECT verified_version, observed_at, assumed
             FROM machine_boot_interfaces
             WHERE machine_id = $1",
        )
        .bind(machine_id)
        .fetch_one(txn)
        .await
    }

    /// Test helper that decodes persisted selection metadata for assertions.
    async fn load_persisted_selection(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
    ) -> Result<BootInterfaceSelection, DatabaseError> {
        load(txn, machine_id)
            .await?
            .decode(machine_id)?
            .map(|decoded| decoded.selection)
            .ok_or_else(|| DatabaseError::Internal {
                message: format!("machine {machine_id} has no boot interface selection"),
            })
    }

    /// Test fixture helper that sets a deterministic selection timestamp without
    /// exercising production selection or version update behavior.
    async fn overwrite_selection_updated_at(
        txn: &mut PgConnection,
        machine_id: &HostMachineId,
        updated_at: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE machine_boot_interfaces
             SET selection_updated_at = $1
             WHERE machine_id = $2",
        )
        .bind(updated_at)
        .bind(machine_id)
        .execute(txn)
        .await?;
        Ok(())
    }

    fn assert_target(
        actual: &Versioned<MachineBootInterfaceTarget>,
        expected: &MachineBootInterfaceTarget,
    ) {
        assert_eq!(&actual.value, expected);
    }

    #[crate::sqlx_test]
    async fn initialize_only_sets_an_uninitialized_host(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(1);
        let initial_machine_version = seed_machine(txn.as_mut(), &machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let initial_target = MachineBootInterfaceTarget::MacOnly(mac_address);

        assert!(get(txn.as_mut(), &machine_id).await?.is_none());
        let initialized =
            initialize_if_unset(txn.as_mut(), &machine_id, &initial_target, RedfishUefiPci).await?;
        assert_target(&initialized, &initial_target);
        assert_eq!(initialized.version.version_nr(), 1);
        let initialized_selection = load_persisted_selection(txn.as_mut(), &machine_id).await?;
        assert_eq!(initialized_selection.source, RedfishUefiPci);
        assert!(initialized_selection.updated_at.is_some());

        let replacement = MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 0, 2]));
        let existing = initialize_if_unset(
            txn.as_mut(),
            &machine_id,
            &replacement,
            BootInterfaceSelectionSource::RedfishSerialNumber,
        )
        .await?;
        assert_target(&existing, &initial_target);
        assert_eq!(existing.version, initialized.version);
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &machine_id).await?,
            initialized_selection
        );

        let (machine_version, desired_version) = versions(txn.as_mut(), &machine_id).await?;
        assert_eq!(
            machine_version.version_nr(),
            initial_machine_version.version_nr() + 1
        );
        assert_eq!(desired_version, Some(initialized.version));

        Ok(())
    }

    #[crate::sqlx_test]
    async fn rollout_baseline_only_applies_to_existing_stable_ready_or_assigned_hosts(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let cases = [
            (host_machine_id(30), ManagedHostState::Ready, true),
            (
                host_machine_id(31),
                ManagedHostState::Assigned {
                    instance_state: InstanceState::Init,
                },
                true,
            ),
            (
                host_machine_id(32),
                ManagedHostState::HostInit {
                    machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
                },
                false,
            ),
            (
                predicted_host_machine_id(33),
                ManagedHostState::Ready,
                false,
            ),
        ];

        for (index, (machine_id, state, expect_assumed)) in cases.into_iter().enumerate() {
            seed_machine(txn.as_mut(), &machine_id).await?;
            set_controller_state(txn.as_mut(), &machine_id, state).await?;
            let target =
                MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 3, index as u8]));
            let initialized =
                initialize_if_unset(txn.as_mut(), &machine_id, &target, RedfishUefiPci).await?;
            let (verified_version, observed_at, assumed) =
                status_observation(txn.as_mut(), &machine_id).await?;

            if expect_assumed {
                assert_eq!(verified_version, Some(initialized.version));
                assert!(observed_at.is_some());
                assert!(assumed);
                assert_eq!(
                    load_persisted_selection(txn.as_mut(), &machine_id).await?,
                    BootInterfaceSelection {
                        source: BootInterfaceSelectionSource::LegacyUnknown,
                        updated_at: None,
                    },
                );
            } else {
                assert_eq!(verified_version, None);
                assert_eq!(observed_at, None);
                assert!(!assumed);
                let selection = load_persisted_selection(txn.as_mut(), &machine_id).await?;
                assert_eq!(selection.source, RedfishUefiPci);
                assert!(selection.updated_at.is_some());
            }
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn verification_is_a_cas_and_is_exposed_in_machine_snapshots(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(34);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let target = MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 3, 4]));
        let initialized =
            initialize_if_unset(txn.as_mut(), &machine_id, &target, RedfishUefiPci).await?;
        let selection_updated_at =
            DateTime::from_timestamp(1_700_000_034, 123_000_000).expect("fixture timestamp");
        overwrite_selection_updated_at(txn.as_mut(), &machine_id, selection_updated_at).await?;
        let observed_at =
            DateTime::from_timestamp(1_722_000_000, 123_000_000).expect("fixture timestamp");

        assert!(
            !mark_verified(
                txn.as_mut(),
                &machine_id,
                ConfigVersion::invalid(),
                observed_at,
            )
            .await?
        );
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (None, None, false)
        );

        assert!(mark_verified(txn.as_mut(), &machine_id, initialized.version, observed_at,).await?);
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (Some(initialized.version), Some(observed_at), false)
        );

        let machine =
            crate::machine::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
                .await?
                .expect("machine snapshot");
        let observation = machine
            .status
            .boot_interface_status_observation
            .expect("boot interface status observation");
        assert_eq!(observation.config_version, initialized.version);
        assert_eq!(observation.observed_at, observed_at);
        assert!(!observation.assumed);
        assert_eq!(
            machine.config.boot_interface_selection,
            Some(BootInterfaceSelection {
                source: RedfishUefiPci,
                updated_at: Some(selection_updated_at),
            }),
        );

        let replacement = MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 3, 5]));
        let updated = set(txn.as_mut(), &machine_id, &replacement, Operator)
            .await?
            .desired;
        assert_ne!(updated.version, initialized.version);
        assert!(!mark_verified(txn.as_mut(), &machine_id, initialized.version, Utc::now(),).await?);
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (Some(initialized.version), Some(observed_at), false),
            "changing the target keeps the last factual observation but makes its version stale",
        );

        Ok(())
    }

    /// A drift result for an old generation cannot overwrite newer operator intent.
    #[crate::sqlx_test]
    async fn observation_results_reject_a_newer_desired_generation(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(44);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let inspected_target = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address: MacAddress::new([2, 0, 0, 0, 4, 4]),
            interface_id: "NIC.Slot.4-1-1".to_string(),
        });
        let inspected_desired = set(txn.as_mut(), &machine_id, &inspected_target, Operator)
            .await?
            .desired;
        let observed_at =
            DateTime::from_timestamp(1_722_000_300, 123_000_000).expect("fixture timestamp");
        assert!(
            mark_verified(
                txn.as_mut(),
                &machine_id,
                inspected_desired.version,
                observed_at,
            )
            .await?
        );

        let operator_target =
            MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 4, 5]));
        let operator_desired = set(txn.as_mut(), &machine_id, &operator_target, Operator)
            .await?
            .desired;
        assert!(
            try_reopen_after_observed_drift(txn.as_mut(), &machine_id, &inspected_desired)
                .await?
                .is_none()
        );
        let persisted_desired = get(txn.as_mut(), &machine_id)
            .await?
            .expect("operator-selected target");
        assert_target(&persisted_desired, &operator_desired.value);
        assert_eq!(persisted_desired.version, operator_desired.version);
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (Some(inspected_desired.version), Some(observed_at), false,),
        );

        Ok(())
    }

    /// Exact Pair drift opens one pending generation for the same target.
    #[crate::sqlx_test]
    async fn observation_drift_reopens_the_exact_pair_once(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(45);
        let initial_machine_version = seed_machine(txn.as_mut(), &machine_id).await?;
        let inspected_target = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address: MacAddress::new([2, 0, 0, 0, 4, 6]),
            interface_id: "NIC.Slot.4-1-2".to_string(),
        });
        let inspected_desired = set(txn.as_mut(), &machine_id, &inspected_target, Operator)
            .await?
            .desired;
        let selection_time =
            DateTime::from_timestamp(1_700_000_400, 123_000_000).expect("fixture timestamp");
        overwrite_selection_updated_at(txn.as_mut(), &machine_id, selection_time).await?;
        let observed_at =
            DateTime::from_timestamp(1_722_000_400, 123_000_000).expect("fixture timestamp");
        assert!(
            mark_verified(
                txn.as_mut(),
                &machine_id,
                inspected_desired.version,
                observed_at,
            )
            .await?
        );

        let pending_desired =
            try_reopen_after_observed_drift(txn.as_mut(), &machine_id, &inspected_desired)
                .await?
                .expect("fresh pending generation");
        assert_target(&pending_desired, &inspected_target);
        assert_eq!(
            pending_desired.version.version_nr(),
            inspected_desired.version.version_nr() + 1
        );
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (Some(inspected_desired.version), Some(observed_at), false,),
            "drift keeps the last factual observation while the new generation is pending",
        );
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &machine_id).await?,
            BootInterfaceSelection {
                source: Operator,
                updated_at: Some(selection_time),
            },
            "drift reopening must preserve the original selection",
        );
        let (machine_version_after_reopen, desired_version_after_reopen) =
            versions(txn.as_mut(), &machine_id).await?;
        assert_eq!(
            machine_version_after_reopen.version_nr(),
            initial_machine_version.version_nr() + 2,
        );
        assert_eq!(desired_version_after_reopen, Some(pending_desired.version));

        assert!(
            try_reopen_after_observed_drift(txn.as_mut(), &machine_id, &pending_desired)
                .await?
                .is_none(),
            "an already-pending generation must not be reopened",
        );
        assert_eq!(
            versions(txn.as_mut(), &machine_id).await?,
            (machine_version_after_reopen, desired_version_after_reopen),
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn try_set_uses_cas_without_bumping_noops(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = predicted_host_machine_id(2);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 3]);
        let pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.7-1-1".to_string(),
        });
        let padded_pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: " \tNIC.Slot.7-1-1\n ".to_string(),
        });
        assert!(matches!(
            try_set(
                txn.as_mut(),
                &machine_id,
                None,
                &padded_pair,
                RedfishUefiPci,
            )
            .await,
            Err(DatabaseError::InvalidArgument(_))
        ));

        let initialized =
            initialize_if_unset(txn.as_mut(), &machine_id, &pair, RedfishUefiPci).await?;
        let versions_before = versions(txn.as_mut(), &machine_id).await?;
        let selection_before = load_persisted_selection(txn.as_mut(), &machine_id).await?;

        assert!(
            try_set(
                txn.as_mut(),
                &machine_id,
                Some(ConfigVersion::invalid()),
                &pair,
                RedfishUefiPci,
            )
            .await?
        );
        assert!(
            try_set(
                txn.as_mut(),
                &machine_id,
                Some(ConfigVersion::invalid()),
                &pair,
                Operator,
            )
            .await?
        );
        assert!(
            try_set(
                txn.as_mut(),
                &machine_id,
                Some(initialized.version),
                &MachineBootInterfaceTarget::MacOnly(mac_address),
                RedfishUefiPci,
            )
            .await?
        );
        assert_eq!(versions(txn.as_mut(), &machine_id).await?, versions_before);
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &machine_id).await?,
            selection_before,
            "an already-satisfied request must preserve its selection source and time",
        );
        assert_target(
            &get(txn.as_mut(), &machine_id).await?.expect("target"),
            &pair,
        );

        let replacement = MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 0, 4]));
        assert!(
            !try_set(
                txn.as_mut(),
                &machine_id,
                Some(ConfigVersion::invalid()),
                &replacement,
                RedfishUefiPci,
            )
            .await?
        );
        assert!(
            try_set(
                txn.as_mut(),
                &machine_id,
                Some(initialized.version),
                &replacement,
                RedfishUefiPci,
            )
            .await?
        );

        let updated = get(txn.as_mut(), &machine_id).await?.expect("target");
        assert_target(&updated, &replacement);
        assert_eq!(
            updated.version.version_nr(),
            initialized.version.version_nr() + 1
        );
        let versions_after = versions(txn.as_mut(), &machine_id).await?;
        assert_eq!(
            versions_after.0.version_nr(),
            versions_before.0.version_nr() + 1
        );
        assert_eq!(versions_after.1, Some(updated.version));

        Ok(())
    }

    #[crate::sqlx_test]
    async fn set_serializes_operator_updates_without_weakening_pairs(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(5);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 8]);
        let pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.8-1-1".to_string(),
        });

        let initialized = set(
            txn.as_mut(),
            &machine_id,
            &MachineBootInterfaceTarget::MacOnly(mac_address),
            Operator,
        )
        .await?
        .desired;
        let paired = set(txn.as_mut(), &machine_id, &pair, Operator)
            .await?
            .desired;
        assert_eq!(
            paired.version.version_nr(),
            initialized.version.version_nr() + 1
        );

        let unchanged = set(
            txn.as_mut(),
            &machine_id,
            &MachineBootInterfaceTarget::MacOnly(mac_address),
            Operator,
        )
        .await?
        .desired;
        assert_target(&unchanged, &pair);
        assert_eq!(unchanged.version, paired.version);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn every_selection_source_persists_and_source_changes_update_the_timestamp(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        struct Case {
            scenario: &'static str,
            source: BootInterfaceSelectionSource,
            replacement_source: BootInterfaceSelectionSource,
        }

        let cases = [
            Case {
                scenario: "ExpectedMachine",
                source: BootInterfaceSelectionSource::ExpectedMachine,
                replacement_source: Operator,
            },
            Case {
                scenario: "Operator",
                source: Operator,
                replacement_source: RedfishUefiPci,
            },
            Case {
                scenario: "RedfishUefiPci",
                source: RedfishUefiPci,
                replacement_source: BootInterfaceSelectionSource::RedfishChassisId,
            },
            Case {
                scenario: "RedfishChassisId",
                source: BootInterfaceSelectionSource::RedfishChassisId,
                replacement_source: BootInterfaceSelectionSource::RedfishSerialNumber,
            },
            Case {
                scenario: "RedfishSerialNumber",
                source: BootInterfaceSelectionSource::RedfishSerialNumber,
                replacement_source: BootInterfaceSelectionSource::ScoutReportPci,
            },
            Case {
                scenario: "ScoutReportPci",
                source: BootInterfaceSelectionSource::ScoutReportPci,
                replacement_source: BootInterfaceSelectionSource::LegacyUnknown,
            },
            Case {
                scenario: "LegacyUnknown",
                source: BootInterfaceSelectionSource::LegacyUnknown,
                replacement_source: BootInterfaceSelectionSource::ExpectedMachine,
            },
        ];
        let sentinel =
            DateTime::from_timestamp(1_700_004_000, 123_000_000).expect("fixture timestamp");
        let mut txn = pool.begin().await?;

        for (index, case) in cases.into_iter().enumerate() {
            let machine_id = predicted_host_machine_id(70 + index as u8);
            seed_machine(txn.as_mut(), &machine_id).await?;
            let target =
                MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 7, index as u8]));
            initialize_if_unset(txn.as_mut(), &machine_id, &target, case.source).await?;
            overwrite_selection_updated_at(txn.as_mut(), &machine_id, sentinel).await?;

            let same_source = set(txn.as_mut(), &machine_id, &target, case.source).await?;
            assert!(!same_source.desired_changed, "{} desired", case.scenario);
            assert_eq!(
                load_persisted_selection(txn.as_mut(), &machine_id).await?,
                BootInterfaceSelection {
                    source: case.source,
                    updated_at: Some(sentinel),
                },
                "{} identical write",
                case.scenario,
            );

            let replaced = set(txn.as_mut(), &machine_id, &target, case.replacement_source).await?;
            assert!(
                !replaced.desired_changed,
                "{} desired takeover",
                case.scenario
            );
            let selection = load_persisted_selection(txn.as_mut(), &machine_id).await?;
            assert_eq!(
                selection.source, case.replacement_source,
                "{} replacement source",
                case.scenario,
            );
            assert!(
                selection
                    .updated_at
                    .is_some_and(|updated_at| updated_at != sentinel),
                "{} replacement timestamp",
                case.scenario,
            );
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn operator_reconciliation_creates_a_pending_generation_without_weakening_pairs(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(37);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 3, 10]);
        let pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.10-1-1".to_string(),
        });
        let paired = set(txn.as_mut(), &machine_id, &pair, RedfishUefiPci)
            .await?
            .desired;
        let observed_at =
            DateTime::from_timestamp(1_722_000_200, 123_000_000).expect("fixture timestamp");
        assert!(mark_verified(txn.as_mut(), &machine_id, paired.version, observed_at).await?);
        let versions_before = versions(txn.as_mut(), &machine_id).await?;

        let forced = force_reconcile(
            txn.as_mut(),
            &machine_id,
            &MachineBootInterfaceTarget::MacOnly(mac_address),
            BootInterfaceSelectionAuthority::Operator,
        )
        .await?;
        assert_target(&forced.desired, &pair);
        assert_eq!(
            forced.desired.version.version_nr(),
            paired.version.version_nr() + 1,
        );
        assert!(forced.desired_changed);
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &machine_id)
                .await?
                .source,
            Operator,
        );
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (Some(paired.version), Some(observed_at), false),
            "the fresh generation must remain pending",
        );

        let versions_after = versions(txn.as_mut(), &machine_id).await?;
        assert_eq!(
            versions_after.0.version_nr(),
            versions_before.0.version_nr() + 1
        );
        assert_eq!(versions_after.1, Some(forced.desired.version));

        Ok(())
    }

    #[crate::sqlx_test]
    async fn existing_reconciliation_preserves_selection_and_records_active_unknowns(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let attributed_machine_id = host_machine_id(61);
        seed_machine(txn.as_mut(), &attributed_machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 6, 1]);
        let pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.6-1-1".to_string(),
        });
        let initial = initialize_if_unset(
            txn.as_mut(),
            &attributed_machine_id,
            &pair,
            BootInterfaceSelectionSource::ExpectedMachine,
        )
        .await?;
        let selection_time =
            DateTime::from_timestamp(1_700_003_000, 123_000_000).expect("fixture timestamp");
        overwrite_selection_updated_at(txn.as_mut(), &attributed_machine_id, selection_time)
            .await?;
        let observed_at =
            DateTime::from_timestamp(1_700_003_100, 123_000_000).expect("fixture timestamp");
        assert!(
            mark_verified(
                txn.as_mut(),
                &attributed_machine_id,
                initial.version,
                observed_at,
            )
            .await?
        );

        let reconciled = force_reconcile(
            txn.as_mut(),
            &attributed_machine_id,
            &MachineBootInterfaceTarget::MacOnly(mac_address),
            Existing,
        )
        .await?;
        assert!(reconciled.desired_changed);
        assert_target(&reconciled.desired, &pair);
        assert_eq!(
            reconciled.desired.version.version_nr(),
            initial.version.version_nr() + 1,
        );
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &attributed_machine_id).await?,
            BootInterfaceSelection {
                source: BootInterfaceSelectionSource::ExpectedMachine,
                updated_at: Some(selection_time),
            },
        );
        assert_eq!(
            status_observation(txn.as_mut(), &attributed_machine_id).await?,
            (Some(initial.version), Some(observed_at), false),
        );

        let versions_before_mismatch = versions(txn.as_mut(), &attributed_machine_id).await?;
        let mismatch = MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 6, 2]));
        assert!(matches!(
            force_reconcile(txn.as_mut(), &attributed_machine_id, &mismatch, Existing,).await,
            Err(DatabaseError::InvalidArgument(_)),
        ));
        assert_eq!(
            versions(txn.as_mut(), &attributed_machine_id).await?,
            versions_before_mismatch,
            "a mismatched reconciliation request must not change desired or aggregate versions",
        );

        let uninitialized_machine_id = host_machine_id(62);
        seed_machine(txn.as_mut(), &uninitialized_machine_id).await?;
        let unknown = force_reconcile(
            txn.as_mut(),
            &uninitialized_machine_id,
            &MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 6, 3])),
            Existing,
        )
        .await?;
        assert!(unknown.desired_changed);
        let unknown_selection =
            load_persisted_selection(txn.as_mut(), &uninitialized_machine_id).await?;
        assert_eq!(
            unknown_selection.source,
            BootInterfaceSelectionSource::LegacyUnknown,
        );
        assert!(
            unknown_selection.updated_at.is_some(),
            "an active unknown decision must retain its known decision time",
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn existing_reconciliation_refreshes_same_mac_without_reselecting(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(63);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 6, 4]);
        let initial = initialize_if_unset(
            txn.as_mut(),
            &machine_id,
            &MachineBootInterfaceTarget::MacOnly(mac_address),
            BootInterfaceSelectionSource::RedfishSerialNumber,
        )
        .await?;
        let selection_time =
            DateTime::from_timestamp(1_700_003_200, 123_000_000).expect("fixture timestamp");
        overwrite_selection_updated_at(txn.as_mut(), &machine_id, selection_time).await?;
        let pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.6-4-1".to_string(),
        });

        let reconciled = force_reconcile(txn.as_mut(), &machine_id, &pair, Existing).await?;

        assert_target(&reconciled.desired, &pair);
        assert_eq!(
            reconciled.desired.version.version_nr(),
            initial.version.version_nr() + 1,
        );
        assert!(reconciled.desired_changed);
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &machine_id).await?,
            BootInterfaceSelection {
                source: BootInterfaceSelectionSource::RedfishSerialNumber,
                updated_at: Some(selection_time),
            },
        );

        let refreshed_pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.6-4-2".to_string(),
        });
        let refreshed =
            force_reconcile(txn.as_mut(), &machine_id, &refreshed_pair, Existing).await?;

        assert_target(&refreshed.desired, &refreshed_pair);
        assert_eq!(
            refreshed.desired.version.version_nr(),
            reconciled.desired.version.version_nr() + 1,
        );
        assert!(refreshed.desired_changed);
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &machine_id).await?,
            BootInterfaceSelection {
                source: BootInterfaceSelectionSource::RedfishSerialNumber,
                updated_at: Some(selection_time),
            },
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn enrichment_only_strengthens_the_matching_mac(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(3);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 5]);
        let target = MachineBootInterfaceTarget::MacOnly(mac_address);
        let initialized =
            initialize_if_unset(txn.as_mut(), &machine_id, &target, RedfishUefiPci).await?;
        let selection_time =
            DateTime::from_timestamp(1_700_001_000, 123_000_000).expect("fixture timestamp");
        overwrite_selection_updated_at(txn.as_mut(), &machine_id, selection_time).await?;
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (None, None, false),
        );

        let mismatch = enrich_interface_id(
            txn.as_mut(),
            &machine_id,
            MacAddress::new([2, 0, 0, 0, 0, 6]),
            "wrong",
        )
        .await?
        .expect("target");
        assert_target(&mismatch, &target);
        assert_eq!(mismatch.version, initialized.version);

        let enriched = enrich_interface_id(
            txn.as_mut(),
            &machine_id,
            mac_address,
            " \t\n\u{000b}\u{000c}\rNIC.Slot.7-1-1 \t\n\u{000b}\u{000c}\r",
        )
        .await?
        .expect("target");
        let pair = MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id: "NIC.Slot.7-1-1".to_string(),
        });
        assert_target(&enriched, &pair);
        assert_eq!(
            enriched.version.version_nr(),
            initialized.version.version_nr() + 1
        );
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (None, None, false),
            "enrichment must not make an unverified target look converged",
        );

        let unchanged = enrich_interface_id(txn.as_mut(), &machine_id, mac_address, "replacement")
            .await?
            .expect("target");
        assert_target(&unchanged, &pair);
        assert_eq!(unchanged.version, enriched.version);
        assert_eq!(
            load_persisted_selection(txn.as_mut(), &machine_id).await?,
            BootInterfaceSelection {
                source: RedfishUefiPci,
                updated_at: Some(selection_time),
            },
            "learning the Redfish id must preserve the original selection",
        );

        assert!(matches!(
            enrich_interface_id(txn.as_mut(), &machine_id, mac_address, "\t\n").await,
            Err(DatabaseError::InvalidArgument(_))
        ));

        Ok(())
    }

    #[crate::sqlx_test]
    async fn enrichment_carries_current_verification_without_hiding_target_changes(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = host_machine_id(36);
        seed_machine(txn.as_mut(), &machine_id).await?;
        let mac_address = MacAddress::new([2, 0, 0, 0, 3, 8]);
        let initialized = initialize_if_unset(
            txn.as_mut(),
            &machine_id,
            &MachineBootInterfaceTarget::MacOnly(mac_address),
            RedfishUefiPci,
        )
        .await?;
        let observed_at =
            DateTime::from_timestamp(1_722_000_100, 123_000_000).expect("fixture timestamp");
        assert!(mark_verified(txn.as_mut(), &machine_id, initialized.version, observed_at,).await?);

        let enriched =
            enrich_interface_id(txn.as_mut(), &machine_id, mac_address, "NIC.Slot.8-1-1")
                .await?
                .expect("enriched target");
        assert_eq!(
            enriched.version.version_nr(),
            initialized.version.version_nr() + 1,
            "the enriched value remains a distinct immutable desired generation",
        );
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (Some(enriched.version), Some(observed_at), false),
            "adding an id for the same MAC must not schedule redundant convergence",
        );

        let replacement = MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 3, 9]));
        let replaced = set(txn.as_mut(), &machine_id, &replacement, Operator)
            .await?
            .desired;
        assert_ne!(replaced.version, enriched.version);
        assert_eq!(
            status_observation(txn.as_mut(), &machine_id).await?,
            (Some(enriched.version), Some(observed_at), false),
            "a different target must retain only the stale factual observation",
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn incomplete_targets_are_keyset_paged(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let unset_host = host_machine_id(10);
        let mac_only_host = host_machine_id(11);
        let unset_prediction = predicted_host_machine_id(12);
        let complete_host = host_machine_id(13);
        let dpu = dpu_machine_id(14);
        for machine_id in [
            &unset_host,
            &mac_only_host,
            &unset_prediction,
            &complete_host,
            &dpu,
        ] {
            seed_machine(txn.as_mut(), machine_id).await?;
        }

        set(
            txn.as_mut(),
            &mac_only_host,
            &MachineBootInterfaceTarget::MacOnly(MacAddress::new([2, 0, 0, 0, 1, 1])),
            Operator,
        )
        .await?;
        set(
            txn.as_mut(),
            &complete_host,
            &MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address: MacAddress::new([2, 0, 0, 0, 1, 2]),
                interface_id: "NIC.Slot.1-1-1".to_string(),
            }),
            Operator,
        )
        .await?;
        txn.commit().await?;

        let mut found = Vec::new();
        let mut after_id = None;
        loop {
            let page = find_incomplete_machine_ids(&pool, after_id.as_ref(), 2).await?;
            let Some(last) = page.last() else {
                break;
            };
            after_id = Some(*last);
            found.extend(page);
        }

        let mut expected = vec![unset_host, mac_only_host, unset_prediction];
        expected.sort();
        assert_eq!(found, expected);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn migration_creates_an_empty_constrained_host_table(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query("DROP TABLE machine_boot_interfaces")
            .execute(&pool)
            .await?;

        let mut txn = pool.begin().await?;
        let predicted_id = predicted_host_machine_id(20);
        let dpu_id = dpu_machine_id(21);
        seed_machine(txn.as_mut(), &predicted_id).await?;
        seed_machine(txn.as_mut(), &dpu_id).await?;
        txn.commit().await?;

        sqlx::raw_sql(MIGRATION).execute(&pool).await?;
        let row_count: i64 = sqlx::query_scalar("SELECT count(*) FROM machine_boot_interfaces")
            .fetch_one(&pool)
            .await?;
        assert_eq!(row_count, 0, "the schema migration must not backfill data");

        let desired_version = ConfigVersion::initial();
        sqlx::query(
            "INSERT INTO machine_boot_interfaces (
                 machine_id,
                 desired_mac_address,
                 desired_interface_id,
                 desired_version
             )
             VALUES ($1, $2, $3, $4)",
        )
        .bind(predicted_id)
        .bind(MacAddress::new([2, 0, 0, 0, 2, 1]))
        .bind("NIC.Slot.2-1-1")
        .bind(desired_version)
        .execute(&pool)
        .await?;

        for noncanonical_id in ["\t\n", " \tcanonical-id\n "] {
            let result = sqlx::query(
                "UPDATE machine_boot_interfaces
                 SET desired_interface_id = $1
                 WHERE machine_id = $2",
            )
            .bind(noncanonical_id)
            .bind(predicted_id)
            .execute(&pool)
            .await;
            assert!(
                result.is_err(),
                "the table constraint should reject {noncanonical_id:?}"
            );
        }

        let dpu_result = sqlx::query(
            "INSERT INTO machine_boot_interfaces (
                 machine_id,
                 desired_mac_address,
                 desired_version
             )
             VALUES ($1, $2, $3)",
        )
        .bind(dpu_id)
        .bind(MacAddress::new([2, 0, 0, 0, 2, 2]))
        .bind(desired_version)
        .execute(&pool)
        .await;
        assert!(dpu_result.is_err(), "the table must reject DPU targets");

        let stable_id = host_machine_id(22);
        sqlx::query("UPDATE machines SET id = $1 WHERE id = $2")
            .bind(stable_id)
            .bind(predicted_id)
            .execute(&pool)
            .await?;
        let stored_machine_id: HostMachineId =
            sqlx::query_scalar("SELECT machine_id FROM machine_boot_interfaces")
                .fetch_one(&pool)
                .await?;
        assert_eq!(stored_machine_id, stable_id);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn selection_migration_backfills_defaults_and_enforces_constraints(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query("DROP TABLE machine_boot_interfaces")
            .execute(&pool)
            .await?;
        sqlx::query("DROP TYPE boot_interface_selection_source")
            .execute(&pool)
            .await?;

        let preexisting_id = predicted_host_machine_id(60);
        let defaulted_insert_id = host_machine_id(61);
        let mut txn = pool.begin().await?;
        seed_machine(txn.as_mut(), &preexisting_id).await?;
        seed_machine(txn.as_mut(), &defaulted_insert_id).await?;
        txn.commit().await?;

        sqlx::raw_sql(MIGRATION).execute(&pool).await?;
        sqlx::raw_sql(STATUS_MIGRATION).execute(&pool).await?;
        sqlx::query(
            "INSERT INTO machine_boot_interfaces (
                 machine_id,
                 desired_mac_address,
                 desired_version
             )
             VALUES ($1, $2, $3)",
        )
        .bind(preexisting_id)
        .bind(MacAddress::new([2, 0, 0, 0, 6, 0]))
        .bind(ConfigVersion::initial())
        .execute(&pool)
        .await?;

        sqlx::raw_sql(SELECTION_MIGRATION).execute(&pool).await?;
        let legacy_selection: (BootInterfaceSelectionSource, Option<DateTime<Utc>>) =
            sqlx::query_as(
                "SELECT selection_source, selection_updated_at
                 FROM machine_boot_interfaces
                 WHERE machine_id = $1",
            )
            .bind(preexisting_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(
            legacy_selection,
            (BootInterfaceSelectionSource::LegacyUnknown, None),
            "rows created before source tracking must have an explicit unknown source and no invented time",
        );

        // Callers that omit `selection_source` and `selection_updated_at`
        // receive the schema default without inventing a decision time.
        sqlx::query(
            "INSERT INTO machine_boot_interfaces (
                 machine_id,
                 desired_mac_address,
                 desired_version
             )
             VALUES ($1, $2, $3)",
        )
        .bind(defaulted_insert_id)
        .bind(MacAddress::new([2, 0, 0, 0, 6, 2]))
        .bind(ConfigVersion::initial())
        .execute(&pool)
        .await?;
        let defaulted_insert_selection: (BootInterfaceSelectionSource, Option<DateTime<Utc>>) =
            sqlx::query_as(
                "SELECT selection_source, selection_updated_at
             FROM machine_boot_interfaces
             WHERE machine_id = $1",
            )
            .bind(defaulted_insert_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(
            defaulted_insert_selection,
            (BootInterfaceSelectionSource::LegacyUnknown, None),
            "omitted selection metadata must receive the explicit unknown default without an invented time",
        );

        let missing_time = sqlx::query(
            "UPDATE machine_boot_interfaces
             SET selection_source = 'operator',
                 selection_updated_at = NULL",
        )
        .execute(&pool)
        .await;
        assert!(
            missing_time.is_err(),
            "attributed selections must record when the decision was made",
        );

        let missing_source = sqlx::query(
            "UPDATE machine_boot_interfaces
             SET selection_source = NULL
             WHERE machine_id = $1",
        )
        .bind(preexisting_id)
        .execute(&pool)
        .await;
        assert!(
            missing_source.is_err(),
            "every desired boot interface must have an explicit selection source",
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn status_migration_scopes_rollout_baseline_and_constrains_status(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query("DROP TABLE machine_boot_interfaces")
            .execute(&pool)
            .await?;

        let mut txn = pool.begin().await?;
        let existing_host = host_machine_id(40);
        let in_flight_host = host_machine_id(42);
        seed_machine(txn.as_mut(), &existing_host).await?;
        seed_machine(txn.as_mut(), &in_flight_host).await?;
        set_controller_state(txn.as_mut(), &existing_host, ManagedHostState::Ready).await?;
        set_controller_state(
            txn.as_mut(),
            &in_flight_host,
            ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
            },
        )
        .await?;
        txn.commit().await?;

        sqlx::raw_sql(MIGRATION).execute(&pool).await?;
        let existing_version = ConfigVersion::initial();
        for (machine_id, mac_address) in [
            (existing_host, MacAddress::new([2, 0, 0, 0, 4, 0])),
            (in_flight_host, MacAddress::new([2, 0, 0, 0, 4, 2])),
        ] {
            sqlx::query(
                "INSERT INTO machine_boot_interfaces (
                     machine_id,
                     desired_mac_address,
                     desired_version
                 )
                 VALUES ($1, $2, $3)",
            )
            .bind(machine_id)
            .bind(mac_address)
            .bind(existing_version)
            .execute(&pool)
            .await?;
        }

        sqlx::raw_sql(STATUS_MIGRATION).execute(&pool).await?;
        let (verified_version, observed_at, assumed): (
            Option<ConfigVersion>,
            Option<DateTime<Utc>>,
            bool,
        ) = sqlx::query_as(
            "SELECT verified_version, observed_at, assumed
             FROM machine_boot_interfaces
             WHERE machine_id = $1",
        )
        .bind(existing_host)
        .fetch_one(&pool)
        .await?;
        assert_eq!(verified_version, Some(existing_version));
        assert!(observed_at.is_some());
        assert!(assumed);

        let in_flight_status: (Option<ConfigVersion>, Option<DateTime<Utc>>, bool) =
            sqlx::query_as(
                "SELECT verified_version, observed_at, assumed
                 FROM machine_boot_interfaces
                 WHERE machine_id = $1",
            )
            .bind(in_flight_host)
            .fetch_one(&pool)
            .await?;
        assert_eq!(in_flight_status, (None, None, false));

        for inconsistent_update in [
            "UPDATE machine_boot_interfaces
             SET observed_at = CURRENT_TIMESTAMP
             WHERE machine_id = $1",
            "UPDATE machine_boot_interfaces
             SET assumed = true
             WHERE machine_id = $1",
        ] {
            let result = sqlx::query(inconsistent_update)
                .bind(in_flight_host)
                .execute(&pool)
                .await;
            assert!(
                result.is_err(),
                "the status consistency constraint should reject {inconsistent_update:?}",
            );
        }

        Ok(())
    }
}
