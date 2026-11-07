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

// src/profile.rs
// Defines the main MlxConfigProfile type and supporting
// implementation for our  mlxconfig-profile crate.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::profile::error::MlxProfileError;
use crate::profile::serialization::SerializableProfile;
use crate::runner::exec_options::ExecOptions;
use crate::runner::result_types::{ComparisonResult, SyncResult};
use crate::runner::runner::MlxConfigRunner;
use crate::variables::registry::MlxVariableRegistry;
use crate::variables::value::{IntoMlxValue, MlxConfigValue};

// MlxConfigProfile is a configuration profile that defines a complete set of
// variable values to apply to a device (DPU, SuperNIC, etc) -- any device whose
// configuration is controlled via `mlxconfig`. Every profile is backed by a
// given MlxVariableRegistry, which defines the variable types known to that
// registry, and what device(s) those variables are valid for. You can then
// define a profile of "expected" configuration, and then compare and/or sync
// the profile to the device (which uses mlxconfig-runner behind the scenes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlxConfigProfile {
    // name is the profile name.
    pub name: String,
    // registry is the target registry that defines the variables and
    // device filters available to this profile.
    pub registry: MlxVariableRegistry,
    // description is an optional description for this profile, which
    // will probably come in handy for site operators.
    pub description: Option<String>,
    // config_values are the values to set on the device. Each value
    // will be verified to exist in the backing registry.
    pub config_values: Vec<MlxConfigValue>,
    // config_lookup is an internally-managed hashmap to look up a
    // variable by name. The value is the index into `config_values`.
    #[serde(skip)]
    config_lookup: HashMap<String, usize>,
}

impl MlxConfigProfile {
    // new creates a new configuration profile.
    pub fn new<N: Into<String>>(name: N, registry: MlxVariableRegistry) -> Self {
        Self {
            name: name.into(),
            registry,
            description: None,
            config_values: Vec::new(),
            config_lookup: HashMap::new(),
        }
    }

    // with_description sets a description for this profile.
    pub fn with_description<D: Into<String>>(mut self, description: D) -> Self {
        self.description = Some(description.into());
        self
    }

    // with adds a variable setting to this profile, leveraging all of
    // the trait implementations that exist for backing specs, so you
    // should be able to toss the value up in most formats, and we'll
    // make sure it is properly typed according to the spec for the variable.
    pub fn with<T: IntoMlxValue>(
        self,
        variable_name: &str,
        value: T,
    ) -> Result<Self, MlxProfileError> {
        let variable = self.registry.get_variable(variable_name).ok_or_else(|| {
            MlxProfileError::variable_not_found(variable_name, &self.registry.name)
        })?;

        let config_value = variable
            .with(value)
            .map_err(|error| MlxProfileError::value_validation(variable_name, error))?;

        // NOTE(chet): I'm doing this to feed the new MlxConfigValue
        // through the same codepath that we use to feed MlxConfigValues
        // in. Technically it's inefficient since we end up looking up
        // the variable in the registry again, but it also means adding
        // a variable follows the same codepath that eventually updates
        // internal data structures.
        self.with_value(config_value)
    }

    // with_value adds a pre-built MlxConfigValue to this profile.
    // The value must exist in the backing registry.
    pub fn with_value(mut self, config_value: MlxConfigValue) -> Result<Self, MlxProfileError> {
        if self.registry.get_variable(config_value.name()).is_none() {
            return Err(MlxProfileError::variable_not_found(
                config_value.name(),
                &self.registry.name,
            ));
        }

        self.add_config_value(config_value);
        Ok(self)
    }

    // add_config_value is an internal method to add a config value
    // to the profile and update the lookup map.
    fn add_config_value(&mut self, config_value: MlxConfigValue) {
        let name = config_value.name().to_string();

        // Check if we already have this variable configured.
        if let Some(&existing_index) = self.config_lookup.get(&name) {
            // Replace the existing configuration in the same
            // index, leaving the config lookup map untouched.
            self.config_values[existing_index] = config_value;
        } else {
            // ..or not, and just add the new one.
            let index = self.config_values.len();
            self.config_values.push(config_value);
            self.config_lookup.insert(name, index);
        }
    }

    // get_variable returns a configured variable value by name.
    pub fn get_variable(&self, name: &str) -> Option<&MlxConfigValue> {
        self.config_lookup
            .get(name)
            .and_then(|&index| self.config_values.get(index))
    }

