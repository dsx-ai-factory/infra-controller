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

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use carbide_uuid::site_prefix::SitePrefixId;
use clap::{CommandFactory, Parser};
use rpc::forge::{
    PrefixMatchType, SitePrefixCreationRequest, SitePrefixDeletionRequest, SitePrefixUpdateRequest,
};

use super::show::ShowMethod;
use super::*;
use crate::test_support::parse_leaf;

const SITE_PREFIX_ID: &str = "12345678-1234-5678-90ab-cdef01234567";
const TENANT_ORGANIZATION_ID: &str = "fds34511233a";

#[test]
fn command_tree_is_valid() {
    Cmd::command().debug_assert();
}

#[derive(Debug, Eq, PartialEq)]
struct ShowRequestView {
    site_prefix_id: Option<String>,
    tenant_organization_id: Option<String>,
    authority: Option<i32>,
    routing_scope: Option<i32>,
    lifecycle_state: Option<i32>,
    prefix_match: Option<String>,
    prefix_match_type: Option<i32>,
}

fn show_request(argv: &[&str]) -> Result<ShowRequestView, ()> {
    let Cmd::Show(args) = Cmd::try_parse_from(argv.iter().copied()).map_err(drop)? else {
        return Err(());
    };

    Ok(match ShowMethod::from(args) {
        ShowMethod::Get(site_prefix_id) => ShowRequestView {
            site_prefix_id: Some(site_prefix_id.to_string()),
            tenant_organization_id: None,
            authority: None,
            routing_scope: None,
            lifecycle_state: None,
            prefix_match: None,
            prefix_match_type: None,
        },
        ShowMethod::Search(filter) => ShowRequestView {
            site_prefix_id: None,
            tenant_organization_id: filter.tenant_organization_id,
            authority: filter.authority,
            routing_scope: filter.routing_scope,
            lifecycle_state: filter.lifecycle_state,
            prefix_match: filter.prefix_match,
            prefix_match_type: filter.prefix_match_type,
        },
    })
}

#[test]
fn show_builds_duplicate_safe_requests() {
    scenarios!(
        run = show_request;
        "inventory" {
            &["site-prefix", "show"][..] => Yields(ShowRequestView {
                site_prefix_id: None,
                tenant_organization_id: None,
                authority: None,
                routing_scope: None,
                lifecycle_state: None,
                prefix_match: None,
                prefix_match_type: None,
            }),
        }

        "one resource by globally unique ID" {
            &["site-prefix", "show", SITE_PREFIX_ID][..] => Yields(ShowRequestView {
                site_prefix_id: Some(SITE_PREFIX_ID.to_string()),
                tenant_organization_id: None,
                authority: None,
                routing_scope: None,
                lifecycle_state: None,
                prefix_match: None,
                prefix_match_type: None,
            }),
        }

        "an exact CIDR without a tenant remains a multi-match search" {
            &["site-prefix", "show", "--prefix", "10.0.0.0/16"][..] => Yields(ShowRequestView {
                site_prefix_id: None,
                tenant_organization_id: None,
                authority: None,
                routing_scope: None,
                lifecycle_state: None,
                prefix_match: Some("10.0.0.0/16".to_string()),
                prefix_match_type: Some(PrefixMatchType::PrefixExact as i32),
            }),
        }

        "an exact CIDR can be scoped to its owning tenant" {
            &[
                "site-prefix",
                "show",
                "--prefix",
                "10.0.0.0/16",
                "--tenant-organization-id",
                TENANT_ORGANIZATION_ID,
            ][..] => Yields(ShowRequestView {
                site_prefix_id: None,
                tenant_organization_id: Some(TENANT_ORGANIZATION_ID.to_string()),
                authority: None,
                routing_scope: None,
                lifecycle_state: None,
                prefix_match: Some("10.0.0.0/16".to_string()),
                prefix_match_type: Some(PrefixMatchType::PrefixExact as i32),
            }),
        }

        "inventory filters combine" {
            &[
                "site-prefix",
                "show",
                "--tenant-org-id",
                TENANT_ORGANIZATION_ID,
                "--authority",
                "tenant-managed",
                "--routing-scope",
                "datacenter-only",
                "--lifecycle-state",
                "ready",
                "--contained-by",
                "10.0.0.0/8",
            ][..] => Yields(ShowRequestView {
                site_prefix_id: None,
                tenant_organization_id: Some(TENANT_ORGANIZATION_ID.to_string()),
                authority: Some(rpc::forge::SitePrefixAuthority::TenantManaged as i32),
                routing_scope: Some(rpc::forge::SitePrefixRoutingScope::DatacenterOnly as i32),
                lifecycle_state: Some(rpc::forge::SitePrefixLifecycleState::Ready as i32),
                prefix_match: Some("10.0.0.0/8".to_string()),
                prefix_match_type: Some(PrefixMatchType::PrefixContainedBy as i32),
            }),
        }

        "contains uses the API's containing-prefix relationship" {
            &["site-prefix", "show", "--contains", "10.0.8.0/24"][..] => Yields(ShowRequestView {
                site_prefix_id: None,
                tenant_organization_id: None,
                authority: None,
                routing_scope: None,
                lifecycle_state: None,
                prefix_match: Some("10.0.8.0/24".to_string()),
                prefix_match_type: Some(PrefixMatchType::PrefixContains as i32),
            }),
        }
    );
}

