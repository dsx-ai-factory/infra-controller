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

// src/registry.rs
// This defines code for the MlxVariableRegistry, which has a name,
// defines the variables that are a part of this registry, as well
// as any device filters, as in devices which are allowed to use
// this registry.

use ::rpc::errors::RpcDataConversionError;
use ::rpc::protos::mlx_device::MlxVariableRegistry as MlxVariableRegistryPb;
use serde::{Deserialize, Serialize};

use crate::device::filters::{DeviceFilter, DeviceFilterSet};
use crate::variables::variable::MlxConfigVariable;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlxVariableRegistry {
    pub name: String,
    pub variables: Vec<MlxConfigVariable>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<DeviceFilterSet>,
}

impl MlxVariableRegistry {
    // new creates a new empty registry with the given name.
    pub fn new<N: Into<String>>(name: N) -> Self {
        Self {
            name: name.into(),
            variables: Vec::new(),
            filters: None,
        }
    }

    // name sets the registry name (builder pattern).
    pub fn name<N: Into<String>>(mut self, name: N) -> Self {
        self.name = name.into();
        self
    }

    // variables sets the variables list (builder pattern).
    pub fn variables(mut self, variables: Vec<MlxConfigVariable>) -> Self {
        self.variables = variables;
        self
    }

    // add_variable adds a single variable to the registry (builder pattern).
    pub fn add_variable(mut self, variable: MlxConfigVariable) -> Self {
        self.variables.push(variable);
        self
    }

    // with_filters sets the device filter set (builder pattern).
    pub fn with_filters(mut self, filters: DeviceFilterSet) -> Self {
        self.filters = Some(filters);
        self
    }

    // with_filter adds a single device filter to the registry (builder pattern).
    // If no filter set exists, creates a new one. If one exists, adds to it.
    pub fn with_filter(mut self, filter: DeviceFilter) -> Self {
        match self.filters {
            Some(ref mut filter_set) => {
                filter_set.add_filter(filter);
            }
            None => {
                let mut filter_set = DeviceFilterSet::new();
                filter_set.add_filter(filter);
                self.filters = Some(filter_set);
            }
        }
        self
    }

    // get_variable returns a variable from the registry,
    // or None if it's not in there.
    pub fn get_variable(&self, name: &str) -> Option<&MlxConfigVariable> {
        self.variables.iter().find(|v| v.name == name)
    }

    // variable_names returns all the variable
    // names defined in the registry.
    pub fn variable_names(&self) -> Vec<&str> {
        self.variables.iter().map(|v| v.name.as_str()).collect()
    }

    // has_filters returns whether this registry has device filters configured.
    pub fn has_filters(&self) -> bool {
        self.filters.as_ref().is_some_and(|f| f.has_filters())
    }

    // filter_summary returns a summary of configured device filters for logging.
    pub fn filter_summary(&self) -> String {
        match &self.filters {
            Some(filters) => filters.to_string(),
            None => "No filters".to_string(),
        }
    }

    // matches_device checks if a device matches this registry's filters.
    // Returns true if no filters are configured (allows all devices).
    pub fn matches_device(
        &self,
        device_info: &carbide_libmlx_model::device::info::MlxDeviceInfo,
    ) -> bool {
        self.filters
            .as_ref()
            .is_none_or(|filter_set| filter_set.matches(device_info))
    }
}

impl From<MlxVariableRegistry> for MlxVariableRegistryPb {
    fn from(registry: MlxVariableRegistry) -> Self {
        let variables: Vec<_> = registry.variables.into_iter().map(|v| v.into()).collect();

        MlxVariableRegistryPb {
            name: registry.name,
            filters: registry.filters.map(|f| f.into()),
            variables,
        }
    }
}

impl TryFrom<MlxVariableRegistryPb> for MlxVariableRegistry {
    type Error = RpcDataConversionError;

    fn try_from(pb: MlxVariableRegistryPb) -> Result<Self, Self::Error> {
        let variables: Result<Vec<_>, _> = pb.variables.into_iter().map(|v| v.try_into()).collect();

        let filters: Option<Result<DeviceFilterSet, _>> = pb.filters.map(|f| f.try_into());

        let filters = match filters {
            Some(Ok(f)) => Some(f),
            Some(Err(e)) => {
                return Err(RpcDataConversionError::InvalidArgument(format!(
                    "failed to convert filters: {e}"
                )));
            }
            None => None,
        };

        Ok(MlxVariableRegistry {
            name: pb.name,
            variables: variables?,
            filters,
        })
    }
}

