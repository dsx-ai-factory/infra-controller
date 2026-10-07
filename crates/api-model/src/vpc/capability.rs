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

//! Profile-driven capability policy for `VpcVirtualizationType`.
//!
//! Each VPC virtualization type maps to a single `VpcCapabilities`
//! profile that declares its policy answers as data, e.g.
//! - Which host fabric interface it attaches to.
//! - Which segment types it accepts.
//! - Whether it honors routing profiles.
//! - Whether it supports IPv6.
//! - Which VPC types it peers with.
//! - Etc...
//!
//! The methods on the `VpcVirtualizationTypeCapabilities` extension
//! trait just read from that profile.
//!
//! The idea is that adding a new VPC virtualization type means:
//! (1) Add the enum variant in the `network` crate.
//! (2) Declare a `VpcCapabilities` constant here.
//! (3) Add the arm in `VpcVirtualizationTypeCapabilities::capabilities`.
//!
//! ...and then hopefully you have limited code changes you need to
//! make (e.g. less match arms, less conditional branching, etc etc).
//!
//! Technically most of this is serde-serializable, so we could maybe
//! even some day drive it from config files.
//!
//! This also introduces a DataPlaneKind enum, which gives us an
//! additional level of flexibility in how we define the capabilities
//! of a VPC virtualization type, with the idea being we should be
//! able to express how different data plane types wire into our
//! business logic, letting us derive a certain collection of
//! capabilities based on this kind. It's also intended to make it so
//! certain mutually exclusive configs can't cause a misconfiguration.

use std::fmt;

use carbide_network::virtualization::VpcVirtualizationType;
use ipnetwork::IpNetwork;

use crate::network_segment::{NetworkSegment, NetworkSegmentType, NewNetworkSegment};

/// Which host-side fabric interface kind a VPC virtualization type
/// attaches to. Used at instance-allocation time to decide which hosts
/// are eligible for which VPCs: the host's primary fabric interface
/// kind must match the VPC's declared `fabric_interface_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FabricInterfaceType {
    /// A DPU-managed fabric attachment. The host's primary data path
    /// is its DPU, and NICo drives the overlay (VRFs, EVPN, routing
    /// profiles) via the DPU agent.
    Dpu,

    /// A plain NIC fabric attachment. The host's data NIC sits
    /// directly on the operator's segment (`HostInband`); NICo
    /// does not mediate the data plane.
    Nic,
}

impl fmt::Display for FabricInterfaceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dpu => write!(f, "dpu"),
            Self::Nic => write!(f, "nic"),
        }
    }
}

/// Which kind of data plane a VPC virtualization type runs. This is the
/// knob that drives most of the correlated per-type capabilities (fabric
/// interface, routing profile support, SVI allocation, VNI exchange, etc.),
/// rather than declaring those as independent bools per variant (which can
/// be combined nonsensically); we encode them as a function of the kind of
/// data plane for the VPC virtualization type.
///
/// Adding a new VPC virtualization type usually means picking the
/// closest `DataPlaneKind` and letting all the derived capabilities
/// fall out. If a future type genuinely needs a new layout (e.g. a
/// hybrid that doesn't fit any current kind), add a new variant here
/// and a single match arm per derived accessor below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataPlaneKind {
    /// DPU-managed L2 overlay. The DPU stretches an L2 broadcast
    /// domain across hosts via overlay tunneling. Example: ETV.
    DpuOverlayL2,

    /// DPU-managed L3 overlay. The DPU runs per-VPC L3 routing,
    /// applying routing profiles (route-targets, leak rules) and
    /// importing peer VNIs via EVPN. Example: FNN.
    DpuOverlayL3,

    /// No NICo-managed data plane. The operator's network fabric
    /// owns reachability; NICo only persists VPC bookkeeping and
    /// exposes optional metadata (e.g. the VNI) for SDN integrations
    /// to consume. Example: Flat.
    OperatorManaged,
}

impl DataPlaneKind {
    /// Host fabric interface kind required by VPCs of this data plane.
    pub const fn fabric_interface_type(self) -> FabricInterfaceType {
        match self {
            Self::DpuOverlayL2 | Self::DpuOverlayL3 => FabricInterfaceType::Dpu,
            Self::OperatorManaged => FabricInterfaceType::Nic,
        }
    }

