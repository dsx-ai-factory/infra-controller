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

//! Attribution of configuration keys back to the sources that provided them.
//!
//! The merged [`Figment`] retained on `CarbideConfig::config_ctx` knows which
//! provider (base config file, site overlay, `CARBIDE_API_*` env vars)
//! supplied each key. This module walks it to answer "which keys did the
//! operator explicitly set, and where?" — used by the admin UI's
//! Configuration page to distinguish overrides from compiled-in defaults.

use std::collections::BTreeMap;

use figment::Figment;

/// Returns the dotted paths of every configuration key explicitly provided by
/// one of the merged configuration sources, mapped to a human-readable source
/// label such as the providing file's name.
///
/// Keys absent from the map fall back to their compiled-in defaults.
pub(super) fn explicit_value_paths(figment: &Figment) -> BTreeMap<String, String> {
    let mut paths = BTreeMap::new();
    let Ok(root) = figment.extract::<figment::value::Value>() else {
        return paths;
    };
    collect_explicit_paths(figment, &root, String::new(), &mut paths);
    paths
}

/// Walk the merged figment value tree, recording each leaf key's dotted path
/// and the label of the source that provided it. Dicts recurse; arrays and
/// scalars are leaves (an array is always provided wholesale by one source).
fn collect_explicit_paths(
    figment: &Figment,
    value: &figment::value::Value,
    path: String,
    paths: &mut BTreeMap<String, String>,
) {
    if let figment::value::Value::Dict(_, dict) = value {
        for (key, child) in dict {
            let child_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            collect_explicit_paths(figment, child, child_path, paths);
        }
    } else {
        let source = figment
            .find_metadata(&path)
            .map(source_label)
            .unwrap_or_else(|| "configuration".to_string());
        paths.insert(path, source);
    }
}

pub(super) fn source_label(metadata: &figment::Metadata) -> String {
    match metadata.source.as_ref() {
        Some(figment::Source::File(path)) => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        _ => metadata.name.to_string(),
    }
}