#[cfg(test)]
mod coverage_tests {
    use ::rpc::protos::mlx_device::{
        DeviceFilter as DeviceFilterPb, DeviceFilterSet as DeviceFilterSetPb,
        MlxConfigVariable as MlxConfigVariablePb, MlxVariableSpec as MlxVariableSpecPb,
        mlx_variable_spec as mlx_variable_spec_pb,
    };
    use carbide_libmlx_model::device::info::MlxDeviceInfo;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;
    use crate::device::filters::{DeviceField, MatchMode};
    use crate::variables::spec::MlxVariableSpec;

    // var builds a minimal MlxConfigVariable with the given name and a Boolean
    // spec; the spec is irrelevant to registry behavior, only the name is read.
    fn var(name: &str) -> MlxConfigVariable {
        MlxConfigVariable {
            name: name.to_string(),
            description: format!("desc for {name}"),
            read_only: false,
            spec: MlxVariableSpec::Boolean,
        }
    }

    // exact_filter builds a DeviceFilter on the given field that matches `value`
    // exactly (case-insensitive), used for the matches_device cases.
    fn exact_filter(field: DeviceField, value: &str) -> DeviceFilter {
        DeviceFilter {
            field,
            values: vec![value.to_string()],
            match_mode: MatchMode::Exact,
        }
    }

    // ----- new ---------------------------------------------------------------

    // new starts empty: the provided name, no variables, and no filters.
    #[test]
    fn new_starts_empty() {
        let reg = MlxVariableRegistry::new("reg-a");
        assert_eq!(reg.name, "reg-a");
        assert!(reg.variables.is_empty());
        assert!(reg.filters.is_none());
        assert!(!reg.has_filters());
    }

    // ----- builder name / variables / add_variable ---------------------------

    // The builder setters overwrite/append as documented. Each row projects the
    // single field the setter touches so the non-PartialEq registry stays usable.
    #[test]
    fn builder_name_overwrites() {
        value_scenarios!(
            run = |new_name| MlxVariableRegistry::new("orig").name(new_name).name;
            "name() replaces the constructor name" {
                "renamed" => "renamed".to_string(),
            }

            "name() accepts an empty string" {
                "" => String::new(),
            }
        );
    }

    // variables() replaces the whole list; the projected length proves the swap.
    #[test]
    fn builder_variables_replaces_list() {
        value_scenarios!(
            run = |vars| {
                MlxVariableRegistry::new("r")
                    .add_variable(var("preexisting"))
                    .variables(vars)
                    .variables
                    .len()
            };
            "empty replacement" {
                vec![] => 0usize,
            }

            "two-element replacement" {
                vec![var("a"), var("b")] => 2usize,
            }
        );
    }

    // add_variable appends; chaining three adds yields the three names in order.
    #[test]
    fn add_variable_appends_in_order() {
        let reg = MlxVariableRegistry::new("r")
            .add_variable(var("first"))
            .add_variable(var("second"))
            .add_variable(var("third"));
        assert_eq!(reg.variable_names(), vec!["first", "second", "third"]);
    }

    // ----- get_variable ------------------------------------------------------

    // get_variable finds by exact name and returns None for a miss. The closure
    // projects the found variable's name (or "<none>") so the row stays robust.
    #[test]
    fn get_variable_finds_by_name() {
        let reg = MlxVariableRegistry::new("r")
            .add_variable(var("alpha"))
            .add_variable(var("beta"));

        value_scenarios!(
            run = |name| {
                reg.get_variable(name)
                    .map(|v| v.name.clone())
                    .unwrap_or_else(|| "<none>".to_string())
            };
            "first variable" {
                "alpha" => "alpha".to_string(),
            }

            "later variable" {
                "beta" => "beta".to_string(),
            }

            "missing variable yields None" {
                "gamma" => "<none>".to_string(),
            }

            "name match is case-sensitive" {
                "ALPHA" => "<none>".to_string(),
            }
        );
    }

    // get_variable on an empty registry is always None.
    #[test]
    fn get_variable_empty_registry_is_none() {
        let reg = MlxVariableRegistry::new("r");
        assert!(reg.get_variable("anything").is_none());
    }

    // ----- variable_names ----------------------------------------------------

    // variable_names lists names in insertion order; empty for an empty registry.
    #[test]
    fn variable_names_empty_is_empty() {
        let reg = MlxVariableRegistry::new("r");
        assert!(reg.variable_names().is_empty());
    }

    // ----- has_filters -------------------------------------------------------