    /// Whether this data plane uses routing profiles -- both accepting
    /// `routing_profile_type` in the VPC create API and applying the
    /// looked-up profile's policy (route-target imports/exports, leak
    /// rules) to the DPU's VRF. L3-overlay only. L2 doesn't have a VRF
    /// to apply the profile to, and operator-managed has no NICo-side
    /// routing layer at all -- both reject the field at the API and
    /// always allocate VNIs from the internal pool.
    ///
    /// The REST API at `infra-controller-rest/api/pkg/api/handler/vpc.go`
    /// rejects `routingProfile` on non-FNN VPC create requests; this
    /// capability is the defense-in-depth gate at the carbide-core layer.
    pub const fn supports_routing_profiles(self) -> bool {
        matches!(self, Self::DpuOverlayL3)
    }

    /// Whether this data plane is *capable* of allocating an SVI IP
    /// for segments. This is a precondition, not a guarantee per
    /// segment: only stretched-L2 tenant segments get an SVI IP --
    /// non-stretched segments (e.g. tenant /31 link segments) don't
    /// get one even on data planes where this returns `true`. The
    /// per-segment combination lives in
    /// [`VpcVirtualizationTypeCapabilities::allocates_svi_for`].
    ///
    /// L3-overlay only today (see
    /// `carbide_network::virtualization::get_svi_ip`).
    pub const fn allocates_svi_ip(self) -> bool {
        matches!(self, Self::DpuOverlayL3)
    }

    /// Whether the DPU agent imports peer VPCs' VNIs into the local
    /// VRF for EVPN-style route exchange. L3-overlay only.
    pub const fn imports_peer_vnis_into_overlay(self) -> bool {
        matches!(self, Self::DpuOverlayL3)
    }

    /// Whether this data plane's VNI should be exposed to peers in
    /// their `vpc_peer_vnis` lists. L3-overlay (peers actively use
    /// the VNI for EVPN imports) and operator-managed (operator's
    /// SDN may use it for switch-side VTEPs/ACLs/etc.). L2-overlay
    /// has a VNI but doesn't surface it to peers.
    pub const fn vni_advertised_to_peers(self) -> bool {
        matches!(self, Self::DpuOverlayL3 | Self::OperatorManaged)
    }
}

/// Per-variant policy profile for a `VpcVirtualizationType`. The
/// `data_plane` field is the source of truth for the correlated
/// capabilities (fabric interface, routing profile support, SVI, VNI
/// exchange) -- derived accessors live on [`DataPlaneKind`]. The
/// remaining fields capture per-variant policy that varies
/// independently of kind (segment-type whitelist, address-family
/// support, peering relation).
pub struct VpcCapabilities {
    /// What kind of data plane this VPC type runs. Drives the
    /// correlated capabilities (see [`DataPlaneKind`]).
    pub data_plane: DataPlaneKind,

    /// Which `NetworkSegmentType`s may be bound to a VPC of this
    /// kind. The handler enforces this both ways: a VPC of this
    /// type rejects segments whose type is absent from this list,
    /// and a segment whose type appears here only in `Flat`'s profile
    /// (e.g. `HostInband`) is correspondingly rejected from other
    /// VPC types.
    pub allowed_segment_types: &'static [NetworkSegmentType],

    /// Whether this type supports IPv4 prefixes (either on VPC
    /// prefixes or on network segments contained in the VPC). All
    /// current VPC types support v4; declared here for symmetry with
    /// `supports_ipv6_prefix` so a future v6-only type can be
    /// expressed without an implicit "v4 is always fine" assumption.
    pub supports_ipv4_prefix: bool,

    /// Whether this type supports IPv6 prefixes (either on VPC
    /// prefixes or on network segments contained in the VPC). Varies
    /// independently of data plane kind today: ETV (L2 overlay)
    /// doesn't support v6 because that overlay generation was never
    /// wired for it; a future L2 overlay type with v6 support would
    /// set this true with the same `DpuOverlayL2` kind.
    pub supports_ipv6_prefix: bool,

    /// Whether this VPC type supports per-VPC SLAAC allocation mode.
    ///
    /// This is intentionally independent of IPv6 prefix support: Flat VPCs
    /// can carry IPv6 prefixes, but NICo does not manage the data-plane
    /// behavior needed for SLAAC on them.
    pub supports_slaac: bool,

    /// Which other VPC virtualization types this one can be peered
    /// with under the site's `Exclusive` peering policy. Must be
    /// maintained symmetrically -- if A lists B, B should list A.
    /// The `peering_relation_is_symmetric` test in this module
    /// enforces that at test time.
    pub peers_with: &'static [VpcVirtualizationType],
}

