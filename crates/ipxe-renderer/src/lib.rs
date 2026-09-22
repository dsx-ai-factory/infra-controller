/*
 * SPDX-FileCopyrightText: Copyright (c) 2021-2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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

use serde::Deserialize;
use sha2::{Digest, Sha256};

const STATIC_IPXE_MENU_TEMPLATE_ID: &str = "c816a939-0993-5ebf-82dd-5227ad215703";
const STATIC_IPXE_MENU_TEMPLATE: &str = include_str!("../../../pxe/ipxe/local/embed.ipxe");

/// iPXE OS definition with template-based rendering support
#[derive(Debug, Clone)]
pub struct IpxeScript {
    pub name: String,
    pub description: Option<String>,
    pub hash: String,
    pub tenant_id: Option<String>,
    pub ipxe_template_id: String,
    pub parameters: Vec<IpxeTemplateParameter>,
    pub artifacts: Vec<IpxeTemplateArtifact>,
}

/// Parameter for iPXE template substitution
#[derive(Debug, Clone, PartialEq)]
pub struct IpxeTemplateParameter {
    pub name: String,
    pub value: String,
}

/// Artifact cache strategy
#[derive(Debug, Clone, PartialEq)]
pub enum IpxeTemplateArtifactCacheStrategy {
    CacheAsNeeded, // Download and cache artifact locally if/when possible (default)
    LocalOnly,     // Artifact URL is site-specific/local: use url directly, no caching applicable
    CachedOnly,    // Artifact must be cached locally before use: fail if cached_url is absent
    RemoteOnly,    // Always fetch from remote URL, never cache locally (global)
}

/// Remote artifact to allow awareness for potential local caching/proxy
#[derive(Debug, Clone)]
pub struct IpxeTemplateArtifact {
    pub name: String,
    pub url: String,
    pub sha: Option<String>,
    pub auth_type: Option<String>,
    pub auth_token: Option<String>,
    pub cache_strategy: IpxeTemplateArtifactCacheStrategy,
    pub cached_url: Option<String>,
}

/// Visibility for iPXE script templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IpxeTemplateVisibility {
    /// NICo Core usage only.
    #[default]
    Internal,
    /// Usable by tenant.
    Public,
}

/// iPXE script template definition
#[derive(Debug, Clone, Deserialize)]
pub struct IpxeTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub template: String, // iPXE script template: `#!ipxe\n...`
    #[serde(default)]
    pub reserved_params: Vec<String>,
    #[serde(default)]
    pub required_params: Vec<String>,
    #[serde(default)]
    pub required_artifacts: Vec<String>,
    #[serde(default, alias = "scope")]
    pub visibility: IpxeTemplateVisibility,
}

/// Template collection loaded from YAML
#[derive(Debug, Deserialize)]
struct TemplateCollection {
    templates: Vec<IpxeTemplate>,
}

/// Error types for iPXE OS rendering
#[derive(Debug, thiserror::Error)]
pub enum IpxeScriptError {
    #[error("template not found: {0}")]
    TemplateNotFound(String),

    #[error("reserved parameter found in OS definition: {0}")]
    ReservedParameterFound(String),

    #[error("required parameter missing or empty: {0}")]
    RequiredParameterMissing(String),

    #[error("optional parameters provided but {{{{extra}}}} not in template")]
    ExtraParametersNotSupported,

    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("artifact not found: {0}")]
    ArtifactNotFound(String),

    #[error("missing reserved parameter: {0}")]
    MissingReservedParameter(String),

    #[error("unexpected reserved parameter: {0}")]
    UnexpectedReservedParameter(String),

    #[error("unreplaced placeholders found: {0}")]
    UnreplacedPlaceholders(String),

    #[error("artifact '{0}' has cache_strategy CachedOnly but no cached_url is available")]
    CachedOnlyNotCached(String),
}

pub type Result<T> = std::result::Result<T, IpxeScriptError>;

/// IpxeScriptRenderer is the trait for rendering IPXEOS objects to iPXE scripts
pub trait IpxeScriptRenderer {
    /// Render generates the final iPXE script from an IpxeScript object.
    /// Artifact URLs are replaced by local cached URLs when available (cached_url).
    /// `reserved_params` must contain exactly the reserved parameters defined
    /// in the template (provided by NICo Core).
    fn render(
        &self,
        ipxeos: &IpxeScript,
        reserved_params: &[IpxeTemplateParameter],
    ) -> Result<String>;

    /// GetTemplateByName returns a template by name
    fn get_template_by_name(&self, name: &str) -> Option<&IpxeTemplate>;

    /// GetTemplateById returns a template by its stable UUID
    fn get_template_by_id(&self, id: &str) -> Option<&IpxeTemplate>;

    /// ListTemplates returns all available template names
    fn list_templates(&self) -> Vec<String>;

    /// Validate checks if an IpxeScript object is valid for rendering.
    /// Returns error if:
    /// - Reserved parameters appear in OS definition parameters or artifacts
    /// - Required parameters are missing or empty
    /// - Required artifacts are missing or empty
    /// - Optional parameters are provided but {{extra}} not in template
    /// - Hash does not match hash in OS definition
    fn validate(&self, ipxeos: &IpxeScript) -> Result<()>;

    /// Hash returns a deterministic hash of an IpxeScript object.
    /// Includes: template name, all parameters, and artifact fields
    /// Excludes: cache_strategy, cached_url, and hash field itself.
    fn hash(&self, ipxeos: &IpxeScript) -> String;

    /// FabricateCachedURLs generates local URLs for artifacts based on specific rules:
    /// - If CacheStrategy is REMOTE_ONLY or LOCAL_ONLY: skip (cannot be cached)
    /// - If cached_url already set: skip (already processed)
    /// - Otherwise: generate ${base_url}/[hash] where hash is:
    ///   - sha field of the artifact (if present)
    ///   - SHA256 hash of the artifact record + name + URL (if sha is empty)
    fn fabricate_cached_urls(&self, ipxeos: &IpxeScript) -> IpxeScript;
}

/// Default implementation of IpxeScriptRenderer
pub struct DefaultIpxeScriptRenderer {
    templates: HashMap<String, IpxeTemplate>,
}

impl DefaultIpxeScriptRenderer {
    pub fn new() -> Self {
        // Load templates from embedded YAML file at compile time
        const TEMPLATES_YAML: &str = include_str!("../templates.yaml");

        let template_collection: TemplateCollection = serde_yaml::from_str(TEMPLATES_YAML)
            .expect("Failed to parse templates.yaml - this is a compile-time error");

        let templates = template_collection
            .templates
            .into_iter()
            .map(|mut t| {
                if t.id == STATIC_IPXE_MENU_TEMPLATE_ID {
                    t.template = STATIC_IPXE_MENU_TEMPLATE.to_string();
                }

                (t.name.clone(), t)
            })
            .collect();

        Self { templates }
    }

    pub fn with_templates(templates: HashMap<String, IpxeTemplate>) -> Self {
        Self { templates }
    }
}

impl Default for DefaultIpxeScriptRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the effective URL for an artifact respecting cache_strategy:
/// - RemoteOnly: always use the remote url, ignore cached_url
/// - LocalOnly: use url directly (it is already site-local, no caching applicable)
/// - CachedOnly: require cached_url, error if absent
/// - CacheAsNeeded: prefer cached_url, fall back to remote url
fn resolve_artifact_url(artifact: &IpxeTemplateArtifact) -> Result<String> {
    match artifact.cache_strategy {
        IpxeTemplateArtifactCacheStrategy::RemoteOnly => Ok(artifact.url.clone()),
        IpxeTemplateArtifactCacheStrategy::LocalOnly => Ok(artifact.url.clone()),
        IpxeTemplateArtifactCacheStrategy::CachedOnly => artifact
            .cached_url
            .clone()
            .ok_or_else(|| IpxeScriptError::CachedOnlyNotCached(artifact.name.clone())),
        IpxeTemplateArtifactCacheStrategy::CacheAsNeeded => Ok(artifact
            .cached_url
            .as_ref()
            .unwrap_or(&artifact.url)
            .clone()),
    }
}

impl IpxeScriptRenderer for DefaultIpxeScriptRenderer {
    fn render(
        &self,
        ipxeos: &IpxeScript,
        reserved_params: &[IpxeTemplateParameter],
    ) -> Result<String> {
        // Validate first
        self.validate(ipxeos)?;

        // Get template
        let template = self
            .get_template_by_id(&ipxeos.ipxe_template_id)
            .ok_or_else(|| IpxeScriptError::TemplateNotFound(ipxeos.ipxe_template_id.clone()))?;

        // Validate reserved parameters match template requirements
        self.validate_reserved_params(reserved_params, template)?;

        // Occurrence-based parameter consumption (with artifact substitution, case-insensitive)
        let mut consumption_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut consumed_param_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        let mut consumed_artifact_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        // Step 1: Consume parameters for required_params (in order with duplicates, case-insensitive)
        for required_name in &template.required_params {
            let required_lower = required_name.to_lowercase();
            // Find next unconsumed parameter with this name (case-insensitive)
            for (idx, param) in ipxeos.parameters.iter().enumerate() {
                if param.name.to_lowercase() == required_lower
                    && !consumed_param_indices.contains(&idx)
                    && !param.value.is_empty()
                {
                    consumption_map
                        .entry(required_lower.clone())
                        .or_default()
                        .push(param.value.clone());
                    consumed_param_indices.insert(idx);
                    break;
                }
            }
        }

        // Step 2: Consume artifacts for required_artifacts (in order with duplicates, case-insensitive)
        for required_artifact_name in &template.required_artifacts {
            let required_artifact_lower = required_artifact_name.to_lowercase();
            // Find next unconsumed artifact with this name (case-insensitive)
            for (idx, artifact) in ipxeos.artifacts.iter().enumerate() {
                if artifact.name.to_lowercase() == required_artifact_lower
                    && !consumed_artifact_indices.contains(&idx)
                {
                    let url = resolve_artifact_url(artifact)?;
                    consumption_map
                        .entry(required_artifact_lower.clone())
                        .or_default()
                        .push(url);
                    consumed_artifact_indices.insert(idx);
                    break;
                }
            }
        }

        // Step 3: Add reserved parameters (they override and are always provided, case-insensitive)
        for reserved_name in &template.reserved_params {
            let reserved_lower = reserved_name.to_lowercase();
            if let Some(reserved_param) = reserved_params
                .iter()
                .find(|p| p.name.to_lowercase() == reserved_lower)
            {
                consumption_map
                    .entry(reserved_lower)
                    .or_default()
                    .push(reserved_param.value.clone());
            }
        }

        // Step 4: Replace placeholders in template (lowercase keys)
        let mut result = template.template.clone();
        let mut processed_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for (param_name_lower, values) in &consumption_map {
            if processed_names.contains(param_name_lower) {
                continue;
            }
            processed_names.insert(param_name_lower.clone());

            // Placeholder should be lowercase
            let placeholder = format!("{{{{{}}}}}", param_name_lower);
            for value in values {
                if let Some(pos) = result.find(&placeholder) {
                    result.replace_range(pos..pos + placeholder.len(), value);
                }
            }
        }

        // Step 5: Handle {{extra}} placeholder for unconsumed parameters
        // PRESERVE ORIGINAL CASE for parameter names in {{extra}}
        if result.contains("{{extra}}") {
            let mut extra_params: Vec<String> = Vec::new();

            // Collect all unconsumed parameters (not artifacts for extra, preserve original case)
            for (idx, param) in ipxeos.parameters.iter().enumerate() {
                if !consumed_param_indices.contains(&idx) && !param.value.is_empty() {
                    extra_params.push(format!("{}={}", param.name, param.value)); // Original case preserved
                }
            }

            result = result.replace("{{extra}}", &extra_params.join(" "));
        }

        // Post-processing: replace multiple spaces with single space
        while result.contains("  ") {
            result = result.replace("  ", " ");
        }

        // Trim trailing spaces from each line
        result = result
            .lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n");

        // Check for unreplaced placeholders
        self.check_unreplaced_placeholders(&result)?;

        Ok(result)
    }

    fn get_template_by_name(&self, name: &str) -> Option<&IpxeTemplate> {
        self.templates.get(name)
    }

    fn get_template_by_id(&self, id: &str) -> Option<&IpxeTemplate> {
        self.templates.values().find(|t| t.id == id)
    }

    fn list_templates(&self) -> Vec<String> {
        self.templates.keys().cloned().collect()
    }

    fn validate(&self, ipxeos: &IpxeScript) -> Result<()> {
        // Get template
        let template = self
            .get_template_by_id(&ipxeos.ipxe_template_id)
            .ok_or_else(|| IpxeScriptError::TemplateNotFound(ipxeos.ipxe_template_id.clone()))?;

        // Check for globally reserved names: "extra" is reserved for {{extra}} placeholder
        // Names are case-insensitive (normalized to lowercase)
        for param in &ipxeos.parameters {
            if param.name.to_lowercase() == "extra" {
                return Err(IpxeScriptError::ReservedParameterFound(format!(
                    "'{}' (normalized to 'extra' which is globally reserved for {{{{extra}}}} placeholder)",
                    param.name
                )));
            }
        }

        for artifact in &ipxeos.artifacts {
            if artifact.name.to_lowercase() == "extra" {
                return Err(IpxeScriptError::ReservedParameterFound(format!(
                    "'{}' (normalized to 'extra' which is globally reserved, cannot be used as artifact name)",
                    artifact.name
                )));
            }
        }

        // Check for template-specific reserved parameters in OS definition
        // Case-insensitive comparison
        for param in &ipxeos.parameters {
            let param_lower = param.name.to_lowercase();
            for reserved in &template.reserved_params {
                if param_lower == reserved.to_lowercase() {
                    return Err(IpxeScriptError::ReservedParameterFound(param.name.clone()));
                }
            }
        }

        // Check for required parameters - count occurrences
        // Build map of required counts for each parameter name (case-insensitive, use lowercase)
        let mut required_param_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for required_param in &template.required_params {
            let key = required_param.to_lowercase();
            *required_param_counts.entry(key).or_insert(0) += 1;
        }

        // Count available non-empty parameters and artifacts (case-insensitive comparison)
        for (required_name_lower, required_count) in &required_param_counts {
            let param_count = ipxeos
                .parameters
                .iter()
                .filter(|p| p.name.to_lowercase() == *required_name_lower && !p.value.is_empty())
                .count();

            let artifact_count = ipxeos
                .artifacts
                .iter()
                .filter(|a| a.name.to_lowercase() == *required_name_lower)
                .count();

            let available_count = param_count + artifact_count;

            if available_count < *required_count {
                return Err(IpxeScriptError::RequiredParameterMissing(format!(
                    "{} (need {} occurrences, have {})",
                    required_name_lower, required_count, available_count
                )));
            }
        }

        // Check for required artifacts - count occurrences (case-insensitive)
        let mut required_artifact_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for required_artifact in &template.required_artifacts {
            let key = required_artifact.to_lowercase();
            *required_artifact_counts.entry(key).or_insert(0) += 1;
        }

        for (required_name_lower, required_count) in &required_artifact_counts {
            let artifact_count = ipxeos
                .artifacts
                .iter()
                .filter(|a| a.name.to_lowercase() == *required_name_lower && !a.url.is_empty())
                .count();

            if artifact_count < *required_count {
                return Err(IpxeScriptError::ArtifactNotFound(format!(
                    "{} (need {} occurrences, have {})",
                    required_name_lower, required_count, artifact_count
                )));
            }
        }

        // Check if optional parameters are provided but {{extra}} is not in template
        // Case-insensitive comparison using lowercase
        let used_params_lower: std::collections::HashSet<String> = template
            .required_params
            .iter()
            .chain(template.reserved_params.iter())
            .map(|s| s.to_lowercase())
            .collect();

        let has_extra_params = ipxeos
            .parameters
            .iter()
            .any(|p| !used_params_lower.contains(&p.name.to_lowercase()));

        if has_extra_params && !template.template.contains("{{extra}}") {
            return Err(IpxeScriptError::ExtraParametersNotSupported);
        }

        // Validate hash
        let computed_hash = self.hash(ipxeos);
        if computed_hash != ipxeos.hash {
            return Err(IpxeScriptError::HashMismatch {
                expected: ipxeos.hash.clone(),
                actual: computed_hash,
            });
        }

        Ok(())
    }

    fn hash(&self, ipxeos: &IpxeScript) -> String {
        let mut hasher = Sha256::new();

        // Hash template name (lowercase for case-insensitivity)
        hasher.update(ipxeos.ipxe_template_id.to_lowercase().as_bytes());

        let mut params = ipxeos.parameters.clone();
        params.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.value.cmp(&b.value))
        });
        for param in params {
            hasher.update(param.name.to_lowercase().as_bytes()); // Lowercase for case-insensitivity
            hasher.update(param.value.as_bytes()); // Value keeps original case
        }

        let mut artifacts = ipxeos.artifacts.clone();
        artifacts.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.url.cmp(&b.url))
        });
        for artifact in artifacts {
            hasher.update(artifact.name.to_lowercase().as_bytes()); // Lowercase for case-insensitivity
            hasher.update(artifact.url.as_bytes());
            if let Some(sha) = &artifact.sha {
                hasher.update(sha.as_bytes());
            }
            if let Some(auth_type) = &artifact.auth_type {
                hasher.update(auth_type.as_bytes());
            }
            if let Some(auth_token) = &artifact.auth_token {
                hasher.update(auth_token.as_bytes());
            }
        }

        hex::encode(hasher.finalize())
    }

    fn fabricate_cached_urls(&self, ipxeos: &IpxeScript) -> IpxeScript {
        // TODO: this is a placeholder until we have a caching service.
        // It is an example of what should happen if we had such a service.
        let mut new_ipxeos = ipxeos.clone();

        for artifact in &mut new_ipxeos.artifacts {
            // Skip if RemoteOnly/LocalOnly (no caching applicable) or already has cached_url
            if artifact.cache_strategy == IpxeTemplateArtifactCacheStrategy::RemoteOnly
                || artifact.cache_strategy == IpxeTemplateArtifactCacheStrategy::LocalOnly
                || artifact.cached_url.is_some()
            {
                continue;
            }

            // Generate local URL hash
            let hash = if let Some(sha) = &artifact.sha {
                // Use the sha field directly (it's already a hash/checksum)
                sha.clone()
            } else {
                // Compute SHA256 hash of the artifact record (name + URL)
                let mut hasher = Sha256::new();
                hasher.update(artifact.name.as_bytes());
                hasher.update(artifact.url.as_bytes());
                hex::encode(hasher.finalize())
            };

            artifact.cached_url = Some(format!("${{base_url}}/{}", hash));
        }

        new_ipxeos
    }
}

impl DefaultIpxeScriptRenderer {
    /// Validate that reserved parameters provided match template requirements exactly
    fn validate_reserved_params(
        &self,
        reserved_params: &[IpxeTemplateParameter],
        template: &IpxeTemplate,
    ) -> Result<()> {
        // Build sets of unique names for comparison (case-insensitive, use lowercase)
        let provided: std::collections::HashSet<String> = reserved_params
            .iter()
            .map(|p| p.name.to_lowercase())
            .collect();
        let required: std::collections::HashSet<String> = template
            .reserved_params
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        // Check for missing reserved parameters (case-insensitive)
        for required_param in &template.reserved_params {
            let required_lower = required_param.to_lowercase();
            if !provided.contains(&required_lower) {
                return Err(IpxeScriptError::MissingReservedParameter(
                    required_param.to_string(),
                ));
            }
        }

        // Check for unexpected reserved parameters (case-insensitive)
        for provided_param in reserved_params {
            let provided_lower = provided_param.name.to_lowercase();
            if !required.contains(&provided_lower) {
                return Err(IpxeScriptError::UnexpectedReservedParameter(
                    provided_param.name.clone(),
                ));
            }
        }

        Ok(())
    }

    /// Check if there are any unreplaced placeholders in the result
    fn check_unreplaced_placeholders(&self, result: &str) -> Result<()> {
        // Find all {{...}} patterns
        let mut unreplaced = Vec::new();
        let mut start = 0;
        while let Some(pos) = result[start..].find("{{") {
            let abs_pos = start + pos;
            if let Some(end_pos) = result[abs_pos..].find("}}") {
                let placeholder = &result[abs_pos..abs_pos + end_pos + 2];
                unreplaced.push(placeholder.to_string());
                start = abs_pos + end_pos + 2;
            } else {
                break;
            }
        }

        if !unreplaced.is_empty() {
            return Err(IpxeScriptError::UnreplacedPlaceholders(
                unreplaced.join(", "),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    use super::*;

    fn create_test_ipxeos() -> IpxeScript {
        IpxeScript {
            name: "Test OS".to_string(),
            description: Some("Test operating system".to_string()),
            hash: "placeholder".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "image_url".to_string(),
                value: "http://example.com/image.qcow2".to_string(),
            }],
            artifacts: vec![],
        }
    }

    /// One row of the cache-strategy URL-selection table: an artifact built from a
    /// `cache_strategy` and whether a `cached_url` is present, resolved through
    /// [`resolve_artifact_url`].
    struct UrlSelectionRow {
        cache_strategy: IpxeTemplateArtifactCacheStrategy,
        cached_url: Option<&'static str>,
    }

    /// Build an artifact from a row and resolve its effective URL. The remote `url`
    /// and the cached URL are distinct sentinels so the yielded value names which
    /// one the strategy selected.
    fn select_url(row: UrlSelectionRow) -> Result<String> {
        let artifact = IpxeTemplateArtifact {
            name: "artifact".to_string(),
            url: "http://remote/url".to_string(),
            sha: None,
            auth_type: None,
            auth_token: None,
            cache_strategy: row.cache_strategy,
            cached_url: row.cached_url.map(str::to_string),
        };
        resolve_artifact_url(&artifact)
    }

    #[test]
    fn resolve_artifact_url_selects_per_cache_strategy() {
        use IpxeTemplateArtifactCacheStrategy::*;

        check_cases(
            [
                Case {
                    scenario: "CacheAsNeeded prefers the cached URL",
                    input: UrlSelectionRow {
                        cache_strategy: CacheAsNeeded,
                        cached_url: Some("http://cache/url"),
                    },
                    expect: Yields("http://cache/url".to_string()),
                },
                Case {
                    scenario: "CacheAsNeeded falls back to the remote URL",
                    input: UrlSelectionRow {
                        cache_strategy: CacheAsNeeded,
                        cached_url: None,
                    },
                    expect: Yields("http://remote/url".to_string()),
                },
                Case {
                    scenario: "RemoteOnly always uses the remote URL, ignoring the cache",
                    input: UrlSelectionRow {
                        cache_strategy: RemoteOnly,
                        cached_url: Some("http://cache/url"),
                    },
                    expect: Yields("http://remote/url".to_string()),
                },
                Case {
                    scenario: "RemoteOnly uses the remote URL when uncached",
                    input: UrlSelectionRow {
                        cache_strategy: RemoteOnly,
                        cached_url: None,
                    },
                    expect: Yields("http://remote/url".to_string()),
                },
                Case {
                    scenario: "LocalOnly uses artifact.url directly, ignoring cached_url",
                    input: UrlSelectionRow {
                        cache_strategy: LocalOnly,
                        cached_url: Some("http://cache/url"),
                    },
                    expect: Yields("http://remote/url".to_string()),
                },
                Case {
                    scenario: "LocalOnly uses artifact.url when uncached",
                    input: UrlSelectionRow {
                        cache_strategy: LocalOnly,
                        cached_url: None,
                    },
                    expect: Yields("http://remote/url".to_string()),
                },
                Case {
                    scenario: "CachedOnly uses the cached URL when present",
                    input: UrlSelectionRow {
                        cache_strategy: CachedOnly,
                        cached_url: Some("http://cache/url"),
                    },
                    expect: Yields("http://cache/url".to_string()),
                },
                Case {
                    scenario: "CachedOnly fails when no cached URL is available",
                    input: UrlSelectionRow {
                        cache_strategy: CachedOnly,
                        cached_url: None,
                    },
                    expect: FailsWith("CachedOnlyNotCached".to_string()),
                },
            ],
            |row| select_url(row).map_err(error_variant),
        );
    }

    /// A stable name for an [`IpxeScriptError`] variant, so error-matrix rows can
    /// assert the exact failure with [`FailsWith`] without the error type needing
    /// `PartialEq`.
    fn error_variant(error: IpxeScriptError) -> String {
        let name = match error {
            IpxeScriptError::TemplateNotFound(_) => "TemplateNotFound",
            IpxeScriptError::ReservedParameterFound(_) => "ReservedParameterFound",
            IpxeScriptError::RequiredParameterMissing(_) => "RequiredParameterMissing",
            IpxeScriptError::ExtraParametersNotSupported => "ExtraParametersNotSupported",
            IpxeScriptError::HashMismatch { .. } => "HashMismatch",
            IpxeScriptError::ArtifactNotFound(_) => "ArtifactNotFound",
            IpxeScriptError::MissingReservedParameter(_) => "MissingReservedParameter",
            IpxeScriptError::UnexpectedReservedParameter(_) => "UnexpectedReservedParameter",
            IpxeScriptError::UnreplacedPlaceholders(_) => "UnreplacedPlaceholders",
            IpxeScriptError::CachedOnlyNotCached(_) => "CachedOnlyNotCached",
        };
        name.to_string()
    }

    /// Build a single CacheAsNeeded artifact, used by error-matrix rows that need
    /// the Ubuntu template's required artifacts to be present (or deliberately
    /// absent).
    fn artifact(name: &str) -> IpxeTemplateArtifact {
        IpxeTemplateArtifact {
            name: name.to_string(),
            url: format!("http://example.com/{name}"),
            sha: None,
            auth_type: None,
            auth_token: None,
            cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
            cached_url: None,
        }
    }

    #[test]
    fn validate_rejects_each_invalid_definition() {
        let renderer = DefaultIpxeScriptRenderer::new();

        // qcow template: requires the `image_url` parameter, reserves `base_url`.
        let qcow = "ea756ddd-add3-5e42-a202-44bfc2d5aac2";
        // Ubuntu autoinstall template: requires install_iso, kernel, initrd artifacts.
        let ubuntu = "a7850943-e3cd-5e9a-93ca-9e12f52939cc";

        // A row hashes itself before running so only the failure under test fires
        // (an unset hash would otherwise trip HashMismatch first); the HashMismatch
        // row deliberately skips that step.
        let run = |mut ipxeos: IpxeScript| {
            if ipxeos.hash != "deliberately-wrong" {
                ipxeos.hash = renderer.hash(&ipxeos);
            }
            renderer.validate(&ipxeos).map_err(error_variant)
        };

        check_cases(
            [
                Case {
                    scenario: "globally reserved 'extra' parameter name",
                    input: IpxeScript {
                        parameters: vec![
                            IpxeTemplateParameter {
                                name: "image_url".to_string(),
                                value: "http://example.com/image.qcow2".to_string(),
                            },
                            IpxeTemplateParameter {
                                name: "extra".to_string(),
                                value: "value".to_string(),
                            },
                        ],
                        ..base_script(qcow)
                    },
                    expect: FailsWith("ReservedParameterFound".to_string()),
                },
                Case {
                    scenario: "template-reserved 'base_url' parameter name",
                    input: IpxeScript {
                        parameters: vec![
                            IpxeTemplateParameter {
                                name: "image_url".to_string(),
                                value: "http://example.com/image.qcow2".to_string(),
                            },
                            IpxeTemplateParameter {
                                name: "base_url".to_string(),
                                value: "http://bad".to_string(),
                            },
                        ],
                        ..base_script(qcow)
                    },
                    expect: FailsWith("ReservedParameterFound".to_string()),
                },
                Case {
                    scenario: "required 'image_url' parameter missing",
                    input: IpxeScript {
                        parameters: vec![],
                        ..base_script(qcow)
                    },
                    expect: FailsWith("RequiredParameterMissing".to_string()),
                },
                Case {
                    scenario: "required 'initrd' artifact missing",
                    input: IpxeScript {
                        parameters: vec![],
                        artifacts: vec![artifact("install_iso"), artifact("kernel")],
                        ..base_script(ubuntu)
                    },
                    expect: FailsWith("ArtifactNotFound".to_string()),
                },
                Case {
                    scenario: "hash does not match the definition",
                    input: IpxeScript {
                        hash: "deliberately-wrong".to_string(),
                        parameters: vec![IpxeTemplateParameter {
                            name: "image_url".to_string(),
                            value: "http://example.com/image.qcow2".to_string(),
                        }],
                        ..base_script(qcow)
                    },
                    expect: FailsWith("HashMismatch".to_string()),
                },
                Case {
                    scenario: "well-formed qcow definition validates",
                    input: IpxeScript {
                        parameters: vec![IpxeTemplateParameter {
                            name: "image_url".to_string(),
                            value: "http://example.com/image.qcow2".to_string(),
                        }],
                        ..base_script(qcow)
                    },
                    expect: Yields(()),
                },
            ],
            run,
        );
    }

    /// A minimal script scaffold for the given template, with no parameters or
    /// artifacts; error-matrix rows fill in only the fields their case exercises.
    fn base_script(template_id: &str) -> IpxeScript {
        IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: String::new(),
            tenant_id: None,
            ipxe_template_id: template_id.to_string(),
            parameters: vec![],
            artifacts: vec![],
        }
    }

    #[test]
    fn test_hash_computation() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Compute hash
        let hash = renderer.hash(&ipxeos);
        ipxeos.hash = hash.clone();

        // Validation should pass
        assert!(renderer.validate(&ipxeos).is_ok());

        // Modify a parameter
        ipxeos.parameters[0].value = "http://example.com/different.qcow2".to_string();

        // Validation should fail due to hash mismatch
        assert!(matches!(
            renderer.validate(&ipxeos),
            Err(IpxeScriptError::HashMismatch { .. })
        ));
    }

    #[test]
    fn test_render_qcow_template() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Update hash
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        assert!(script.contains("http://pxe.local"));
        assert!(script.contains("ttyS0,115200"));
        assert!(script.contains("http://example.com/image.qcow2"));
    }

    #[test]
    fn test_render_with_extra_params() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Add extra parameters
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "image_sha".to_string(),
            value: "sha256:abc123".to_string(),
        });
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "rootfs_uuid".to_string(),
            value: "12345678".to_string(),
        });

        // Update hash
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        assert!(script.contains("image_sha=sha256:abc123"));
        assert!(script.contains("rootfs_uuid=12345678"));
    }

    #[test]
    fn test_render_ubuntu_autoinstall() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = IpxeScript {
            name: "Ubuntu 22.04".to_string(),
            description: Some("Ubuntu autoinstall".to_string()),
            hash: "placeholder".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![],
            artifacts: vec![
                IpxeTemplateArtifact {
                    name: "install_iso".to_string(),
                    url: "http://releases.ubuntu.com/22.04/ubuntu-22.04-live-server-amd64.iso"
                        .to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
                IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "http://archive.ubuntu.com/ubuntu/dists/jammy/main/installer-amd64/current/legacy-images/netboot/ubuntu-installer/amd64/linux".to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
                IpxeTemplateArtifact {
                    name: "initrd".to_string(),
                    url: "http://archive.ubuntu.com/ubuntu/dists/jammy/main/installer-amd64/current/legacy-images/netboot/ubuntu-installer/amd64/initrd.gz".to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
            ],
        };

        // Update hash
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        assert!(script.contains("kernel http://archive.ubuntu.com"));
        assert!(script.contains("initrd http://archive.ubuntu.com"));
        assert!(script.contains("url=http://releases.ubuntu.com"));
    }

    #[test]
    fn test_render_with_artifacts() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = IpxeScript {
            name: "Ubuntu with artifacts".to_string(),
            description: Some("Ubuntu with cached artifacts".to_string()),
            hash: "placeholder".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![],
            artifacts: vec![
                IpxeTemplateArtifact {
                    name: "install_iso".to_string(),
                    url: "http://releases.ubuntu.com/22.04/ubuntu-22.04-live-server-amd64.iso"
                        .to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
                IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "http://archive.ubuntu.com/ubuntu/dists/jammy/main/installer-amd64/current/legacy-images/netboot/ubuntu-installer/amd64/linux".to_string(),
                    sha: Some("sha256:abc123".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: Some("http://pxe.local/artifacts/kernel-abc123".to_string()),
                },
                IpxeTemplateArtifact {
                    name: "initrd".to_string(),
                    url: "http://archive.ubuntu.com/ubuntu/dists/jammy/main/installer-amd64/current/legacy-images/netboot/ubuntu-installer/amd64/initrd.gz".to_string(),
                    sha: Some("sha256:def456".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: Some("http://pxe.local/artifacts/initrd-def456".to_string()),
                },
            ],
        };

        // Update hash
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        // Should use local cached URLs instead of remote URLs
        assert!(script.contains("kernel http://pxe.local/artifacts/kernel-abc123"));
        assert!(script.contains("initrd http://pxe.local/artifacts/initrd-def456"));
    }

    #[test]
    fn test_list_templates() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let templates = renderer.list_templates();

        assert!(templates.contains(&"qcow-image".to_string()));
        assert!(templates.contains(&"ubuntu-autoinstall".to_string()));
        assert!(templates.contains(&"dgx-os".to_string()));
        assert!(templates.contains(&"discovery-scout-aarch64-dpu".to_string()));
        assert!(templates.contains(&"discovery-scout-aarch64".to_string()));
        assert!(templates.contains(&"discovery-scout-x86_64".to_string()));
        assert!(templates.contains(&"error-instructions".to_string()));
        assert!(templates.contains(&"exit-instructions".to_string()));
        assert!(templates.contains(&"unknown-host".to_string()));
        assert!(templates.contains(&"whoami".to_string()));
        assert!(templates.contains(&"carbide-menu-static-ipxe".to_string()));
        // Only assert minimum count (templates referenced in tests); new entries in templates.yaml are allowed
        assert!(templates.len() >= 11);
    }

    /// The host discovery templates must hand Scout its cloud-init datasource,
    /// and the DPU template must not.
    ///
    /// This is the templated twin of the kernel command line that
    /// `PxeInstructions::get_pxe_instruction_for_arch` builds by hand, and the
    /// two have to agree: a host that boots through this path without
    /// `ds=nocloud;s=` comes up with no datasource, so the site's discovery
    /// snippets are silently never applied. Nothing about that failure is
    /// visible from the outside, which is why it is pinned here. The DPU is
    /// excluded deliberately -- it fetches a BlueField kickstart via `bfks=`
    /// from its own route prefix and has no NoCloud datasource at all.
    #[test]
    fn discovery_templates_carry_the_right_cloud_init_datasource() {
        const SCOUT_DATASOURCE: &str = "ds=nocloud;s=${scout-cloudinit-url}";

        let renderer = DefaultIpxeScriptRenderer::new();

        for name in ["discovery-scout-aarch64", "discovery-scout-x86_64"] {
            let template = renderer
                .get_template_by_name(name)
                .unwrap_or_else(|| panic!("{name} should exist"));
            assert!(
                template.template.contains(SCOUT_DATASOURCE),
                "{name} must give Scout its cloud-init datasource, or discovery \
                 snippets silently never apply",
            );
        }

        let dpu = renderer
            .get_template_by_name("discovery-scout-aarch64-dpu")
            .expect("discovery-scout-aarch64-dpu should exist");
        assert!(
            !dpu.template.contains("ds=nocloud"),
            "the DPU boots a BlueField kickstart, not a NoCloud datasource",
        );
        assert!(
            dpu.template.contains("bfks=${dpu-cloudinit-url}/user-data"),
            "the DPU kickstart URL must come from the boot script's variable",
        );
        assert!(
            !dpu.required_params.iter().any(|p| p == "cloudinit_url"),
            "a stored kickstart URL goes stale when the route prefixes change; \
             the boot script supplies it instead",
        );
    }

    #[test]
    fn test_static_ipxe_menu_uses_nico_branding() {
        const BRANDING_H: &str = include_str!("../../../pxe/ipxe/local/branding.h");

        let renderer = DefaultIpxeScriptRenderer::new();
        let menu_template = renderer
            .get_template_by_id(STATIC_IPXE_MENU_TEMPLATE_ID)
            .expect("static iPXE menu template should exist");

        assert_eq!(menu_template.template, STATIC_IPXE_MENU_TEMPLATE);

        for (name, contents) in [
            ("embedded iPXE script", STATIC_IPXE_MENU_TEMPLATE),
            ("iPXE branding header", BRANDING_H),
            (
                "renderer static menu description",
                menu_template.description.as_str(),
            ),
        ] {
            assert!(contents.contains("NICo"), "{name} should mention NICo");
        }

        for (name, contents) in [
            ("embedded iPXE script", STATIC_IPXE_MENU_TEMPLATE),
            ("iPXE branding header", BRANDING_H),
            (
                "renderer static menu description",
                menu_template.description.as_str(),
            ),
        ] {
            assert!(
                !contents.contains("Carbide") && !contents.contains("carbide"),
                "{name} should not mention Carbide"
            );
            assert!(
                !contents.contains("Forge") && !contents.contains("forge"),
                "{name} should not mention Forge"
            );
        }
    }

    #[test]
    fn static_ipxe_menu_uses_the_dhcp_pxe_http_endpoint() {
        for path in ["whoami", "boot?buildarch=${buildarch}"] {
            assert!(
                STATIC_IPXE_MENU_TEMPLATE
                    .contains(&format!("http://${{next-server}}/api/v0/pxe/{path}")),
                "static iPXE menu should use next-server over the standard HTTP port for {path}"
            );
        }

        assert!(
            !STATIC_IPXE_MENU_TEMPLATE.contains("${next-server}:8080"),
            "static iPXE menu should not use the PXE container's internal port"
        );
    }

    #[test]
    fn static_ipxe_menu_retries_only_marked_boot_scripts() {
        assert!(STATIC_IPXE_MENU_TEMPLATE.contains(
            "imgfree nico-boot ||\n\
             set nico-retry-provisioning 0\n\
             time chain --name nico-boot http://${next-server}/api/v0/pxe/boot?buildarch=${buildarch}&platform=${platform}&manufacturer=${manufacturer}&product=${product}&serial=${serial} && exit 1 || goto retry_nico_boot"
        ));
        assert!(STATIC_IPXE_MENU_TEMPLATE.contains(
            ":retry_nico_boot\n\
             iseq ${nico-retry-provisioning} 1 || goto error_handler\n\
             imgstat nico-boot || goto error_handler\n\
             prompt --key p --timeout 30000 Hit the ${bold}p${boldoff} key to open NICo menu; retrying in 30 seconds... && goto nico_menu ||\n\
             time imgexec nico-boot && exit 1 || goto retry_nico_boot"
        ));
        assert_eq!(
            STATIC_IPXE_MENU_TEMPLATE
                .matches("/api/v0/pxe/boot?")
                .count(),
            1,
            "retrying should reuse the served script instead of requesting it again"
        );
    }

    #[test]
    fn test_get_template_by_name() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let template = renderer.get_template_by_name("qcow-image");
        assert!(template.is_some());
        assert_eq!(template.unwrap().name, "qcow-image");

        let missing = renderer.get_template_by_name("nonexistent");
        assert!(missing.is_none());
    }

    #[test]
    fn test_get_template_by_id() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let template = renderer.get_template_by_id("ea756ddd-add3-5e42-a202-44bfc2d5aac2");
        assert!(template.is_some());
        assert_eq!(template.unwrap().name, "qcow-image");

        let missing = renderer.get_template_by_id("00000000-0000-0000-0000-000000000000");
        assert!(missing.is_none());
    }

    #[test]
    fn test_missing_reserved_parameter() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();
        ipxeos.hash = renderer.hash(&ipxeos);

        // Template requires base_url and console, but we only provide base_url
        let reserved_params = vec![IpxeTemplateParameter {
            name: "base_url".to_string(),
            value: "http://pxe.local".to_string(),
        }];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(matches!(
            result,
            Err(IpxeScriptError::MissingReservedParameter(_))
        ));
    }

    #[test]
    fn test_unexpected_reserved_parameter() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();
        ipxeos.hash = renderer.hash(&ipxeos);

        // Provide extra reserved parameter not in template
        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
            IpxeTemplateParameter {
                name: "extra_reserved".to_string(),
                value: "value".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(matches!(
            result,
            Err(IpxeScriptError::UnexpectedReservedParameter(_))
        ));
    }

    #[test]
    fn test_unreplaced_placeholders() {
        let renderer = DefaultIpxeScriptRenderer::new();

        // render() never reaches the placeholder check -- required-parameter validation
        // rejects the script first, which this test's own comment used to admit while
        // asserting nothing more than "some error came back". Pin that rejection for real...
        let mut ipxeos = create_test_ipxeos();
        ipxeos.parameters.clear();
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        assert!(matches!(
            renderer.render(&ipxeos, &reserved_params),
            Err(IpxeScriptError::RequiredParameterMissing(_))
        ));

        // ...and then reach `check_unreplaced_placeholders` directly, because nothing else
        // in the suite does. Without this, `UnreplacedPlaceholders` is a variant we build
        // and never assert.
        let err = renderer
            .check_unreplaced_placeholders("#!ipxe\nkernel {{image_url}} initrd={{initrd}}\n")
            .expect_err("leftover {{...}} placeholders should be rejected");
        assert!(matches!(
            err,
            IpxeScriptError::UnreplacedPlaceholders(ref found)
                if found == "{{image_url}}, {{initrd}}"
        ));

        renderer
            .check_unreplaced_placeholders("#!ipxe\nkernel http://pxe.local/vmlinuz\n")
            .expect("a fully substituted script has nothing left to flag");
    }

    #[test]
    fn test_extra_parameter_name_reserved() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Try to use "extra" as a parameter name (should be rejected)
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "extra".to_string(),
            value: "some_value".to_string(),
        });
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(matches!(
            result,
            Err(IpxeScriptError::ReservedParameterFound(_))
        ));

        // Verify error message mentions "extra"
        if let Err(IpxeScriptError::ReservedParameterFound(msg)) = result {
            assert!(msg.contains("extra"));
        }
    }

    #[test]
    fn test_extra_artifact_name_reserved() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Try to use "extra" as an artifact name (should be rejected)
        ipxeos.artifacts.push(IpxeTemplateArtifact {
            name: "extra".to_string(),
            url: "http://example.com/extra".to_string(),
            sha: None,
            auth_type: None,
            auth_token: None,
            cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
            cached_url: None,
        });
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(matches!(
            result,
            Err(IpxeScriptError::ReservedParameterFound(_))
        ));

        // Verify error message mentions "extra"
        if let Err(IpxeScriptError::ReservedParameterFound(msg)) = result {
            assert!(msg.contains("extra"));
        }
    }

    #[test]
    fn test_extra_case_insensitive_rejected() {
        // All case variations of "extra" should be rejected (case-insensitive)
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // "Extra" (capitalized) should also be rejected (case-insensitive)
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "Extra".to_string(),
            value: "not_allowed".to_string(),
        });
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(matches!(
            result,
            Err(IpxeScriptError::ReservedParameterFound(_))
        ));

        // Verify error message mentions the parameter
        if let Err(IpxeScriptError::ReservedParameterFound(msg)) = result {
            assert!(msg.contains("Extra") || msg.contains("extra"));
        }
    }

    #[test]
    fn test_case_preserved_in_extra() {
        // Original case should be preserved in {{extra}} for non-reserved parameters
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Add parameters with mixed case (not "extra")
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "MyCustomParam".to_string(),
            value: "value1".to_string(),
        });
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "AnotherParam".to_string(),
            value: "value2".to_string(),
        });
        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        // Original case should be preserved in {{extra}}
        assert!(script.contains("MyCustomParam=value1"));
        assert!(script.contains("AnotherParam=value2"));
        // Lowercase versions should NOT appear
        assert!(!script.contains("mycustomparam="));
        assert!(!script.contains("anotherparam="));
    }

    #[test]
    fn test_case_insensitive_parameter_matching() {
        // Parameter names should match case-insensitively
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "IMAGE_URL".to_string(), // Uppercase
                value: "http://example.com/image.qcow2".to_string(),
            }],
            artifacts: vec![],
        };

        let mut ipxeos_with_hash = ipxeos.clone();
        ipxeos_with_hash.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "BASE_URL".to_string(), // Uppercase
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "CONSOLE".to_string(), // Uppercase
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos_with_hash, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        // Should match case-insensitively and render correctly
        assert!(script.contains("http://pxe.local"));
        assert!(script.contains("ttyS0,115200"));
        assert!(script.contains("http://example.com/image.qcow2"));
    }

    #[test]
    fn test_case_insensitive_hash_equivalence() {
        // Different cases of same parameter name should produce same hash
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos1 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "image_url".to_string(), // lowercase
                value: "http://example.com/image.qcow2".to_string(),
            }],
            artifacts: vec![],
        };

        let ipxeos2 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "IMAGE_URL".to_string(), // UPPERCASE
                value: "http://example.com/image.qcow2".to_string(),
            }],
            artifacts: vec![],
        };

        let hash1 = renderer.hash(&ipxeos1);
        let hash2 = renderer.hash(&ipxeos2);

        // Hashes should match (case-insensitive)
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_parameter_and_artifact_same_name_in_required() {
        // If required_params has "foo" and we have both parameter and artifact named "foo",
        // parameter should be consumed first (artifacts only consumed by required_artifacts)
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "image_url".to_string(),
                value: "param_value".to_string(),
            }],
            artifacts: vec![IpxeTemplateArtifact {
                name: "image_url".to_string(), // Same name as parameter
                url: "artifact_url".to_string(),
                sha: None,
                auth_type: None,
                auth_token: None,
                cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                cached_url: None,
            }],
        };

        let mut ipxeos_with_hash = ipxeos.clone();
        ipxeos_with_hash.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos_with_hash, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        // Parameter value should be used (not artifact URL) since required_params consumes parameters
        assert!(script.contains("image_url=param_value"));
        assert!(!script.contains("artifact_url"));
    }

    #[test]
    fn test_duplicate_parameters_occurrence_based() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Add duplicate parameter - first consumed by required_params, second goes to {{extra}}
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "image_url".to_string(),
            value: "http://example.com/duplicate.qcow2".to_string(),
        });

        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        // First occurrence consumed by {{image_url}} placeholder
        assert!(script.contains("image_url=http://example.com/image.qcow2"));
        // Second occurrence goes to {{extra}}
        assert!(script.contains("image_url=http://example.com/duplicate.qcow2"));
    }

    #[test]
    fn test_duplicate_parameters_in_hash() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos1 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "image_url".to_string(),
                value: "http://example.com/image.qcow2".to_string(),
            }],
            artifacts: vec![],
        };

        let mut ipxeos2 = ipxeos1.clone();
        ipxeos2.parameters.push(IpxeTemplateParameter {
            name: "image_url".to_string(),
            value: "http://example.com/duplicate.qcow2".to_string(),
        });

        let hash1 = renderer.hash(&ipxeos1);
        let hash2 = renderer.hash(&ipxeos2);

        // Hashes should be different (duplicates affect hash)
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_parameter_order_determinism() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos1 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![
                IpxeTemplateParameter {
                    name: "image_url".to_string(),
                    value: "http://example.com/image.qcow2".to_string(),
                },
                IpxeTemplateParameter {
                    name: "extra1".to_string(),
                    value: "value1".to_string(),
                },
                IpxeTemplateParameter {
                    name: "extra2".to_string(),
                    value: "value2".to_string(),
                },
            ],
            artifacts: vec![],
        };

        let ipxeos2 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
            parameters: vec![
                IpxeTemplateParameter {
                    name: "extra2".to_string(),
                    value: "value2".to_string(),
                },
                IpxeTemplateParameter {
                    name: "image_url".to_string(),
                    value: "http://example.com/image.qcow2".to_string(),
                },
                IpxeTemplateParameter {
                    name: "extra1".to_string(),
                    value: "value1".to_string(),
                },
            ],
            artifacts: vec![],
        };

        let hash1 = renderer.hash(&ipxeos1);
        let hash2 = renderer.hash(&ipxeos2);

        // Hashes should be same (order-independent)
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_artifact_order_determinism() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos1 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "install_iso".to_string(),
                value: "http://example.com/ubuntu.iso".to_string(),
            }],
            artifacts: vec![
                IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "http://example.com/kernel".to_string(),
                    sha: Some("sha256:kernel123".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
                IpxeTemplateArtifact {
                    name: "initrd".to_string(),
                    url: "http://example.com/initrd".to_string(),
                    sha: Some("sha256:initrd456".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
            ],
        };

        let ipxeos2 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "install_iso".to_string(),
                value: "http://example.com/ubuntu.iso".to_string(),
            }],
            artifacts: vec![
                IpxeTemplateArtifact {
                    name: "initrd".to_string(),
                    url: "http://example.com/initrd".to_string(),
                    sha: Some("sha256:initrd456".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
                IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "http://example.com/kernel".to_string(),
                    sha: Some("sha256:kernel123".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
            ],
        };

        let hash1 = renderer.hash(&ipxeos1);
        let hash2 = renderer.hash(&ipxeos2);

        // Hashes should be same (artifact order independent)
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_excludes_cache_strategy() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos1 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "install_iso".to_string(),
                value: "http://example.com/ubuntu.iso".to_string(),
            }],
            artifacts: vec![IpxeTemplateArtifact {
                name: "kernel".to_string(),
                url: "http://example.com/kernel".to_string(),
                sha: Some("sha256:kernel123".to_string()),
                auth_type: None,
                auth_token: None,
                cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                cached_url: None,
            }],
        };

        let mut ipxeos2 = ipxeos1.clone();
        ipxeos2.artifacts[0].cache_strategy = IpxeTemplateArtifactCacheStrategy::RemoteOnly;

        let hash1 = renderer.hash(&ipxeos1);
        let hash2 = renderer.hash(&ipxeos2);

        // Hashes should be same (cache_strategy excluded)
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_excludes_cached_url() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos1 = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "install_iso".to_string(),
                value: "http://example.com/ubuntu.iso".to_string(),
            }],
            artifacts: vec![IpxeTemplateArtifact {
                name: "kernel".to_string(),
                url: "http://example.com/kernel".to_string(),
                sha: Some("sha256:kernel123".to_string()),
                auth_type: None,
                auth_token: None,
                cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                cached_url: None,
            }],
        };

        let mut ipxeos2 = ipxeos1.clone();
        ipxeos2.artifacts[0].cached_url = Some("http://local-cache/kernel".to_string());

        let hash1 = renderer.hash(&ipxeos1);
        let hash2 = renderer.hash(&ipxeos2);

        // Hashes should be same (cached_url excluded)
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_double_space_cleanup() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Add empty optional parameters that would create double spaces
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "empty1".to_string(),
            value: "".to_string(),
        });
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "opt1".to_string(),
            value: "value1".to_string(),
        });

        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        // Verify no double spaces exist
        assert!(!script.contains("  "));
    }

    #[test]
    fn test_empty_optional_parameters_not_in_extra() {
        let renderer = DefaultIpxeScriptRenderer::new();
        let mut ipxeos = create_test_ipxeos();

        // Add empty optional parameter
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "empty_opt".to_string(),
            value: "".to_string(),
        });
        // Add non-empty optional parameter
        ipxeos.parameters.push(IpxeTemplateParameter {
            name: "valid_opt".to_string(),
            value: "value".to_string(),
        });

        ipxeos.hash = renderer.hash(&ipxeos);

        let reserved_params = vec![
            IpxeTemplateParameter {
                name: "base_url".to_string(),
                value: "http://pxe.local".to_string(),
            },
            IpxeTemplateParameter {
                name: "console".to_string(),
                value: "ttyS0,115200".to_string(),
            },
        ];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok());

        let script = result.unwrap();
        // Empty parameter should not appear
        assert!(!script.contains("empty_opt"));
        // Non-empty parameter should appear
        assert!(script.contains("valid_opt=value"));
    }

    #[test]
    fn test_fabricate_cached_urls_comprehensive() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "test-hash".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "install_iso".to_string(),
                value: "http://example.com/ubuntu.iso".to_string(),
            }],
            artifacts: vec![
                // Artifact with SHA - should generate hash of SHA
                IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "http://example.com/kernel".to_string(),
                    sha: Some("sha256:test123".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
                // Artifact without SHA - should generate hash of artifact record
                IpxeTemplateArtifact {
                    name: "initrd".to_string(),
                    url: "http://example.com/initrd".to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
                // RemoteOnly - should skip
                IpxeTemplateArtifact {
                    name: "remote".to_string(),
                    url: "http://example.com/remote".to_string(),
                    sha: Some("sha256:remote".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::RemoteOnly,
                    cached_url: None,
                },
                // Has iPXE variable - should skip
                IpxeTemplateArtifact {
                    name: "local".to_string(),
                    url: "${base-url}/local.img".to_string(),
                    sha: Some("sha256:local".to_string()),
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    cached_url: None,
                },
            ],
        };

        let result = renderer.fabricate_cached_urls(&ipxeos);

        // First artifact should have cached_url using sha field directly
        assert!(result.artifacts[0].cached_url.is_some());
        let url1 = result.artifacts[0].cached_url.as_ref().unwrap();
        assert_eq!(url1, "${base_url}/sha256:test123");

        // Second artifact (no sha) should have cached_url with generated 64-char hash
        assert!(result.artifacts[1].cached_url.is_some());
        let url2 = result.artifacts[1].cached_url.as_ref().unwrap();
        assert!(url2.starts_with("${base_url}/"));
        let hash2 = url2.strip_prefix("${base_url}/").unwrap();
        assert_eq!(hash2.len(), 64); // Generated SHA256 hex
        assert_ne!(url1, url2); // Different URLs

        // Third artifact (RemoteOnly) should not have cached_url
        assert!(result.artifacts[2].cached_url.is_none());

        // Fourth artifact (has variable) is still eligible for cached_url (no variable check)
        assert!(result.artifacts[3].cached_url.is_some());
        let url4 = result.artifacts[3].cached_url.as_ref().unwrap();
        assert_eq!(url4, "${base_url}/sha256:local");
    }

    #[test]
    fn test_fabricate_cached_urls_deterministic() {
        let renderer = DefaultIpxeScriptRenderer::new();

        let ipxeos = IpxeScript {
            name: "Test".to_string(),
            description: None,
            hash: "test-hash".to_string(),
            tenant_id: None,
            ipxe_template_id: "a7850943-e3cd-5e9a-93ca-9e12f52939cc".to_string(),
            parameters: vec![IpxeTemplateParameter {
                name: "install_iso".to_string(),
                value: "http://example.com/ubuntu.iso".to_string(),
            }],
            artifacts: vec![IpxeTemplateArtifact {
                name: "kernel".to_string(),
                url: "http://example.com/kernel".to_string(),
                sha: Some("sha256:test123".to_string()),
                auth_type: None,
                auth_token: None,
                cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                cached_url: None,
            }],
        };

        let result1 = renderer.fabricate_cached_urls(&ipxeos);
        let result2 = renderer.fabricate_cached_urls(&ipxeos);

        // Should generate same URL both times
        assert_eq!(
            result1.artifacts[0].cached_url,
            result2.artifacts[0].cached_url
        );
    }

    #[test]
    fn test_render_whoami_static_template() {
        let renderer = DefaultIpxeScriptRenderer::new();

        // Get the whoami template
        let template = renderer
            .get_template_by_name("whoami")
            .expect("whoami template should exist");

        // whoami template is static - no parameters, no artifacts, no placeholders
        let mut ipxeos = IpxeScript {
            name: "WhoAmI".to_string(),
            description: Some("Static whoami script".to_string()),
            hash: String::new(),
            tenant_id: None,
            ipxe_template_id: "9bfffafc-4ffa-53b8-b9c4-81d1b01e7257".to_string(),
            parameters: vec![],
            artifacts: vec![],
        };

        // Compute and set the correct hash for validation
        ipxeos.hash = renderer.hash(&ipxeos);

        // No reserved params needed
        let reserved_params = vec![];

        let result = renderer.render(&ipxeos, &reserved_params);
        assert!(result.is_ok(), "Render failed: {:?}", result.err());

        let rendered_script = result.unwrap();

        // Compute checksum of rendered script
        let mut hasher_rendered = Sha256::new();
        hasher_rendered.update(rendered_script.as_bytes());
        let rendered_hash = hex::encode(hasher_rendered.finalize());

        // Compute checksum of template text (normalized - trailing spaces removed per line)
        let normalized_template = template
            .template
            .lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        let mut hasher_template = Sha256::new();
        hasher_template.update(normalized_template.as_bytes());
        let template_hash = hex::encode(hasher_template.finalize());

        // Checksums should match - template rendered as-is with no alterations
        assert_eq!(
            rendered_hash, template_hash,
            "Rendered script should match template exactly (static template with no placeholders)"
        );
    }
}
