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
use std::collections::HashMap;
use std::fmt;

use carbide_uuid::instance::InstanceId;
use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use carbide_uuid::vpc::VpcId;
use chrono::prelude::*;
use config_version::ConfigVersion;
use ipnetwork;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;

use super::tenant::TenantOrganizationId;
use crate::metadata::Metadata;

/* ********************************** */
/*     NetworkSecurityGroupSource     */
/* ********************************** */

/// NetworkSecurityGroupSource describes where a
/// machine's security rules were originally defined,
/// either on an NSG attached to the instance or a VPC.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum NetworkSecurityGroupSource {
    #[serde(rename = "NONE")]
    None,
    #[serde(rename = "VPC")]
    Vpc,
    #[serde(rename = "INSTANCE")]
    Instance,
}

impl fmt::Display for NetworkSecurityGroupSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetworkSecurityGroupSource::None => write!(f, "NONE"),
            NetworkSecurityGroupSource::Vpc => write!(f, "VPC"),
            NetworkSecurityGroupSource::Instance => write!(f, "INSTANCE"),
        }
    }
}

/* ********************************************* */
/*     NetworkSecurityGroupPropagationStatus     */
/* ********************************************* */

/// NetworkSecurityGroupPropagationStatus describes the degree
/// to which propagation of NSG changes has succeeded accross
/// a set of instances (really instance DPUs).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum NetworkSecurityGroupPropagationStatus {
    Unknown,
    Full,
    Partial,
    None,
    Error,
}

impl fmt::Display for NetworkSecurityGroupPropagationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetworkSecurityGroupPropagationStatus::Unknown => write!(f, "UNKNOWN"),
            NetworkSecurityGroupPropagationStatus::Full => write!(f, "FULL"),
            NetworkSecurityGroupPropagationStatus::Partial => write!(f, "PARTIAL"),
            NetworkSecurityGroupPropagationStatus::None => write!(f, "NONE"),
            NetworkSecurityGroupPropagationStatus::Error => write!(f, "ERROR"),
        }
    }
}

/* ********************************************* */
/*       NetworkSecurityGroupRuleDirection       */
/* ********************************************* */

/// NetworkSecurityGroupRuleDirection describes whether a rule
/// is being applied to ingress or egress traffic.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum NetworkSecurityGroupRuleDirection {
    Ingress,
    Egress,
}

impl fmt::Display for NetworkSecurityGroupRuleDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetworkSecurityGroupRuleDirection::Ingress => write!(f, "INGRESS"),
            NetworkSecurityGroupRuleDirection::Egress => write!(f, "EGRESS"),
        }
    }
}

/* ********************************************* */
/*        NetworkSecurityGroupRuleProtocol       */
/* ********************************************* */

/// NetworkSecurityGroupRuleProtocol describes the
/// protocol on which the rule should match/act.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum NetworkSecurityGroupRuleProtocol {
    Any,
    Icmp,
    Icmp6,
    Udp,
    Tcp,
}

impl fmt::Display for NetworkSecurityGroupRuleProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetworkSecurityGroupRuleProtocol::Any => write!(f, "ANY"),
            NetworkSecurityGroupRuleProtocol::Icmp => write!(f, "ICMP"),
            NetworkSecurityGroupRuleProtocol::Icmp6 => write!(f, "ICMP6"),
            NetworkSecurityGroupRuleProtocol::Udp => write!(f, "UDP"),
            NetworkSecurityGroupRuleProtocol::Tcp => write!(f, "TCP"),
        }
    }
}

/* ********************************************* */
/*          NetworkSecurityGroupRuleAction       */
/* ********************************************* */

/// NetworkSecurityGroupRuleAction describes the
/// action that should be taken when a rule matches.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum NetworkSecurityGroupRuleAction {
    Deny,
    Permit,
}

impl fmt::Display for NetworkSecurityGroupRuleAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetworkSecurityGroupRuleAction::Deny => write!(f, "DENY"),
            NetworkSecurityGroupRuleAction::Permit => write!(f, "PERMIT"),
        }
    }
}

/* ************************************** */
/*       NetworkSecurityGroupRuleNet      */
/* ************************************** */
/// NetworkSecurityGroupRuleNet describes a source or
/// destination network to look for when matching
/// network traffic. It can be either an explicit prefix
/// or defined by an object ID.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum NetworkSecurityGroupRuleNet {
    Prefix(ipnetwork::IpNetwork),
    // Not yet supported.  We hide this in the proto spec.
    // Implementation wouldn't be hard, but it would be hard
    // to manage operationally because this would allow users
    // to create a rule that does not exceed any limits but
    // later silently exceeds limits by adding more prefixes
    // to a VPC without ever touching the NSG.  It would be
    // dangerous/irresponsible to simply allow the overflow
    // (the behavior on the DPU could be undefined), or trim
    // out anything beyond our limits.
    // We either need some job checking the state of things
    // and alerting, or the DPU needs to alert when limits
    // are exceeded, but the damage is already done by the
    // time something is alerting.
    // This also perhaps opens a security issue unless we
    // restrict the prefixes that a tenant is allowed to add
    // to a VPC.  Otherwise, If I allow (VPC ID 123), then
    // it means I allow whatever random prefixes the owner
    // of that VPC attaches.  It implicitly hands off part of
    // "my" ACL control over to the other side.
    // VpcId(VpcId),
}

/* ********************************** */
/*       NetworkSecurityGroupRule     */
/* ********************************** */

