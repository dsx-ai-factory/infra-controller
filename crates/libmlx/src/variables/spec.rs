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

// src/spec.rs
// This file defines the specs for different mlxconfig
// variable types (e.g. bools, ints, enums, etc), with
// a builder that is leveraged by a build.rs script to
// make building a little cleaner.

use std::fmt;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::protos::mlx_device::{
    MlxVariableSpec as MlxVariableSpecPb, mlx_variable_spec as mlx_variable_spec_pb,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "config", rename_all = "snake_case")]
pub enum MlxVariableSpec {
    Boolean,
    Integer,
    String,
    Binary,
    Bytes,
    Array,
    Enum { options: Vec<String> },
    Preset { max_preset: u8 },
    BooleanArray { size: usize },
    IntegerArray { size: usize },
    EnumArray { options: Vec<String>, size: usize },
    BinaryArray { size: usize },
    Opaque,
}

// Much simpler builder - no redundant variant enum needed!
pub struct MlxVariableSpecBuilder;

impl MlxVariableSpec {
    pub fn builder() -> MlxVariableSpecBuilder {
        MlxVariableSpecBuilder
    }
}

impl MlxVariableSpecBuilder {
    // Simple variants also return builders for consistency
    pub fn boolean(self) -> SimpleBuilder {
        SimpleBuilder {
            spec: MlxVariableSpec::Boolean,
        }
    }

    pub fn integer(self) -> SimpleBuilder {
        SimpleBuilder {
            spec: MlxVariableSpec::Integer,
        }
    }

    pub fn string(self) -> SimpleBuilder {
        SimpleBuilder {
            spec: MlxVariableSpec::String,
        }
    }

    pub fn binary(self) -> SimpleBuilder {
        SimpleBuilder {
            spec: MlxVariableSpec::Binary,
        }
    }

    pub fn bytes(self) -> SimpleBuilder {
        SimpleBuilder {
            spec: MlxVariableSpec::Bytes,
        }
    }

    pub fn array(self) -> SimpleBuilder {
        SimpleBuilder {
            spec: MlxVariableSpec::Array,
        }
    }

    pub fn opaque(self) -> SimpleBuilder {
        SimpleBuilder {
            spec: MlxVariableSpec::Opaque,
        }
    }

    // For variants that need parameters, create them directly
    pub fn enum_type(self) -> EnumBuilder {
        EnumBuilder { options: None }
    }

    pub fn preset(self) -> PresetBuilder {
        PresetBuilder { max_preset: None }
    }

    pub fn boolean_array(self) -> BooleanArrayBuilder {
        BooleanArrayBuilder { size: None }
    }

    pub fn integer_array(self) -> IntegerArrayBuilder {
        IntegerArrayBuilder { size: None }
    }

    pub fn binary_array(self) -> BinaryArrayBuilder {
        BinaryArrayBuilder { size: None }
    }

    pub fn enum_array(self) -> EnumArrayBuilder {
        EnumArrayBuilder {
            options: None,
            size: None,
        }
    }
}

// Simple builder for variants that don't need configuration
pub struct SimpleBuilder {
    spec: MlxVariableSpec,
}

impl SimpleBuilder {
    pub fn build(self) -> MlxVariableSpec {
        self.spec
    }
}

// Focused builders for variants that need configuration
pub struct EnumBuilder {
    options: Option<Vec<String>>,
}

impl EnumBuilder {
    pub fn with_options<T: Into<Vec<String>>>(mut self, options: T) -> Self {
        self.options = Some(options.into());
        self
    }

    pub fn build(self) -> MlxVariableSpec {
        MlxVariableSpec::Enum {
            options: self.options.unwrap_or_default(),
        }
    }
}

pub struct PresetBuilder {
    max_preset: Option<u8>,
}

impl PresetBuilder {
    pub fn with_max_preset(mut self, max_preset: u8) -> Self {
        self.max_preset = Some(max_preset);
        self
    }

    pub fn build(self) -> MlxVariableSpec {
        MlxVariableSpec::Preset {
            max_preset: self.max_preset.unwrap_or(0),
        }
    }
}

pub struct BooleanArrayBuilder {
    size: Option<usize>,
}

impl BooleanArrayBuilder {
    pub fn with_size(mut self, size: usize) -> Self {
        self.size = Some(size);
        self
    }

