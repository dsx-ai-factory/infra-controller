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

//! View-model builder for the admin-UI Configuration page.
//!
//! The page is generated rather than hand-written so it cannot drift from the
//! actual config surface. Three inputs are joined per option:
//!
//! - the configuration reference (`carbide_api_core::cfg::CONFIG_REFERENCE_MD`,
//!   i.e. `cfg/README.md`) supplies the catalog of documented options with
//!   their type, default, and description, organized into per-struct sections;
//! - the redacted effective `CarbideConfig`, serialized to JSON, supplies each
//!   option's current value (a unit test keeps the reference's coverage of
//!   the config struct complete);
//! - `CarbideConfig::explicit_value_paths` supplies provenance: which dotted
//!   keys were explicitly set by a config file or `CARBIDE_API_*` environment
//!   variable, and by which source.
//!
//! The three inputs use different key spellings for the same option (Rust
//! field names in the reference, serde-serialized names in the JSON, TOML
//! keys in the provenance map — e.g. `mlxconfig_profiles` serializes as
//! `mlx-config-profiles`). All joins therefore go through [`canonical`],
//! which strips everything but alphanumerics per path segment.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

/// Everything the Configuration page template needs.
pub(super) struct ConfigPageView {
    pub(super) groups: Vec<ConfigGroupView>,
}

pub(super) struct ConfigGroupView {
    pub(super) title: &'static str,
    /// Stable identifier used for the tab's `data-tab` / `id` attributes.
    pub(super) slug: &'static str,
    pub(super) sections: Vec<ConfigSectionView>,
}

pub(super) struct ConfigSectionView {
    pub(super) title: String,
    /// Dotted TOML path prefix of this section ("" for top-level options).
    pub(super) path: String,
    pub(super) rows: Vec<ConfigRowView>,
}

pub(super) struct ConfigRowView {
    pub(super) name: String,
    /// Full dotted path, also used by the client-side filter.
    pub(super) path: String,
    pub(super) ty: String,
    pub(super) value: CellValue,
    pub(super) overridden: bool,
    /// Source label ("nico-api-config.toml", env, ...) when overridden.
    pub(super) source: String,
    pub(super) default: CellValue,
    /// Rendered by [`markdown_lite`]; the template inserts it with `|safe`.
    pub(super) description_html: String,
    /// The value shown is the live, runtime-adjustable value.
    pub(super) runtime: bool,
    /// Lowercased haystack for the client-side filter: name, path, value,
    /// source, and description — deliberately not the type or the panel's
    /// "Type/Default/Path" labels, which would match on every row.
    pub(super) search: String,
}

/// A value or default cell. Rendering (and HTML escaping) is centralized in
/// [`CellValue::to_html`] — the template inserts its output with `|safe`.
pub(super) enum CellValue {
    /// The option resolves to null/absent.
    Unset,
    /// The option is set to an empty string, list, or map.
    Empty,
    /// No default exists (used only in the Default column).
    NoDefault,
    /// The option is mandatory (used only in the Default column).
    Required,
    /// Plain text rendered as inline code; escaped at render time.
    Code(String),
    /// Pretty-printed block rendered behind a "show value" expander;
    /// escaped at render time.
    Pre(String),
    /// Pre-rendered HTML. Must only ever wrap [`markdown_lite`] output,
    /// which is the single escaping site for rich text.
    Rich(String),
}

impl CellValue {
    /// The single place cell HTML is produced.
    pub(super) fn to_html(&self) -> String {
        match self {
            CellValue::Unset => r#"<span class="config-unset">unset</span>"#.to_string(),
            CellValue::Empty => r#"<span class="config-unset">empty</span>"#.to_string(),
            CellValue::NoDefault => r#"<span class="config-unset">none</span>"#.to_string(),
            CellValue::Required => "<em>required</em>".to_string(),
            CellValue::Code(text) => format!("<code>{}</code>", escape_html(text)),
            CellValue::Pre(text) => format!(
                "<details><summary>show value</summary><pre>{}</pre></details>",
                escape_html(text)
            ),
            CellValue::Rich(html) => html.clone(),
        }
    }

    /// Used by the template to dim rows whose option isn't set.
    pub(super) fn is_unset(&self) -> bool {
        matches!(self, CellValue::Unset)
    }