/// Every variant NICo handles. Iteration target for capability-driven
/// filters (e.g. "give me all variants that exchange VNI for peering").
/// If a new variant is added to `VpcVirtualizationType`, add it here
/// too.
///
/// ...at least for now. Maybe we can change how we structure this.
pub const ALL_VPC_VIRTUALIZATION_TYPES: &[VpcVirtualizationType] = &[
    VpcVirtualizationType::EthernetVirtualizer,
    VpcVirtualizationType::EthernetVirtualizerWithNvue,
    VpcVirtualizationType::Fnn,
    VpcVirtualizationType::Flat,
];

const ETV_CAPABILITIES: VpcCapabilities = VpcCapabilities {
    data_plane: DataPlaneKind::DpuOverlayL2,
    allowed_segment_types: &[
        NetworkSegmentType::Tenant,
        NetworkSegmentType::Admin,
        NetworkSegmentType::Underlay,
    ],
    supports_ipv4_prefix: true,
    supports_ipv6_prefix: false,
    supports_slaac: false,
    peers_with: &[
        VpcVirtualizationType::EthernetVirtualizer,
        VpcVirtualizationType::EthernetVirtualizerWithNvue,
        VpcVirtualizationType::Flat,
    ],
};

const FNN_CAPABILITIES: VpcCapabilities = VpcCapabilities {
    data_plane: DataPlaneKind::DpuOverlayL3,
    allowed_segment_types: &[
        NetworkSegmentType::Tenant,
        NetworkSegmentType::Admin,
        NetworkSegmentType::Underlay,
    ],
    supports_ipv4_prefix: true,
    supports_ipv6_prefix: true,
    supports_slaac: true,
    peers_with: &[VpcVirtualizationType::Fnn, VpcVirtualizationType::Flat],
};

const FLAT_CAPABILITIES: VpcCapabilities = VpcCapabilities {
    data_plane: DataPlaneKind::OperatorManaged,
    allowed_segment_types: &[NetworkSegmentType::HostInband],
    supports_ipv4_prefix: true,
    supports_ipv6_prefix: true,
    supports_slaac: false,
    peers_with: &[
        VpcVirtualizationType::EthernetVirtualizer,
        VpcVirtualizationType::EthernetVirtualizerWithNvue,
        VpcVirtualizationType::Fnn,
        VpcVirtualizationType::Flat,
    ],
};

/// Why a VPC capability check failed. Each variant carries the parties
/// involved so the message can be formatted without the caller knowing
/// the wording, and so the variant itself is matchable in tests.
#[derive(Debug, thiserror::Error)]
pub enum VpcCapabilityError {
    #[error("{vpc_type} VPCs do not support {segment_type} network segments")]
    UnsupportedSegmentType {
        vpc_type: VpcVirtualizationType,
        segment_type: NetworkSegmentType,
    },

    #[error("{vpc_type} VPCs do not support IPv4 network prefixes")]
    Ipv4Unsupported { vpc_type: VpcVirtualizationType },

    #[error("{vpc_type} VPCs do not support IPv6 network prefixes")]
    Ipv6Unsupported { vpc_type: VpcVirtualizationType },

    #[error(
        "{vpc_type} VPCs do not support routing profiles; the `routing_profile_type` and \
         `routing_profile_overrides` fields are FNN-only"
    )]
    RoutingProfilesUnsupported { vpc_type: VpcVirtualizationType },

    #[error("{vpc_type} VPCs do not support SLAAC allocation mode")]
    SlaacUnsupported { vpc_type: VpcVirtualizationType },

    #[error("{a} and {b} VPCs cannot be peered")]
    PeeringIncompatible {
        a: VpcVirtualizationType,
        b: VpcVirtualizationType,
    },
}

/// Extension trait that exposes [`VpcCapabilities`] policy on
/// [`VpcVirtualizationType`]. Only the [`Self::capabilities`] method
/// is variant-specific; every other method reads from the profile.
pub trait VpcVirtualizationTypeCapabilities {
    /// The policy profile for this variant.
    fn capabilities(self) -> &'static VpcCapabilities;

    /// Which host fabric interface kind a VPC of this type attaches
    /// to. Instance allocation rejects hosts whose primary fabric
    /// interface does not match.
    fn fabric_interface_type(self) -> FabricInterfaceType;

    /// Whether a segment of `segment_type` is allowed in a VPC of
    /// this type.
    fn supports_segment_type(self, segment_type: NetworkSegmentType) -> bool;

