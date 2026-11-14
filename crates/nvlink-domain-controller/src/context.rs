/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

/// Shared database service for Domain lifecycle iterations.
pub struct NvLinkDomainStateHandlerContext;

impl StateHandlerContextObjects for NvLinkDomainStateHandlerContext {
    type Services = PgPool;
    type ObjectMetrics = ();
}