    /// Plain text of the cell for the client-side filter.
    fn search_text(&self) -> &str {
        match self {
            CellValue::Unset => "unset",
            CellValue::Empty => "empty",
            CellValue::NoDefault | CellValue::Required | CellValue::Rich(_) => "",
            CellValue::Code(text) | CellValue::Pre(text) => text,
        }
    }
}

/// Live values of the runtime-adjustable settings, folded into the catalog
/// next to their config options and tagged "runtime". `None` values render
/// as "unset".
pub(super) struct LiveSettings {
    pub(super) log_filter: String,
    pub(super) site_explorer_enabled: String,
    pub(super) create_machines: String,
    pub(super) bmc_proxy: Option<String>,
    pub(super) tracing_enabled: String,
    pub(super) dpu_agent_upgrade_policy: String,
}

/// A dynamic setting joined into the catalog: attached to the config option
/// at `path` when one is documented, otherwise rendered as its own row (with
/// `description`) in that path's group.
struct RuntimeSetting {
    path: &'static str,
    value: Option<String>,
    /// Group slug and description, used only when no documented option
    /// matches `path` and the setting becomes its own row.
    group: &'static str,
    description: &'static str,
}

impl LiveSettings {
    fn into_runtime_settings(self) -> Vec<RuntimeSetting> {
        vec![
            RuntimeSetting {
                path: "log_filter",
                value: Some(self.log_filter),
                group: "integrations",
                description: "Active `RUST_LOG` log filter.",
            },
            RuntimeSetting {
                path: "site_explorer.enabled",
                group: "hardware",
                value: Some(self.site_explorer_enabled),
                description: "Whether site explorer runs periodic hardware explorations.",
            },
            RuntimeSetting {
                path: "site_explorer.create_machines",
                group: "hardware",
                value: Some(self.create_machines),
                description: "Whether site explorer creates machines from discovered endpoints.",
            },
            RuntimeSetting {
                path: "site_explorer.bmc_proxy",
                group: "hardware",
                value: self.bmc_proxy,
                description: "Proxy used for talking to BMCs.",
            },
            RuntimeSetting {
                path: "tracing.enabled",
                value: Some(self.tracing_enabled),
                group: "integrations",
                description: "Whether log tracing is enabled.",
            },
            RuntimeSetting {
                path: "initial_dpu_agent_upgrade_policy",
                value: Some(self.dpu_agent_upgrade_policy),
                group: "machines",
                description: "Active DPU agent upgrade policy.",
            },
        ]
    }
}

/// One `| field | type | default | (group) | description |` row of the
/// reference doc. `group` is present only in the top-level table, where each
/// option declares which admin-UI tab it belongs to.
struct FieldDoc {
    name: String,
    ty: String,
    default: String,
    group: Option<String>,
    description: String,
}

/// Display groups, in page order. Each top-level option declares its group
/// slug in the reference doc's `Group` column; unknown slugs fall through to
/// "Other" (a unit test keeps the column complete and valid).
const GROUP_ORDER: &[(&str, &str)] = &[
    ("Server & API", "server"),
    ("Networking", "networking"),
    ("Machines & Firmware", "machines"),
    ("Security", "security"),
    ("Hardware & Racks", "hardware"),
    ("Integrations & Observability", "integrations"),
    ("Other", "other"),
];

fn group_title(slug: &str) -> &'static str {
    GROUP_ORDER
        .iter()
        .find(|(_, s)| *s == slug)
        .map(|(title, _)| *title)
        .unwrap_or("Other")
}

