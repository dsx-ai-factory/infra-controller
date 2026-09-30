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

use carbide_utils::none_if_empty::NoneIfEmpty;
use config_version::ConfigVersion;
use model::machine_validation::{
    MachineValidationTest, MachineValidationTestAddRequest, MachineValidationTestUpdatePayload,
    MachineValidationTestUpdateRequest, MachineValidationTestsGetRequest,
};
use regex::Regex;
use sqlx::{PgConnection, Postgres, QueryBuilder};

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// `None` means "do not change this column" for `UPDATE ... COALESCE($n, col)` (bind `NULL`).
/// For slice fields from a patch payload, an empty slice is treated like "omit" (same as historical
/// dynamic SQL that skipped empty JSON arrays).
fn patch_vec(values: &[String]) -> Option<Vec<String>> {
    values.to_vec().none_if_empty()
}

pub async fn find(
    txn: impl DbReader<'_>,
    req: MachineValidationTestsGetRequest,
) -> DatabaseResult<Vec<MachineValidationTest>> {
    let mut qb: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT * FROM machine_validation_tests WHERE 1 = 1");

    if !req.supported_platforms.is_empty() {
        qb.push(" AND supported_platforms && ");
        qb.push_bind(req.supported_platforms);
    }
    if !req.contexts.is_empty() {
        qb.push(" AND contexts && ");
        qb.push_bind(req.contexts);
    }
    if let Some(ref test_id) = req.test_id {
        qb.push(" AND LOWER(test_id) = LOWER(");
        qb.push_bind(test_id);
        qb.push(")");
    }
    if let Some(ro) = req.read_only {
        qb.push(" AND read_only = ");
        qb.push_bind(ro);
    }
    if !req.custom_tags.is_empty() {
        qb.push(" AND custom_tags && ");
        qb.push_bind(req.custom_tags);
    }
    if let Some(ref ver) = req.version {
        qb.push(" AND version = ");
        qb.push_bind(ver);
    }
    if let Some(en) = req.is_enabled {
        qb.push(" AND is_enabled = ");
        qb.push_bind(en);
    }
    if let Some(v) = req.verified {
        qb.push(" AND verified = ");
        qb.push_bind(v);
    }

    qb.push(" ORDER BY version DESC, name ASC");

    let q = qb.build_query_as::<MachineValidationTest>();
    let ret = q
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query("SELECT machine_validation_tests", e))?;
    Ok(ret)
}

pub fn generate_test_id(name: &str) -> String {
    format!("forge_{}", name.to_ascii_lowercase())
}

pub async fn save(
    txn: &mut PgConnection,
    mut req: MachineValidationTestAddRequest,
    version: ConfigVersion,
) -> DatabaseResult<String> {
    let test_id = generate_test_id(&req.name);

    let re = Regex::new(r"[ =;:@#\!?\-]").unwrap();
    req.supported_platforms = req
        .supported_platforms
        .iter()
        .map(|p| re.replace_all(p, "_").to_string().to_ascii_lowercase())
        .collect();

    let description = req.description.unwrap_or_default();
    let execute_in_host = req.execute_in_host.unwrap_or(false);
    let timeout = req.timeout.unwrap_or(7200);
    let read_only = req.read_only.unwrap_or(false);
    // Plugin revisions must be explicitly verified before they can be enabled.
    let is_enabled = if req.plugin.is_some() {
        false
    } else {
        req.is_enabled.unwrap_or(true)
    };
    let plugin = req.plugin.clone().map(sqlx::types::Json);
    let img_name = req.img_name.clone();
    let container_arg = req.container_arg.clone();
    let external_config_file = req.external_config_file.clone();
    let extra_output_file = req.extra_output_file.clone();
    let extra_err_file = req.extra_err_file.clone();
    let pre_condition = req.pre_condition.clone();
    let custom_tags = req.custom_tags.clone().none_if_empty();
    let components = if req.components.is_empty() {
        vec!["Compute".to_string()]
    } else {
        req.components.clone()
    };
    let version_str = version.version_string();
    let modified_by = "User";

    sqlx::query_scalar::<Postgres, String>(
        r#"
        INSERT INTO machine_validation_tests (
            test_id,
            name,
            description,
            img_name,
            container_arg,
            execute_in_host,
            external_config_file,
            command,
            args,
            extra_output_file,
            extra_err_file,
            pre_condition,
            contexts,
            timeout,
            version,
            supported_platforms,
            modified_by,
            verified,
            read_only,
            custom_tags,
            components,
            is_enabled,
            plugin
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23
        )
        RETURNING test_id
        "#,
    )
    .bind(&test_id)
    .bind(&req.name)
    .bind(&description)
    .bind(img_name.as_ref())
    .bind(container_arg.as_ref())
    .bind(execute_in_host)
    .bind(external_config_file.as_ref())
    .bind(&req.command)
    .bind(&req.args)
    .bind(extra_output_file.as_ref())
    .bind(extra_err_file.as_ref())
    .bind(pre_condition.as_ref())
    .bind(&req.contexts)
    .bind(timeout)
    .bind(&version_str)
    .bind(&req.supported_platforms)
    .bind(modified_by)
    .bind(false)
    .bind(read_only)
    .bind(custom_tags.as_ref())
    .bind(&components)
    .bind(is_enabled)
    .bind(plugin)
    .fetch_one(txn)
    .await
    .map_err(|e| DatabaseError::query("INSERT machine_validation_tests", e))?;

    Ok(test_id)
}