    pub fn build(self) -> MlxVariableSpec {
        MlxVariableSpec::BooleanArray {
            size: self.size.unwrap_or(1),
        }
    }
}

pub struct IntegerArrayBuilder {
    size: Option<usize>,
}

impl IntegerArrayBuilder {
    pub fn with_size(mut self, size: usize) -> Self {
        self.size = Some(size);
        self
    }

    pub fn build(self) -> MlxVariableSpec {
        MlxVariableSpec::IntegerArray {
            size: self.size.unwrap_or(1),
        }
    }
}

pub struct BinaryArrayBuilder {
    size: Option<usize>,
}

impl BinaryArrayBuilder {
    pub fn with_size(mut self, size: usize) -> Self {
        self.size = Some(size);
        self
    }

    pub fn build(self) -> MlxVariableSpec {
        MlxVariableSpec::BinaryArray {
            size: self.size.unwrap_or(1),
        }
    }
}

pub struct EnumArrayBuilder {
    options: Option<Vec<String>>,
    size: Option<usize>,
}

impl EnumArrayBuilder {
    pub fn with_options<T: Into<Vec<String>>>(mut self, options: T) -> Self {
        self.options = Some(options.into());
        self
    }

    pub fn with_size(mut self, size: usize) -> Self {
        self.size = Some(size);
        self
    }

    pub fn build(self) -> MlxVariableSpec {
        MlxVariableSpec::EnumArray {
            options: self.options.unwrap_or_default(),
            size: self.size.unwrap_or(1),
        }
    }
}

impl fmt::Display for MlxVariableSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MlxVariableSpec::Boolean => write!(f, "Boolean"),
            MlxVariableSpec::Integer => write!(f, "Integer"),
            MlxVariableSpec::String => write!(f, "String"),
            MlxVariableSpec::Binary => write!(f, "Binary"),
            MlxVariableSpec::Bytes => write!(f, "Bytes"),
            MlxVariableSpec::Array => write!(f, "Array"),
            MlxVariableSpec::Enum { options } => {
                write!(f, "Enum [{}]", options.join(", "))
            }
            MlxVariableSpec::Preset { max_preset } => {
                write!(f, "Preset (max: {max_preset})")
            }
            MlxVariableSpec::BooleanArray { size } => {
                write!(f, "BooleanArray[{size}]")
            }
            MlxVariableSpec::IntegerArray { size } => {
                write!(f, "IntegerArray[{size}]")
            }
            MlxVariableSpec::EnumArray { options, size } => {
                write!(f, "EnumArray[{size}] [{}]", options.join(", "))
            }
            MlxVariableSpec::BinaryArray { size } => {
                write!(f, "BinaryArray[{size}]")
            }
            MlxVariableSpec::Opaque => write!(f, "Opaque"),
        }
    }
}

impl From<MlxVariableSpec> for MlxVariableSpecPb {
    fn from(spec: MlxVariableSpec) -> Self {
        match spec {
            MlxVariableSpec::Boolean => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Boolean(
                    mlx_variable_spec_pb::BooleanSpec {},
                )),
            },
            MlxVariableSpec::Integer => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Integer(
                    mlx_variable_spec_pb::IntegerSpec {},
                )),
            },
            MlxVariableSpec::String => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::String(
                    mlx_variable_spec_pb::StringSpec {},
                )),
            },
            MlxVariableSpec::Binary => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Binary(
                    mlx_variable_spec_pb::BinarySpec {},
                )),
            },
            MlxVariableSpec::Bytes => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Bytes(
                    mlx_variable_spec_pb::BytesSpec {},
                )),
            },
            MlxVariableSpec::Array => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Array(
                    mlx_variable_spec_pb::ArraySpec {},
                )),
            },
            MlxVariableSpec::Enum { options } => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::EnumType(
                    mlx_variable_spec_pb::EnumSpec { options },
                )),
            },
            MlxVariableSpec::Preset { max_preset } => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Preset(
                    mlx_variable_spec_pb::PresetSpec {
                        max_preset: max_preset as u32,
                    },
                )),
            },
            MlxVariableSpec::BooleanArray { size } => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::BooleanArray(
                    mlx_variable_spec_pb::BooleanArraySpec { size: size as u64 },
                )),
            },
            MlxVariableSpec::IntegerArray { size } => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::IntegerArray(
                    mlx_variable_spec_pb::IntegerArraySpec { size: size as u64 },
                )),
            },
            MlxVariableSpec::EnumArray { options, size } => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::EnumArray(
                    mlx_variable_spec_pb::EnumArraySpec {
                        options,
                        size: size as u64,
                    },
                )),
            },
            MlxVariableSpec::BinaryArray { size } => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::BinaryArray(
                    mlx_variable_spec_pb::BinaryArraySpec { size: size as u64 },
                )),
            },
            MlxVariableSpec::Opaque => MlxVariableSpecPb {
                spec_type: Some(mlx_variable_spec_pb::SpecType::Opaque(
                    mlx_variable_spec_pb::OpaqueSpec {},
                )),
            },
        }
    }
}

