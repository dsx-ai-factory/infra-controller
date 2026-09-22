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

// tests/traits_tests.rs
// Tests for MlxConfigSettable and MlxConfigQueryable traits

use carbide_test_support::Outcome::*;
use carbide_test_support::{scenarios, value_scenarios};
use libmlx::runner::traits::{self, MlxConfigQueryable, MlxConfigSettable};
use libmlx::variables::value::MlxValueType;

use super::common;

#[test]
fn test_mlx_config_settable_vec_config_value() {
    let registry = common::create_test_registry();
    let sriov_var = registry.get_variable("SRIOV_EN").unwrap();
    let config_value = sriov_var.with(true).unwrap();

    let config_values = vec![config_value.clone()];
    let result = config_values.to_config_values(&registry).unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name(), "SRIOV_EN");
    assert_eq!(result[0].value, MlxValueType::Boolean(true));
}

// `to_config_values` over string-tuple assignments works whether they're passed
// as an owned array `[...]` or an array reference `&[...]`. Each row calls the
// trait on its own container type (identity closure) and projects the unordered
// result to name->value pairs sorted by name.
//
// The `&[...]` borrow is load-bearing: it selects the `&[T; N]` trait impl, which
// is the distinct coverage this row adds over the owned-array row, so the borrow
// must stay despite clippy seeing it as droppable.
#[allow(clippy::needless_borrows_for_generic_args)]
#[test]
fn config_values_from_string_tuples_across_container_forms() {
    fn pairs<T: MlxConfigSettable>(
        assignments: T,
        registry: &libmlx::variables::registry::MlxVariableRegistry,
    ) -> Vec<(String, MlxValueType)> {
        let mut pairs: Vec<(String, MlxValueType)> = assignments
            .to_config_values(registry)
            .unwrap()
            .iter()
            .map(|v| (v.name().to_string(), v.value.clone()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    let registry = common::create_test_registry();

    value_scenarios!(
        run = |p| p;
        "owned array [...]" {
            pairs(
                [
                    ("SRIOV_EN", "true"),
                    ("NUM_OF_VFS", "16"),
                    ("POWER_MODE", "HIGH"),
                ],
                &registry,
            ) => vec![
                ("NUM_OF_VFS".to_string(), MlxValueType::Integer(16)),
                ("POWER_MODE".to_string(), MlxValueType::Enum("HIGH".to_string())),
                ("SRIOV_EN".to_string(), MlxValueType::Boolean(true)),
            ],
        }

        "array reference &[...]" {
            pairs(
                &[("SRIOV_EN", "false"), ("NUM_OF_VFS", "32")],
                &registry,
            ) => vec![
                ("NUM_OF_VFS".to_string(), MlxValueType::Integer(32)),
                ("SRIOV_EN".to_string(), MlxValueType::Boolean(false)),
            ],
        }
    );
}

#[test]
fn test_mlx_config_settable_different_array_sizes() {
    let registry = common::create_test_registry();

    // Test single element array
    let single = [("SRIOV_EN", "true")];
    let result = single.to_config_values(&registry).unwrap();
    assert_eq!(result.len(), 1);

    // Test larger arrays
    let large = [
        ("SRIOV_EN", "true"),
        ("NUM_OF_VFS", "16"),
        ("POWER_MODE", "HIGH"),
        ("PERFORMANCE_PRESET", "5"),
        ("DEVICE_NAME", "test-device"),
    ];
    let result = large.to_config_values(&registry).unwrap();
    assert_eq!(result.len(), 5);
}

// `to_config_values` over indexed array assignments builds the same sparse
// arrays whether passed as an owned array `[...]` or an array reference `&[...]`.
// Each row calls the trait on its own container type and projects the result to
// name->value pairs (the full array value) sorted by name. The expected arrays
// pin the same per-index slots the originals asserted: GPIO_ENABLED size 4 with
// [0]=true,[2]=false and GPIO_MODES size 8 with [1]=output,[3]=bidirectional.
//
// The `&[...]` borrow is load-bearing: it selects the `&[T; N]` trait impl, which
// is the distinct coverage this row adds over the owned-array row, so the borrow
// must stay despite clippy seeing it as droppable.
#[allow(clippy::needless_borrows_for_generic_args)]
#[test]
fn config_values_from_indexed_arrays_across_container_forms() {
    fn pairs<T: MlxConfigSettable>(
        assignments: T,
        registry: &libmlx::variables::registry::MlxVariableRegistry,
    ) -> Vec<(String, MlxValueType)> {
        let mut pairs: Vec<(String, MlxValueType)> = assignments
            .to_config_values(registry)
            .unwrap()
            .iter()
            .map(|v| (v.name().to_string(), v.value.clone()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    let registry = common::create_test_registry();

    let expected = || {
        vec![
            (
                "GPIO_ENABLED".to_string(),
                MlxValueType::BooleanArray(vec![Some(true), None, Some(false), None]),
            ),
            (
                "GPIO_MODES".to_string(),
                MlxValueType::EnumArray(vec![
                    None,
                    Some("output".to_string()),
                    None,
                    Some("bidirectional".to_string()),
                    None,
                    None,
                    None,
                    None,
                ]),
            ),
        ]
    };

    value_scenarios!(
        run = |p| p;
        "owned array [...]" {
            pairs(
                [
                    ("GPIO_ENABLED[0]", "true"),
                    ("GPIO_ENABLED[2]", "false"),
                    ("GPIO_MODES[1]", "output"),
                    ("GPIO_MODES[3]", "bidirectional"),
                ],
                &registry,
            ) => expected(),
        }

        "array reference &[...]" {
            pairs(
                &[
                    ("GPIO_ENABLED[0]", "true"),
                    ("GPIO_ENABLED[2]", "false"),
                    ("GPIO_MODES[1]", "output"),
                    ("GPIO_MODES[3]", "bidirectional"),
                ],
                &registry,
            ) => expected(),
        }
    );
}

#[test]
fn test_mlx_config_settable_vec_string_tuples() {
    let registry = common::create_test_registry();

    let assignments = vec![
        ("SRIOV_EN".to_string(), "false".to_string()),
        ("NUM_OF_VFS".to_string(), "32".to_string()),
    ];

    let result = assignments.to_config_values(&registry).unwrap();

    assert_eq!(result.len(), 2);

    let sriov = result.iter().find(|v| v.name() == "SRIOV_EN").unwrap();
    assert_eq!(sriov.value, MlxValueType::Boolean(false));

    let vfs = result.iter().find(|v| v.name() == "NUM_OF_VFS").unwrap();
    assert_eq!(vfs.value, MlxValueType::Integer(32));
}

#[test]
fn test_mlx_config_settable_variable_not_found() {
    let registry = common::create_test_registry();

    let assignments = &[("NONEXISTENT_VAR", "value")];

    let result = assignments.to_config_values(&registry);
    assert!(result.is_err());

    if let Err(libmlx::runner::error::MlxRunnerError::VariableNotFound { variable_name }) = result {
        assert_eq!(variable_name, "NONEXISTENT_VAR");
    } else {
        panic!("Expected VariableNotFound error");
    }
}

// `to_variable_names` over scalar variable names yields them unchanged whether
// passed as an owned array `[...]` or an array reference `&[...]`. Each row calls
// the trait on its own container type and projects to a sorted name list.
//
// The `&[...]` borrow is load-bearing: it selects the `&[T; N]` trait impl, which
// is the distinct coverage this row adds over the owned-array row, so the borrow
// must stay despite clippy seeing it as droppable.
#[allow(clippy::needless_borrows_for_generic_args)]
#[test]
fn variable_names_from_scalars_across_container_forms() {
    fn names<T: MlxConfigQueryable>(
        variables: T,
        registry: &libmlx::variables::registry::MlxVariableRegistry,
    ) -> Vec<String> {
        let mut names = variables.to_variable_names(registry).unwrap();
        names.sort();
        names
    }

    let registry = common::create_test_registry();
    let expected = || {
        vec![
            "NUM_OF_VFS".to_string(),
            "POWER_MODE".to_string(),
            "SRIOV_EN".to_string(),
        ]
    };

    value_scenarios!(
        run = |n| n;
        "owned array [...]" {
            names(["SRIOV_EN", "NUM_OF_VFS", "POWER_MODE"], &registry) => expected(),
        }

        "array reference &[...]" {
            names(&["SRIOV_EN", "NUM_OF_VFS", "POWER_MODE"], &registry) => expected(),
        }
    );
}

#[test]
fn test_mlx_config_queryable_different_array_sizes() {
    let registry = common::create_test_registry();

    // Test single element array
    let single = ["SRIOV_EN"];
    let result = single.to_variable_names(&registry).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.contains(&"SRIOV_EN".to_string()));

    // Test larger arrays
    let large = ["SRIOV_EN", "NUM_OF_VFS", "POWER_MODE", "DEVICE_NAME"];
    let result = large.to_variable_names(&registry).unwrap();
    assert_eq!(result.len(), 4);
    assert!(result.contains(&"SRIOV_EN".to_string()));
    assert!(result.contains(&"NUM_OF_VFS".to_string()));
    assert!(result.contains(&"POWER_MODE".to_string()));
    assert!(result.contains(&"DEVICE_NAME".to_string()));
}

#[test]
fn test_mlx_config_queryable_vec_string() {
    let registry = common::create_test_registry();

    let variables = vec!["SRIOV_EN".to_string(), "DEVICE_NAME".to_string()];
    let result = variables.to_variable_names(&registry).unwrap();

    assert_eq!(result.len(), 2);
    assert!(result.contains(&"SRIOV_EN".to_string()));
    assert!(result.contains(&"DEVICE_NAME".to_string()));
}

// A full array name expands to one entry per index, sized from the registry
// spec; passing several array names expands them together. This folds the former
// `array_references` (spot-checked `GPIO_ENABLED[0]`/`THERMAL_SENSORS[5]`) and
// `array_expansion` (listed every index) tests -- same `&[...]` input and data --
// into one row that pins the entire combined expansion: GPIO_ENABLED size 4 +
// THERMAL_SENSORS size 6 = 10 indexed names.
#[test]
fn variable_names_expand_full_arrays_to_indices() {
    fn names<T: MlxConfigQueryable>(
        variables: T,
        registry: &libmlx::variables::registry::MlxVariableRegistry,
    ) -> Vec<String> {
        let mut names = variables.to_variable_names(registry).unwrap();
        names.sort();
        names
    }

    let registry = common::create_test_registry();

    value_scenarios!(
        run = |n| n;
        "a full array name expands to every index of each named array" {
            names(["GPIO_ENABLED", "THERMAL_SENSORS"], &registry) => vec![
                "GPIO_ENABLED[0]".to_string(),
                "GPIO_ENABLED[1]".to_string(),
                "GPIO_ENABLED[2]".to_string(),
                "GPIO_ENABLED[3]".to_string(),
                "THERMAL_SENSORS[0]".to_string(),
                "THERMAL_SENSORS[1]".to_string(),
                "THERMAL_SENSORS[2]".to_string(),
                "THERMAL_SENSORS[3]".to_string(),
                "THERMAL_SENSORS[4]".to_string(),
                "THERMAL_SENSORS[5]".to_string(),
            ],
        }
    );
}

#[test]
fn test_mlx_config_queryable_variable_not_found() {
    let registry = common::create_test_registry();

    let variables = &["SRIOV_EN", "NONEXISTENT_VAR"];
    let result = variables.to_variable_names(&registry);

    assert!(result.is_err());
    if let Err(libmlx::runner::error::MlxRunnerError::VariableNotFound { variable_name }) = result {
        assert_eq!(variable_name, "NONEXISTENT_VAR");
    } else {
        panic!("Expected VariableNotFound error");
    }
}

#[test]
fn test_mlx_config_queryable_vec_variables() {
    let registry = common::create_test_registry();

    let variables = vec![
        registry.get_variable("SRIOV_EN").unwrap().clone(),
        registry.get_variable("NUM_OF_VFS").unwrap().clone(),
    ];

    let result = variables.to_variable_names(&registry).unwrap();

    assert_eq!(result.len(), 2);
    assert!(result.contains(&"SRIOV_EN".to_string()));
    assert!(result.contains(&"NUM_OF_VFS".to_string()));
}

// `parse_array_index` splits a `NAME[idx]` string into its base name and index,
// returns None for a plain (non-indexed) name, and errors on a malformed index.
// The runner error isn't PartialEq, so the malformed rows use `Fails`; the rest
// pin the exact `Option<(name, index)>` the parse should yield.
#[test]
fn parse_array_index_splits_name_and_index() {
    scenarios!(
        run = |name| traits::parse_array_index(name).map_err(drop);
        "'ARRAY_VAR[0]'" {
            "ARRAY_VAR[0]" => Yields(Some(("ARRAY_VAR".to_string(), 0))),
        }

        "'GPIO_ENABLED[15]'" {
            "GPIO_ENABLED[15]" => Yields(Some(("GPIO_ENABLED".to_string(), 15))),
        }

        "'COMPLEX_ARRAY_NAME[999]'" {
            "COMPLEX_ARRAY_NAME[999]" => Yields(Some(("COMPLEX_ARRAY_NAME".to_string(), 999))),
        }

        "'SRIOV_EN' is not an indexed name" {
            "SRIOV_EN" => Yields(None),
        }

        "'POWER_MODE' is not an indexed name" {
            "POWER_MODE" => Yields(None),
        }

        "'invalid[0]' lowercase name is not an index" {
            "invalid[0]" => Yields(None),
        }

        "'INVALID[]' has an empty index" {
            "INVALID[]" => Fails,
        }

        "'VAR[not_a_number]' has a non-numeric index" {
            "VAR[not_a_number]" => Fails,
        }
    );
}

#[test]
fn test_expand_variable_for_query() {
    let registry = common::create_test_registry();

    // Test scalar variable expansion
    let sriov_var = registry.get_variable("SRIOV_EN").unwrap();
    let result = traits::expand_variable_for_query(sriov_var);
    assert_eq!(result, vec!["SRIOV_EN".to_string()]);

    // Test boolean array expansion
    let gpio_var = registry.get_variable("GPIO_ENABLED").unwrap();
    let result = traits::expand_variable_for_query(gpio_var);
    assert_eq!(
        result,
        vec![
            "GPIO_ENABLED[0]".to_string(),
            "GPIO_ENABLED[1]".to_string(),
            "GPIO_ENABLED[2]".to_string(),
            "GPIO_ENABLED[3]".to_string(),
        ]
    );

    // Test integer array expansion
    let thermal_var = registry.get_variable("THERMAL_SENSORS").unwrap();
    let result = traits::expand_variable_for_query(thermal_var);
    assert_eq!(
        result,
        vec![
            "THERMAL_SENSORS[0]".to_string(),
            "THERMAL_SENSORS[1]".to_string(),
            "THERMAL_SENSORS[2]".to_string(),
            "THERMAL_SENSORS[3]".to_string(),
            "THERMAL_SENSORS[4]".to_string(),
            "THERMAL_SENSORS[5]".to_string(),
        ]
    );

    // Test enum array expansion
    let gpio_modes_var = registry.get_variable("GPIO_MODES").unwrap();
    let result = traits::expand_variable_for_query(gpio_modes_var);
    assert_eq!(result.len(), 8); // Size 8 from registry
    assert_eq!(result[0], "GPIO_MODES[0]".to_string());
    assert_eq!(result[7], "GPIO_MODES[7]".to_string());
}

#[test]
fn test_build_sparse_array_value_boolean() {
    let registry = common::create_test_registry();
    let gpio_var = registry.get_variable("GPIO_ENABLED").unwrap();

    let indices = vec![(0, true), (2, false)];
    let result = traits::build_sparse_array_value(gpio_var, indices).unwrap();

    if let MlxValueType::BooleanArray(values) = &result.value {
        assert_eq!(values.len(), 4);
        assert_eq!(values[0], Some(true));
        assert_eq!(values[1], None);
        assert_eq!(values[2], Some(false));
        assert_eq!(values[3], None);
    } else {
        panic!("Expected BooleanArray");
    }
}

#[test]
fn test_build_sparse_array_value_integer() {
    let registry = common::create_test_registry();
    let thermal_var = registry.get_variable("THERMAL_SENSORS").unwrap();

    let indices = vec![(1, 42i64), (3, 38i64), (5, 40i64)];
    let result = traits::build_sparse_array_value(thermal_var, indices).unwrap();

    if let MlxValueType::IntegerArray(values) = &result.value {
        assert_eq!(values.len(), 6);
        assert_eq!(values[0], None);
        assert_eq!(values[1], Some(42));
        assert_eq!(values[2], None);
        assert_eq!(values[3], Some(38));
        assert_eq!(values[4], None);
        assert_eq!(values[5], Some(40));
    } else {
        panic!("Expected IntegerArray");
    }
}

#[test]
fn test_build_sparse_array_value_enum() {
    let registry = common::create_test_registry();
    let gpio_modes_var = registry.get_variable("GPIO_MODES").unwrap();

    let indices = vec![(0, "input"), (2, "output"), (7, "bidirectional")];
    let result = traits::build_sparse_array_value(gpio_modes_var, indices).unwrap();

    if let MlxValueType::EnumArray(values) = &result.value {
        assert_eq!(values.len(), 8);
        assert_eq!(values[0], Some("input".to_string()));
        assert_eq!(values[1], None);
        assert_eq!(values[2], Some("output".to_string()));
        assert_eq!(values[3], None);
        assert_eq!(values[4], None);
        assert_eq!(values[5], None);
        assert_eq!(values[6], None);
        assert_eq!(values[7], Some("bidirectional".to_string()));
    } else {
        panic!("Expected EnumArray");
    }
}

#[test]
fn test_build_sparse_array_value_out_of_bounds() {
    let registry = common::create_test_registry();
    let gpio_var = registry.get_variable("GPIO_ENABLED").unwrap(); // Size 4

    let indices = vec![(0, true), (4, false)]; // Index 4 is out of bounds
    let result = traits::build_sparse_array_value(gpio_var, indices);

    assert!(result.is_err());
    if let Err(libmlx::runner::error::MlxRunnerError::ArraySizeMismatch {
        expected, found, ..
    }) = result
    {
        assert_eq!(expected, 4);
        assert_eq!(found, 5); // Index 4 + 1
    } else {
        panic!("Expected ArraySizeMismatch error");
    }
}

#[test]
fn test_build_sparse_array_value_invalid_enum() {
    let registry = common::create_test_registry();
    let gpio_modes_var = registry.get_variable("GPIO_MODES").unwrap();

    let indices = vec![(0, "invalid_mode")];
    let result = traits::build_sparse_array_value(gpio_modes_var, indices);

    assert!(result.is_err());
}

// `get_array_size_from_spec` reads the declared size off any array spec and errors
// on a scalar spec. One row per array variant plus the scalar rejection. The runner
// error isn't PartialEq, so the scalar case uses `Fails`.
#[test]
fn get_array_size_from_spec_reads_array_sizes() {
    use libmlx::variables::spec::MlxVariableSpec;

    scenarios!(
        run = |spec| traits::get_array_size_from_spec(&spec).map_err(drop);
        "boolean array" {
            MlxVariableSpec::builder()
            .boolean_array()
            .with_size(4)
            .build() => Yields(4),
        }

        "integer array" {
            MlxVariableSpec::builder()
            .integer_array()
            .with_size(6)
            .build() => Yields(6),
        }

        "enum array" {
            MlxVariableSpec::builder()
            .enum_array()
            .with_options(vec!["a".to_string(), "b".to_string()])
            .with_size(8)
            .build() => Yields(8),
        }

        "binary array" {
            MlxVariableSpec::builder()
            .binary_array()
            .with_size(2)
            .build() => Yields(2),
        }

        "a scalar spec has no array size" {
            MlxVariableSpec::builder().boolean().build() => Fails,
        }
    );
}

#[test]
fn test_mixed_variables_and_arrays() {
    let registry = common::create_test_registry();

    let assignments = &[
        ("SRIOV_EN", "true"),         // Regular boolean
        ("NUM_OF_VFS", "32"),         // Regular integer
        ("GPIO_ENABLED[0]", "true"),  // Array index
        ("GPIO_ENABLED[3]", "false"), // Array index
        ("POWER_MODE", "HIGH"),       // Regular enum
        ("GPIO_MODES[1]", "output"),  // Array index
    ];

    let result = assignments.to_config_values(&registry).unwrap();

    // Should have 5 config values: SRIOV_EN, NUM_OF_VFS, GPIO_ENABLED array, POWER_MODE, GPIO_MODES array
    assert_eq!(result.len(), 5);

    // Verify regular variables
    let sriov = result.iter().find(|v| v.name() == "SRIOV_EN").unwrap();
    assert_eq!(sriov.value, MlxValueType::Boolean(true));

    let vfs = result.iter().find(|v| v.name() == "NUM_OF_VFS").unwrap();
    assert_eq!(vfs.value, MlxValueType::Integer(32));

    let power = result.iter().find(|v| v.name() == "POWER_MODE").unwrap();
    assert_eq!(power.value, MlxValueType::Enum("HIGH".to_string()));

    // Verify sparse arrays
    let gpio_enabled = result.iter().find(|v| v.name() == "GPIO_ENABLED").unwrap();
    if let MlxValueType::BooleanArray(values) = &gpio_enabled.value {
        assert_eq!(values[0], Some(true));
        assert_eq!(values[1], None);
        assert_eq!(values[2], None);
        assert_eq!(values[3], Some(false));
    } else {
        panic!("Expected BooleanArray for GPIO_ENABLED");
    }

    let gpio_modes = result.iter().find(|v| v.name() == "GPIO_MODES").unwrap();
    if let MlxValueType::EnumArray(values) = &gpio_modes.value {
        assert_eq!(values[0], None);
        assert_eq!(values[1], Some("output".to_string()));
        assert!(values[2..8].iter().all(|v| v.is_none()));
    } else {
        panic!("Expected EnumArray for GPIO_MODES");
    }
}

#[test]
fn test_mlx_config_queryable_mixed_array_and_indices() {
    let registry = common::create_test_registry();

    // Test mixing full array names with specific indices
    let variables = &[
        "SRIOV_EN",           // Regular variable
        "GPIO_ENABLED",       // Full array (should expand)
        "THERMAL_SENSORS[1]", // Specific index
        "GPIO_MODES[7]",      // Specific index
    ];
    let result = variables.to_variable_names(&registry).unwrap();

    // Should have: SRIOV_EN + 4 GPIO_ENABLED indices + 1 THERMAL_SENSORS index + 1 GPIO_MODES index
    assert_eq!(result.len(), 7); // 1 + 4 + 1 + 1

    // Verify regular variable
    assert!(result.contains(&"SRIOV_EN".to_string()));

    // Verify full array expansion
    assert!(result.contains(&"GPIO_ENABLED[0]".to_string()));
    assert!(result.contains(&"GPIO_ENABLED[1]".to_string()));
    assert!(result.contains(&"GPIO_ENABLED[2]".to_string()));
    assert!(result.contains(&"GPIO_ENABLED[3]".to_string()));

    // Verify specific indices
    assert!(result.contains(&"THERMAL_SENSORS[1]".to_string()));
    assert!(result.contains(&"GPIO_MODES[7]".to_string()));

    // Should NOT contain other THERMAL_SENSORS or GPIO_MODES indices
    assert!(!result.contains(&"THERMAL_SENSORS[0]".to_string()));
    assert!(!result.contains(&"GPIO_MODES[0]".to_string()));
}

#[test]
fn test_mlx_config_queryable_array_index_out_of_bounds() {
    let registry = common::create_test_registry();

    // GPIO_ENABLED has size 4, so index 4 is out of bounds
    let variables = &["GPIO_ENABLED[4]"];
    let result = variables.to_variable_names(&registry);

    assert!(result.is_err());
    if let Err(libmlx::runner::error::MlxRunnerError::ArraySizeMismatch {
        variable_name,
        expected,
        found,
    }) = result
    {
        assert_eq!(variable_name, "GPIO_ENABLED");
        assert_eq!(expected, 4);
        assert_eq!(found, 5); // index 4 + 1
    } else {
        panic!("Expected ArraySizeMismatch error");
    }
}

#[test]
fn test_mlx_config_queryable_array_index_base_variable_not_found() {
    let registry = common::create_test_registry();

    // Test array index syntax with non-existent base variable
    let variables = &["NONEXISTENT_ARRAY[0]"];
    let result = variables.to_variable_names(&registry);

    assert!(result.is_err());
    if let Err(libmlx::runner::error::MlxRunnerError::VariableNotFound { variable_name }) = result {
        assert_eq!(variable_name, "NONEXISTENT_ARRAY");
    } else {
        panic!("Expected VariableNotFound error");
    }
}

// Querying a single explicit array index validates that index against the array's
// size from the registry spec: an in-bounds index is preserved verbatim (size 1,
// the name unchanged), an out-of-bounds one is rejected. Folds the per-type
// validation cases and the boundary edge cases into one table. The runner error
// isn't PartialEq, so rejections use `Fails`; a success yields the single-name vec,
// which pins both the length and the preserved name.
#[test]
fn array_index_query_validates_against_spec_size() {
    let registry = common::create_test_registry();

    scenarios!(
        run = |var_name| (&[var_name]).to_variable_names(&registry).map_err(drop);
        "GPIO_ENABLED[0]: boolean array size 4, first index" {
            "GPIO_ENABLED[0]" => Yields(vec!["GPIO_ENABLED[0]".to_string()]),
        }

        "GPIO_ENABLED[3]: boolean array size 4, last valid index" {
            "GPIO_ENABLED[3]" => Yields(vec!["GPIO_ENABLED[3]".to_string()]),
        }

        "GPIO_ENABLED[4]: boolean array size 4, out of bounds" {
            "GPIO_ENABLED[4]" => Fails,
        }

        "THERMAL_SENSORS[5]: integer array size 6, last valid index" {
            "THERMAL_SENSORS[5]" => Yields(vec!["THERMAL_SENSORS[5]".to_string()]),
        }

        "THERMAL_SENSORS[6]: integer array size 6, out of bounds" {
            "THERMAL_SENSORS[6]" => Fails,
        }

        "GPIO_MODES[0]: enum array size 8, first index" {
            "GPIO_MODES[0]" => Yields(vec!["GPIO_MODES[0]".to_string()]),
        }

        "GPIO_MODES[7]: enum array size 8, last valid index" {
            "GPIO_MODES[7]" => Yields(vec!["GPIO_MODES[7]".to_string()]),
        }

        "GPIO_MODES[8]: enum array size 8, out of bounds" {
            "GPIO_MODES[8]" => Fails,
        }
    );
}

#[test]
fn test_mlx_config_queryable_array_index_with_non_array_variable() {
    let registry = common::create_test_registry();

    // Test what happens when we try to use array syntax on a non-array variable
    // This should fail when trying to get array size from the spec
    let variables = &["SRIOV_EN[0]"]; // SRIOV_EN is boolean, not array
    let result = variables.to_variable_names(&registry);

    assert!(result.is_err());
    // Should get a ValueConversion error when trying to get array size from boolean spec
}