/// Builds the page view from the reference doc, the redacted effective config
/// (as JSON), the explicitly-set key paths with their source labels, and the
/// live runtime-adjustable values.
pub(super) fn build_config_page(
    reference_md: &str,
    effective: &serde_json::Value,
    explicit_paths: &BTreeMap<String, String>,
    live: LiveSettings,
) -> ConfigPageView {
    let sections = parse_reference(reference_md);
    let builder = CatalogBuilder::new(&sections, effective, explicit_paths);

    // Grouped sections, keyed by group title. Each group lazily gets a
    // leading section for the top-level scalar options assigned to it.
    let mut grouped: HashMap<&'static str, Vec<ConfigSectionView>> = HashMap::new();
    let mut top_level_rows: HashMap<&'static str, Vec<ConfigRowView>> = HashMap::new();

    let top_level = sections
        .iter()
        .find(|(name, _)| name == "NicoConfig")
        .map(|(_, fields)| fields.as_slice())
        .unwrap_or(&[]);

    for field in top_level {
        let group = group_title(field.group.as_deref().unwrap_or(""));
        match builder.nested_section_for(field) {
            Some(section_fields) => {
                let mut nested = Vec::new();
                builder.build_section(
                    humanize(&field.name),
                    field.name.clone(),
                    section_fields,
                    &mut nested,
                    &mut HashSet::new(),
                );
                grouped.entry(group).or_default().extend(nested);
            }
            None => top_level_rows
                .entry(group)
                .or_default()
                .push(builder.build_row(field, &field.name)),
        }
    }

    // Fold live runtime values into their documented rows; settings without a
    // documented option become their own rows in the matching group.
    'settings: for setting in live.into_runtime_settings() {
        let all_rows = top_level_rows
            .values_mut()
            .chain(grouped.values_mut().flatten().map(|s| &mut s.rows));
        for rows in all_rows {
            if let Some(row) = rows.iter_mut().find(|row| row.path == setting.path) {
                row.value = runtime_cell(setting.value);
                row.runtime = true;
                row.search =
                    format!("{} runtime {}", row.search, row.value.search_text()).to_lowercase();
                continue 'settings;
            }
        }
        let value = runtime_cell(setting.value);
        let search = format!(
            "{} runtime {} {}",
            setting.path,
            value.search_text(),
            setting.description
        )
        .to_lowercase();
        top_level_rows
            .entry(group_title(setting.group))
            .or_default()
            .push(ConfigRowView {
                name: setting
                    .path
                    .rsplit('.')
                    .next()
                    .unwrap_or(setting.path)
                    .to_string(),
                path: setting.path.to_string(),
                ty: String::new(),
                value,
                overridden: false,
                source: String::new(),
                default: CellValue::NoDefault,
                description_html: markdown_lite(setting.description),
                runtime: true,
                search,
            });
    }

    let mut groups = Vec::new();
    for (title, slug) in GROUP_ORDER {
        let mut sections = Vec::new();
        if let Some(rows) = top_level_rows.remove(title) {
            sections.push(ConfigSectionView {
                title: "Core Options".to_string(),
                path: String::new(),
                rows,
            });
        }
        sections.extend(grouped.remove(title).unwrap_or_default());
        if !sections.is_empty() {
            groups.push(ConfigGroupView {
                title,
                slug,
                sections,
            });
        }
    }

    ConfigPageView { groups }
}

fn runtime_cell(value: Option<String>) -> CellValue {
    match value {
        Some(value) => CellValue::Code(value),
        None => CellValue::Unset,
    }
}

/// Shared context for assembling catalog sections: the reference sections to
/// recurse into, the effective config values, and the override provenance.
struct CatalogBuilder<'a> {
    sections_by_name: HashMap<String, &'a [FieldDoc]>,
    effective: &'a serde_json::Value,
    /// Explicitly-set paths with canonicalized keys, for spelling-insensitive
    /// exact and prefix matching.
    explicit: BTreeMap<String, &'a str>,
}

impl<'a> CatalogBuilder<'a> {
    fn new(
        sections: &'a [(String, Vec<FieldDoc>)],
        effective: &'a serde_json::Value,
        explicit_paths: &'a BTreeMap<String, String>,
    ) -> Self {
        CatalogBuilder {
            sections_by_name: sections
                .iter()
                .map(|(name, fields)| (name.to_lowercase(), fields.as_slice()))
                .collect(),
            effective,
            explicit: explicit_paths
                .iter()
                .map(|(path, source)| (canonical(path), source.as_str()))
                .collect(),
        }
    }

