/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Shared controller infrastructure for NVLink Domain ingestion and maintenance.

mod context;
mod created;
mod deleting;
mod discovering;
mod error_state;
mod handler;
mod io;
mod maintenance;
mod ready;
mod validating;

pub use context::{NvLinkDomainStateHandlerContext, NvLinkDomainStateHandlerServices};
pub use handler::NvLinkDomainStateHandler;
pub use io::NvLinkDomainStateControllerIO;

#[cfg(test)]
mod tests;
