// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use prost::Message;
use sha2::{Digest, Sha256};

use crate::forge::InterfaceLldp;

const FINGERPRINT_DOMAIN: &[u8] = b"nico-lldp-snapshot-v1\0";

/// Returns a stable fingerprint for a complete LLDP snapshot.
///
/// Interface ordering is not meaningful, so each protobuf message is encoded
/// independently and sorted before hashing. Length prefixes keep adjacent
/// encodings unambiguous.
pub fn fingerprint(interfaces: &[InterfaceLldp]) -> Vec<u8> {
    let mut encoded_interfaces = interfaces
        .iter()
        .map(|interface| interface.encode_to_vec())
        .collect::<Vec<_>>();
    encoded_interfaces.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    for encoded in encoded_interfaces {
        hasher.update((encoded.len() as u64).to_be_bytes());
        hasher.update(encoded);
    }
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves collection ordering cannot change the identifier for one topology.
    #[test]
    fn fingerprint_ignores_interface_order() {
        let first = InterfaceLldp {
            mac_address: "02:00:00:00:00:01".to_string(),
            ..Default::default()
        };
        let second = InterfaceLldp {
            mac_address: "02:00:00:00:00:02".to_string(),
            ..Default::default()
        };

        // Fingerprint the same two interfaces in both collection orders.
        let canonical = fingerprint(&[first.clone(), second.clone()]);
        let reversed = fingerprint(&[second, first]);

        // Ordering alone must not produce a pending snapshot.
        assert_eq!(canonical, reversed);
    }

    /// Proves an empty snapshot has a stable non-absent identifier.
    #[test]
    fn empty_snapshot_has_a_fingerprint() {
        // Fingerprint the successful observation of no neighbors.
        let empty = fingerprint(&[]);

        // SHA-256 always identifies the snapshot, including an empty one.
        assert_eq!(empty.len(), 32);
    }
}
