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

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bmc_drivers::{CapabilitySelection, DriverMap, ResolvedSelection, Rules};
use bmc_platform::{PlatformIdentity, Quirk};
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use mac_address::MacAddress;

use crate::BmcRef;

/// A selection of `drivers` with `quirks` under the empty rule set.
pub(crate) fn selection(drivers: DriverMap, quirks: BTreeSet<Quirk>) -> ResolvedSelection {
    ResolvedSelection {
        drivers,
        matched_rules: Vec::new(),
        quirks,
        hash: Rules::new(Vec::new())
            .expect("empty rules are valid")
            .hash(),
    }
}

/// The per-device root credential key of the BMC with `mac_address`.
pub(crate) const fn root_credential_key(mac_address: MacAddress) -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: mac_address,
        },
    }
}

/// A BMC at `192.0.2.10:443` selecting standard drivers with `quirks`.
pub(crate) fn endpoint(quirks: BTreeSet<Quirk>) -> BmcRef {
    BmcRef::new(
        "192.0.2.10:443".parse().expect("valid socket address"),
        root_credential_key(MacAddress::new([2, 0, 0, 0, 0, 1])),
        PlatformIdentity::default(),
        selection(DriverMap::filled(CapabilitySelection::Standard), quirks),
    )
    .expect("root credential key is valid")
}

/// A credential store that answers every key with the same result and counts
/// how often it was read; `Err(())` answers as an unavailable store.
pub(crate) struct RecordingStore {
    stored: Result<Option<Credentials>, ()>,
    reads: AtomicUsize,
}

impl RecordingStore {
    pub(crate) fn new(stored: Result<Option<Credentials>, ()>) -> Self {
        Self {
            stored,
            reads: AtomicUsize::new(0),
        }
    }

    pub(crate) fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CredentialReader for RecordingStore {
    async fn get_credentials(
        &self,
        _key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.stored
            .clone()
            .map_err(|()| SecretsError::UfmCredentialReadBlocked {
                fabric: "store unavailable".to_string(),
            })
    }
}
