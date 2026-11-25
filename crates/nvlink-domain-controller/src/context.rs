/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

/// Context types shared by Domain lifecycle handlers.
pub struct NvLinkDomainStateHandlerContext;

/// Services shared by Domain ingestion, maintenance, and deletion.
#[derive(Clone)]
pub struct NvLinkDomainStateHandlerServices {
    /// Database pool for inventory reads and lifecycle persistence.
    pub db_pool: PgPool,
}

impl StateHandlerContextObjects for NvLinkDomainStateHandlerContext {
    type Services = NvLinkDomainStateHandlerServices;
    type ObjectMetrics = ();
}
