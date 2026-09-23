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

use model::site_explorer::ComponentIntegrityEntry;
use nv_redfish::core::query::ExpandQuery;
use nv_redfish::core::{Bmc, EntityTypeRef, Expandable, ODataETag, ODataId};
use serde::Deserialize;

/// nv-redfish models no `ComponentIntegrity`, so the collection is
/// deserialized here. Expanded one level, which is the query the libredfish
/// client already issues for this collection when attestation is scheduled.
#[derive(Deserialize)]
struct Collection {
    #[serde(rename = "@odata.id")]
    odata_id: ODataId,
    /// Required rather than defaulted: a collection resource always carries
    /// `Members`, empty included, so a response without it is one this could
    /// not read. Defaulting would report it as a BMC with nothing to attest,
    /// which is a set in its own right and would record a variant the hardware
    /// never had.
    #[serde(rename = "Members")]
    members: Vec<Member>,
}

impl EntityTypeRef for Collection {
    fn odata_id(&self) -> &ODataId {
        &self.odata_id
    }

    fn etag(&self) -> Option<&ODataETag> {
        None
    }
}

impl Expandable for Collection {}

/// The properties the report keeps. Both are required of a
/// `ComponentIntegrity`, so a member missing either is not one this can
/// describe, and the whole collection is discarded rather than reported short.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Member {
    id: String,
    component_integrity_type: String,
    component_integrity_enabled: bool,
}

/// What an exploration learned about the BMC's `ComponentIntegrity`
/// collection.
///
/// A BMC that advertises no collection and one whose collection could not be
/// read both leave `entries` absent, but only the second is a missing answer:
/// the first is the BMC saying it has nothing to attest. Coverage reads the
/// two differently, so they are kept apart here rather than merged into one
/// absence.
#[derive(Default)]
pub(crate) struct Observation {
    /// The members the collection listed, unfiltered.
    pub(crate) entries: Option<Vec<ComponentIntegrityEntry>>,
    /// Set when the collection was advertised but fetching it failed.
    pub(crate) unavailable: bool,
}

/// What the BMC says it can attest, unfiltered.
///
/// A failed fetch is reported rather than raised: the list drives attestation
/// coverage, while scheduling reads the collection live from the BMC, so
/// losing it must not fail an exploration that otherwise succeeded.
pub(crate) async fn explore<B: Bmc>(bmc: &B, root: &nv_redfish::ServiceRoot<B>) -> Observation {
    let Some(link) = root.root.component_integrity.as_ref() else {
        return Observation::default();
    };
    match bmc
        .expand::<Collection>(&link.odata_id, ExpandQuery::default())
        .await
    {
        Ok(collection) => Observation {
            entries: Some(
                collection
                    .members
                    .iter()
                    .map(|member| ComponentIntegrityEntry {
                        id: member.id.clone(),
                        component_integrity_type: member.component_integrity_type.clone(),
                        component_integrity_enabled: member.component_integrity_enabled,
                    })
                    .collect(),
            ),
            unavailable: false,
        },
        Err(error) => {
            tracing::warn!(%error, "Failed to fetch the ComponentIntegrity collection.");
            Observation {
                entries: None,
                unavailable: true,
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// A collection with no members and a response that never carried them
    /// are different answers. Reading the second as the first would record a
    /// BMC reporting nothing to attest, which is a set the hardware never had
    /// and one that outlives the endpoint that produced it.
    #[test]
    fn a_response_without_members_is_not_an_empty_collection() {
        let empty = r#"{"@odata.id": "/redfish/v1/ComponentIntegrity", "Members": []}"#;
        assert!(
            serde_json::from_str::<Collection>(empty)
                .expect("a collection listing no members reads")
                .members
                .is_empty()
        );

        assert!(
            serde_json::from_str::<Collection>(
                r#"{"@odata.id": "/redfish/v1/ComponentIntegrity"}"#
            )
            .is_err(),
            "a response omitting Members is unreadable, so exploration reports it \
             unavailable rather than as a collection holding nothing"
        );
    }
}
