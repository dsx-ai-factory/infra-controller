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

// src/traits.rs
// This implements various traits for working with mlxconfig-runner
// to make working with different variable types and results a lot
// cleaner. The two main ones are MlxConfigSettable and
// MlxConfigQueryable, which allow callers to pass different types
// of data to {set, query, compare} and {query}, and we will handle
// converting them to an underlying type that can be processed by
// the runner. A lot of this happened because I wanted to be able
// to pass Vec<MlxConfigValues> (for user-defined inputs), but I
// also wanted to be able to pass the raw JSON data we get back
// the devices. Doing this allows us to be more flexible at the
// call sites.

use std::collections::HashMap;

use regex::Regex;

use crate::runner::error::MlxRunnerError;
use crate::variables::registry::MlxVariableRegistry;
use crate::variables::spec::MlxVariableSpec;
use crate::variables::value::{IntoMlxValue, MlxConfigValue, MlxValueType};
use crate::variables::variable::MlxConfigVariable;

// MlxConfigSettable is a trait for types that can be converted to
// a Vec<MlxConfigValue> for set/sync operations.
pub trait MlxConfigSettable {
    // to_config_values converts to a vector of configuration values,
    // resolving variable names from registry.
    fn to_config_values(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<MlxConfigValue>, MlxRunnerError>;
}

// MlxConfigQueryable is a trait for types that can be converted to
// variable names for query operations.
pub trait MlxConfigQueryable {
    // to_variable_names converts to a vector of variable names,
    // expanding arrays as needed.
    fn to_variable_names(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError>;
}

// MlxConfigSettable implementation for Vec<MlxConfigValue>,
// which there's nothing to do, because  we're already in the
// format we want!
impl MlxConfigSettable for Vec<MlxConfigValue> {
    fn to_config_values(
        self,
        _registry: &MlxVariableRegistry,
    ) -> Result<Vec<MlxConfigValue>, MlxRunnerError> {
        Ok(self)
    }
}

impl MlxConfigSettable for &Vec<MlxConfigValue> {
    fn to_config_values(
        self,
        _registry: &MlxVariableRegistry,
    ) -> Result<Vec<MlxConfigValue>, MlxRunnerError> {
        Ok(self.clone())
    }
}

// MlxConfigSettable implementation for string-based variable
// specifications; requires T to be cloneable and debuggable for
// error handling.
//
// This will sort between array and non-array variable types, and
// then do the necessary work to build sparse arrays from any
// variable indices whose values are set.
impl<T> MlxConfigSettable for &[(&str, T)]
where
    T: IntoMlxValue + Clone + std::fmt::Debug,
{
    fn to_config_values(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<MlxConfigValue>, MlxRunnerError> {
        let mut config_values = Vec::new();
        let mut sparse_arrays: HashMap<String, Vec<(usize, T)>> = HashMap::new();

        // First, separate regular variables from array indices.
        for (var_name, value) in self {
            if let Some((base_name, index)) = parse_array_index(var_name)? {
                // Handle a sparse array index.
                sparse_arrays
                    .entry(base_name)
                    .or_default()
                    .push((index, value.clone()));
            } else {
                // Handle a regular variable.
                let variable = registry.get_variable(var_name).ok_or_else(|| {
                    MlxRunnerError::VariableNotFound {
                        variable_name: var_name.to_string(),
                    }
                })?;

                let config_value = variable.with(value.clone()).map_err(|error| {
                    MlxRunnerError::value_conversion(
                        var_name.to_string(),
                        format!("{value:?}"),
                        error,
                    )
                })?;

                config_values.push(config_value);
            }
        }

        // Then, build sparse arrays for those needing it.
        for (array_name, indices) in sparse_arrays {
            let variable = registry.get_variable(&array_name).ok_or_else(|| {
                MlxRunnerError::VariableNotFound {
                    variable_name: array_name.clone(),
                }
            })?;

            let sparse_value = build_sparse_array_value(variable, indices)?;
            config_values.push(sparse_value);
        }

        Ok(config_values)
    }
}

// MlxConfigSettable implementation for Vec<(String, String)>,
// which ultimately delegates to the existing &[(&str, T)]
// implementation.
impl MlxConfigSettable for Vec<(String, String)> {
    fn to_config_values(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<MlxConfigValue>, MlxRunnerError> {
        let str_refs: Vec<(&str, &str)> =
            self.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        str_refs.as_slice().to_config_values(registry)
    }
}

// Add implementations for common array sizes
impl<T, const N: usize> MlxConfigSettable for [(&str, T); N]
where
    T: IntoMlxValue + Clone + std::fmt::Debug,
{
    fn to_config_values(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<MlxConfigValue>, MlxRunnerError> {
        self.as_slice().to_config_values(registry)
    }
}

impl<T, const N: usize> MlxConfigSettable for &[(&str, T); N]
where
    T: IntoMlxValue + Clone + std::fmt::Debug,
{
    fn to_config_values(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<MlxConfigValue>, MlxRunnerError> {
        self.as_slice().to_config_values(registry)
    }
}

// MlxConfigQueryable implementation for string slices.
impl MlxConfigQueryable for &[&str] {
    fn to_variable_names(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError> {
        let mut names = Vec::new();
        for var_name in self {
            // Check if this is a specific array index like "ARRAY[0]"
            if let Some((base_name, index)) = parse_array_index(var_name)? {
                // For specific array indices, we want to query that exact index
                // Validate that the base variable exists and is an array
                let variable = registry.get_variable(&base_name).ok_or_else(|| {
                    MlxRunnerError::VariableNotFound {
                        variable_name: base_name.clone(),
                    }
                })?;

                // Validate that the index is within bounds
                let array_size = get_array_size_from_spec(&variable.spec)?;
                if index >= array_size {
                    return Err(MlxRunnerError::ArraySizeMismatch {
                        variable_name: base_name,
                        expected: array_size,
                        found: index + 1,
                    });
                }

                // Add the exact indexed variable name
                names.push(var_name.to_string());
            } else {
                // For regular variables or base array names, expand as before
                if let Some(variable) = registry.get_variable(var_name) {
                    names.extend(expand_variable_for_query(variable));
                } else {
                    return Err(MlxRunnerError::VariableNotFound {
                        variable_name: var_name.to_string(),
                    });
                }
            }
        }
        Ok(names)
    }
}

// MlxConfigQueryable implementation for Vec<String>.
impl MlxConfigQueryable for Vec<String> {
    fn to_variable_names(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError> {
        let str_refs: Vec<&str> = self.iter().map(|s| s.as_str()).collect();
        str_refs.as_slice().to_variable_names(registry)
    }
}

// MlxConfigQueryable implementation for &[String].
impl MlxConfigQueryable for &[String] {
    fn to_variable_names(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError> {
        let str_refs: Vec<&str> = self.iter().map(|s| s.as_str()).collect();
        str_refs.as_slice().to_variable_names(registry)
    }
}

// MlxConfigQueryable implementation for &MlxConfigVariable.
impl MlxConfigQueryable for &[&MlxConfigVariable] {
    fn to_variable_names(
        self,
        _registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError> {
        let mut names = Vec::new();
        for variable in self {
            names.extend(expand_variable_for_query(variable));
        }
        Ok(names)
    }
}

// MlxConfigQueryable implementation for Vec<MlxConfigVariable>,
// which ultimately delegates to &MlxConfigVariable.
impl MlxConfigQueryable for Vec<MlxConfigVariable> {
    fn to_variable_names(
        self,
        _registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError> {
        let var_refs: Vec<&MlxConfigVariable> = self.iter().collect();
        var_refs.as_slice().to_variable_names(_registry)
    }
}

impl<const N: usize> MlxConfigQueryable for [&str; N] {
    fn to_variable_names(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError> {
        self.as_slice().to_variable_names(registry)
    }
}

impl<const N: usize> MlxConfigQueryable for &[&str; N] {
    fn to_variable_names(
        self,
        registry: &MlxVariableRegistry,
    ) -> Result<Vec<String>, MlxRunnerError> {
        self.as_slice().to_variable_names(registry)
    }
}

// parse_array_index parses array index syntax (e.g. "ARRAY_VAR[index]")
// into a (base_name, index) tuple.
pub fn parse_array_index(var_name: &str) -> Result<Option<(String, usize)>, MlxRunnerError> {
    // Note that I used to have this as \d+, but that would make it so INVALID[]
    // didn't match the regex and would return "Ok(None)", when really we'd want
    // it to be an error, so I changed it to \d* to let parse::<usize> naturally
    // fail and return an error for something janky like that, but then that
    // wouldn't catch strings, so it eventually just became .* in there.
    let re = Regex::new(r"^([A-Z][A-Z0-9_]+)\[(.*)\]$").unwrap();

    if let Some(captures) = re.captures(var_name) {
        let base_name = captures[1].to_string();
        let index =
            captures[2]
                .parse::<usize>()
                .map_err(|_| MlxRunnerError::InvalidArrayIndex {
                    variable_name: var_name.to_string(),
                })?;
        Ok(Some((base_name, index)))
    } else {
        Ok(None)
    }
}

// expand_variable_for_query expands a variable for query operations,
// including support for handling array types.
pub fn expand_variable_for_query(variable: &MlxConfigVariable) -> Vec<String> {
    match &variable.spec {
        // Array types need to be expanded to individual indices
        MlxVariableSpec::BooleanArray { size }
        | MlxVariableSpec::IntegerArray { size }
        | MlxVariableSpec::BinaryArray { size } => (0..*size)
            .map(|i| format!("{}[{}]", variable.name, i))
            .collect(),
        MlxVariableSpec::EnumArray { size, .. } => (0..*size)
            .map(|i| format!("{}[{}]", variable.name, i))
            .collect(),
        // Non-array types use the variable name directly
        _ => vec![variable.name.clone()],
    }
}

// build_sparse_array_value builds a a sparse array value from
// individual indices.  This function handles the conversion from
// individual index/value pairs  to the proper sparse array format
// that our IntoMlxValue trait can handle.
pub fn build_sparse_array_value<T>(
    variable: &MlxConfigVariable,
    mut indices: Vec<(usize, T)>,
) -> Result<MlxConfigValue, MlxRunnerError>
where
    T: IntoMlxValue + Clone,
{
    // Sort by index for consistent processing
    indices.sort_by_key(|(index, _)| *index);

    // Get array size from the spec
    let array_size = get_array_size_from_spec(&variable.spec)?;

    // Validate all indices are within bounds
    for (index, _) in &indices {
        if *index >= array_size {
            return Err(MlxRunnerError::ArraySizeMismatch {
                variable_name: variable.name.clone(),
                expected: array_size,
                found: *index + 1,
            });
        }
    }

    // Build sparse array based on the variable's array type
    match &variable.spec {
        MlxVariableSpec::BooleanArray { .. } => {
            let mut sparse_array = vec![None; array_size];
            for (index, value) in indices {
                if let MlxValueType::Boolean(b) =
                    value.into_mlx_value_for_spec(&MlxVariableSpec::Boolean)?
                {
                    sparse_array[index] = Some(b);
                }
            }
            variable.with(sparse_array)
        }

        MlxVariableSpec::IntegerArray { .. } => {
            let mut sparse_array = vec![None; array_size];
            for (index, value) in indices {
                if let MlxValueType::Integer(i) =
                    value.into_mlx_value_for_spec(&MlxVariableSpec::Integer)?
                {
                    sparse_array[index] = Some(i);
                }
            }
            variable.with(sparse_array)
        }

        MlxVariableSpec::EnumArray { options, .. } => {
            let mut sparse_array = vec![None; array_size];
            let enum_spec = MlxVariableSpec::Enum {
                options: options.clone(),
            };
            for (index, value) in indices {
                if let MlxValueType::Enum(s) = value.into_mlx_value_for_spec(&enum_spec)? {
                    sparse_array[index] = Some(s);
                }
            }
            variable.with(sparse_array)
        }

        MlxVariableSpec::BinaryArray { .. } => {
            let mut sparse_array = vec![None; array_size];
            for (index, value) in indices {
                if let MlxValueType::Binary(bytes) =
                    value.into_mlx_value_for_spec(&MlxVariableSpec::Binary)?
                {
                    sparse_array[index] = Some(bytes);
                }
            }
            variable.with(sparse_array)
        }

        _ => {
            return Err(MlxRunnerError::ValueConversion {
                variable_name: variable.name.clone(),
                value: "sparse array".to_string(),
                error: crate::variables::value::MlxValueError::TypeMismatch {
                    expected: "array type".to_string(),
                    got: format!("{:?}", variable.spec),
                },
            });
        }
    }
    .map_err(|error| {
        MlxRunnerError::value_conversion(variable.name.clone(), "sparse array".to_string(), error)
    })
}

/// Helper function to extract array size from a variable spec
pub fn get_array_size_from_spec(spec: &MlxVariableSpec) -> Result<usize, MlxRunnerError> {
    match spec {
        MlxVariableSpec::BooleanArray { size }
        | MlxVariableSpec::IntegerArray { size }
        | MlxVariableSpec::BinaryArray { size } => Ok(*size),
        MlxVariableSpec::EnumArray { size, .. } => Ok(*size),
        _ => Err(MlxRunnerError::ValueConversion {
            variable_name: "unknown".to_string(),
            value: "array".to_string(),
            error: crate::variables::value::MlxValueError::TypeMismatch {
                expected: "array type".to_string(),
                got: format!("{spec:?}"),
            },
        }),
    }
}