    /// If the field's type refers to a documented sub-struct section (and
    /// isn't a collection of them), return that section's fields.
    fn nested_section_for(&self, field: &FieldDoc) -> Option<&'a [FieldDoc]> {
        let ty = clean_type(&field.ty);
        // Collections of structs stay leaf rows: their keys are
        // operator-chosen, so there is no fixed set of options to enumerate.
        if ty.contains("HashMap") || ty.contains("Vec<") || ty.contains("nested") {
            return None;
        }
        let inner = ty
            .trim_start_matches("Option<")
            .trim_end_matches('>')
            .trim();
        self.sections_by_name
            .get(&inner.to_lowercase())
            .copied()
            // Sections without field rows (e.g. enum value tables like
            // `RepublishScope`) stay leaf rows rather than dissolving into
            // an empty section.
            .filter(|fields| !fields.is_empty())
    }

    /// Recursively emit a section for `fields` under `path`, appending nested
    /// sub-struct sections after it. `visited` guards against cycles.
    fn build_section(
        &self,
        title: String,
        path: String,
        fields: &'a [FieldDoc],
        out: &mut Vec<ConfigSectionView>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(path.clone()) {
            return;
        }
        let mut rows = Vec::new();
        let mut nested = Vec::new();
        for field in fields {
            let field_path = format!("{path}.{}", field.name);
            match self.nested_section_for(field) {
                Some(section_fields) => self.build_section(
                    format!("{title} · {}", humanize(&field.name)),
                    field_path,
                    section_fields,
                    &mut nested,
                    visited,
                ),
                None => rows.push(self.build_row(field, &field_path)),
            }
        }
        out.push(ConfigSectionView { title, path, rows });
        out.extend(nested);
    }

    fn build_row(&self, field: &FieldDoc, path: &str) -> ConfigRowView {
        let source = self.explicit_source(path);
        let value = match self.lookup(path) {
            Some(value) => format_value(value),
            None => CellValue::Unset,
        };
        let search = format!(
            "{} {} {} {} {}",
            field.name,
            path,
            value.search_text(),
            source.as_deref().unwrap_or(""),
            field.description
        )
        .to_lowercase();
        ConfigRowView {
            name: field.name.clone(),
            path: path.to_string(),
            ty: clean_type(&field.ty),
            value,
            overridden: source.is_some(),
            source: source.unwrap_or_default(),
            default: format_default(&field.default),
            description_html: match field.description.trim() {
                "" => r#"<span class="config-unset">No description yet.</span>"#.to_string(),
                description => markdown_lite(description),
            },
            runtime: false,
            search,
        }
    }

    /// Effective value at `path`, matching each segment spelling-insensitively
    /// (reference field names vs. serde-renamed JSON keys).
    fn lookup(&self, path: &str) -> Option<&'a serde_json::Value> {
        let mut current = self.effective;
        for segment in path.split('.') {
            let object = current.as_object()?;
            let want = canonical(segment);
            current = object
                .iter()
                .find(|(key, _)| canonical(key) == want)
                .map(|(_, value)| value)?;
        }
        Some(current)
    }

    /// A key counts as explicitly set when the merged config sources provided
    /// it or anything beneath it (e.g. `pools` is overridden when
    /// `pools.lo-ip.pool_type` was set in a file). Nested keys can come from
    /// different files (base defines an entry, the site overlay tweaks one
    /// field of it), so the label names every distinct source.
    fn explicit_source(&self, path: &str) -> Option<String> {
        let key = canonical(path);
        if let Some(source) = self.explicit.get(&key) {
            return Some((*source).to_string());
        }
        let prefix = format!("{key}.");
        let sources: std::collections::BTreeSet<&str> = self
            .explicit
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(_, source)| *source)
            .collect();
        (!sources.is_empty()).then(|| sources.into_iter().collect::<Vec<_>>().join(", "))
    }
}