pub async fn update(
    txn: &mut PgConnection,
    req: MachineValidationTestUpdateRequest,
) -> DatabaseResult<String> {
    let Some(mut payload) = req.payload else {
        return Err(DatabaseError::InvalidArgument(
            "Payload is missing".to_owned(),
        ));
    };
    let re = Regex::new(r"[ =;:@#\!?\-]").unwrap();
    payload.supported_platforms = payload
        .supported_platforms
        .iter()
        .map(|p| re.replace_all(p, "_").to_string().to_ascii_lowercase())
        .collect();

    // Match prior behavior: any update without an explicit `verified` forces re-verification.
    let verified = payload.verified.unwrap_or(false);
    let modified_by = "User";

    let contexts = patch_vec(&payload.contexts);
    let supported_platforms = patch_vec(&payload.supported_platforms);
    let custom_tags = patch_vec(&payload.custom_tags);
    let components = patch_vec(&payload.components);

    let name = payload.name.as_deref();
    let description = payload.description.as_deref();
    let img_name = payload.img_name.as_deref();
    let container_arg = payload.container_arg.as_deref();
    let execute_in_host = payload.execute_in_host;
    let external_config_file = payload.external_config_file.as_deref();
    let command = payload.command.as_deref();
    let args = payload.args.as_deref();
    let extra_output_file = payload.extra_output_file.as_deref();
    let extra_err_file = payload.extra_err_file.as_deref();
    let pre_condition = payload.pre_condition.as_deref();
    let timeout = payload.timeout;
    let is_enabled = payload.is_enabled;

    let test_id = sqlx::query_scalar::<Postgres, String>(
        r#"
        UPDATE machine_validation_tests SET
            name = COALESCE($1, name),
            description = COALESCE($2, description),
            img_name = COALESCE($3, img_name),
            container_arg = COALESCE($4, container_arg),
            execute_in_host = COALESCE($5, execute_in_host),
            external_config_file = COALESCE($6, external_config_file),
            command = COALESCE($7, command),
            args = COALESCE($8, args),
            extra_output_file = COALESCE($9, extra_output_file),
            extra_err_file = COALESCE($10, extra_err_file),
            pre_condition = COALESCE($11, pre_condition),
            contexts = COALESCE($12, contexts),
            timeout = COALESCE($13, timeout),
            supported_platforms = COALESCE($14, supported_platforms),
            custom_tags = COALESCE($15, custom_tags),
            components = COALESCE($16, components),
            is_enabled = COALESCE($17, is_enabled),
            verified = $18,
            modified_by = $19
        WHERE test_id = $20 AND version = $21
        RETURNING test_id
        "#,
    )
    .bind(name)
    .bind(description)
    .bind(img_name)
    .bind(container_arg)
    .bind(execute_in_host)
    .bind(external_config_file)
    .bind(command)
    .bind(args)
    .bind(extra_output_file)
    .bind(extra_err_file)
    .bind(pre_condition)
    .bind(contexts.as_ref())
    .bind(timeout)
    .bind(supported_platforms.as_ref())
    .bind(custom_tags.as_ref())
    .bind(components.as_ref())
    .bind(is_enabled)
    .bind(verified)
    .bind(modified_by)
    .bind(&req.test_id)
    .bind(&req.version)
    .fetch_optional(txn)
    .await
    .map_err(|e| DatabaseError::query("UPDATE machine_validation_tests", e))?
    .ok_or_else(|| {
        DatabaseError::InvalidArgument(format!(
            "No row updated for test_id={} version={}",
            req.test_id, req.version
        ))
    })?;

    Ok(test_id)
}

