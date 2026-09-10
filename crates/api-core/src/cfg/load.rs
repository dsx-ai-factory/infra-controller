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

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use eyre::WrapErr;
use figment::providers::{Env, Format, Toml};
use figment::value::{Dict, Map, Value};
use figment::{Figment, Metadata, Profile, Provider};
use serde::de::DeserializeOwned;

use super::file::{CarbideConfig, InitialObjectsConfig};

#[derive(Clone, Debug, Eq, PartialEq)]
struct UnknownConfigurationField {
    path: String,
    source: String,
}

fn remove_value_at_path(value: &mut Value, path: &[String]) -> bool {
    let Some((head, tail)) = path.split_first() else {
        return false;
    };

    match value {
        Value::Dict(_, values) if tail.is_empty() => values.remove(head).is_some(),
        Value::Dict(_, values) => values
            .get_mut(head)
            .is_some_and(|value| remove_value_at_path(value, tail)),
        Value::Array(_, values) => head
            .parse::<usize>()
            .ok()
            .and_then(|index| values.get_mut(index))
            .is_some_and(|value| remove_value_at_path(value, tail)),
        _ => false,
    }
}

fn resolve_error_metadata(mut error: figment::Error, figment: &Figment) -> figment::Error {
    if error.metadata.is_none() {
        error.metadata = figment.find_metadata(&error.path.join(".")).cloned();
    }
    if error.profile.is_none() {
        error.profile = Some(figment.profile().clone());
    }
    error
}

#[allow(clippy::result_large_err)] // Figment controls the error representation.
fn extract_with_unknown_fields<T>(
    figment: &Figment,
) -> Result<(T, Vec<UnknownConfigurationField>), figment::Error>
where
    T: DeserializeOwned,
{
    let mut value = figment.extract::<Value>()?;
    let mut unknown_fields = BTreeMap::new();

    loop {
        match T::deserialize(&value) {
            Ok(config) => return Ok((config, unknown_fields.into_values().collect())),
            Err(error) => {
                let figment::error::Kind::UnknownField(field, _) = &error.kind else {
                    return Err(resolve_error_metadata(error, figment));
                };

                let mut path = error.path.clone();
                if path.last().map(String::as_str) != Some(field) {
                    path.push(field.clone());
                }
                let dotted_path = path.join(".");
                let source = figment
                    .find_metadata(&dotted_path)
                    .map(super::provenance::source_label)
                    .unwrap_or_else(|| "configuration".to_string());

                if !remove_value_at_path(&mut value, &path) {
                    return Err(resolve_error_metadata(error, figment));
                }

                unknown_fields.insert(
                    dotted_path.clone(),
                    UnknownConfigurationField {
                        path: dotted_path,
                        source,
                    },
                );
            }
        }
    }
}

fn apply_unknown_field_policy(
    unknown_fields: &[UnknownConfigurationField],
    deny_unknown_fields: bool,
) -> eyre::Result<()> {
    if unknown_fields.is_empty() {
        return Ok(());
    }

    if deny_unknown_fields {
        let fields = unknown_fields
            .iter()
            .map(|field| format!("{} ({})", field.path, field.source))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(eyre::eyre!("unknown configuration fields: {fields}"));
    }

    for field in unknown_fields {
        tracing::warn!(
            config_key = %field.path,
            config_source = %field.source,
            "Ignoring unknown configuration key"
        );
    }
    Ok(())
}

/// Parse the `InitialObjectsConfig` file referenced by
/// [`CarbideConfig::initial_objects_file`], warning about unknown fields.
pub fn parse_initial_objects_config(path: &Path) -> eyre::Result<InitialObjectsConfig> {
    parse_initial_objects_config_with_policy(path, false)
}

/// Parse an `InitialObjectsConfig` using the caller's unknown-field policy.
pub fn parse_initial_objects_config_with_policy(
    path: &Path,
    deny_unknown_fields: bool,
) -> eyre::Result<InitialObjectsConfig> {
    let figment = Figment::new().merge(Toml::file(path));
    let (config, unknown_fields) = extract_with_unknown_fields::<InitialObjectsConfig>(&figment)
        .wrap_err_with(|| format!("while parsing InitialObjectsConfig at {}", path.display()))?;
    apply_unknown_field_policy(&unknown_fields, deny_unknown_fields)
        .wrap_err_with(|| format!("while parsing InitialObjectsConfig at {}", path.display()))?;
    Ok(config)
}

/// Return a list of all configuration files that were merged to create the
/// effective configuration, for logging purposes. This is used in error messages
/// when there is a problem with the configuration, to help the operator
/// understand which files to look at to fix the problem.
pub(crate) fn all_configuration_files(carbide_config: &CarbideConfig) -> Vec<&Path> {
    carbide_config
        .config_ctx
        .as_ref()
        .into_iter()
        .flat_map(|figment| figment.metadata())
        .filter_map(|metadata| metadata.source.as_ref()?.file_path())
        .collect()
}