/// Canonical form of a dotted config path: each segment lowercased with
/// everything but alphanumerics stripped, so `mlxconfig_profiles`,
/// `mlx-config-profiles`, and `MlxConfigProfiles` all compare equal while
/// path structure is preserved.
fn canonical(path: &str) -> String {
    path.split('.')
        .map(|segment| {
            segment
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Renders a JSON value as a display cell.
fn format_value(value: &serde_json::Value) -> CellValue {
    use serde_json::Value;
    match value {
        Value::Null => CellValue::Unset,
        Value::Bool(b) => CellValue::Code(b.to_string()),
        Value::Number(n) => CellValue::Code(n.to_string()),
        Value::String(s) if s.is_empty() => CellValue::Empty,
        Value::String(s) => CellValue::Code(s.clone()),
        Value::Array(items) if items.is_empty() => CellValue::Empty,
        Value::Array(items) if items.iter().all(is_scalar) => {
            let joined = items.iter().map(scalar_text).collect::<Vec<_>>().join(", ");
            if joined.len() <= 120 {
                CellValue::Code(joined)
            } else {
                pretty(value)
            }
        }
        Value::Object(map) if map.is_empty() => CellValue::Empty,
        // std::time::Duration serializes as {secs, nanos}; show it humanely.
        Value::Object(map)
            if map.len() == 2 && map.contains_key("secs") && map.contains_key("nanos") =>
        {
            let secs = map["secs"].as_u64().unwrap_or(0);
            let nanos = map["nanos"].as_u64().unwrap_or(0);
            if nanos == 0 {
                CellValue::Code(humanize_seconds(secs))
            } else {
                CellValue::Code(format!("{secs}.{nanos:09}s"))
            }
        }
        _ => pretty(value),
    }
}

fn pretty(value: &serde_json::Value) -> CellValue {
    CellValue::Pre(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
}

fn is_scalar(value: &serde_json::Value) -> bool {
    !(value.is_array() || value.is_object())
}

fn scalar_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn humanize_seconds(secs: u64) -> String {
    if secs > 0 && secs.is_multiple_of(86400) {
        format!("{}d", secs / 86400)
    } else if secs > 0 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs > 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn format_default(default: &str) -> CellValue {
    match default.trim() {
        "" | "—" | "-" | "*(see below)*" | "*(default)*" => CellValue::NoDefault,
        "**required**" => CellValue::Required,
        other => CellValue::Rich(markdown_lite(other)),
    }
}

/// Strips markdown emphasis from the reference's type cell for display.
fn clean_type(ty: &str) -> String {
    ty.trim().replace('`', "")
}

/// "dpa_config" -> "DPA", "machine_validation_config" -> "Machine Validation".
/// The `_config` suffix is dropped: every section is config, so it's noise.
fn humanize(field: &str) -> String {
    field
        .strip_suffix("_config")
        .unwrap_or(field)
        .split('_')
        .map(|word| match word {
            "dpu" => "DPU".to_string(),
            "dpa" => "DPA".to_string(),
            "dpf" => "DPF".to_string(),
            "ib" => "IB".to_string(),
            "nvlink" => "NVLink".to_string(),
            "rms" => "RMS".to_string(),
            "spdm" => "SPDM".to_string(),
            "tls" => "TLS".to_string(),
            "kms" => "KMS".to_string(),
            "fnn" => "FNN".to_string(),
            "vpc" => "VPC".to_string(),
            "mqtt" => "MQTT".to_string(),
            "oauth2" => "OAuth2".to_string(),
            "dsx" => "DSX".to_string(),
            "vmaas" => "VMaaS".to_string(),
            "bom" => "BOM".to_string(),
            "nsg" => "NSG".to_string(),
            "oem" => "OEM".to_string(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses the reference markdown into `(section_name, fields)` pairs in
/// document order. A section starts at a `## `/`### ` heading whose text is a
/// backticked struct name (e.g. ``### `TlsConfig` ``); prose headings without
/// tables contribute nothing.
fn parse_reference(markdown: &str) -> Vec<(String, Vec<FieldDoc>)> {
    let mut sections: Vec<(String, Vec<FieldDoc>)> = Vec::new();
    let mut current: Option<String> = None;
    let mut in_code_block = false;

    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        if let Some(heading) = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
        {
            current = heading_struct_name(heading);
            if let Some(name) = &current
                && !sections.iter().any(|(existing, _)| existing == name)
            {
                sections.push((name.clone(), Vec::new()));
            }
            continue;
        }
        let Some(section) = &current else { continue };
        if let Some(field) = parse_table_row(trimmed)
            && let Some((_, fields)) = sections.iter_mut().find(|(name, _)| name == section)
        {
            fields.push(field);
        }
    }
    sections
}

/// Extracts the struct name from a heading like `` `TlsConfig` `` or
/// `` `NicoConfig` (top-level) ``; returns None for prose headings.
fn heading_struct_name(heading: &str) -> Option<String> {
    let rest = heading.trim().strip_prefix('`')?;
    let (name, _) = rest.split_once('`')?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Parses a markdown table row into a FieldDoc; header and separator rows
/// (and rows whose first cell isn't a backticked field name) return None.
/// A fourth `Group` cell (top-level table only) is recognized by matching a
/// known group slug.
fn parse_table_row(line: &str) -> Option<FieldDoc> {
    if !line.starts_with('|') {
        return None;
    }
    let cells: Vec<&str> = split_table_cells(line);
    if cells.len() < 4 {
        return None;
    }
    let name = cells[0].trim();
    let name = name.strip_prefix('`')?.strip_suffix('`')?;
    if name.is_empty() {
        return None;
    }
    let maybe_slug = cells[3].trim().trim_matches('`');
    let (group, description_cells) = if GROUP_ORDER.iter().any(|(_, slug)| *slug == maybe_slug) {
        (Some(maybe_slug.to_string()), &cells[4..])
    } else {
        (None, &cells[3..])
    };
    Some(FieldDoc {
        name: name.to_string(),
        ty: cells[1].trim().to_string(),
        default: cells[2].trim().to_string(),
        group,
        description: description_cells.join("|").trim().to_string(),
    })
}

/// Splits a markdown table row on unescaped pipes, honoring `\|` escapes.
fn split_table_cells(line: &str) -> Vec<&str> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = Vec::new();
    let mut start = 0;
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            cells.push(&inner[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    cells.push(&inner[start..]);
    cells
}

fn escape_html(text: &str) -> String {
    use askama_escape::Escaper;
    let mut out = String::with_capacity(text.len());
    askama_escape::Html
        .write_escaped(&mut out, text)
        .expect("writing to a String cannot fail");
    out
}

/// Minimal markdown renderer for the reference's table cells: HTML-escapes,
/// then supports `` `code` ``, `**bold**`, and `[text](target)` links (only
/// http(s) targets become anchors; the reference's intra-doc anchors have no
/// counterpart on the rendered page, so they unwrap to their label). This is
/// the single escaping site for all rich text on the page — [`CellValue::Rich`]
/// and `description_html` must only ever carry its output.
fn markdown_lite(text: &str) -> String {
    static LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)]*)\)").unwrap());
    static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").unwrap());
    static BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*").unwrap());

    let escaped = escape_html(&text.replace("\\|", "|"));
    let linked = LINK.replace_all(&escaped, |caps: &regex::Captures| {
        let (label, target) = (&caps[1], &caps[2]);
        if target.starts_with("http://") || target.starts_with("https://") {
            format!("<a href=\"{target}\">{label}</a>")
        } else {
            label.to_string()
        }
    });
    let coded = CODE.replace_all(&linked, "<code>$1</code>");
    BOLD.replace_all(&coded, "<strong>$1</strong>").into_owned()
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    /// The shared compile-checked fixture with every `Option` section
    /// populated, so the coverage and stale-row guards can see inside all
    /// sections. Maintained by rustc: new config fields fail to compile in
    /// `test_support::default_config` until added there.
    fn fixture_json() -> serde_json::Value {
        let config = carbide_api_core::test_support::default_config::fully_populated();
        serde_json::to_value(config.redacted()).expect("config serializes")
    }

    fn no_live_settings() -> LiveSettings {
        LiveSettings {
            log_filter: "info".to_string(),
            site_explorer_enabled: "true".to_string(),
            create_machines: "true".to_string(),
            bmc_proxy: None,
            tracing_enabled: "false".to_string(),
            dpu_agent_upgrade_policy: "Off".to_string(),
        }
    }

    #[test]
    fn parses_real_reference() {
        let sections = parse_reference(carbide_api_core::cfg::CONFIG_REFERENCE_MD);
        let names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"NicoConfig"),
            "top-level section missing: {names:?}"
        );
        assert!(names.contains(&"TlsConfig"));
        assert!(names.contains(&"SiteExplorerConfig"));
        assert!(names.contains(&"TracingConfig"));

        let (_, top) = sections.iter().find(|(n, _)| n == "NicoConfig").unwrap();
        assert!(
            top.len() > 50,
            "expected many top-level fields, got {}",
            top.len()
        );
        let listen = top.iter().find(|f| f.name == "listen").unwrap();
        assert!(listen.ty.contains("SocketAddr"));
        assert!(listen.default.contains("1079"));
    }

    /// Guards the drift the generated page exists to eliminate: every
    /// documented top-level option must declare a valid group slug in the
    /// reference doc's `Group` column.
    #[test]
    fn every_documented_field_has_a_group() {
        let sections = parse_reference(carbide_api_core::cfg::CONFIG_REFERENCE_MD);
        let (_, top) = sections.iter().find(|(n, _)| n == "NicoConfig").unwrap();
        let ungrouped: Vec<&str> = top
            .iter()
            .filter(|f| f.group.is_none())
            .map(|f| f.name.as_str())
            .collect();
        assert!(
            ungrouped.is_empty(),
            "fields without a valid Group cell: {ungrouped:?}"
        );
    }

    /// Guards README coverage: every top-level key of the serialized config
    /// must be documented in the reference. A failure means a field was added
    /// to `CarbideConfig` without a README row.
    #[test]
    fn every_config_field_is_documented() {
        let effective = fixture_json();
        let sections = parse_reference(carbide_api_core::cfg::CONFIG_REFERENCE_MD);
        let (_, top) = sections.iter().find(|(n, _)| n == "NicoConfig").unwrap();
        let documented: HashSet<String> = top.iter().map(|f| canonical(&f.name)).collect();
        let undocumented: Vec<&String> = effective
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| !documented.contains(&canonical(key)))
            .collect();
        assert!(
            undocumented.is_empty(),
            "config fields missing from cfg/README.md: {undocumented:?}"
        );
    }

    /// Extends the coverage guard below the top level: every key the config
    /// actually serializes under a documented section must be a documented
    /// row (or a documented sub-section) of that section. Catches a field
    /// added to e.g. `SiteExplorerConfig` without a README row.
    #[test]
    fn every_serialized_section_key_is_documented() {
        let effective = fixture_json();
        let page = build_config_page(
            carbide_api_core::cfg::CONFIG_REFERENCE_MD,
            &effective,
            &BTreeMap::new(),
            no_live_settings(),
        );

        let sections: Vec<&ConfigSectionView> = page
            .groups
            .iter()
            .flat_map(|g| g.sections.iter())
            .filter(|s| !s.path.is_empty())
            .collect();
        let section_paths: HashSet<String> = sections.iter().map(|s| canonical(&s.path)).collect();

        let mut problems = Vec::new();
        for section in &sections {
            // Walk the serialized config to this section's object, matching
            // segments the same way the builder does.
            let mut value = &effective;
            let mut found = true;
            for segment in section.path.split('.') {
                let want = canonical(segment);
                match value
                    .as_object()
                    .and_then(|o| o.iter().find(|(k, _)| canonical(k) == want).map(|(_, v)| v))
                {
                    Some(v) => value = v,
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            let Some(object) = (found.then_some(value)).and_then(|v| v.as_object()) else {
                continue; // unset Option section or non-object value
            };
            let documented: HashSet<String> = section
                .rows
                .iter()
                .filter_map(|r| r.path.rsplit('.').next())
                .map(canonical)
                .collect();
            for key in object.keys() {
                let child_path = canonical(&format!("{}.{}", section.path, key));
                if !documented.contains(&canonical(key)) && !section_paths.contains(&child_path) {
                    problems.push(format!("{}.{}", section.path, key));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "serialized config keys missing from their cfg/README.md section: {problems:?}"
        );
    }

    /// The inverse of the coverage guards: a documented row whose parent
    /// object serializes but whose key doesn't exist documents a field that
    /// was removed or renamed — delete or fix the README row. Rows under
    /// unset `Option` sections (e.g. `tls` in the minimal config) can't be
    /// verified and are skipped, as are synthetic runtime rows and the one
    /// `skip_serializing_if` field.
    #[test]
    fn every_documented_row_matches_a_config_field() {
        let effective = fixture_json();
        let page = build_config_page(
            carbide_api_core::cfg::CONFIG_REFERENCE_MD,
            &effective,
            &BTreeMap::new(),
            no_live_settings(),
        );

        const SKIP_SERIALIZING: &[&str] = &["mlxconfig_profiles"];
        let mut stale = Vec::new();
        for row in page
            .groups
            .iter()
            .flat_map(|g| g.sections.iter())
            .flat_map(|s| s.rows.iter())
            .filter(|r| !r.runtime && !SKIP_SERIALIZING.contains(&r.path.as_str()))
        {
            let mut value = &effective;
            for segment in row.path.split('.') {
                let Some(object) = value.as_object() else {
                    break; // parent is unset (Option section) — unverifiable
                };
                let want = canonical(segment);
                match object.iter().find(|(k, _)| canonical(k) == want) {
                    Some((_, child)) => value = child,
                    None => {
                        stale.push(row.path.clone());
                        break;
                    }
                }
            }
        }
        assert!(
            stale.is_empty(),
            "cfg/README.md rows documenting nonexistent config fields: {stale:?}"
        );
    }

    #[test]
    fn builds_page_with_overrides() {
        let effective = serde_json::json!({
            "listen": "[::]:1079",
            "asn": 65001,
            "mlx-config-profiles": { "profileA": {} },
            "tls": { "root_cafile_path": "/etc/ca.crt" },
        });
        let mut explicit = BTreeMap::new();
        explicit.insert("asn".to_string(), "site.toml".to_string());
        explicit.insert(
            "mlx-config-profiles.profileA".to_string(),
            "base.toml".to_string(),
        );
        explicit.insert("tls.root_cafile_path".to_string(), "base.toml".to_string());

        let page = build_config_page(
            carbide_api_core::cfg::CONFIG_REFERENCE_MD,
            &effective,
            &explicit,
            no_live_settings(),
        );

        let rows: Vec<&ConfigRowView> = page
            .groups
            .iter()
            .flat_map(|g| g.sections.iter())
            .flat_map(|s| s.rows.iter())
            .collect();
        assert!(
            rows.len() > 150,
            "expected full catalog, got {}",
            rows.len()
        );

        let asn = rows.iter().find(|r| r.path == "asn").unwrap();
        assert!(asn.overridden);
        assert_eq!(asn.source, "site.toml");
        assert!(asn.value.to_html().contains("65001"));

        let listen = rows.iter().find(|r| r.path == "listen").unwrap();
        assert!(!listen.overridden);

        // Serde-renamed keys join spelling-insensitively: the reference's
        // `mlxconfig_profiles` matches the serialized `mlx-config-profiles`.
        let mlx = rows
            .iter()
            .find(|r| r.path == "mlxconfig_profiles")
            .unwrap();
        assert!(!mlx.value.is_unset(), "renamed key must resolve a value");
        assert!(mlx.overridden, "renamed key must resolve provenance");

        // Nested section rows exist with dotted paths, and a field whose
        // sub-struct was set is marked overridden by prefix.
        let tls_row = rows
            .iter()
            .find(|r| r.path == "tls.root_cafile_path")
            .unwrap();
        assert!(tls_row.overridden);

        // Runtime settings are folded in: documented options get tagged...
        let se = rows
            .iter()
            .find(|r| r.path == "site_explorer.enabled")
            .unwrap();
        assert!(se.runtime);
        // ...and undocumented ones become synthetic runtime rows.
        let lf = rows.iter().find(|r| r.path == "log_filter").unwrap();
        assert!(lf.runtime);
        assert!(lf.value.to_html().contains("info"));
    }

    #[test]
    fn markdown_lite_renders_and_escapes() {
        assert_eq!(
            markdown_lite("Use `ip_address` for <new> hosts"),
            "Use <code>ip_address</code> for &#60;new&#62; hosts"
        );
        assert_eq!(
            markdown_lite("**Deprecated.**"),
            "<strong>Deprecated.</strong>"
        );
        assert_eq!(
            markdown_lite("see [SiteExplorerConfig](#siteexplorerconfig)."),
            "see SiteExplorerConfig."
        );
        // Plain bracketed text before a real link must not fuse into it.
        assert_eq!(
            markdown_lite("index [0] then [docs](https://example.com) end"),
            "index [0] then <a href=\"https://example.com\">docs</a> end"
        );
    }

    #[test]
    fn cell_values_render_and_escape() {
        assert_eq!(
            CellValue::Code("<x>".to_string()).to_html(),
            "<code>&#60;x&#62;</code>"
        );
        assert!(
            CellValue::Pre("{\"a\": 1}".to_string())
                .to_html()
                .contains("show value")
        );
        assert!(CellValue::Unset.is_unset());
        assert_eq!(
            format_value(&serde_json::json!({"secs": 3600, "nanos": 0})).to_html(),
            "<code>1h</code>"
        );
        let list = format_value(&serde_json::json!(["10.0.0.1", "10.0.0.2"]));
        assert!(list.to_html().contains("10.0.0.1, 10.0.0.2"));
        assert!(matches!(
            format_value(&serde_json::json!({"a": {"b": 1}})),
            CellValue::Pre(_)
        ));
        assert!(matches!(
            format_default("**required**"),
            CellValue::Required
        ));
    }
}