    // has_filters is false with no filter set, false with an *empty* set, and
    // true only once a set holds at least one filter.
    #[test]
    fn has_filters_distinguishes_none_empty_and_nonempty() {
        value_scenarios!(
            run = |reg| reg.has_filters();
            "no filter set" {
                MlxVariableRegistry::new("r") => false,
            }

            "present but empty filter set" {
                MlxVariableRegistry::new("r").with_filters(DeviceFilterSet::new()) => false,
            }

            "non-empty filter set" {
                MlxVariableRegistry::new("r")
                .with_filter(exact_filter(DeviceField::DeviceType, "ConnectX-6 Dx")) => true,
            }
        );
    }

    // ----- with_filter (both match arms) -------------------------------------

    // with_filter on a fresh registry takes the None arm and creates a one-filter
    // set; a second with_filter takes the Some arm and appends. The projected
    // filter count distinguishes the two arms.
    #[test]
    fn with_filter_creates_then_extends() {
        value_scenarios!(
            run = |count| {
                let mut reg = MlxVariableRegistry::new("r");
                for i in 0..count {
                    reg = reg.with_filter(exact_filter(DeviceField::DeviceType, &format!("v{i}")));
                }
                reg.filters.map(|f| f.filters.len()).unwrap_or(0)
            };
            "first filter creates the set (None arm)" {
                1usize => 1usize,
            }

            "second filter extends the set (Some arm)" {
                2usize => 2usize,
            }
        );
    }

    // with_filters replaces any prior set wholesale.
    #[test]
    fn with_filters_sets_the_set() {
        let set = DeviceFilterSet::new()
            .with_filter(exact_filter(DeviceField::PartNumber, "MCX623106AN-CDAT"));
        let reg = MlxVariableRegistry::new("r").with_filters(set);
        assert!(reg.has_filters());
    }

    // ----- filter_summary ----------------------------------------------------

    // filter_summary reports "No filters" with no set, and otherwise delegates to
    // the DeviceFilterSet Display. An empty set also Displays as "No filters".
    #[test]
    fn filter_summary_reports_none_and_delegates() {
        value_scenarios!(
            run = |reg| reg.filter_summary();
            "no filter set" {
                MlxVariableRegistry::new("r") => "No filters".to_string(),
            }

            "empty set Displays as No filters" {
                MlxVariableRegistry::new("r").with_filters(DeviceFilterSet::new()) => "No filters".to_string(),
            }

            "one exact filter renders field:value:mode" {
                MlxVariableRegistry::new("r")
                .with_filter(exact_filter(DeviceField::DeviceType, "ConnectX")) => "device_type:ConnectX:exact".to_string(),
            }
        );
    }

    // ----- matches_device ----------------------------------------------------

    // matches_device allows every device when no filters are configured, allows
    // when the set is empty (vacuous all()), matches on a satisfied exact filter,
    // and rejects on an unsatisfied one. The device's device_type is
    // "ConnectX-6 Dx" and part_number "MCX623106AN-CDAT" (create_test_device).
    #[test]
    fn matches_device_respects_filters() {
        let device = MlxDeviceInfo::create_test_device();

        value_scenarios!(
            run = |reg| reg.matches_device(&device);
            "no filters allows all" {
                MlxVariableRegistry::new("r") => true,
            }

            "empty filter set allows all" {
                MlxVariableRegistry::new("r").with_filters(DeviceFilterSet::new()) => true,
            }

            "matching exact device_type filter" {
                MlxVariableRegistry::new("r")
                .with_filter(exact_filter(DeviceField::DeviceType, "ConnectX-6 Dx")) => true,
            }

            "exact filter is case-insensitive" {
                MlxVariableRegistry::new("r")
                .with_filter(exact_filter(DeviceField::DeviceType, "connectx-6 dx")) => true,
            }

            "non-matching device_type filter" {
                MlxVariableRegistry::new("r")
                .with_filter(exact_filter(DeviceField::DeviceType, "BlueField3")) => false,
            }

            "all filters must match (one fails)" {
                MlxVariableRegistry::new("r")
                .with_filter(exact_filter(DeviceField::DeviceType, "ConnectX-6 Dx"))
                .with_filter(exact_filter(DeviceField::PartNumber, "WRONG-PART")) => false,
            }
        );
    }

    // ----- pb round-trip and conversion error paths --------------------------

    // From<MlxVariableRegistry> for the pb preserves name, variable count, and
    // whether a filter set is present.
    #[test]
    fn into_pb_preserves_shape() {
        let reg = MlxVariableRegistry::new("reg-name")
            .add_variable(var("v1"))
            .add_variable(var("v2"))
            .with_filter(exact_filter(DeviceField::DeviceType, "ConnectX-6 Dx"));

        let pb: MlxVariableRegistryPb = reg.into();
        assert_eq!(pb.name, "reg-name");
        assert_eq!(pb.variables.len(), 2);
        assert!(pb.filters.is_some());
    }