/// Normalizes the legacy site-explorer DPU-policy key within one configuration
/// provider, before Figment applies provider precedence.
///
/// Keeping this at the provider boundary makes global < site < environment win
/// regardless of whether a source uses `dpu_policy` or legacy `dpu_mode`.
/// Within one source, the canonical key wins when both are present.
struct NormalizeLegacyDpuPolicy<P>(P);

impl<P: Provider> Provider for NormalizeLegacyDpuPolicy<P> {
    fn metadata(&self) -> Metadata {
        self.0.metadata()
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        let mut data = self.0.data()?;
        for profile in data.values_mut() {
            let Some(Value::Dict(_, site_explorer)) = profile.get_mut("site_explorer") else {
                continue;
            };

            let legacy = site_explorer.remove("dpu_mode");
            let canonical_is_set = site_explorer
                .get("dpu_policy")
                .is_some_and(|value| !matches!(value, Value::Empty(..)));
            if !canonical_is_set && let Some(value) = legacy {
                site_explorer.insert("dpu_policy".to_string(), value);
            }
        }

        Ok(data)
    }

    fn profile(&self) -> Option<Profile> {
        self.0.profile()
    }
}

pub(crate) fn merged_carbide_config_figment(
    config_path: &Path,
    site_config_path: Option<&Path>,
) -> Figment {
    let mut figment = Figment::new().merge(NormalizeLegacyDpuPolicy(Toml::file(config_path)));
    if let Some(site_config_path) = site_config_path {
        figment = figment.merge(NormalizeLegacyDpuPolicy(Toml::file(site_config_path)));
    }

    figment.merge(NormalizeLegacyDpuPolicy(Env::prefixed("CARBIDE_API_")))
}

