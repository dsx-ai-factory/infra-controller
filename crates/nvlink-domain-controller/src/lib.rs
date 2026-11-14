/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Shared controller infrastructure for NVLink Domain ingestion and maintenance.

mod context;
mod handler;
mod io;

pub use context::NvLinkDomainStateHandlerContext;
pub use handler::NvLinkDomainStateHandler;
pub use io::NvLinkDomainStateControllerIO;

#[cfg(test)]
mod tests;