    // A registry with no filters maps to a pb with filters None.
    #[test]
    fn into_pb_without_filters_has_none() {
        let pb: MlxVariableRegistryPb = MlxVariableRegistry::new("r").into();
        assert!(pb.filters.is_none());
    }

    // Round-trip Rust -> pb -> Rust preserves name, the variable names, and
    // whether filters are present. The registry is not PartialEq, so we compare
    // the projected pieces instead of the whole value.
    #[test]
    fn registry_round_trips_through_pb() {
        scenarios!(
            run = |reg: MlxVariableRegistry| {
                let pb: MlxVariableRegistryPb = reg.into();
                let back: MlxVariableRegistry = pb.try_into().map_err(drop)?;
                let names: Vec<String> = back
                    .variable_names()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let has_filters = back.has_filters();
                Ok::<_, ()>((back.name, names, has_filters))
            };
            "no filters, no variables" {
                MlxVariableRegistry::new("empty") => Yields(("empty".to_string(), Vec::<String>::new(), false)),
            }

            "variables only" {
                MlxVariableRegistry::new("vars")
                .add_variable(var("a"))
                .add_variable(var("b")) => Yields((
                    "vars".to_string(),
                    vec!["a".to_string(), "b".to_string()],
                    false,
                )),
            }

            "variables and a filter" {
                MlxVariableRegistry::new("full")
                .add_variable(var("only"))
                .with_filter(exact_filter(DeviceField::PartNumber, "MCX")) => Yields(("full".to_string(), vec!["only".to_string()], true)),
            }
        );
    }

    // TryFrom<pb> fails when a contained filter is invalid (an unspecified device
    // field, i32 = 0), surfacing an InvalidArgument from the registry conversion.
    // A variable missing its spec also fails the conversion. The happy path with a
    // valid filter succeeds. Project to Ok-vs-Err since the error isn't PartialEq.
    #[test]
    fn try_from_pb_rejects_bad_filters_and_variables() {
        fn bool_spec_pb() -> MlxVariableSpecPb {
            MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Boolean(
                    mlx_variable_spec_pb::BooleanSpec {},
                )),
            }
        }

        scenarios!(
            run = |pb: MlxVariableRegistryPb| MlxVariableRegistry::try_from(pb).map(drop).map_err(drop);
            "valid filter converts" {
                MlxVariableRegistryPb {
                    name: "ok".to_string(),
                    variables: vec![],
                    filters: Some(DeviceFilterSetPb {
                        filters: vec![DeviceFilterPb {
                            // 1 == DEVICE_FIELD_DEVICE_TYPE
                            field: 1,
                            values: vec!["x".to_string()],
                            // 2 == MATCH_MODE_EXACT
                            match_mode: 2,
                        }],
                    }),
                } => Yields(()),
            }

            "unspecified device field rejected" {
                MlxVariableRegistryPb {
                    name: "bad-field".to_string(),
                    variables: vec![],
                    filters: Some(DeviceFilterSetPb {
                        filters: vec![DeviceFilterPb {
                            // 0 == DEVICE_FIELD_UNSPECIFIED -> conversion error
                            field: 0,
                            values: vec!["x".to_string()],
                            match_mode: 2,
                        }],
                    }),
                } => Fails,
            }

            "out-of-range device field rejected" {
                MlxVariableRegistryPb {
                    name: "bad-field-range".to_string(),
                    variables: vec![],
                    filters: Some(DeviceFilterSetPb {
                        filters: vec![DeviceFilterPb {
                            field: 999,
                            values: vec!["x".to_string()],
                            match_mode: 2,
                        }],
                    }),
                } => Fails,
            }

            "variable missing its spec rejected" {
                MlxVariableRegistryPb {
                    name: "bad-var".to_string(),
                    variables: vec![MlxConfigVariablePb {
                        name: "v".to_string(),
                        description: "d".to_string(),
                        read_only: false,
                        spec: None,
                    }],
                    filters: None,
                } => Fails,
            }

            "no filters, valid variable converts" {
                MlxVariableRegistryPb {
                    name: "plain".to_string(),
                    variables: vec![MlxConfigVariablePb {
                        name: "v".to_string(),
                        description: "d".to_string(),
                        read_only: true,
                        spec: Some(bool_spec_pb()),
                    }],
                    filters: None,
                } => Yields(()),
            }
        );
    }
}