/// Load, normalize, and validate the Carbide API configuration.
pub fn parse_carbide_config(
    config_path: &Path,
    site_config_path: Option<&Path>,
) -> eyre::Result<Arc<CarbideConfig>> {
    let merged_config = merged_carbide_config_figment(config_path, site_config_path);
    let (mut config, unknown_fields) = extract_with_unknown_fields::<CarbideConfig>(&merged_config)
        .wrap_err("failed to load configuration files")?;
    tracing::info!(
        deny_unknown_fields = config.deny_unknown_fields,
        unknown_field_policy = if config.deny_unknown_fields {
            "deny"
        } else {
            "warn"
        },
        "Using configuration unknown-field policy"
    );
    apply_unknown_field_policy(&unknown_fields, config.deny_unknown_fields)
        .wrap_err("failed to load configuration files")?;

    config.config_ctx = Some(merged_config);

    for (path, is_set) in [
        (
            "force_dpu_nic_mode",
            config.deprecated_force_dpu_nic_mode.is_some(),
        ),
        (
            "site_explorer.force_dpu_nic_mode",
            config.site_explorer.deprecated_force_dpu_nic_mode.is_some(),
        ),
    ] {
        if !is_set {
            continue;
        }
        let source = config
            .config_ctx
            .as_ref()
            .and_then(|figment| figment.find_metadata(path))
            .map(super::provenance::source_label)
            .unwrap_or_else(|| "configuration".to_string());
        tracing::warn!(
            config_key = path,
            config_source = %source,
            replacement = "site_explorer.dpu_policy",
            "Ignoring deprecated configuration key"
        );
    }

    for (label, _) in config
        .host_models
        .iter()
        .filter(|(_, host)| host.vendor == bmc_vendor::BMCVendor::Unknown)
    {
        tracing::error!(label = %label, "Host firmware configuration has invalid vendor");
    }

    // If the carbide config does not say whether to allow dynamically changing the bmc_proxy or
    // not, the API handler for changing the bmc_proxy setting will reject changes to it for safety
    // reasons (it can be dangerous in production environments.) But if the config already sets
    // bmc_proxy, default to allow_changing_bmc_proxy=true, as we only should be setting bmc_proxy
    // in dev environments in the first place.
    if config.site_explorer.allow_changing_bmc_proxy.is_none()
        && (config.site_explorer.bmc_proxy.load().is_some()
            || config.site_explorer.override_target_port.is_some()
            || config.site_explorer.override_target_ip.is_some())
    {
        tracing::debug!(
            "Carbide config contains override for bmc_proxy, allowing dynamic bmc_proxy configuration"
        );
        config.site_explorer.allow_changing_bmc_proxy = Some(true);
    }

    if let Some(old_update_limit) = config.max_concurrent_machine_updates {
        if let Some(new_update_limit) = config
            .machine_updater
            .max_concurrent_machine_updates_absolute
        {
            // Both specified, use the smaller
            config
                .machine_updater
                .max_concurrent_machine_updates_absolute =
                Some(std::cmp::min(old_update_limit, new_update_limit));
        } else {
            config
                .machine_updater
                .max_concurrent_machine_updates_absolute = config.max_concurrent_machine_updates;
        }
    }

    // Validate that admin-UI tool entries have unique names.
    config.validate_web_ui_sidebar_tools()?;
    config.validate_service_vpc_slots()?;
    config.api_admission_control.validate()?;

    if let Some(config) = &config.dsx_exchange_event_bus {
        config.periodic_state_republish.validate()?;
    }

    // Publish the configured tool list so the admin-UI sidebar and per-machine
    // "Logs" deep link can read it back via `crate::configured_tools`. The list
    // is owned here (not in `carbide-api-web`) because it is derived from the
    // parsed config, before the web layer exists.
    crate::init_tools(config.web_ui_sidebar_tools.clone());

    // Publish the site name the same way, for the admin-UI sidebar header.
    crate::init_site_name(config.sitename.clone());

    // Publish the logs link URL template for the "Logs" link on machine and
    // endpoint detail pages.
    crate::init_logs_link_template(config.web_ui_logs_link_template.clone());

    // Publish the deployment-wide host naming policy so the DB layer can read it
    // wherever an interface is [re]named (same way we do it w/ `init_tools` above).
    db::host_naming::configure(config.host_naming_strategy);

    // Validate that the firmware profile config keys match their inner
    // part_number and psid values. Mismatches are logged as warnings.
    config.validate_supernic_firmware_profiles();

    if let Some(manager_config) = &config.component_manager {
        component_manager::rms::validate_rms_backend_rack_profiles(
            manager_config,
            &config.rack_profiles,
        )
        .map_err(|error| eyre::eyre!(error).wrap_err("invalid configuration"))?;
    }

    model::tenant::validate_trust_domain_allowlist_patterns(
        &config.machine_identity.trust_domain_allowlist,
    )
    .map_err(|error| eyre::eyre!(error).wrap_err("invalid configuration"))?;

    model::tenant::validate_token_endpoint_domain_allowlist_patterns(
        &config.machine_identity.token_endpoint_domain_allowlist,
    )
    .map_err(|error| eyre::eyre!(error).wrap_err("invalid configuration"))?;

    if config.machine_identity.enabled
        && config.machine_identity.current_encryption_key_id.is_none()
    {
        return Err(eyre::eyre!(
            "current_encryption_key_id must be set in [machine_identity] when machine identity is enabled"
        )
        .wrap_err("invalid configuration"));
    }

    tracing::trace!(config = ?config.redacted(), "Carbide config");
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::result_large_err)]
    fn unknown_site_override_field_is_collected_with_source() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "base.toml",
                r#"
                database_url = "postgres://test"
                listen = "[::]:1081"
                asn = 1
                "#,
            )?;
            jail.create_file(
                "site.toml",
                "[site_explorer]\nunknown_site_override_field = true",
            )?;

            let figment =
                merged_carbide_config_figment(Path::new("base.toml"), Some(Path::new("site.toml")));
            let (config, unknown_fields) = extract_with_unknown_fields::<CarbideConfig>(&figment)?;

            assert!(!config.deny_unknown_fields);
            assert_eq!(
                unknown_fields,
                vec![UnknownConfigurationField {
                    path: "site_explorer.unknown_site_override_field".to_string(),
                    source: "site.toml".to_string(),
                }]
            );
            apply_unknown_field_policy(&unknown_fields, config.deny_unknown_fields)
                .expect("unknown fields warn by default");
            Ok(())
        })
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn strict_mode_rejects_all_unknown_fields() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "base.toml",
                r#"
                database_url = "postgres://test"
                listen = "[::]:1081"
                asn = 1
                deny_unknown_fields = true
                unknown_root_field = true
                [site_explorer]
                unknown_nested_field = true
                "#,
            )?;

            let figment = merged_carbide_config_figment(Path::new("base.toml"), None);
            let (config, unknown_fields) = extract_with_unknown_fields::<CarbideConfig>(&figment)?;
            let error = apply_unknown_field_policy(&unknown_fields, config.deny_unknown_fields)
                .expect_err("strict mode rejects unknown fields");
            let message = error.to_string();
            assert!(message.contains("unknown_root_field (base.toml)"));
            assert!(message.contains("site_explorer.unknown_nested_field (base.toml)"));
            Ok(())
        })
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn invalid_known_field_still_fails_in_warning_mode() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "base.toml",
                r#"
                database_url = "postgres://test"
                listen = "[::]:1081"
                asn = 1
                max_database_connections = "many"
                "#,
            )?;

            let figment = merged_carbide_config_figment(Path::new("base.toml"), None);
            let error = extract_with_unknown_fields::<CarbideConfig>(&figment)
                .expect_err("invalid known values remain fatal");
            assert!(matches!(error.kind, figment::error::Kind::InvalidType(..)));
            assert_eq!(error.path, vec!["max_database_connections".to_string()]);
            Ok(())
        })
    }
}
