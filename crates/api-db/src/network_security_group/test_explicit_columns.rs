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

use model::network_security_group::{
    NetworkSecurityGroupRuleAction, NetworkSecurityGroupRuleDirection, NetworkSecurityGroupRuleNet,
    NetworkSecurityGroupRuleProtocol,
};
use sqlx::{Connection, PgPool};

use super::*;

#[crate::sqlx_test]
async fn network_security_group_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_network_security_group_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE network_security_groups ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_network_security_group_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_network_security_group_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let tenant_organization_id: TenantOrganizationId = "projection-tenant".parse()?;
    crate::tenant::create_and_persist(
        tenant_organization_id.to_string(),
        Metadata {
            name: "projection tenant".to_string(),
            ..Default::default()
        },
        None,
        &mut txn,
    )
    .await?;

    let id = "fd3ab096-d811-11ef-8fe9-7be4b2483448".parse()?;
    let metadata = Metadata {
        name: "projection group".to_string(),
        description: "original rules".to_string(),
        labels: [("stage".to_string(), "original".to_string())].into(),
    };
    let rules = vec![NetworkSecurityGroupRule {
        id: Some("allow-https".to_string()),
        src_net: NetworkSecurityGroupRuleNet::Prefix("192.0.2.0/24".parse()?),
        dst_net: NetworkSecurityGroupRuleNet::Prefix("198.51.100.0/24".parse()?),
        direction: NetworkSecurityGroupRuleDirection::Ingress,
        ipv6: false,
        src_port_start: Some(1024),
        src_port_end: Some(65535),
        dst_port_start: Some(443),
        dst_port_end: Some(443),
        protocol: NetworkSecurityGroupRuleProtocol::Tcp,
        action: NetworkSecurityGroupRuleAction::Permit,
        priority: 100,
    }];
    let created = create(
        &mut txn,
        &id,
        &tenant_organization_id,
        Some("group-creator"),
        &metadata,
        true,
        &rules,
    )
    .await?;
    let mut expected = NetworkSecurityGroup {
        id,
        tenant_organization_id,
        stateful_egress: true,
        rules,
        version: created.version,
        created: created.created,
        deleted: None,
        metadata,
        created_by: Some("group-creator".to_string()),
        updated_by: None,
    };
    assert_eq!(created, expected);
    assert_eq!(created.version.version_nr(), 1);

    expected.metadata = Metadata {
        name: "updated projection group".to_string(),
        description: "updated rules".to_string(),
        labels: [("stage".to_string(), "updated".to_string())].into(),
    };
    expected.rules[0].dst_port_start = Some(8443);
    expected.rules[0].dst_port_end = Some(8443);
    expected.stateful_egress = false;
    expected.updated_by = Some("group-updater".to_string());
    let updated = update(
        &mut txn,
        &expected.id,
        &expected.tenant_organization_id,
        &expected.metadata,
        expected.stateful_egress,
        &expected.rules,
        expected.version,
        expected.updated_by.as_deref(),
    )
    .await?;
    assert_eq!(
        updated.version.version_nr(),
        created.version.version_nr() + 1
    );
    expected.version = updated.version;
    assert_eq!(updated, expected);
    assert_eq!(
        find_by_ids(
            &mut txn,
            std::slice::from_ref(&expected.id),
            Some(&expected.tenant_organization_id),
            true,
        )
        .await?,
        vec![expected]
    );

    // Roll back the fixture so both passes decode an inserted row.
    // The connection retains its prepared statements after the rollback.
    txn.rollback().await?;
    Ok(())
}
