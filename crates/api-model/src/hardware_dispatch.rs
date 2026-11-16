// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! A hardware command's durable ownership, independent of its transport connection.

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `HardwareDispatch` records the last command reserved on a host.
/// An unsettled command excludes NIC work without consuming an update-manager
/// slot. Settlement retains the record until another command replaces it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareDispatch {
    /// Scout stream flow identity, assigned before the command is sent.
    pub token: uuid::Uuid,
    /// Sender and expected completion, never inferred from inventory.
    pub owner: HardwareDispatchOwner,
    /// Database time when ownership was recorded, before transport delivery.
    pub started_at: DateTime<Utc>,
    /// Database time of recognized completion or proof the command was not sent.
    /// Absence retains ownership across timeouts and Core restarts.
    pub settled_at: Option<DateTime<Utc>>,
}

impl HardwareDispatch {
    /// Whether the command's result or partial effects remain unresolved.
    pub fn is_pending(&self) -> bool {
        self.settled_at.is_none()
    }
}

/// The sender allowed to settle a command using its exact token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareDispatchOwner {
    /// An MLX admin request sent through one Scout's command stream.
    MlxAdmin {
        /// Host or attached DPU whose Scout receives the request.
        machine_id: MachineId,
        /// Reply contract frozen before delivery.
        command: MlxAdminCommand,
    },
}

/// Mutating MLX requests and their distinct successful reply contracts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MlxAdminCommand {
    /// Apply a named configuration profile.
    ProfileSync,
    /// Apply individual configuration settings.
    ConfigSet,
    /// Synchronize the supplied configuration assignments.
    ConfigSync,
    /// Enable lockdown; completion must report Locked.
    Lock,
    /// Disable lockdown; completion must report Unlocked.
    Unlock,
}

#[cfg(test)]
mod tests {
    /// Older serialized snapshots omit the dispatch field.
    #[test]
    fn hardware_dispatch_is_absent_on_older_snapshots() {
        use crate::machine::json::MachineSnapshotPgJson;
        use crate::test_support::machine_snapshot::{host_machine_id, machine_snapshot_pg_json};

        let snapshot = machine_snapshot_pg_json(host_machine_id());
        let mut json = serde_json::to_value(snapshot).unwrap();
        json.as_object_mut().unwrap().remove("hardware_dispatch");
        let older: MachineSnapshotPgJson = serde_json::from_value(json).unwrap();
        assert!(older.hardware_dispatch.is_none());
    }
}
