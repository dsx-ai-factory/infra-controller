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

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};

/// Work carbide still owes an external system for one machine.
///
/// Mirrors the `machine_pending_action_kind` Postgres enum
/// (`20260810150847_machine_pending_actions.sql`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(type_name = "machine_pending_action_kind", rename_all = "snake_case")]
pub enum MachinePendingActionKind {
    /// DPF has staged a DPUService update and parked the DPU in the NodeEffect
    /// phase, waiting for carbide to release the maintenance hold.
    DpuServiceSync,
}

/// Who completed a pending action.
///
/// Mirrors the `machine_pending_action_actor` Postgres enum
/// (`20260814102046_machine_pending_action_completed_by.sql`) and the
/// `UpdateInitiator` RPC enum, which draws the same distinction for
/// reprovisioning.
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(type_name = "machine_pending_action_actor", rename_all = "snake_case")]
pub enum MachinePendingActionActor {
    /// Carbide, having confirmed on its own that the work was safe to do.
    Automatic,
    /// An operator, through the admin API.
    AdminCli,
}

/// One requested action for one machine, outstanding or already completed.
///
/// There is no payload beyond the timing: a `None` `completed_at` means the work
/// is still owed. `requested_at` is the first time the action was requested and
/// survives re-requests, so it measures how long the machine waited rather than
/// when the request was last observed.
#[derive(Clone, Debug, Eq, PartialEq, sqlx::FromRow)]
pub struct MachinePendingAction {
    pub machine_id: MachineId,
    pub kind: MachinePendingActionKind,
    pub requested_at: DateTime<Utc>,
    /// When the work succeeded, or `None` while it is still owed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Who completed it, set together with [`Self::completed_at`].
    pub completed_by: Option<MachinePendingActionActor>,
}

impl MachinePendingAction {
    /// Whether this action is still owed.
    pub fn is_outstanding(&self) -> bool {
        self.completed_at.is_none()
    }
}
