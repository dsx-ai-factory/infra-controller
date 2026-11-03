/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Administrator-owned declarations, independent of NMX-C observed domain identities.

use carbide_uuid::rack::RackGroupId;
use mac_address::MacAddress;
use sqlx::FromRow;
use uuid::Uuid;

/// An immutable declaration of compute and NVSwitch tray membership.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct ExpectedNvLinkDomain {
    /// Core-generated declaration identity, not an observed NMX-C domain UUID.
    pub id: Uuid,
    /// Physical inventory group containing the declared trays' racks.
    pub rack_group_id: RackGroupId,
    /// Optional operator-facing name; names need not be unique.
    pub name: Option<String>,
    /// Canonical BMC MACs, returned in ascending address order.
    pub member_bmc_mac_addresses: Vec<MacAddress>,
}
