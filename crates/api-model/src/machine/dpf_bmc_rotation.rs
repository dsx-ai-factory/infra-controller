// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::machine::DpuMachineId;
use chrono::{DateTime, Utc};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Progress of a managed host whose DPU BMC credentials are rotated by DPF.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DpfBmcRotation {
    pub bmc_macs: Vec<MacAddress>,
    pub phase: DpfBmcRotationPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum DpfBmcRotationPhase {
    SuppressExploration,
    AwaitExplorationSuppression,
    RotateHost,
    RotateDpus {
        completed: Vec<DpuMachineId>,
        current: Option<Box<DpfDpuBmcRotation>>,
    },
    ResumeExploration,
}

/// An immutable target and the operation that must be retried for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DpfDpuBmcRotation {
    pub operation_id: Uuid,
    pub dpu_machine_id: DpuMachineId,
    pub device_mac: MacAddress,
    pub device_uid: String,
    pub previous_secret: Option<String>,
    pub target_secret: String,
    pub previous_version: Option<i32>,
    pub target_version: i32,
    pub forced: bool,
    pub step: DpfDpuBmcRotationStep,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum DpfDpuBmcRotationStep {
    SaveRecoveryCredentials,
    EnsureTargetSecret,
    UpdateReference,
    AwaitAdoption,
    Recover {
        reason: String,
        next: DpfBmcRecoveryStep,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_at: Option<DateTime<Utc>>,
    },
    ReconcileCredential,
    InvalidateSessions,
    RecordVersion,
    DeletePreviousSecret,
    ForgetRecoveryCredentials,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DpfBmcRecoveryStep {
    ObserveReference,
    ObserveAdoption,
    RestoreTargetSecret,
}