impl TryFrom<MlxVariableSpecPb> for MlxVariableSpec {
    type Error = RpcDataConversionError;

    fn try_from(pb: MlxVariableSpecPb) -> Result<Self, Self::Error> {
        let spec_type = pb
            .spec_type
            .ok_or(RpcDataConversionError::MissingArgument("spec_type"))?;

        match spec_type {
            mlx_variable_spec_pb::SpecType::Boolean(_) => Ok(MlxVariableSpec::Boolean),
            mlx_variable_spec_pb::SpecType::Integer(_) => Ok(MlxVariableSpec::Integer),
            mlx_variable_spec_pb::SpecType::String(_) => Ok(MlxVariableSpec::String),
            mlx_variable_spec_pb::SpecType::Binary(_) => Ok(MlxVariableSpec::Binary),
            mlx_variable_spec_pb::SpecType::Bytes(_) => Ok(MlxVariableSpec::Bytes),
            mlx_variable_spec_pb::SpecType::Array(_) => Ok(MlxVariableSpec::Array),
            mlx_variable_spec_pb::SpecType::EnumType(e) => {
                Ok(MlxVariableSpec::Enum { options: e.options })
            }
            mlx_variable_spec_pb::SpecType::Preset(p) => Ok(MlxVariableSpec::Preset {
                max_preset: p.max_preset as u8,
            }),
            mlx_variable_spec_pb::SpecType::BooleanArray(ba) => Ok(MlxVariableSpec::BooleanArray {
                size: ba.size as usize,
            }),
            mlx_variable_spec_pb::SpecType::IntegerArray(ia) => Ok(MlxVariableSpec::IntegerArray {
                size: ia.size as usize,
            }),
            mlx_variable_spec_pb::SpecType::EnumArray(ea) => Ok(MlxVariableSpec::EnumArray {
                options: ea.options,
                size: ea.size as usize,
            }),
            mlx_variable_spec_pb::SpecType::BinaryArray(ba) => Ok(MlxVariableSpec::BinaryArray {
                size: ba.size as usize,
            }),
            mlx_variable_spec_pb::SpecType::Opaque(_) => Ok(MlxVariableSpec::Opaque),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, value_scenarios};

    use super::*;

    // Display covers every enum arm: the six plain scalars, the untyped Array, the
    // configured-scalar Enum/Preset, the sized arrays, the sized+optioned EnumArray,
    // and Opaque. Each row pins the exact rendered string, including the join/format
    // details (comma-space joins, bracketed sizes, "max:" / "[size]" layouts).
    #[test]
    fn display_renders_each_variant() {
        value_scenarios!(
            run = |spec| &*spec.to_string().leak();
            "boolean" {
                MlxVariableSpec::Boolean => "Boolean",
            }

            "integer" {
                MlxVariableSpec::Integer => "Integer",
            }

            "string" {
                MlxVariableSpec::String => "String",
            }

            "binary" {
                MlxVariableSpec::Binary => "Binary",
            }

            "bytes" {
                MlxVariableSpec::Bytes => "Bytes",
            }

            "array" {
                MlxVariableSpec::Array => "Array",
            }

            "opaque" {
                MlxVariableSpec::Opaque => "Opaque",
            }

            "enum with options" {
                MlxVariableSpec::Enum {
                    options: vec!["low".to_string(), "high".to_string()],
                } => "Enum [low, high]",
            }

            "enum with no options" {
                MlxVariableSpec::Enum { options: vec![] } => "Enum []",
            }

            "preset" {
                MlxVariableSpec::Preset { max_preset: 7 } => "Preset (max: 7)",
            }

            "boolean array" {
                MlxVariableSpec::BooleanArray { size: 3 } => "BooleanArray[3]",
            }

            "integer array" {
                MlxVariableSpec::IntegerArray { size: 5 } => "IntegerArray[5]",
            }

            "enum array" {
                MlxVariableSpec::EnumArray {
                    options: vec!["a".to_string(), "b".to_string()],
                    size: 2,
                } => "EnumArray[2] [a, b]",
            }

            "enum array with no options" {
                MlxVariableSpec::EnumArray {
                    options: vec![],
                    size: 4,
                } => "EnumArray[4] []",
            }

            "binary array" {
                MlxVariableSpec::BinaryArray { size: 8 } => "BinaryArray[8]",
            }
        );
    }

    // The builder entry points for the configuration-free variants each return a
    // SimpleBuilder whose build() yields exactly the matching variant. Driving them
    // through one table exercises every simple builder method.
    #[test]
    fn simple_builders_yield_their_variant() {
        value_scenarios!(
            run = |built| built;
            "boolean" {
                MlxVariableSpec::builder().boolean().build() => MlxVariableSpec::Boolean,
            }

            "integer" {
                MlxVariableSpec::builder().integer().build() => MlxVariableSpec::Integer,
            }

            "string" {
                MlxVariableSpec::builder().string().build() => MlxVariableSpec::String,
            }

            "binary" {
                MlxVariableSpec::builder().binary().build() => MlxVariableSpec::Binary,
            }

            "bytes" {
                MlxVariableSpec::builder().bytes().build() => MlxVariableSpec::Bytes,
            }

            "array" {
                MlxVariableSpec::builder().array().build() => MlxVariableSpec::Array,
            }

            "opaque" {
                MlxVariableSpec::builder().opaque().build() => MlxVariableSpec::Opaque,
            }
        );
    }

    // The configured builders each have a with_* setter path and an unset-default
    // path. This pins both: enum defaults to empty options, preset defaults to 0,
    // and the sized arrays default to size 1; the configured rows pin the set value.
    #[test]
    fn configured_builders_apply_values_and_defaults() {
        value_scenarios!(
            run = |built| built;
            "enum with options set" {
                MlxVariableSpec::builder()
                .enum_type()
                .with_options(vec!["x".to_string(), "y".to_string()])
                .build() => MlxVariableSpec::Enum {
                    options: vec!["x".to_string(), "y".to_string()],
                },
            }

            "enum default options is empty" {
                MlxVariableSpec::builder().enum_type().build() => MlxVariableSpec::Enum { options: vec![] },
            }

            "preset with max set" {
                MlxVariableSpec::builder()
                .preset()
                .with_max_preset(9)
                .build() => MlxVariableSpec::Preset { max_preset: 9 },
            }

            "preset default max is 0" {
                MlxVariableSpec::builder().preset().build() => MlxVariableSpec::Preset { max_preset: 0 },
            }

            "boolean array with size set" {
                MlxVariableSpec::builder()
                .boolean_array()
                .with_size(6)
                .build() => MlxVariableSpec::BooleanArray { size: 6 },
            }

            "boolean array default size is 1" {
                MlxVariableSpec::builder().boolean_array().build() => MlxVariableSpec::BooleanArray { size: 1 },
            }

            "integer array with size set" {
                MlxVariableSpec::builder()
                .integer_array()
                .with_size(4)
                .build() => MlxVariableSpec::IntegerArray { size: 4 },
            }

            "integer array default size is 1" {
                MlxVariableSpec::builder().integer_array().build() => MlxVariableSpec::IntegerArray { size: 1 },
            }

            "binary array with size set" {
                MlxVariableSpec::builder()
                .binary_array()
                .with_size(2)
                .build() => MlxVariableSpec::BinaryArray { size: 2 },
            }

            "binary array default size is 1" {
                MlxVariableSpec::builder().binary_array().build() => MlxVariableSpec::BinaryArray { size: 1 },
            }

            "enum array with options and size set" {
                MlxVariableSpec::builder()
                .enum_array()
                .with_options(vec!["a".to_string()])
                .with_size(3)
                .build() => MlxVariableSpec::EnumArray {
                    options: vec!["a".to_string()],
                    size: 3,
                },
            }

            "enum array defaults: empty options, size 1" {
                MlxVariableSpec::builder().enum_array().build() => MlxVariableSpec::EnumArray {
                    options: vec![],
                    size: 1,
                },
            }
        );
    }

    // Every variant survives a From -> TryFrom round trip through the protobuf
    // representation. This drives both the From conversion (all 13 arms) and the
    // TryFrom success path (all 13 arms), pinning the recovered spec to the input.
    // Sizes are chosen small so the usize/u64 and u8/u32 casts are lossless.
    #[test]
    fn pb_round_trip_preserves_each_variant() {
        let specs = [
            ("boolean", MlxVariableSpec::Boolean),
            ("integer", MlxVariableSpec::Integer),
            ("string", MlxVariableSpec::String),
            ("binary", MlxVariableSpec::Binary),
            ("bytes", MlxVariableSpec::Bytes),
            ("array", MlxVariableSpec::Array),
            ("opaque", MlxVariableSpec::Opaque),
            (
                "enum",
                MlxVariableSpec::Enum {
                    options: vec!["one".to_string(), "two".to_string()],
                },
            ),
            (
                "enum empty options",
                MlxVariableSpec::Enum { options: vec![] },
            ),
            ("preset zero", MlxVariableSpec::Preset { max_preset: 0 }),
            ("preset max u8", MlxVariableSpec::Preset { max_preset: 255 }),
            ("boolean array", MlxVariableSpec::BooleanArray { size: 0 }),
            ("integer array", MlxVariableSpec::IntegerArray { size: 7 }),
            (
                "enum array",
                MlxVariableSpec::EnumArray {
                    options: vec!["in".to_string(), "out".to_string()],
                    size: 4,
                },
            ),
            ("binary array", MlxVariableSpec::BinaryArray { size: 1 }),
        ];

        check_cases(
            specs.into_iter().map(|(scenario, spec)| Case {
                scenario,
                input: spec.clone(),
                expect: Yields(spec),
            }),
            |spec| {
                let pb: MlxVariableSpecPb = spec.into();
                MlxVariableSpec::try_from(pb).map_err(drop)
            },
        );
    }

    // TryFrom rejects a protobuf with no spec_type set (the MissingArgument arm).
    // The error type is not asserted for equality here, so map_err(drop) + Fails
    // keeps the row to an Ok-vs-Err contract.
    #[test]
    fn try_from_rejects_missing_spec_type() {
        Case {
            scenario: "absent spec_type fails",
            input: MlxVariableSpecPb { spec_type: None },
            expect: Fails,
        }
        .check(|pb| MlxVariableSpec::try_from(pb).map_err(drop));
    }

    // The serde tag/content/snake_case attributes mean a spec serializes to JSON and
    // deserializes back to itself. Round-tripping every variant exercises the derived
    // Serialize/Deserialize over the full enum without pinning the exact JSON text.
    #[test]
    fn json_round_trip_preserves_each_variant() {
        let specs = [
            ("boolean", MlxVariableSpec::Boolean),
            ("opaque", MlxVariableSpec::Opaque),
            (
                "enum",
                MlxVariableSpec::Enum {
                    options: vec!["a".to_string(), "b".to_string()],
                },
            ),
            ("preset", MlxVariableSpec::Preset { max_preset: 12 }),
            ("boolean array", MlxVariableSpec::BooleanArray { size: 3 }),
            ("integer array", MlxVariableSpec::IntegerArray { size: 9 }),
            (
                "enum array",
                MlxVariableSpec::EnumArray {
                    options: vec!["x".to_string()],
                    size: 2,
                },
            ),
            ("binary array", MlxVariableSpec::BinaryArray { size: 4 }),
        ];

        check_cases(
            specs.into_iter().map(|(scenario, spec)| Case {
                scenario,
                input: spec.clone(),
                expect: Yields(spec),
            }),
            |spec| {
                let json = serde_json::to_string(&spec).map_err(drop)?;
                serde_json::from_str::<MlxVariableSpec>(&json).map_err(drop)
            },
        );
    }
}
