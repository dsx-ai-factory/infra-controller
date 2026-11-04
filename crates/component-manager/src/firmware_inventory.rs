// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Passive firmware inventory observations shared by component backends.
//! Reported versions do not establish whether an image is active or staged.
//! Optional fields describe what the backend model exposes. A present value,
//! including an empty string, does not prove that its wire field was supplied:
//! non-optional protobuf strings cannot distinguish omitted and empty values.

use mac_address::MacAddress;

/// `FirmwareInventorySource` identifies a backend with passive inventory reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareInventorySource {
    /// Inventory obtained directly from the BMC through Redfish.
    Redfish,
    /// Inventory obtained through Rack Management Service.
    Rms,
}

/// `FirmwareReadError` describes unavailable evidence without backend error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareReadError {
    /// Client creation, collection metadata, or one member read failed.
    ReadFailed,
    /// The shared request deadline expired before this read completed.
    DeadlineExceeded,
    /// The inventory or its text exceeds the reader's documented bound.
    LimitExceeded,
}

/// `ComponentFirmwareInventoryResult` keeps an observation with its requested endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFirmwareInventoryResult {
    /// Requested BMC MAC for compute/switches, or PMC MAC for a power shelf.
    pub mac_address: MacAddress,
    /// Collection failure or every listed member, in backend order, including repeats.
    /// An empty success means the backend returned an empty collection. Each member
    /// must also be checked: a successful list can contain failed member reads.
    pub inventory: Result<Vec<FirmwareInventoryEntry>, FirmwareReadError>,
}

/// `FirmwareInventoryEntry` preserves a listed member even when its read fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareInventoryEntry {
    /// Lookup identifier used to read this member separately. `None` means the
    /// backend returns the member directly without exposing a separate lookup ID.
    /// A component name is not a substitute for an absent lookup identifier.
    pub inventory_id: Option<String>,
    /// Reported component metadata or the reason this member could not be read.
    pub firmware: Result<FirmwareComponent, FirmwareReadError>,
}

/// `FirmwareComponent` contains backend metadata, including vendor normalization.
/// Missing versions stay absent; none of these fields proves active-image selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareComponent {
    /// Component identifier from the member response. `None` means the backend
    /// exposes no distinct ID; neither the name nor lookup ID supplies one.
    pub id: Option<String>,
    /// Inventory resource URI, such as Redfish `@odata.id`. `None` means the
    /// backend exposes no resource URI; an update target is kept separately.
    pub resource_uri: Option<String>,
    /// Backend component name, which need not be unique across inventory entries.
    /// `None` means no name is exposed; do not infer one from an ID or URI.
    pub name: Option<String>,
    /// Backend target used for firmware updates, distinct from the inventory
    /// resource URI. `None` means no target is exposed; none is inferred.
    pub update_target: Option<String>,
    /// Backend description, or `None` when the backend model supplies none.
    pub description: Option<String>,
    /// Backend-provided version, which may be vendor-normalized. `None` means
    /// the model supplies no version; a present empty string remains `Some("")`.
    pub version: Option<String>,
    /// Backend release date, retained without inference or parsing. `None` means
    /// the backend model supplies no release date.
    pub release_date: Option<String>,
}