    /// Whether a given segment is allowed in a VPC of this type.
    /// Composite of [`Self::supports_segment_type`] and the
    /// per-address-family prefix checks ([`Self::supports_ipv4_prefix`]
    /// and [`Self::supports_ipv6_prefix`]) for any prefixes the segment
    /// carries.
    fn supports_segment(self, segment: impl NetworkSegmentProperties) -> bool;

    /// Whether this type can have IPv4 network prefixes.
    fn supports_ipv4_prefix(self) -> bool;

    /// Whether this type can have IPv6 network prefixes.
    fn supports_ipv6_prefix(self) -> bool;

    /// Whether this type uses routing profiles -- accepts
    /// `routing_profile_type` on create AND applies the looked-up
    /// profile's policy to the DPU's VRF. See
    /// [`DataPlaneKind::supports_routing_profiles`].
    fn supports_routing_profiles(self) -> bool;

    /// Whether this type supports per-VPC SLAAC allocation mode.
    fn supports_slaac(self) -> bool;

    /// Whether this type is *capable* of allocating an SVI IP for its
    /// segments (precondition, not guarantee per segment). See
    /// [`DataPlaneKind::allocates_svi_ip`].
    fn allocates_svi_ip(self) -> bool;

    /// Whether a SVI IP should be allocated for this specific segment.
    /// Combines the data plane's capability with the segment's
    /// `can_stretch` opt-in -- a SVI is only allocated on stretched-L2
    /// segments in a SVI-capable VPC type. Tenant /31 link segments
    /// (`can_stretch = false`) don't get one even on FNN.
    fn allocates_svi_for(self, segment: impl NetworkSegmentProperties) -> bool;

    /// Whether this type's DPU agent imports peer VPCs' VNIs into the
    /// local VRF for EVPN-style route exchange. See
    /// [`VpcCapabilities::imports_peer_vnis_into_overlay`].
    fn imports_peer_vnis_into_overlay(self) -> bool;

    /// Whether this type's VNI should be exposed to peers in their
    /// `vpc_peer_vnis` lists. See
    /// [`VpcCapabilities::vni_advertised_to_peers`].
    fn vni_advertised_to_peers(self) -> bool;

    /// Whether two VPC virtualization types can be peered under the
    /// `Exclusive` peering policy.
    fn can_peer_with(self, other: Self) -> bool;

    /// `ensure_*` variants of the above; return a structured error
    /// suitable for `?` propagation when a check fails.
    fn ensure_supports_segment_type(
        self,
        segment_type: NetworkSegmentType,
    ) -> Result<(), VpcCapabilityError>;
    /// Validates segment-type compatibility plus per-address-family
    /// support for any prefixes the segment carries.
    fn ensure_supports_segment(
        self,
        segment: impl NetworkSegmentProperties,
    ) -> Result<(), VpcCapabilityError>;
    fn ensure_supports_ipv4_prefix(self) -> Result<(), VpcCapabilityError>;
    fn ensure_supports_ipv6_prefix(self) -> Result<(), VpcCapabilityError>;
    fn ensure_supports_routing_profiles(self) -> Result<(), VpcCapabilityError>;
    fn ensure_supports_slaac(self) -> Result<(), VpcCapabilityError>;
    fn ensure_can_peer_with(self, other: VpcVirtualizationType) -> Result<(), VpcCapabilityError>;
}

/// Trait representing the information we need from a NetworkSegment to perform validation. Included
/// so that you can pass a `&NewNetworkSegment` or a `&NetworkSegment` to
/// VpcVirtualizationTypeCapabilities.
pub trait NetworkSegmentProperties {
    fn segment_type(&self) -> NetworkSegmentType;
    fn prefixes(&self) -> impl Iterator<Item = IpNetwork>;
    fn can_stretch(&self) -> Option<bool>;
}

impl NetworkSegmentProperties for &NewNetworkSegment {
    fn segment_type(&self) -> NetworkSegmentType {
        self.segment_type
    }

    fn prefixes(&self) -> impl Iterator<Item = IpNetwork> {
        self.prefixes.iter().map(|p| p.prefix)
    }

    fn can_stretch(&self) -> Option<bool> {
        self.can_stretch
    }
}

impl NetworkSegmentProperties for &NetworkSegment {
    fn segment_type(&self) -> NetworkSegmentType {
        self.config.segment_type
    }

    fn prefixes(&self) -> impl Iterator<Item = IpNetwork> {
        self.prefixes.iter().map(|p| p.prefix)
    }

    fn can_stretch(&self) -> Option<bool> {
        self.status.can_stretch
    }
}

