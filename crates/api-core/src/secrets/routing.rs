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

use super::PgSecretsError;

/// Maps secret path prefixes to the KEK that encrypts new writes under
/// them, longest prefix winning. Routing only decides the key for writes
/// (and the target key for re-wrap) -- reads never consult it, because
/// every stored row records the KEK that wrapped it.
///
/// Prefixes match whole path segments ("machines/bmc" matches
/// `machines/bmc/...` but not `machines/bmc-archive/...`), and a `"/"`
/// catch-all route is required so that every path -- including ones for
/// credential types that did not exist when the config was written -- has
/// a key to write with.
#[derive(Clone)]
pub struct SecretRouting {
    /// `(prefix, kek_id)` sorted longest-prefix-first, with the `"/"`
    /// catch-all stored as the empty prefix so it matches everything and
    /// sorts last.
    routes: Vec<(String, String)>,
}

/// The route key that matches every path. Secret paths are Vault-style with
/// no leading slash (`machines/bmc/...`), so `"/"` is purely the config
/// spelling for "everything else".
const CATCH_ALL: &str = "/";

/// Normalize a config prefix for matching: the catch-all becomes the empty
/// prefix (matches everything, sorts last), and every other prefix gets a
/// trailing slash so it matches whole path segments -- "machines/bmc" must
/// not capture a sibling like `machines/bmc-archive/`.
fn normalize_prefix(prefix: &str) -> String {
    if prefix == CATCH_ALL {
        String::new()
    } else if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

impl SecretRouting {
    /// Build routing from the `[secrets.routing]` config map. Requires a
    /// `"/"` catch-all entry, and rejects empty prefixes, empty kek_ids,
    /// and prefixes that collide once normalized -- "machines/bmc" and
    /// "machines/bmc/" are distinct TOML keys but the same route, and
    /// letting both through would pick a winner at random.
    pub fn from_config(routing: &HashMap<String, String>) -> Result<Self, PgSecretsError> {
        if !routing.contains_key(CATCH_ALL) {
            return Err(PgSecretsError::RoutingConfig(format!(
                "routing must include a {CATCH_ALL:?} catch-all entry; without one, \
                 writes to unrouted paths have no key and fail at runtime"
            )));
        }

        let mut seen: HashMap<String, &String> = HashMap::new();
        for (prefix, kek_id) in routing {
            if prefix.is_empty() {
                return Err(PgSecretsError::RoutingConfig(
                    "empty routing prefix; use \"/\" for the catch-all".to_string(),
                ));
            }
            if kek_id.is_empty() {
                return Err(PgSecretsError::RoutingConfig(format!(
                    "empty kek_id for prefix {prefix:?}"
                )));
            }
            // Credential paths never start with a slash ("machines/..."),
            // so a leading-slash prefix other than the catch-all could
            // never match -- its writes would silently fall through to the
            // catch-all KEK. Reject it rather than encrypt under the wrong
            // key.
            if prefix != CATCH_ALL && prefix.starts_with('/') {
                return Err(PgSecretsError::RoutingConfig(format!(
                    "routing prefix {prefix:?} starts with '/' but credential paths do not; \
                     use \"/\" only for the catch-all"
                )));
            }
            let normalized = normalize_prefix(prefix);
            if let Some(other) = seen.insert(normalized, prefix) {
                return Err(PgSecretsError::RoutingConfig(format!(
                    "routing prefixes {other:?} and {prefix:?} are the same route"
                )));
            }
        }

        Ok(Self::new(
            routing
                .iter()
                .map(|(prefix, kek_id)| (prefix.clone(), kek_id.clone()))
                .collect(),
        ))
    }

    /// Build routing from pre-built `(prefix, kek_id)` entries. Unlike
    /// [`SecretRouting::from_config`] this does not require a catch-all or
    /// reject collisions, which lets tests construct partial routing on
    /// purpose.
    pub fn new(routes: Vec<(String, String)>) -> Self {
        let mut routes: Vec<(String, String)> = routes
            .into_iter()
            .map(|(prefix, kek_id)| (normalize_prefix(&prefix), kek_id))
            .collect();
        // Longest prefix first; the prefix itself breaks length ties so the
        // order never depends on HashMap iteration.
        routes.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Self { routes }
    }

    /// Return the kek_id that encrypts a new write at `path`, using the
    /// longest matching prefix.
    pub(crate) fn active_kek_for_path(&self, path: &str) -> Result<&str, PgSecretsError> {
        self.routes
            .iter()
            .find(|(prefix, _)| path.starts_with(prefix.as_str()))
            .map(|(_, kek_id)| kek_id.as_str())
            .ok_or_else(|| {
                PgSecretsError::RoutingConfig(format!("no routing prefix matches path {path:?}"))
            })
    }

    /// Iterate the configured `(prefix, kek_id)` routes. Startup validation
    /// uses this to confirm every routed KEK actually exists in the KMS.
    pub fn routes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.routes
            .iter()
            .map(|(prefix, kek_id)| (prefix.as_str(), kek_id.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verifies that the most specific prefix wins (longest-prefix-match).
    #[test]
    fn longest_prefix_match() {
        let routing = SecretRouting::new(vec![
            ("/".to_string(), "default-key".to_string()),
            ("machines/bmc".to_string(), "bmc-key".to_string()),
        ]);

        assert_eq!(
            routing
                .active_kek_for_path("machines/bmc/aa:bb/root")
                .expect("bmc"),
            "bmc-key"
        );
        assert_eq!(
            routing
                .active_kek_for_path("ufm/fabric/auth")
                .expect("default"),
            "default-key"
        );
    }

    // Verifies that the catch-all matches paths no specific prefix does,
    // including the internal import-marker path.
    #[test]
    fn catch_all_matches_everything() {
        let routing = SecretRouting::new(vec![
            ("/".to_string(), "default-key".to_string()),
            ("a".to_string(), "a-key".to_string()),
        ]);

        assert_eq!(routing.active_kek_for_path("a/bc").expect("a"), "a-key");
        assert_eq!(
            routing.active_kek_for_path("xyz").expect("rest"),
            "default-key"
        );
        assert_eq!(
            routing
                .active_kek_for_path("/_vault_import")
                .expect("marker"),
            "default-key"
        );
    }

    // Verifies that prefixes match whole path segments: a prefix must not
    // capture a sibling namespace that merely shares its leading
    // characters.
    #[test]
    fn prefix_does_not_match_mid_segment() {
        let routing = SecretRouting::new(vec![
            ("/".to_string(), "default-key".to_string()),
            ("machines/bmc".to_string(), "bmc-key".to_string()),
        ]);

        assert_eq!(
            routing
                .active_kek_for_path("machines/bmc/aa:bb/root")
                .expect("bmc"),
            "bmc-key"
        );
        assert_eq!(
            routing
                .active_kek_for_path("machines/bmc-archive/aa:bb/root")
                .expect("sibling"),
            "default-key"
        );
    }

    // Verifies that from_config rejects a config without a catch-all --
    // without one, some credential writes would have no key.
    #[test]
    fn from_config_requires_catch_all() {
        let mut routing = HashMap::new();
        routing.insert("machines/bmc".to_string(), "bmc-key".to_string());
        assert!(SecretRouting::from_config(&routing).is_err());
    }

    // Verifies that from_config rejects an empty kek_id.
    #[test]
    fn from_config_empty_kek_id_errors() {
        let mut routing = HashMap::new();
        routing.insert("/".to_string(), String::new());
        assert!(SecretRouting::from_config(&routing).is_err());
    }

    // Verifies that two spellings of the same route are rejected instead
    // of one silently winning.
    #[test]
    fn from_config_rejects_colliding_prefixes() {
        let mut routing = HashMap::new();
        routing.insert("/".to_string(), "key1".to_string());
        routing.insert("machines/bmc".to_string(), "key2".to_string());
        routing.insert("machines/bmc/".to_string(), "key3".to_string());
        assert!(SecretRouting::from_config(&routing).is_err());
    }

    // Verifies that an empty prefix is rejected -- the catch-all is
    // spelled "/".
    #[test]
    fn from_config_rejects_empty_prefix() {
        let mut routing = HashMap::new();
        routing.insert("/".to_string(), "key1".to_string());
        routing.insert(String::new(), "key2".to_string());
        assert!(SecretRouting::from_config(&routing).is_err());
    }

    // Verifies that a non-catch-all prefix starting with "/" is rejected:
    // credential paths have no leading slash, so it could never match and
    // its writes would silently land under the catch-all KEK.
    #[test]
    fn from_config_rejects_leading_slash_prefix() {
        let mut routing = HashMap::new();
        routing.insert("/".to_string(), "key1".to_string());
        routing.insert("/machines/bmc".to_string(), "key2".to_string());
        assert!(SecretRouting::from_config(&routing).is_err());
    }

    // Verifies that from_config parses a valid config.
    #[test]
    fn from_config_valid() {
        let mut routing = HashMap::new();
        routing.insert("/".to_string(), "key1".to_string());
        routing.insert("machines/bmc".to_string(), "key2".to_string());

        let r = SecretRouting::from_config(&routing).expect("from_config");
        assert_eq!(
            r.active_kek_for_path("machines/bmc/x").expect("bmc"),
            "key2"
        );
        assert_eq!(r.active_kek_for_path("other").expect("default"), "key1");
    }

    // Verifies that routes() reports the normalized entries for startup
    // validation.
    #[test]
    fn routes_iterates_all_entries() {
        let mut routing = HashMap::new();
        routing.insert("/".to_string(), "key1".to_string());
        routing.insert("machines/bmc".to_string(), "key2".to_string());

        let r = SecretRouting::from_config(&routing).expect("from_config");
        let keks: Vec<&str> = r.routes().map(|(_, kek)| kek).collect();
        assert_eq!(keks, vec!["key2", "key1"]);
    }
}