    // variable_names returns a list of all configured variable
    // names in this profile.
    pub fn variable_names(&self) -> Vec<&str> {
        self.config_values.iter().map(|cv| cv.name()).collect()
    }

    // variable_count returns the number of variables configured
    // in this profile.
    pub fn variable_count(&self) -> usize {
        self.config_values.len()
    }

    // validate validates the entire profile for internal consistency,
    // including validation of each stored MlxConfigValue.
    pub fn validate(&self) -> Result<(), MlxProfileError> {
        if self.config_values.is_empty() {
            return Err(MlxProfileError::profile_validation(
                "Profile contains no variable configurations",
            ));
        }

        for config_value in &self.config_values {
            config_value
                .validate()
                .map_err(|error| MlxProfileError::value_validation(config_value.name(), error))?;
        }

        Ok(())
    }

    // compare compares this profile against the current device state.
    pub fn compare(
        &self,
        device: &str,
        options: Option<ExecOptions>,
    ) -> Result<ComparisonResult, MlxProfileError> {
        // First, validate the profile.
        self.validate()?;

        // Then, create the runner with the registry and options.
        let runner = if let Some(opts) = options {
            MlxConfigRunner::with_options(device.to_string(), self.registry.clone(), opts)
        } else {
            MlxConfigRunner::new(device.to_string(), self.registry.clone())
        };

        // And finally, perform the comparison!
        let comparison_result = runner.compare(&self.config_values)?;
        Ok(comparison_result)
    }

    // sync synchronizes this profile to the specified device, applying any
    // necessary changes.
    pub fn sync(
        &self,
        device: &str,
        options: Option<ExecOptions>,
    ) -> Result<SyncResult, MlxProfileError> {
        // First, validate the profile.
        self.validate()?;

        // Then, create the runner with the registry and options.
        let runner = if let Some(opts) = options {
            MlxConfigRunner::with_options(device.to_string(), self.registry.clone(), opts)
        } else {
            MlxConfigRunner::new(device.to_string(), self.registry.clone())
        };

        // And finally, perform the sync!
        let sync_result = runner.sync(&self.config_values)?;
        Ok(sync_result)
    }

