// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! A firmware operation outlives its submitting RPC and Scout process.
//!
//! Receipts identify immutable requests. An unfinished receipt after helper loss
//! blocks mutation until reconciliation; a free process lock is not evidence
//! that a vendor tool stopped. Local state supports process recovery, not loss
//! of the state filesystem or reboot of a PXE host with a temporary root.

mod claim;
mod client;
mod execution;
mod identity;
mod journal;
mod launch;
mod peer;
mod service;

pub use carbide_libmlx_model::firmware::operation::{
    FirmwareOperation, FirmwareOperationPhase, FirmwareOperationState, ObserverExitBehavior,
};
pub use claim::DirectMlxClaim;
pub use client::{
    FirmwareOperationObservation, FirmwareSupervisorClient, FirmwareSupervisorClientError,
    SupervisorStatus,
};
pub use launch::{
    SupervisorConfig, SupervisorLaunchError, SupervisorLaunchMode, connect_or_launch,
    has_unresolved_firmware_claim,
};
pub use service::serve;