impl VpcVirtualizationTypeCapabilities for VpcVirtualizationType {
    fn capabilities(self) -> &'static VpcCapabilities {
        match self {
            Self::EthernetVirtualizer | Self::EthernetVirtualizerWithNvue => &ETV_CAPABILITIES,
            Self::Fnn => &FNN_CAPABILITIES,
            Self::Flat => &FLAT_CAPABILITIES,
        }
    }

    fn fabric_interface_type(self) -> FabricInterfaceType {
        self.capabilities().data_plane.fabric_interface_type()
    }

    fn supports_segment_type(self, segment_type: NetworkSegmentType) -> bool {
        self.capabilities()
            .allowed_segment_types
            .contains(&segment_type)
    }

    fn supports_segment(self, segment: impl NetworkSegmentProperties) -> bool {
        if !self.supports_segment_type(segment.segment_type()) {
            return false;
        }
        let has_ipv4_prefix = segment.prefixes().any(|p| p.is_ipv4());
        let has_ipv6_prefix = segment.prefixes().any(|p| p.is_ipv6());
        (!has_ipv4_prefix || self.supports_ipv4_prefix())
            && (!has_ipv6_prefix || self.supports_ipv6_prefix())
    }

    fn supports_ipv4_prefix(self) -> bool {
        self.capabilities().supports_ipv4_prefix
    }

    fn supports_ipv6_prefix(self) -> bool {
        self.capabilities().supports_ipv6_prefix
    }

    fn supports_routing_profiles(self) -> bool {
        self.capabilities().data_plane.supports_routing_profiles()
    }

    fn supports_slaac(self) -> bool {
        self.capabilities().supports_slaac
    }

    fn allocates_svi_ip(self) -> bool {
        self.capabilities().data_plane.allocates_svi_ip()
    }

    fn allocates_svi_for(self, segment: impl NetworkSegmentProperties) -> bool {
        segment.can_stretch().unwrap_or(true) && self.allocates_svi_ip()
    }

    fn imports_peer_vnis_into_overlay(self) -> bool {
        self.capabilities()
            .data_plane
            .imports_peer_vnis_into_overlay()
    }

    fn vni_advertised_to_peers(self) -> bool {
        self.capabilities().data_plane.vni_advertised_to_peers()
    }

    fn can_peer_with(self, other: Self) -> bool {
        self.capabilities().peers_with.contains(&other)
    }

    fn ensure_supports_segment_type(
        self,
        segment_type: NetworkSegmentType,
    ) -> Result<(), VpcCapabilityError> {
        if self.supports_segment_type(segment_type) {
            Ok(())
        } else {
            Err(VpcCapabilityError::UnsupportedSegmentType {
                vpc_type: self,
                segment_type,
            })
        }
    }

    fn ensure_supports_segment(
        self,
        segment: impl NetworkSegmentProperties,
    ) -> Result<(), VpcCapabilityError> {
        self.ensure_supports_segment_type(segment.segment_type())?;
        if segment.prefixes().any(|p| p.is_ipv4()) {
            self.ensure_supports_ipv4_prefix()?;
        }
        if segment.prefixes().any(|p| p.is_ipv6()) {
            self.ensure_supports_ipv6_prefix()?;
        }
        Ok(())
    }

    fn ensure_supports_ipv4_prefix(self) -> Result<(), VpcCapabilityError> {
        if self.supports_ipv4_prefix() {
            Ok(())
        } else {
            Err(VpcCapabilityError::Ipv4Unsupported { vpc_type: self })
        }
    }

    fn ensure_supports_ipv6_prefix(self) -> Result<(), VpcCapabilityError> {
        if self.supports_ipv6_prefix() {
            Ok(())
        } else {
            Err(VpcCapabilityError::Ipv6Unsupported { vpc_type: self })
        }
    }

    fn ensure_supports_routing_profiles(self) -> Result<(), VpcCapabilityError> {
        if self.supports_routing_profiles() {
            Ok(())
        } else {
            Err(VpcCapabilityError::RoutingProfilesUnsupported { vpc_type: self })
        }
    }

    fn ensure_supports_slaac(self) -> Result<(), VpcCapabilityError> {
        if self.supports_slaac() {
            Ok(())
        } else {
            Err(VpcCapabilityError::SlaacUnsupported { vpc_type: self })
        }
    }

    fn ensure_can_peer_with(self, other: VpcVirtualizationType) -> Result<(), VpcCapabilityError> {
        if self.can_peer_with(other) {
            Ok(())
        } else {
            Err(VpcCapabilityError::PeeringIncompatible { a: self, b: other })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::discriminant;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn data_plane_maps_to_expected_variants() {
        assert_eq!(
            VpcVirtualizationType::EthernetVirtualizer
                .capabilities()
                .data_plane,
            DataPlaneKind::DpuOverlayL2,
        );
        assert_eq!(
            VpcVirtualizationType::EthernetVirtualizerWithNvue
                .capabilities()
                .data_plane,
            DataPlaneKind::DpuOverlayL2,
        );
        assert_eq!(
            VpcVirtualizationType::Fnn.capabilities().data_plane,
            DataPlaneKind::DpuOverlayL3,
        );
        assert_eq!(
            VpcVirtualizationType::Flat.capabilities().data_plane,
            DataPlaneKind::OperatorManaged,
        );
    }

    #[test]
    fn data_plane_derived_capabilities() {
        // Spot-check that the derived accessors agree with the per-kind
        // semantics documented on `DataPlaneKind`. If a variant ever
        // diverges from its kind's defaults, the trait impl would need
        // a per-variant override -- and these assertions would be the
        // first to fire.
        assert_eq!(
            DataPlaneKind::DpuOverlayL3.fabric_interface_type(),
            FabricInterfaceType::Dpu
        );
        assert_eq!(
            DataPlaneKind::OperatorManaged.fabric_interface_type(),
            FabricInterfaceType::Nic
        );
        assert!(DataPlaneKind::DpuOverlayL3.supports_routing_profiles());
        assert!(!DataPlaneKind::DpuOverlayL2.supports_routing_profiles());
        assert!(!DataPlaneKind::OperatorManaged.supports_routing_profiles());
        assert!(DataPlaneKind::DpuOverlayL3.imports_peer_vnis_into_overlay());
        assert!(!DataPlaneKind::OperatorManaged.imports_peer_vnis_into_overlay());
        assert!(DataPlaneKind::DpuOverlayL3.vni_advertised_to_peers());
        assert!(DataPlaneKind::OperatorManaged.vni_advertised_to_peers());
        assert!(!DataPlaneKind::DpuOverlayL2.vni_advertised_to_peers());
    }

    #[test]
    fn fabric_interface_type_matches_intuition() {
        assert_eq!(
            VpcVirtualizationType::EthernetVirtualizer.fabric_interface_type(),
            FabricInterfaceType::Dpu
        );
        assert_eq!(
            VpcVirtualizationType::Fnn.fabric_interface_type(),
            FabricInterfaceType::Dpu
        );
        assert_eq!(
            VpcVirtualizationType::Flat.fabric_interface_type(),
            FabricInterfaceType::Nic
        );
    }

    #[test]
    fn flat_only_supports_host_inband_segments() {
        let flat = VpcVirtualizationType::Flat;
        assert!(flat.supports_segment_type(NetworkSegmentType::HostInband));
        assert!(!flat.supports_segment_type(NetworkSegmentType::Tenant));
        assert!(!flat.supports_segment_type(NetworkSegmentType::Admin));
        assert!(!flat.supports_segment_type(NetworkSegmentType::Underlay));
    }

    #[test]
    fn host_inband_segments_only_supported_on_flat() {
        for vt in [
            VpcVirtualizationType::EthernetVirtualizer,
            VpcVirtualizationType::EthernetVirtualizerWithNvue,
            VpcVirtualizationType::Fnn,
        ] {
            assert!(!vt.supports_segment_type(NetworkSegmentType::HostInband));
            assert!(matches!(
                vt.ensure_supports_segment_type(NetworkSegmentType::HostInband),
                Err(VpcCapabilityError::UnsupportedSegmentType { .. })
            ));
        }
    }

    #[test]
    fn ipv6_supported_on_fnn_and_flat_only() {
        assert!(VpcVirtualizationType::Fnn.supports_ipv6_prefix());
        assert!(VpcVirtualizationType::Flat.supports_ipv6_prefix());
        assert!(!VpcVirtualizationType::EthernetVirtualizer.supports_ipv6_prefix());
        assert!(matches!(
            VpcVirtualizationType::EthernetVirtualizer.ensure_supports_ipv6_prefix(),
            Err(VpcCapabilityError::Ipv6Unsupported { .. })
        ));
    }

    #[test]
    fn routing_profiles_are_fnn_only() {
        // FNN is the only data plane that uses routing profiles. ETV
        // and Flat both reject the field at the API and short-circuit
        // resolution to defaults (no profile stored, internal VNI
        // pool). The REST API enforces this same gate upstream; this
        // is the defense-in-depth at carbide-core.
        assert!(VpcVirtualizationType::Fnn.supports_routing_profiles());
        assert!(!VpcVirtualizationType::EthernetVirtualizer.supports_routing_profiles());
        assert!(!VpcVirtualizationType::EthernetVirtualizerWithNvue.supports_routing_profiles());
        assert!(!VpcVirtualizationType::Flat.supports_routing_profiles());
        assert!(matches!(
            VpcVirtualizationType::Flat.ensure_supports_routing_profiles(),
            Err(VpcCapabilityError::RoutingProfilesUnsupported { .. })
        ));
        assert!(matches!(
            VpcVirtualizationType::EthernetVirtualizer.ensure_supports_routing_profiles(),
            Err(VpcCapabilityError::RoutingProfilesUnsupported { .. })
        ));
    }

    #[test]
    fn slaac_is_fnn_only() {
        assert!(VpcVirtualizationType::Fnn.supports_slaac());
        assert!(!VpcVirtualizationType::EthernetVirtualizer.supports_slaac());
        assert!(!VpcVirtualizationType::EthernetVirtualizerWithNvue.supports_slaac());
        assert!(!VpcVirtualizationType::Flat.supports_slaac());
        assert!(matches!(
            VpcVirtualizationType::Flat.ensure_supports_slaac(),
            Err(VpcCapabilityError::SlaacUnsupported { .. })
        ));
    }

    #[test]
    fn svi_ip_allocation_is_fnn_only() {
        assert!(VpcVirtualizationType::Fnn.allocates_svi_ip());
        assert!(!VpcVirtualizationType::EthernetVirtualizer.allocates_svi_ip());
        assert!(!VpcVirtualizationType::Flat.allocates_svi_ip());
    }

    #[test]
    fn only_fnn_imports_peer_vnis_into_overlay() {
        assert!(VpcVirtualizationType::Fnn.imports_peer_vnis_into_overlay());
        assert!(!VpcVirtualizationType::EthernetVirtualizer.imports_peer_vnis_into_overlay());
        assert!(!VpcVirtualizationType::Flat.imports_peer_vnis_into_overlay());
    }

    #[test]
    fn fnn_and_flat_advertise_vni_to_peers_etv_does_not() {
        // FNN advertises its VNI because peers (other FNN VPCs) actively
        // use it for EVPN route imports. Flat advertises because
        // operator-side pluggable SDN integrations may consume the VNI
        // (switch-side VTEPs, ACLs, etc.). ETV does not advertise its
        // VNI -- legacy and not surfaced to peers.
        assert!(VpcVirtualizationType::Fnn.vni_advertised_to_peers());
        assert!(VpcVirtualizationType::Flat.vni_advertised_to_peers());
        assert!(!VpcVirtualizationType::EthernetVirtualizer.vni_advertised_to_peers());
        assert!(!VpcVirtualizationType::EthernetVirtualizerWithNvue.vni_advertised_to_peers());
    }

    #[test]
    fn flat_peers_with_everything() {
        for vt in ALL_VPC_VIRTUALIZATION_TYPES {
            assert!(VpcVirtualizationType::Flat.can_peer_with(*vt));
        }
    }

    #[test]
    fn etv_cannot_peer_with_fnn() {
        for etv in [
            VpcVirtualizationType::EthernetVirtualizer,
            VpcVirtualizationType::EthernetVirtualizerWithNvue,
        ] {
            for (a, b) in [
                (etv, VpcVirtualizationType::Fnn),
                (VpcVirtualizationType::Fnn, etv),
            ] {
                assert!(!a.can_peer_with(b));
                assert!(matches!(
                    a.ensure_can_peer_with(b),
                    Err(VpcCapabilityError::PeeringIncompatible { .. })
                ));
            }
        }
    }

    #[test]
    fn etv_nvue_treated_as_etv_for_peering() {
        assert!(
            VpcVirtualizationType::EthernetVirtualizer
                .can_peer_with(VpcVirtualizationType::EthernetVirtualizerWithNvue)
        );
    }

    /// Guards against forgetting to declare a reciprocal entry in
    /// another variant's `peers_with` slice. If A says it peers with
    /// B, B must say it peers with A.
    #[test]
    fn peering_relation_is_symmetric() {
        for a in ALL_VPC_VIRTUALIZATION_TYPES {
            for b in ALL_VPC_VIRTUALIZATION_TYPES {
                assert_eq!(
                    a.can_peer_with(*b),
                    b.can_peer_with(*a),
                    "peering relation is asymmetric between {a} and {b}; \
                     check the `peers_with` slices in their profiles",
                );
            }
        }
    }

    fn segment_with(
        segment_type: NetworkSegmentType,
        prefixes: Vec<&str>,
        can_stretch: Option<bool>,
    ) -> NewNetworkSegment {
        use crate::network_prefix::NewNetworkPrefix;
        use crate::network_segment::AllocationStrategy;

        NewNetworkSegment {
            id: uuid::Uuid::new_v4().into(),
            name: "test-segment".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: prefixes
                .into_iter()
                .map(|p| NewNetworkPrefix {
                    prefix: p.parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 0,
                })
                .collect(),
            vlan_id: None,
            vni: None,
            segment_type,
            can_stretch,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        }
    }

    // `ensure_supports_segment` gating: segment-type whitelist plus
    // per-address-family prefix support. `VpcCapabilityError` is not
    // `PartialEq`, so rejection rows assert the error *variant* via its
    // discriminant; the success row yields `()`.
    #[test]
    fn ensure_supports_segment_gates_by_type_and_address_family() {
        // A representative error value per variant we expect, used only to
        // pull its discriminant for comparison.
        let unsupported_segment_type = discriminant(&VpcCapabilityError::UnsupportedSegmentType {
            vpc_type: VpcVirtualizationType::Flat,
            segment_type: NetworkSegmentType::Tenant,
        });
        let ipv6_unsupported = discriminant(&VpcCapabilityError::Ipv6Unsupported {
            vpc_type: VpcVirtualizationType::EthernetVirtualizer,
        });

        scenarios!(
            // The operation under test: gate a segment against a VPC type. The
            // error type isn't `PartialEq`, so we project it to its discriminant
            // so the asserted variant is comparable.
            run = |(vt, segment)| {
                vt.ensure_supports_segment(&segment)
                    .map_err(|e| discriminant(&e))
            };
            "FNN + Tenant + IPv4 is the standard happy path" {
                (
                    VpcVirtualizationType::Fnn,
                    segment_with(NetworkSegmentType::Tenant, vec!["192.0.2.0/24"], None),
                ) => Yields(()),
            }

            "Flat doesn't accept Tenant segments" {
                (
                    VpcVirtualizationType::Flat,
                    segment_with(NetworkSegmentType::Tenant, vec!["192.0.2.0/24"], None),
                ) => FailsWith(unsupported_segment_type),
            }

            "ETV doesn't accept IPv6 prefixes even if segment-type matches" {
                (
                    VpcVirtualizationType::EthernetVirtualizer,
                    segment_with(
                        NetworkSegmentType::Tenant,
                        vec!["192.0.2.0/24", "2001:db8::/64"],
                        None,
                    ),
                ) => FailsWith(ipv6_unsupported),
            }

            "Flat + HostInband + IPv6 is a supported combination" {
                (
                    VpcVirtualizationType::Flat,
                    segment_with(
                        NetworkSegmentType::HostInband,
                        vec!["192.0.2.0/24", "2001:db8::/64"],
                        None,
                    ),
                ) => Yields(()),
            }

            "ETV rejects an IPv6-only Tenant segment" {
                (
                    VpcVirtualizationType::EthernetVirtualizer,
                    segment_with(NetworkSegmentType::Tenant, vec!["2001:db8::/64"], None),
                ) => FailsWith(ipv6_unsupported),
            }

            "Flat + HostInband accepts an IPv6-only segment" {
                (
                    VpcVirtualizationType::Flat,
                    segment_with(NetworkSegmentType::HostInband, vec!["2001:db8::/64"], None),
                ) => Yields(()),
            }
        );
    }

    #[test]
    fn allocates_svi_for_only_when_fnn_and_can_stretch() {
        let stretchable = segment_with(NetworkSegmentType::Tenant, vec!["192.0.2.0/24"], None);
        let unstretchable = segment_with(
            NetworkSegmentType::Tenant,
            vec!["192.0.2.0/24"],
            Some(false),
        );

        assert!(VpcVirtualizationType::Fnn.allocates_svi_for(&stretchable));
        assert!(!VpcVirtualizationType::Fnn.allocates_svi_for(&unstretchable));
        assert!(!VpcVirtualizationType::EthernetVirtualizer.allocates_svi_for(&stretchable));
        assert!(!VpcVirtualizationType::Flat.allocates_svi_for(&stretchable));
    }
}