#[derive(Debug, Eq, PartialEq)]
struct CreateRequestView {
    has_id: bool,
    tenant_organization_id: String,
    prefix: String,
    name: String,
    description: String,
    labels: Vec<(String, Option<String>)>,
}

fn create_request(argv: &[&str]) -> Result<(SitePrefixCreationRequest, CreateRequestView), ()> {
    let Cmd::Create(args) = Cmd::try_parse_from(argv.iter().copied()).map_err(drop)? else {
        return Err(());
    };
    let request = SitePrefixCreationRequest::from(args);
    let metadata = request.metadata.as_ref().ok_or(())?;
    let view = CreateRequestView {
        has_id: request.id.is_some(),
        tenant_organization_id: request.tenant_organization_id.clone(),
        prefix: request.prefix.clone(),
        name: metadata.name.clone(),
        description: metadata.description.clone(),
        labels: metadata
            .labels
            .iter()
            .map(|label| (label.key.clone(), label.value.clone()))
            .collect(),
    };
    Ok((request, view))
}

#[test]
fn create_builds_complete_tenant_requests() {
    scenarios!(
        run = |argv| create_request(argv).map(|(_, view)| view);
        "required fields generate an ID and empty optional metadata" {
            &[
                "site-prefix",
                "create",
                "--tenant-organization-id",
                TENANT_ORGANIZATION_ID,
                "--prefix",
                "10.0.0.0/16",
                "--name",
                "private-space",
            ][..] => Yields(CreateRequestView {
                has_id: true,
                tenant_organization_id: TENANT_ORGANIZATION_ID.to_string(),
                prefix: "10.0.0.0/16".to_string(),
                name: "private-space".to_string(),
                description: String::new(),
                labels: Vec::new(),
            }),
        }

        "description and repeated labels are preserved" {
            &[
                "site-prefix",
                "create",
                "--tenant-org-id",
                TENANT_ORGANIZATION_ID,
                "--prefix",
                "192.168.0.0/20",
                "--name",
                "lab-space",
                "--description",
                "Lab address space",
                "--label",
                "environment:lab",
                "--label",
                "owner",
            ][..] => Yields(CreateRequestView {
                has_id: true,
                tenant_organization_id: TENANT_ORGANIZATION_ID.to_string(),
                prefix: "192.168.0.0/20".to_string(),
                name: "lab-space".to_string(),
                description: "Lab address space".to_string(),
                labels: vec![
                    ("environment".to_string(), Some("lab".to_string())),
                    ("owner".to_string(), None),
                ],
            }),
        }
    );
}

#[test]
fn create_generates_distinct_non_nil_ids() {
    let argv = [
        "site-prefix",
        "create",
        "--tenant-organization-id",
        TENANT_ORGANIZATION_ID,
        "--prefix",
        "10.0.0.0/16",
        "--name",
        "private-space",
    ];
    let first = create_request(&argv)
        .expect("the first create arguments are valid")
        .0
        .id
        .expect("create generates an ID");
    let second = create_request(&argv)
        .expect("the second create arguments are valid")
        .0
        .id
        .expect("create generates an ID");

    assert_ne!(first, SitePrefixId::nil());
    assert_ne!(first, second);
}

#[test]
fn create_preserves_a_caller_supplied_id() {
    let (request, _) = create_request(&[
        "site-prefix",
        "create",
        "--tenant-organization-id",
        TENANT_ORGANIZATION_ID,
        "--site-prefix-id",
        SITE_PREFIX_ID,
        "--prefix",
        "172.16.0.0/16",
        "--name",
        "private-space",
    ])
    .expect("create arguments should produce a request");

    assert_eq!(request.id, Some(SITE_PREFIX_ID.parse().unwrap()));
}

#[derive(Debug, Eq, PartialEq)]
struct UpdateRequestView {
    site_prefix_id: String,
    tenant_organization_id: String,
    name: String,
    description: String,
    labels: Vec<(String, Option<String>)>,
    if_version_match: Option<String>,
}

fn update_request(argv: &[&str]) -> Result<UpdateRequestView, ()> {
    let Cmd::Update(args) = Cmd::try_parse_from(argv.iter().copied()).map_err(drop)? else {
        return Err(());
    };
    let request = SitePrefixUpdateRequest::from(args);
    let metadata = request.metadata.ok_or(())?;

    Ok(UpdateRequestView {
        site_prefix_id: request.id.ok_or(())?.to_string(),
        tenant_organization_id: request.tenant_organization_id,
        name: metadata.name,
        description: metadata.description,
        labels: metadata
            .labels
            .into_iter()
            .map(|label| (label.key, label.value))
            .collect(),
        if_version_match: request.if_version_match,
    })
}

