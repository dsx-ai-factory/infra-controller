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

//! Per-release catalogues of the Core site TOML's top-level keys,
//! `register/toml-keys/<release>.yaml`: each key's TOML type and deprecation, so a site's
//! TOML can be held to the release it targets without pulling Core's config types in.

use std::sync::OnceLock;

use eyre::Result;
use serde::Deserialize;

const CATALOGUES: &[&str] = &[include_str!("../register/toml-keys/v2.0.10.yaml")];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalogue {
    pub(crate) release: String,
    /// Where the keys were taken from, for the reader of the file.
    #[allow(
        dead_code,
        reason = "provenance for the file's reader; tests assert it"
    )]
    pub(crate) source: String,
    pub(crate) keys: Vec<Key>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Key {
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) kind: Kind,
    /// Why the key is deprecated, or what replaces it.
    #[serde(default)]
    pub(crate) deprecated: Option<String>,
}

/// The TOML type a key takes; `Any` when the catalogue does not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    String,
    Integer,
    Boolean,
    Array,
    Table,
    Any,
}

impl Kind {
    pub(crate) fn accepts(self, value: &toml::Value) -> bool {
        match self {
            Kind::Any => true,
            Kind::String => value.is_str() || value.is_datetime(),
            Kind::Integer => value.is_integer(),
            Kind::Boolean => value.is_bool(),
            Kind::Array => value.is_array(),
            Kind::Table => value.is_table(),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::String => "a string",
            Kind::Integer => "an integer",
            Kind::Boolean => "a boolean",
            Kind::Array => "an array",
            Kind::Table => "a table",
            Kind::Any => "any value",
        }
    }
}

/// The TOML type of a value, as the messages name it.
pub(crate) fn type_name(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "an integer",
        toml::Value::Float(_) => "a float",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "a datetime",
        toml::Value::Array(_) => "an array",
        toml::Value::Table(_) => "a table",
    }
}

impl Catalogue {
    pub(crate) fn key(&self, name: &str) -> Option<&Key> {
        self.keys.iter().find(|k| k.name == name)
    }
}

fn all() -> &'static [Catalogue] {
    static ALL: OnceLock<Vec<Catalogue>> = OnceLock::new();
    ALL.get_or_init(|| {
        CATALOGUES
            .iter()
            .map(|text| parse(text).expect("register/toml-keys/*.yaml parses"))
            .collect()
    })
}

pub(crate) fn parse(text: &str) -> Result<Catalogue> {
    let c: Catalogue = serde_yaml::from_str(text)?;
    if c.keys.is_empty() {
        eyre::bail!("`keys` is empty");
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(k) = c.keys.iter().find(|k| !seen.insert(k.name.as_str())) {
        eyre::bail!("key `{}` is listed twice", k.name);
    }
    Ok(c)
}

/// The catalogue of `release` (`v2.0.10`, with or without the `v`), if one is compiled in.
pub(crate) fn for_release(release: &str) -> Option<&'static Catalogue> {
    let wanted = release.trim().trim_start_matches('v');
    all()
        .iter()
        .find(|c| c.release.trim_start_matches('v') == wanted)
}

/// The releases with a catalogue, for the message when one is missing.
pub(crate) fn releases() -> Vec<&'static str> {
    all().iter().map(|c| c.release.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v2.0.10 catalogue is the struct's field list: 103 keys, two deprecated, found
    /// with or without the `v`; an unknown release has none.
    #[test]
    fn the_v2_0_10_catalogue_is_the_struct() {
        let c = for_release("v2.0.10").unwrap();
        assert_eq!(c.keys.len(), 103);
        assert!(c.source.contains("CarbideConfig"), "{}", c.source);
        assert_eq!(c.keys.iter().filter(|k| k.deprecated.is_some()).count(), 2);
        assert_eq!(c.key("sitename").map(|k| k.kind), Some(Kind::String));
        assert_eq!(c.key("pools").map(|k| k.kind), Some(Kind::Table));
        assert!(
            c.key("nvue_enabled").is_none(),
            "a proto field, not a TOML key"
        );
        assert!(for_release("2.0.10").is_some());
        assert!(for_release("v2.2.0").is_none());
        assert_eq!(releases(), ["v2.0.10"]);
    }

    /// A catalogue listing a key twice is refused naming it.
    #[test]
    fn a_duplicate_key_is_refused() {
        let e = parse(
            "release: x\nsource: s\nkeys:\n  - { name: a, type: string }\n  - { name: a, type: table }\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("`a` is listed twice"), "{e}");
    }
}