/// NetworkSecurityGroupRule holds the details of a
/// single rule that will be applied on a DPU to restrict
/// traffic.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct NetworkSecurityGroupRule {
    pub id: Option<String>,
    pub src_net: NetworkSecurityGroupRuleNet,
    pub dst_net: NetworkSecurityGroupRuleNet,
    pub direction: NetworkSecurityGroupRuleDirection,
    pub ipv6: bool,
    pub src_port_start: Option<u32>,
    pub src_port_end: Option<u32>,
    pub dst_port_start: Option<u32>,
    pub dst_port_end: Option<u32>,
    pub protocol: NetworkSecurityGroupRuleProtocol,
    pub action: NetworkSecurityGroupRuleAction,
    pub priority: u32,
}

/* ********************************** */
/*         NetworkSecurityGroup       */
/* ********************************** */

/// NetworkSecurityGroup represents a collection of L4 traffic
/// ACLs to permit or deny network traffic based on a set of
/// matching properties.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkSecurityGroup {
    pub id: NetworkSecurityGroupId,
    pub tenant_organization_id: TenantOrganizationId,
    pub stateful_egress: bool,
    pub rules: Vec<NetworkSecurityGroupRule>,
    pub version: ConfigVersion,
    pub created: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub metadata: Metadata,
    pub created_by: Option<String>,
    pub updated_by: Option<String>,
}

/* ******************************************* */
/*         NetworkSecurityGroupAttachments     */
/* ******************************************* */

/// NetworkSecurityGroupAttachments holds lists of objects that have
/// the NetworkSecurityGroup attached.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkSecurityGroupAttachments {
    pub id: NetworkSecurityGroupId,
    pub vpc_ids: Vec<VpcId>,
    pub instance_ids: Vec<InstanceId>,
}

impl NetworkSecurityGroupAttachments {
    pub fn has_attachments(&self) -> bool {
        !(self.vpc_ids.is_empty() && self.instance_ids.is_empty())
    }
}

/* ******************************************* */
/* NetworkSecurityGroupPropagationObjectStatus */
/* ******************************************* */

/// NetworkSecurityGroupPropagationObjectStatus holds
/// the propagation status of a single object (vpc, instance, etc)
/// The status of propagation depends on the propagation of all
/// underlying objects that do not have a more specific NSG applied.
/// For example, for a VPC to be fully propagated, all interfaces
/// of all instances under that VPC must be fully propagated.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkSecurityGroupPropagationObjectStatus {
    pub id: String,
    pub interfaces_expected: u32,
    pub interfaces_applied: u32,
    pub related_instance_ids: Vec<InstanceId>,
    pub unpropagated_instance_ids: Vec<InstanceId>,
}

/* ******************************************* */
/*    NetworkSecurityGroupStatusObservation    */
/* ******************************************* */

/// NetworkSecurityGroupStatusObservation holds he
/// network security group details observed on an
/// interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSecurityGroupStatusObservation {
    pub id: NetworkSecurityGroupId,
    pub version: ConfigVersion,
    pub source: NetworkSecurityGroupSource,
}

impl<'r> sqlx::FromRow<'r, PgRow> for NetworkSecurityGroupAttachments {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let vpc_ids: sqlx::types::Json<Vec<VpcId>> = row.try_get("vpc_ids")?;
        let instance_ids: sqlx::types::Json<Vec<InstanceId>> = row.try_get("instance_ids")?;

        Ok(NetworkSecurityGroupAttachments {
            id: row.try_get("id")?,
            vpc_ids: vpc_ids.0,
            instance_ids: instance_ids.0,
        })
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for NetworkSecurityGroupPropagationObjectStatus {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let expected: i32 = row.try_get("interfaces_expected")?;
        let applied: i32 = row.try_get("interfaces_applied")?;

        let related_instance_ids: sqlx::types::Json<Vec<InstanceId>> =
            row.try_get("related_instance_ids")?;
        let unpropagated_instance_ids: sqlx::types::Json<Vec<InstanceId>> =
            row.try_get("unpropagated_instance_ids")?;

        Ok(NetworkSecurityGroupPropagationObjectStatus {
            id: row.try_get("id")?,
            interfaces_expected: expected
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            interfaces_applied: applied
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            related_instance_ids: related_instance_ids.0,
            unpropagated_instance_ids: unpropagated_instance_ids.0,
        })
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for NetworkSecurityGroup {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;

        let metadata = Metadata {
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            labels: labels.0,
        };

        let rules: sqlx::types::Json<Vec<NetworkSecurityGroupRule>> = row.try_get("rules")?;
        let tenant_organization_id: String = row.try_get("tenant_organization_id")?;

        Ok(NetworkSecurityGroup {
            id: row.try_get("id")?,
            version: row.try_get("version")?,
            stateful_egress: row.try_get("stateful_egress")?,
            tenant_organization_id: tenant_organization_id
                .parse::<TenantOrganizationId>()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            created: row.try_get("created")?,
            deleted: row.try_get("deleted")?,
            created_by: row.try_get("created_by")?,
            updated_by: row.try_get("updated_by")?,
            metadata,
            rules: rules.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_rule_ignores_unknown_legacy_fields() {
        let rule: NetworkSecurityGroupRule = serde_json::from_value(serde_json::json!({
            "id": null,
            "src_net": { "Prefix": "0.0.0.0/0" },
            "dst_net": { "Prefix": "0.0.0.0/0" },
            "direction": "Ingress",
            "ipv6": false,
            "src_port_start": null,
            "src_port_end": null,
            "dst_port_start": null,
            "dst_port_end": null,
            "protocol": "Any",
            "action": "Deny",
            "priority": 1,
            "legacy_field": "ignored"
        }))
        .expect("persisted NSG rules remain backward-compatible");

        assert_eq!(rule.priority, 1);
    }
}