#[test]
fn update_builds_complete_metadata_replacements() {
    scenarios!(
        run = update_request;
        "omitted optional metadata clears the old values" {
            &[
                "site-prefix",
                "update",
                SITE_PREFIX_ID,
                "--tenant-organization-id",
                TENANT_ORGANIZATION_ID,
                "--name",
                "new-name",
            ][..] => Yields(UpdateRequestView {
                site_prefix_id: SITE_PREFIX_ID.to_string(),
                tenant_organization_id: TENANT_ORGANIZATION_ID.to_string(),
                name: "new-name".to_string(),
                description: String::new(),
                labels: Vec::new(),
                if_version_match: None,
            }),
        }

        "description, labels, and expected version are preserved" {
            &[
                "site-prefix",
                "update",
                SITE_PREFIX_ID,
                "--tenant-org-id",
                TENANT_ORGANIZATION_ID,
                "--name",
                "new-name",
                "--description",
                "new description",
                "--label",
                "environment:production",
                "--label",
                "owner",
                "--if-version-match",
                "V4-T1750000000000000",
            ][..] => Yields(UpdateRequestView {
                site_prefix_id: SITE_PREFIX_ID.to_string(),
                tenant_organization_id: TENANT_ORGANIZATION_ID.to_string(),
                name: "new-name".to_string(),
                description: "new description".to_string(),
                labels: vec![
                    ("environment".to_string(), Some("production".to_string())),
                    ("owner".to_string(), None),
                ],
                if_version_match: Some("V4-T1750000000000000".to_string()),
            }),
        }
    );
}

#[test]
fn delete_and_retire_build_the_same_retirement_request() {
    scenarios!(
        run = |command| {
            let argv = [
                "site-prefix",
                command,
                SITE_PREFIX_ID,
                "--tenant-organization-id",
                TENANT_ORGANIZATION_ID,
            ];
            let Cmd::Delete(args) = Cmd::try_parse_from(argv).map_err(drop)? else {
                return Err(());
            };
            let request = SitePrefixDeletionRequest::from(args);
            Ok((
                request.id.map(|id| id.to_string()),
                request.tenant_organization_id,
            ))
        };
        "canonical delete command" {
            "delete" => Yields((
                Some(SITE_PREFIX_ID.to_string()),
                TENANT_ORGANIZATION_ID.to_string(),
            )),
        }

        "explicit retirement alias" {
            "retire" => Yields((
                Some(SITE_PREFIX_ID.to_string()),
                TENANT_ORGANIZATION_ID.to_string(),
            )),
        }
    );
}

#[test]
fn state_history_requires_an_id() {
    let matches = parse_leaf::<Cmd>(
        &["site-prefix", "state-history", SITE_PREFIX_ID],
        &["state-history"],
    )
    .expect("state-history arguments should parse");

    assert_eq!(
        matches
            .get_one::<SitePrefixId>("site_prefix_id")
            .map(ToString::to_string),
        Some(SITE_PREFIX_ID.to_string())
    );
}

#[test]
fn invalid_invocations_fail_during_parsing() {
    scenarios!(
        run = |argv| Cmd::try_parse_from(argv.iter().copied()).map(drop).map_err(drop);
        "show cannot combine an ID with inventory filters" {
            &[
                "site-prefix",
                "show",
                SITE_PREFIX_ID,
                "--tenant-organization-id",
                TENANT_ORGANIZATION_ID,
            ][..] => Fails,
        }

        "show cannot combine exact and contains relationships" {
            &[
                "site-prefix",
                "show",
                "--prefix",
                "10.0.0.0/16",
                "--contains",
                "10.0.1.0/24",
            ][..] => Fails,
        }

        "show cannot combine contains and contained-by relationships" {
            &[
                "site-prefix",
                "show",
                "--contains",
                "10.0.1.0/24",
                "--contained-by",
                "10.0.0.0/8",
            ][..] => Fails,
        }

        "create requires a tenant" {
            &["site-prefix", "create", "--prefix", "10.0.0.0/16", "--name", "private-space"][..] => Fails,
        }

        "create requires a prefix" {
            &[
                "site-prefix",
                "create",
                "--tenant-organization-id",
                TENANT_ORGANIZATION_ID,
                "--name",
                "private-space",
            ][..] => Fails,
        }

        "update requires replacement metadata name" {
            &[
                "site-prefix",
                "update",
                SITE_PREFIX_ID,
                "--tenant-organization-id",
                TENANT_ORGANIZATION_ID,
            ][..] => Fails,
        }

        "delete requires the owning tenant" {
            &["site-prefix", "delete", SITE_PREFIX_ID][..] => Fails,
        }

        "state-history requires an ID" {
            &["site-prefix", "state-history"][..] => Fails,
        }
    );
}