pub async fn clone(
    txn: &mut PgConnection,
    test: &MachineValidationTest,
) -> DatabaseResult<(String, ConfigVersion)> {
    let add_req = MachineValidationTestAddRequest {
        name: test.name.clone(),
        description: test.description.clone(),
        contexts: test.contexts.clone(),
        img_name: test.img_name.clone(),
        execute_in_host: test.execute_in_host,
        container_arg: test.container_arg.clone(),
        command: test.command.clone(),
        args: test.args.clone(),
        extra_err_file: test.extra_err_file.clone(),
        external_config_file: test.external_config_file.clone(),
        pre_condition: test.pre_condition.clone(),
        timeout: test.timeout,
        extra_output_file: test.extra_output_file.clone(),
        supported_platforms: test.supported_platforms.clone(),
        read_only: None,
        custom_tags: test.custom_tags.clone().unwrap_or_default(),
        components: test.components.clone(),
        is_enabled: Some(test.is_enabled),
        plugin: test.plugin.clone(),
    };
    let next_version = test.version.increment();
    let test_id = save(txn, add_req, next_version).await?;
    Ok((test_id, next_version))
}

pub async fn mark_verified(
    txn: &mut PgConnection,
    test_id: String,
    version: ConfigVersion,
) -> DatabaseResult<String> {
    let req = MachineValidationTestUpdateRequest {
        test_id,
        version: version.version_string(),
        payload: Some(MachineValidationTestUpdatePayload {
            verified: Some(true),
            ..Default::default()
        }),
    };
    update(txn, req).await
}

pub async fn enable_disable(
    txn: &mut PgConnection,
    test_id: String,
    version: ConfigVersion,
    is_enabled: bool,
    is_verified: bool,
    is_plugin: bool,
) -> DatabaseResult<String> {
    let version = version.version_string();
    let is_enabled = is_enabled && (!is_plugin || is_verified);
    if is_enabled && is_plugin {
        // Run items are keyed by test ID. Keep one enabled revision for a
        // plugin family so later selection has one unambiguous definition.
        sqlx::query("SELECT 1 FROM machine_validation_tests WHERE test_id = $1 FOR UPDATE")
            .bind(&test_id)
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query("lock machine validation plugin revisions", e))?;
        sqlx::query(
            "UPDATE machine_validation_tests SET is_enabled = FALSE \
             WHERE test_id = $1 AND version <> $2 AND plugin IS NOT NULL AND is_enabled = TRUE",
        )
        .bind(&test_id)
        .bind(&version)
        .execute(&mut *txn)
        .await
        .map_err(|e| {
            DatabaseError::query("disable previous machine validation plugin revisions", e)
        })?;
    }
    let req = MachineValidationTestUpdateRequest {
        test_id,
        version,
        payload: Some(MachineValidationTestUpdatePayload {
            is_enabled: Some(is_enabled),
            verified: Some(is_verified),
            ..Default::default()
        }),
    };
    update(txn, req).await
}

/// Record the separate, revision-scoped approval required for a plugin with a
/// writable host-root mount. The API validates eligibility before calling this.
pub async fn approve_full_host(
    txn: &mut PgConnection,
    test_id: String,
    version: ConfigVersion,
) -> DatabaseResult<String> {
    let version = version.version_string();
    sqlx::query_scalar::<Postgres, String>(
        "UPDATE machine_validation_tests SET full_host_approved = TRUE \
         WHERE test_id = $1 AND version = $2 RETURNING test_id",
    )
    .bind(&test_id)
    .bind(&version)
    .fetch_optional(txn)
    .await
    .map_err(|e| DatabaseError::query("approve machine validation full-host access", e))?
    .ok_or_else(|| {
        DatabaseError::InvalidArgument(format!(
            "machine validation test revision not found: {test_id} {version}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_test_id_lowercases_name() {
        assert_eq!(generate_test_id("MyTest"), "forge_mytest");
        assert_eq!(generate_test_id("ALLCAPS"), "forge_allcaps");
        assert_eq!(generate_test_id("already_lower"), "forge_already_lower");
        assert_eq!(generate_test_id("MiXeD_CaSe_123"), "forge_mixed_case_123");
    }

    #[test]
    fn patch_vec_empty_is_none() {
        assert!(patch_vec(&[]).is_none());
        assert_eq!(patch_vec(&["a".into()]), Some(vec!["a".to_string()]));
    }
}