    // from_yaml_file loads a profile from a YAML file path.
    pub fn from_yaml_file<P: AsRef<Path>>(path: P) -> Result<Self, MlxProfileError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_yaml(&content)
    }

    // from_yaml loads a profile from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, MlxProfileError> {
        let serializable = SerializableProfile::from_yaml(yaml)?;
        serializable.into_profile()
    }

    // from_json_file lads a profile from a JSON file path.
    pub fn from_json_file<P: AsRef<Path>>(path: P) -> Result<Self, MlxProfileError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_json(&content)
    }

    // from_json loads a profile from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, MlxProfileError> {
        let serializable = SerializableProfile::from_json(json)?;
        serializable.into_profile()
    }

    // to_yaml_file serializes + writes this profile to a YAML file path.
    pub fn to_yaml_file<P: AsRef<Path>>(&self, path: P) -> Result<(), MlxProfileError> {
        let yaml = self.to_yaml()?;
        std::fs::write(path, yaml)?;
        Ok(())
    }

    // to_yaml converts this profile to a YAML string.
    pub fn to_yaml(&self) -> Result<String, MlxProfileError> {
        let serializable = SerializableProfile::from_profile(self)?;
        serializable.to_yaml()
    }

    // to_json_file serializes + writes this profile to a JSON file path.
    pub fn to_json_file<P: AsRef<Path>>(&self, path: P) -> Result<(), MlxProfileError> {
        let json = self.to_json()?;
        std::fs::write(path, json)?;
        Ok(())
    }

    // to_json converts this profile to a JSON string.
    pub fn to_json(&self) -> Result<String, MlxProfileError> {
        let serializable = SerializableProfile::from_profile(self)?;
        serializable.to_json()
    }

    // summary creates a summary string describing this profile,
    // mainly used for integrating with the CLI reference example.
    pub fn summary(&self) -> String {
        match &self.description {
            Some(desc) => format!(
                "Profile '{}': {} - {} variables for registry '{}'",
                self.name,
                desc,
                self.config_values.len(),
                self.registry.name,
            ),
            None => format!(
                "Profile '{}': {} variables for registry '{}'",
                self.name,
                self.config_values.len(),
                self.registry.name,
            ),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;
    use crate::variables::spec::MlxVariableSpec;
    use crate::variables::value::MlxValueType;
    use crate::variables::variable::MlxConfigVariable;

    // a local registry with a boolean, integer, and enum variable. We avoid the
    // global registries on purpose -- those are only needed by the serialization
    // round-trips below, which key off the registry *name*.
    fn test_registry() -> MlxVariableRegistry {
        MlxVariableRegistry::new("test_reg").variables(vec![
            MlxConfigVariable {
                name: "BOOL_VAR".to_string(),
                description: "a boolean".to_string(),
                read_only: false,
                spec: MlxVariableSpec::Boolean,
            },
            MlxConfigVariable {
                name: "INT_VAR".to_string(),
                description: "an integer".to_string(),
                read_only: false,
                spec: MlxVariableSpec::Integer,
            },
            MlxConfigVariable {
                name: "ENUM_VAR".to_string(),
                description: "an enum".to_string(),
                read_only: false,
                spec: MlxVariableSpec::Enum {
                    options: vec!["low".to_string(), "high".to_string()],
                },
            },
        ])
    }

    // new starts with the given name, no description, and an empty config. This
    // pins each field of a freshly-created profile.
    #[test]
    fn new_initializes_empty_profile() {
        let profile = MlxConfigProfile::new("p1", test_registry());
        assert_eq!(profile.name, "p1");
        assert_eq!(profile.description, None);
        assert_eq!(profile.variable_count(), 0);
        assert!(profile.variable_names().is_empty());
        assert_eq!(profile.registry.name, "test_reg");
    }

    // with_description sets the optional description; absent vs present are the two
    // states the rest of the type branches on (notably `summary`).
    #[test]
    fn with_description_sets_description() {
        let profile = MlxConfigProfile::new("p1", test_registry()).with_description("hello");
        assert_eq!(profile.description, Some("hello".to_string()));
    }

    // `with` walks the registry-lookup + value-validation paths: a known variable
    // with a valid value succeeds; an unknown variable name fails (VariableNotFound);
    // a known variable with a value that violates its spec fails (ValueValidation).
    // MlxProfileError isn't PartialEq, so failures use `Fails` + map_err(drop). On
    // success we project to the configured value name.
    #[test]
    fn with_validates_name_and_value() {
        scenarios!(
            run = |(name, value)| {
                MlxConfigProfile::new("p", test_registry())
                    .with(name, value)
                    .map(|p| p.get_variable(name).unwrap().name().to_string())
                    .map_err(drop)
            };
            "known boolean variable, valid value" {
                ("BOOL_VAR", "true") => Yields("BOOL_VAR".to_string()),
            }

            "unknown variable name is rejected" {
                ("NOPE", "true") => Fails,
            }

            "known boolean variable, unparseable value fails validation" {
                ("BOOL_VAR", "maybe") => Fails,
            }
        );
    }

    // `with` on an enum variable: an allowed option succeeds; a disallowed option
    // fails. The exact error is ValueValidation wrapping the enum rejection -- we
    // just assert failure here, since the inner MlxValueError is exercised in the
    // variables tests.
    #[test]
    fn with_enum_validation_path() {
        scenarios!(
            run = |opt| {
                MlxConfigProfile::new("p", test_registry())
                    .with("ENUM_VAR", opt)
                    .map(|p| p.get_variable("ENUM_VAR").unwrap().value.clone())
                    .map_err(drop)
            };
            "valid enum option" {
                "high" => Yields(MlxValueType::Enum("high".to_string())),
            }

            "invalid enum option fails value validation" {
                "middle" => Fails,
            }
        );
    }

    // with_value takes a pre-built MlxConfigValue. A value whose variable lives in
    // the registry is accepted; a value whose variable is foreign to the registry
    // is rejected with VariableNotFound. We build the foreign value against its own
    // one-variable registry so it is internally valid but absent from `test_reg`.
    #[test]
    fn with_value_checks_registry_membership() {
        let known_value = {
            let reg = test_registry();
            reg.get_variable("INT_VAR").unwrap().with(7i64).unwrap()
        };
        let foreign_value = {
            let other = MlxVariableRegistry::new("other").add_variable(MlxConfigVariable {
                name: "FOREIGN".to_string(),
                description: "x".to_string(),
                read_only: false,
                spec: MlxVariableSpec::Integer,
            });
            other.get_variable("FOREIGN").unwrap().with(1i64).unwrap()
        };

        scenarios!(
            run = |value| {
                let name = value.name().to_string();
                MlxConfigProfile::new("p", test_registry())
                    .with_value(value)
                    .map(|p| p.get_variable(&name).unwrap().name().to_string())
                    .map_err(drop)
            };
            "value for a known variable is accepted" {
                known_value => Yields("INT_VAR".to_string()),
            }

            "value for a foreign variable is rejected" {
                foreign_value => Fails,
            }
        );
    }

    // add_config_value (driven through `with`) dedups by name: re-adding the same
    // variable replaces it in place rather than appending, so the count stays put
    // and the stored value reflects the latest write. A second, distinct variable
    // does append. Each row reports (variable_count, INT_VAR value).
    #[test]
    fn adding_same_variable_replaces_in_place() {
        value_scenarios!(
            run = |writes| {
                let mut profile = MlxConfigProfile::new("p", test_registry());
                for (name, value) in writes {
                    profile = profile.with(name, value).unwrap();
                }
                (
                    profile.variable_count(),
                    profile.get_variable("INT_VAR").map(|cv| cv.value.clone()),
                )
            };
            "single write" {
                vec![("INT_VAR", 1i64)] => (1usize, Some(MlxValueType::Integer(1))),
            }

            "re-write same variable replaces, count stays 1" {
                vec![("INT_VAR", 1i64), ("INT_VAR", 2i64)] => (1usize, Some(MlxValueType::Integer(2))),
            }

            "distinct second variable appends" {
                vec![("INT_VAR", 5i64), ("INT_VAR", 9i64)] => (1usize, Some(MlxValueType::Integer(9))),
            }
        );
    }

    // a profile with two distinct variables keeps both, in insertion order, and
    // variable_count reflects the total.
    #[test]
    fn distinct_variables_accumulate() {
        let profile = MlxConfigProfile::new("p", test_registry())
            .with("BOOL_VAR", true)
            .unwrap()
            .with("INT_VAR", 42i64)
            .unwrap();
        assert_eq!(profile.variable_count(), 2);
        assert_eq!(profile.variable_names(), vec!["BOOL_VAR", "INT_VAR"]);
    }

    // get_variable returns Some for a configured variable and None otherwise. We
    // configure exactly BOOL_VAR, then probe present / unconfigured / nonexistent.
    #[test]
    fn get_variable_present_and_absent() {
        let profile = MlxConfigProfile::new("p", test_registry())
            .with("BOOL_VAR", true)
            .unwrap();
        value_scenarios!(
            run = |name| profile.get_variable(name).is_some();
            "configured variable is found" {
                "BOOL_VAR" => true,
            }

            "known-but-unconfigured variable is absent" {
                "INT_VAR" => false,
            }

            "nonexistent variable is absent" {
                "MISSING" => false,
            }
        );
    }

    // validate rejects an empty profile (ProfileValidation) and accepts a profile
    // whose values were all built through `with` (each validates against its spec).
    #[test]
    fn validate_empty_vs_populated() {
        scenarios!(
            run = |populate| {
                let mut profile = MlxConfigProfile::new("p", test_registry());
                if populate {
                    profile = profile.with("BOOL_VAR", true).unwrap();
                }
                profile.validate().map_err(drop)
            };
            "empty profile is rejected" {
                false => Fails,
            }

            "populated profile validates" {
                true => Yields(()),
            }
        );
    }

    // compare/sync validate the profile first, so an empty profile fails before any
    // runner work. These exercise the early-return validation branch without
    // touching a real device.
    #[test]
    fn compare_and_sync_reject_empty_profile() {
        let empty = MlxConfigProfile::new("p", test_registry());
        assert!(empty.compare("dev", None).is_err());
        assert!(empty.sync("dev", None).is_err());
    }

    // summary picks between the with-description and without-description formats.
    // Both exact strings are derived from the format! literals in `summary`.
    #[test]
    fn summary_formats_with_and_without_description() {
        value_scenarios!(
            run = |desc: Option<&str>| {
                let mut profile = MlxConfigProfile::new("p", test_registry());
                if let Some(d) = desc {
                    profile = profile.with_description(d);
                }
                profile = profile.with("BOOL_VAR", true).unwrap();
                profile.summary()
            };
            "no description" {
                None => "Profile 'p': 1 variables for registry 'test_reg'".to_string(),
            }

            "with description" {
                Some("desc") => "Profile 'p': desc - 1 variables for registry 'test_reg'".to_string(),
            }
        );
    }

    // from_yaml / from_json resolve the registry by *name* against the global
    // registry set. An unknown registry name fails with RegistryNotFound;
    // malformed YAML/JSON fails at the parse stage. We only assert that each bad
    // input fails, since the wrapped parser error isn't PartialEq.
    #[test]
    fn from_yaml_and_json_reject_bad_input() {
        scenarios!(
            run = |yaml| MlxConfigProfile::from_yaml(yaml).map(|_| ()).map_err(drop);
            "yaml: unknown registry" {
                "name: p\nregistry_name: does_not_exist\nconfig: {}\n" => Fails,
            }

            "yaml: malformed document" {
                "name: [unterminated" => Fails,
            }
        );
        scenarios!(
            run = |json| MlxConfigProfile::from_json(json).map(|_| ()).map_err(drop);
            "json: unknown registry" {
                r#"{"name":"p","registry_name":"does_not_exist","config":{}}"# => Fails,
            }

            "json: malformed document" {
                "{not json" => Fails,
            }
        );
    }

    // a profile built on the real `mlx_generic` registry round-trips through YAML
    // and JSON: serialize, deserialize, and the configured values come back intact.
    // We project to (variable_count, SRIOV_EN value, NUM_OF_VFS value) since the
    // profile type isn't PartialEq and config ordering isn't guaranteed.
    #[test]
    fn yaml_json_round_trip_preserves_values() {
        let registry = crate::registry::registries::get("mlx_generic")
            .expect("mlx_generic registry exists")
            .clone();
        let profile = MlxConfigProfile::new("rt", registry)
            .with_description("round trip")
            .with("SRIOV_EN", true)
            .unwrap()
            .with("NUM_OF_VFS", 8i64)
            .unwrap();

        let yaml = profile.to_yaml().unwrap();
        let from_yaml = MlxConfigProfile::from_yaml(&yaml).unwrap();
        assert_eq!(from_yaml.variable_count(), 2);
        assert_eq!(
            from_yaml
                .get_variable("SRIOV_EN")
                .map(|cv| cv.value.clone()),
            Some(MlxValueType::Boolean(true))
        );
        assert_eq!(
            from_yaml
                .get_variable("NUM_OF_VFS")
                .map(|cv| cv.value.clone()),
            Some(MlxValueType::Integer(8))
        );
        assert_eq!(from_yaml.description, Some("round trip".to_string()));

        let json = profile.to_json().unwrap();
        let from_json = MlxConfigProfile::from_json(&json).unwrap();
        assert_eq!(from_json.variable_count(), 2);
        assert_eq!(
            from_json
                .get_variable("SRIOV_EN")
                .map(|cv| cv.value.clone()),
            Some(MlxValueType::Boolean(true))
        );
        assert_eq!(
            from_json
                .get_variable("NUM_OF_VFS")
                .map(|cv| cv.value.clone()),
            Some(MlxValueType::Integer(8))
        );
    }

    // from_yaml_file / from_json_file surface I/O errors for a missing path before
    // any parsing happens.
    #[test]
    fn from_file_missing_path_errors() {
        assert!(MlxConfigProfile::from_yaml_file("/nonexistent/path/profile.yaml").is_err());
        assert!(MlxConfigProfile::from_json_file("/nonexistent/path/profile.json").is_err());
    }

    // to_yaml_file / to_json_file write a serialized profile that can be read back
    // through the file loaders, closing the file round-trip loop.
    #[test]
    fn file_round_trip_via_temp_dir() {
        let registry = crate::registry::registries::get("mlx_generic")
            .expect("mlx_generic registry exists")
            .clone();
        let profile = MlxConfigProfile::new("file_rt", registry)
            .with("SRIOV_EN", false)
            .unwrap();

        let dir = std::env::temp_dir();
        let yaml_path = dir.join(format!("libmlx_profile_{}.yaml", std::process::id()));
        let json_path = dir.join(format!("libmlx_profile_{}.json", std::process::id()));

        profile.to_yaml_file(&yaml_path).unwrap();
        let loaded_yaml = MlxConfigProfile::from_yaml_file(&yaml_path).unwrap();
        assert_eq!(
            loaded_yaml
                .get_variable("SRIOV_EN")
                .map(|cv| cv.value.clone()),
            Some(MlxValueType::Boolean(false))
        );

        profile.to_json_file(&json_path).unwrap();
        let loaded_json = MlxConfigProfile::from_json_file(&json_path).unwrap();
        assert_eq!(
            loaded_json
                .get_variable("SRIOV_EN")
                .map(|cv| cv.value.clone()),
            Some(MlxValueType::Boolean(false))
        );

        let _ = std::fs::remove_file(&yaml_path);
        let _ = std::fs::remove_file(&json_path);
    }
}
