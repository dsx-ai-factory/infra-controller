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

use carbide_test_support::Outcome::*;
use carbide_test_support::{Case, scenarios, value_scenarios};
use libmlx::variables::spec::MlxVariableSpec;
use libmlx::variables::value::{MlxValueError, MlxValueType};
use libmlx::variables::variable::MlxConfigVariable;

// create_test_variable creates a test variable with a given spec
// to use for testing. This is leveraged for basically each test.
fn create_test_variable(name: &str, spec: MlxVariableSpec) -> MlxConfigVariable {
    MlxConfigVariable {
        name: name.to_string(),
        description: format!("Test variable: {name}"),
        read_only: false,
        spec,
    }
}

// test_boolean_value_creation creates a new variable called "test_bool"
// with a boolean spec, and then makes sure we can call `with`
// on it with a boolean, ensuring the IntoMlxValue trait is working
// as expected for booleans (among other things).
#[test]
fn test_boolean_value_creation() {
    let var = create_test_variable("test_bool", MlxVariableSpec::Boolean);
    let value = var.with(true).unwrap();

    assert_eq!(value.name(), "test_bool");
    assert_eq!(value.value, MlxValueType::Boolean(true));
    assert!(!value.is_read_only());
}

// test_integer_value_creation creates a new variable called "test_int"
// with an integer spec, and then makes sure we can call `with`
// on it with an integer, ensuring the IntoMlxValue trait is working
// as expected for integers (among other things).
#[test]
fn test_integer_value_creation() {
    let var = create_test_variable("test_int", MlxVariableSpec::Integer);

    // Works with different integer types.
    let value1 = var.with(42i64).unwrap();
    let value2 = var.with(42i32).unwrap();

    assert_eq!(value1.value, MlxValueType::Integer(42));
    assert_eq!(value2.value, MlxValueType::Integer(42));
}

// test_string_value_creation creates a new variable called "test_string"
// with a string spec, and then makes sure we can call `with`
// on it with a string, ensuring the IntoMlxValue trait is working
// as expected for strings (among other things, that rhymes).
#[test]
fn test_string_value_creation() {
    let var = create_test_variable("test_string", MlxVariableSpec::String);

    // Works with &str, String, etc.
    let value1 = var.with("hello").unwrap();
    let value2 = var.with("world".to_string()).unwrap();

    assert_eq!(value1.value, MlxValueType::String("hello".to_string()));
    assert_eq!(value2.value, MlxValueType::String("world".to_string()));
}

// enum_values_validate_against_the_spec migrates the old hand-written enum test
// onto carbide-test-support: each row is a labeled input + an expected `Outcome`,
// and `check_cases` runs the operation under test (`var.with`) over them. A valid
// option yields an Enum value; an unknown option fails with the exact
// InvalidEnumOption error — no `match … panic!` to read past.
#[test]
fn enum_values_validate_against_the_spec() {
    let var = create_test_variable(
        "test_enum",
        MlxVariableSpec::Enum {
            options: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        },
    );

    scenarios!(
        run = |input| var.with(input).map(|v| v.value);
        "a valid option" {
            "medium" => Yields(MlxValueType::Enum("medium".to_string())),
        }

        "another valid option" {
            "high" => Yields(MlxValueType::Enum("high".to_string())),
        }

        "an unknown option" {
            "invalid" => FailsWith(MlxValueError::InvalidEnumOption {
                value: "invalid".to_string(),
                allowed: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
            }),
        }
    );
}

// preset_values_respect_the_max migrates the old preset test onto `Outcome`: an
// in-range u8 yields a Preset, an out-of-range one fails. We don't pin which error
// the out-of-range case produces, so `Fails` says exactly that.
#[test]
fn preset_values_respect_the_max() {
    let var = create_test_variable("test_preset", MlxVariableSpec::Preset { max_preset: 5 });

    scenarios!(
        run = |input| var.with(input).map(|v| v.value);
        "in range" {
            3u8 => Yields(MlxValueType::Preset(3)),
        }

        "above the max" {
            10u8 => Fails,
        }
    );
}

// boolean_arrays_validate_size_and_convert migrates the bool-array test onto
// check_cases. The valid row shows `Yields` carrying real data — a dense Vec<bool>
// is converted to the sparse BooleanArray form — while a wrong-sized input fails.
#[test]
fn boolean_arrays_validate_size_and_convert() {
    let var = create_test_variable("test_bool_array", MlxVariableSpec::BooleanArray { size: 4 });

    scenarios!(
        run = |input| var.with(input).map(|v| v.value);
        "a right-sized Vec<bool> converts to the sparse form" {
            vec![true, false, true, false] => Yields(MlxValueType::BooleanArray(vec![
                Some(true),
                Some(false),
                Some(true),
                Some(false),
            ])),
        }

        "a wrong-sized input is rejected" {
            vec![true, false] => Fails,
        }
    );
}

// test_sparse_boolean_array_creation tests creating sparse boolean arrays
// where some indices are unset (None).
#[test]
fn test_sparse_boolean_array_creation() {
    let var = create_test_variable(
        "test_sparse_bool_array",
        MlxVariableSpec::BooleanArray { size: 4 },
    );

    // Vec<Option<bool>> for sparse arrays
    let sparse_value = var.with(vec![Some(true), None, Some(false), None]).unwrap();
    assert_eq!(
        sparse_value.value,
        MlxValueType::BooleanArray(vec![Some(true), None, Some(false), None])
    );

    // Display should show "-" for None values
    let display = sparse_value.to_display_string();
    assert_eq!(display, "[true, -, false, -]");

    // Wrong size gets caught
    let invalid_result = var.with(vec![Some(true), None]);
    assert!(invalid_result.is_err());
}

// test_enum_array_creation creates a new variable called "test_enum_array"
// with an enum array spec, and then makes sure we can call `with`
// on it with an enum array, ensuring the IntoMlxValue trait is working
// as expected for enum arrays (among other things).
#[test]
fn test_enum_array_creation() {
    let var = create_test_variable(
        "test_enum_array",
        MlxVariableSpec::EnumArray {
            options: vec!["input".to_string(), "output".to_string()],
            size: 3,
        },
    );

    let valid_value = var.with(vec!["input", "output", "input"]).unwrap();
    assert_eq!(
        valid_value.value,
        MlxValueType::EnumArray(vec![
            Some("input".to_string()),
            Some("output".to_string()),
            Some("input".to_string())
        ])
    );

    let invalid_result = var.with(vec!["input", "invalid", "output"]);
    assert!(invalid_result.is_err());
    match invalid_result.unwrap_err() {
        MlxValueError::InvalidEnumArrayOption {
            position, value, ..
        } => {
            assert_eq!(position, 1);
            assert_eq!(value, "invalid");
        }
        _ => panic!("Expected InvalidEnumArrayOption error"),
    }
}

// test_sparse_enum_array_creation tests creating sparse enum arrays
// where some indices are unset (None).
#[test]
fn test_sparse_enum_array_creation() {
    let var = create_test_variable(
        "test_sparse_enum_array",
        MlxVariableSpec::EnumArray {
            options: vec![
                "input".to_string(),
                "output".to_string(),
                "bidirectional".to_string(),
            ],
            size: 4,
        },
    );

    // Vec<Option<String>> for sparse arrays
    let sparse_value = var
        .with(vec![
            Some("input".to_string()),
            None,
            Some("output".to_string()),
            None,
        ])
        .unwrap();

    assert_eq!(
        sparse_value.value,
        MlxValueType::EnumArray(vec![
            Some("input".to_string()),
            None,
            Some("output".to_string()),
            None
        ])
    );

    // Display should show "-" for None values
    let display = sparse_value.to_display_string();
    assert_eq!(display, "[input, -, output, -]");

    // Validation should still work for Some values
    let invalid_result = var.with(vec![
        Some("input".to_string()),
        Some("invalid".to_string()),
        None,
        None,
    ]);
    assert!(invalid_result.is_err());
}

// test_integer_array_creation tests creating integer arrays with sparse support.
#[test]
fn test_integer_array_creation() {
    let var = create_test_variable("test_int_array", MlxVariableSpec::IntegerArray { size: 3 });

    // Dense array (Vec<i64>) gets converted to sparse format
    let dense_value = var.with(vec![42i64, -123, 0]).unwrap();
    assert_eq!(
        dense_value.value,
        MlxValueType::IntegerArray(vec![Some(42), Some(-123), Some(0)])
    );

    // Sparse array (Vec<Option<i64>>)
    let sparse_value = var.with(vec![Some(42), None, Some(0)]).unwrap();
    assert_eq!(
        sparse_value.value,
        MlxValueType::IntegerArray(vec![Some(42), None, Some(0)])
    );

    // Display should show "-" for None values
    let display = sparse_value.to_display_string();
    assert_eq!(display, "[42, -, 0]");

    // Wrong size gets caught
    let invalid_result = var.with(vec![1i64, 2]);
    assert!(invalid_result.is_err());
}

// test_binary_array_creation tests creating binary arrays with sparse support.
#[test]
fn test_binary_array_creation() {
    let var = create_test_variable(
        "test_binary_array",
        MlxVariableSpec::BinaryArray { size: 2 },
    );

    // Dense array (Vec<Vec<u8>>) gets converted to sparse format
    let dense_value = var.with(vec![vec![0x1a, 0x2b], vec![0x3c, 0x4d]]).unwrap();
    assert_eq!(
        dense_value.value,
        MlxValueType::BinaryArray(vec![Some(vec![0x1a, 0x2b]), Some(vec![0x3c, 0x4d])])
    );

    // Sparse array (Vec<Option<Vec<u8>>>)
    let sparse_value = var.with(vec![Some(vec![0x1a, 0x2b]), None]).unwrap();
    assert_eq!(
        sparse_value.value,
        MlxValueType::BinaryArray(vec![Some(vec![0x1a, 0x2b]), None])
    );

    // Display should show count including sparse info
    let display = sparse_value.to_display_string();
    assert_eq!(display, "[2 binary values, 1 set]");
}

// test_type_mismatch makes sure we can't create a new variable
// value with an incorrect type by passing a bool to an integer
// variable spec.
#[test]
fn test_type_mismatch() {
    let var = create_test_variable("test_int", MlxVariableSpec::Integer);

    let result = var.with(true);
    assert!(result.is_err());
    match result.unwrap_err() {
        MlxValueError::TypeMismatch { expected, got } => {
            assert!(expected.contains("Integer"));
            assert!(got.contains("bool"));
        }
        _ => panic!("Expected TypeMismatch error"),
    }
}

// test_contextual_string_handling tests the same string input,
// and verifies different behavior based on spec.
#[test]
fn test_contextual_string_handling() {
    // String spec - just stores the string.
    let string_var = create_test_variable("test_string", MlxVariableSpec::String);
    let string_value = string_var.with("medium").unwrap();
    assert_eq!(
        string_value.value,
        MlxValueType::String("medium".to_string())
    );

    // Enum spec - validates against options.
    let enum_var = create_test_variable(
        "test_enum",
        MlxVariableSpec::Enum {
            options: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        },
    );
    let enum_value = enum_var.with("medium").unwrap();
    assert_eq!(enum_value.value, MlxValueType::Enum("medium".to_string()));
}

// mlxconfig hands every value back as a string (via `--json`), so `with` has to
// parse a string into each spec. This walks every single-value spec, grouped by
// spec; `parsed` pulls the value out and drops the error, since the rejection rows
// only assert *that* a bad string fails.
#[test]
fn test_string_parsing_for_single_values() {
    fn parsed(var: &MlxConfigVariable, raw: &str) -> Result<MlxValueType, ()> {
        var.with(raw.to_string()).map(|v| v.value).map_err(drop)
    }

    let bool_var = create_test_variable("test_bool", MlxVariableSpec::Boolean);
    scenarios!(
        run = |raw| parsed(&bool_var, raw);
        "'true'" {
            "true" => Yields(MlxValueType::Boolean(true)),
        }

        "'1'" {
            "1" => Yields(MlxValueType::Boolean(true)),
        }

        "'YES'" {
            "YES" => Yields(MlxValueType::Boolean(true)),
        }

        "'enabled'" {
            "enabled" => Yields(MlxValueType::Boolean(true)),
        }

        "'on'" {
            "on" => Yields(MlxValueType::Boolean(true)),
        }

        "'false'" {
            "false" => Yields(MlxValueType::Boolean(false)),
        }

        "'0'" {
            "0" => Yields(MlxValueType::Boolean(false)),
        }

        "'NO'" {
            "NO" => Yields(MlxValueType::Boolean(false)),
        }

        "'disabled'" {
            "disabled" => Yields(MlxValueType::Boolean(false)),
        }

        "'off'" {
            "off" => Yields(MlxValueType::Boolean(false)),
        }

        "'maybe' is not a boolean" {
            "maybe" => Fails,
        }
    );

    let int_var = create_test_variable("test_int", MlxVariableSpec::Integer);
    scenarios!(
        run = |raw| parsed(&int_var, raw);
        "positive" {
            "42" => Yields(MlxValueType::Integer(42)),
        }

        "negative" {
            "-123" => Yields(MlxValueType::Integer(-123)),
        }

        "zero" {
            "0" => Yields(MlxValueType::Integer(0)),
        }

        "non-number is rejected" {
            "not_a_number" => Fails,
        }
    );

    let str_var = create_test_variable("test_string", MlxVariableSpec::String);
    scenarios!(
        run = |raw| parsed(&str_var, raw);
        "stored as-is" {
            "hello world" => Yields(MlxValueType::String("hello world".to_string())),
        }

        "surrounding whitespace trimmed" {
            "  trimmed  " => Yields(MlxValueType::String("trimmed".to_string())),
        }
    );

    let enum_var = create_test_variable(
        "test_enum",
        MlxVariableSpec::Enum {
            options: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        },
    );
    scenarios!(
        run = |raw| parsed(&enum_var, raw);
        "a valid option" {
            "medium" => Yields(MlxValueType::Enum("medium".to_string())),
        }

        "another valid option" {
            "high" => Yields(MlxValueType::Enum("high".to_string())),
        }

        "trimmed before matching" {
            " low " => Yields(MlxValueType::Enum("low".to_string())),
        }

        "an unknown option is rejected" {
            "invalid" => Fails,
        }
    );

    let preset_var =
        create_test_variable("test_preset", MlxVariableSpec::Preset { max_preset: 10 });
    scenarios!(
        run = |raw| parsed(&preset_var, raw);
        "mid-range" {
            "5" => Yields(MlxValueType::Preset(5)),
        }

        "the floor" {
            "0" => Yields(MlxValueType::Preset(0)),
        }

        "the ceiling" {
            "10" => Yields(MlxValueType::Preset(10)),
        }

        "above the max is rejected" {
            "15" => Fails,
        }

        "non-number is rejected" {
            "not_a_number" => Fails,
        }
    );

    // Binary, Bytes, and Opaque all parse hex -- with or without an 0x/0X prefix.
    let binary_var = create_test_variable("test_binary", MlxVariableSpec::Binary);
    scenarios!(
        run = |raw| parsed(&binary_var, raw);
        "0x-prefixed hex" {
            "0x1a2b3c" => Yields(MlxValueType::Binary(vec![0x1a, 0x2b, 0x3c])),
        }

        "non-hex is rejected" {
            "not_hex" => Fails,
        }
    );
    let bytes_var = create_test_variable("test_bytes", MlxVariableSpec::Bytes);
    Case {
        scenario: "bare hex",
        input: "1a2b3c",
        expect: Yields(MlxValueType::Bytes(vec![0x1a, 0x2b, 0x3c])),
    }
    .check(|raw| parsed(&bytes_var, raw));
    let opaque_var = create_test_variable("test_opaque", MlxVariableSpec::Opaque);
    Case {
        scenario: "uppercase 0X hex",
        input: "0X1A2B3C",
        expect: Yields(MlxValueType::Opaque(vec![0x1a, 0x2b, 0x3c])),
    }
    .check(|raw| parsed(&opaque_var, raw));

    // A single string can't satisfy an array spec.
    let bool_array_var =
        create_test_variable("test_bool_array", MlxVariableSpec::BooleanArray { size: 3 });
    Case {
        scenario: "single string rejects an array spec",
        input: "true",
        expect: Fails,
    }
    .check(|raw| parsed(&bool_array_var, raw));
}

// The array-spec counterpart to the single-value parser -- a Vec<String> per row
// (mlxconfig delivers arrays as string vecs over `--json`). Grouped by spec. The
// enum-array case pins its exact error, so `parsed` keeps the MlxValueError; the
// size/element failures just use `Fails`.
#[test]
fn test_vec_string_parsing_for_array_values() {
    fn parsed(var: &MlxConfigVariable, raw: Vec<&str>) -> Result<MlxValueType, MlxValueError> {
        var.with(raw.into_iter().map(String::from).collect::<Vec<String>>())
            .map(|v| v.value)
    }

    // Generic string array -- trims each element.
    let array_var = create_test_variable("test_array", MlxVariableSpec::Array);
    Case {
        scenario: "trims each element",
        input: vec!["hello", " world ", "test"],
        expect: Yields(MlxValueType::Array(vec![
            "hello".to_string(),
            "world".to_string(),
            "test".to_string(),
        ])),
    }
    .check(|raw| parsed(&array_var, raw));

    // Boolean array -- dense, sparse ("-" or "" = None), wrong size, bad element.
    let bool_array_var =
        create_test_variable("test_bool_array", MlxVariableSpec::BooleanArray { size: 4 });
    scenarios!(
        run = |raw| parsed(&bool_array_var, raw);
        "dense" {
            vec!["true", "0", "YES", "disabled"] => Yields(MlxValueType::BooleanArray(vec![
                Some(true),
                Some(false),
                Some(true),
                Some(false),
            ])),
        }

        "sparse via - and empty string" {
            vec!["true", "-", "false", ""] => Yields(MlxValueType::BooleanArray(vec![
                Some(true),
                None,
                Some(false),
                None,
            ])),
        }

        "wrong size" {
            vec!["true", "false"] => Fails,
        }

        "invalid boolean element" {
            vec!["true", "maybe", "false", "true"] => Fails,
        }
    );

    // Integer array -- same dense/sparse/size/element coverage.
    let int_array_var =
        create_test_variable("test_int_array", MlxVariableSpec::IntegerArray { size: 3 });
    scenarios!(
        run = |raw| parsed(&int_array_var, raw);
        "dense" {
            vec!["42", "-123", "0"] => Yields(MlxValueType::IntegerArray(vec![
                Some(42),
                Some(-123),
                Some(0),
            ])),
        }

        "sparse via -" {
            vec!["42", "-", "0"] => Yields(MlxValueType::IntegerArray(vec![Some(42), None, Some(0)])),
        }

        "wrong size" {
            vec!["1", "2"] => Fails,
        }

        "invalid integer element" {
            vec!["42", "not_a_number", "0"] => Fails,
        }
    );

    // Enum array -- trims; dense/sparse pass; wrong size fails; a bad option fails
    // with the exact position / value / allowed set.
    let enum_array_var = create_test_variable(
        "test_enum_array",
        MlxVariableSpec::EnumArray {
            options: vec![
                "input".to_string(),
                "output".to_string(),
                "bidirectional".to_string(),
            ],
            size: 4,
        },
    );
    scenarios!(
        run = |raw| parsed(&enum_array_var, raw);
        "dense, trimmed" {
            vec!["input", " output ", "bidirectional", "input"] => Yields(MlxValueType::EnumArray(vec![
                Some("input".to_string()),
                Some("output".to_string()),
                Some("bidirectional".to_string()),
                Some("input".to_string()),
            ])),
        }

        "sparse via - and empty string" {
            vec!["input", "-", "output", ""] => Yields(MlxValueType::EnumArray(vec![
                Some("input".to_string()),
                None,
                Some("output".to_string()),
                None,
            ])),
        }

        "wrong size" {
            vec!["input", "output"] => Fails,
        }

        "bad option pins position, value, and allowed set" {
            vec!["input", "invalid", "output", "input"] => FailsWith(MlxValueError::InvalidEnumArrayOption {
                position: 1,
                value: "invalid".to_string(),
                allowed: vec![
                    "input".to_string(),
                    "output".to_string(),
                    "bidirectional".to_string(),
                ],
            }),
        }
    );

    // Binary array -- hex with or without an 0x/0X prefix, dense and sparse.
    let binary_array_var = create_test_variable(
        "test_binary_array",
        MlxVariableSpec::BinaryArray { size: 3 },
    );
    scenarios!(
        run = |raw| parsed(&binary_array_var, raw);
        "dense, mixed prefixes and whitespace" {
            vec!["0x1a2b", "3c4d", " 0X5E6F "] => Yields(MlxValueType::BinaryArray(vec![
                Some(vec![0x1a, 0x2b]),
                Some(vec![0x3c, 0x4d]),
                Some(vec![0x5e, 0x6f]),
            ])),
        }

        "sparse via -" {
            vec!["0x1a2b", "-", "3c4d"] => Yields(MlxValueType::BinaryArray(vec![
                Some(vec![0x1a, 0x2b]),
                None,
                Some(vec![0x3c, 0x4d]),
            ])),
        }

        "wrong size" {
            vec!["0x1a2b", "3c4d"] => Fails,
        }

        "invalid hex element" {
            vec!["0x1a2b", "invalid", "3c4d"] => Fails,
        }
    );

    // A multi-element vec can't satisfy a single-value spec.
    let string_var = create_test_variable("test_string", MlxVariableSpec::String);
    Case {
        scenario: "string spec rejects a vec",
        input: vec!["hello", "world"],
        expect: Fails,
    }
    .check(|raw| parsed(&string_var, raw));
    let enum_var = create_test_variable(
        "test_enum",
        MlxVariableSpec::Enum {
            options: vec!["low".to_string(), "high".to_string()],
        },
    );
    Case {
        scenario: "enum spec rejects a vec",
        input: vec!["low", "high"],
        expect: Fails,
    }
    .check(|raw| parsed(&enum_var, raw));
}

// test_sparse_array_validation tests that sparse arrays properly validate
// their Some values while ignoring None values.
#[test]
fn test_sparse_array_validation() {
    // Test enum array validation with sparse values
    let enum_array_var = create_test_variable(
        "test_sparse_validation",
        MlxVariableSpec::EnumArray {
            options: vec!["valid1".to_string(), "valid2".to_string()],
            size: 3,
        },
    );

    // Valid sparse array - None values should be ignored during validation
    let valid_sparse = enum_array_var
        .with(vec![
            Some("valid1".to_string()),
            None,
            Some("valid2".to_string()),
        ])
        .unwrap();

    assert_eq!(
        valid_sparse.value,
        MlxValueType::EnumArray(vec![
            Some("valid1".to_string()),
            None,
            Some("valid2".to_string())
        ])
    );

    // Invalid sparse array - Some values still need to be validated
    let invalid_sparse = enum_array_var.with(vec![
        Some("valid1".to_string()),
        None,
        Some("invalid".to_string()),
    ]);

    assert!(invalid_sparse.is_err());
    match invalid_sparse.unwrap_err() {
        MlxValueError::InvalidEnumArrayOption {
            position, value, ..
        } => {
            assert_eq!(position, 2);
            assert_eq!(value, "invalid");
        }
        _ => panic!("Expected InvalidEnumArrayOption error"),
    }
}

// test_display_formatting_sparse_arrays tests that sparse arrays display
// correctly with "-" for None values.
#[test]
fn test_display_formatting_sparse_arrays() {
    // Boolean array display
    let bool_var = create_test_variable(
        "test_bool_display",
        MlxVariableSpec::BooleanArray { size: 3 },
    );
    let bool_value = bool_var.with(vec![Some(true), None, Some(false)]).unwrap();
    assert_eq!(bool_value.to_display_string(), "[true, -, false]");

    // Integer array display
    let int_var = create_test_variable(
        "test_int_display",
        MlxVariableSpec::IntegerArray { size: 4 },
    );
    let int_value = int_var.with(vec![Some(42), None, Some(-10), None]).unwrap();
    assert_eq!(int_value.to_display_string(), "[42, -, -10, -]");

    // Enum array display
    let enum_var = create_test_variable(
        "test_enum_display",
        MlxVariableSpec::EnumArray {
            options: vec!["option1".to_string(), "option2".to_string()],
            size: 3,
        },
    );
    let enum_value = enum_var
        .with(vec![
            Some("option1".to_string()),
            None,
            Some("option2".to_string()),
        ])
        .unwrap();
    assert_eq!(enum_value.to_display_string(), "[option1, -, option2]");

    // Binary array display shows count information
    let binary_var = create_test_variable(
        "test_binary_display",
        MlxVariableSpec::BinaryArray { size: 4 },
    );
    let binary_value = binary_var
        .with(vec![Some(vec![0x1a]), None, Some(vec![0x2b, 0x3c]), None])
        .unwrap();
    assert_eq!(binary_value.to_display_string(), "[4 binary values, 2 set]");
}

// Only the typed array variants report as array types; every scalar -- and the
// untyped `Array` -- does not. Folds the four per-variant tests and the non-array
// loop into one table over `is_array_type`.
#[test]
fn is_array_type_flags_only_typed_arrays() {
    value_scenarios!(
        run = |value| value.is_array_type();
        "boolean array" {
            MlxValueType::BooleanArray(vec![Some(true), None, Some(false)]) => true,
        }

        "integer array" {
            MlxValueType::IntegerArray(vec![Some(42), None, Some(100)]) => true,
        }

        "enum array" {
            MlxValueType::EnumArray(vec![
                Some("option1".to_string()),
                None,
                Some("option2".to_string()),
            ]) => true,
        }

        "binary array" {
            MlxValueType::BinaryArray(vec![
                Some(vec![0x01, 0x02]),
                None,
                Some(vec![0x03, 0x04]),
            ]) => true,
        }

        "boolean scalar" {
            MlxValueType::Boolean(true) => false,
        }

        "integer scalar" {
            MlxValueType::Integer(42) => false,
        }

        "string scalar" {
            MlxValueType::String("test".to_string()) => false,
        }

        "enum scalar" {
            MlxValueType::Enum("option".to_string()) => false,
        }

        "preset" {
            MlxValueType::Preset(5) => false,
        }

        "binary scalar" {
            MlxValueType::Binary(vec![0x01, 0x02]) => false,
        }

        "bytes" {
            MlxValueType::Bytes(vec![0x01, 0x02]) => false,
        }

        "untyped string array" {
            MlxValueType::Array(vec!["item1".to_string(), "item2".to_string()]) => false,
        }

        "opaque" {
            MlxValueType::Opaque(vec![0x01, 0x02]) => false,
        }
    );
}

// `get_set_indices` lists the set (Some) positions of an array value in ascending
// order, or None for any non-array. Folds the per-variant cases, the edge cases
// (empty / all-set / all-unset / single), and the non-array loop into one table.
// Each row's exact index vec already pins the ordering, so the old separate
// "is it ascending?" assertion is subsumed.
#[test]
fn get_set_indices_lists_set_positions_for_arrays_only() {
    scenarios!(
        run = |value| value.get_set_indices().ok_or(());
        "boolean array, mixed set/unset" {
            MlxValueType::BooleanArray(vec![
                Some(true),
                None,
                Some(false),
                None,
                Some(true),
            ]) => Yields(vec![0, 2, 4]),
        }

        "integer array, leading gap" {
            MlxValueType::IntegerArray(vec![None, Some(42), Some(100), None]) => Yields(vec![1, 2]),
        }

        "enum array, interior gap" {
            MlxValueType::EnumArray(vec![
                Some("option1".to_string()),
                Some("option2".to_string()),
                None,
                Some("option3".to_string()),
            ]) => Yields(vec![0, 1, 3]),
        }

        "binary array, interior gaps" {
            MlxValueType::BinaryArray(vec![
                Some(vec![0x01, 0x02]),
                None,
                None,
                Some(vec![0x03, 0x04]),
            ]) => Yields(vec![0, 3]),
        }

        "all unset" {
            MlxValueType::BooleanArray(vec![None, None, None]) => Yields(vec![]),
        }

        "all set" {
            MlxValueType::IntegerArray(vec![Some(1), Some(2), Some(3), Some(4)]) => Yields(vec![0, 1, 2, 3]),
        }

        "empty array" {
            MlxValueType::BooleanArray(vec![]) => Yields(vec![]),
        }

        "single set element" {
            MlxValueType::EnumArray(vec![Some("only_option".to_string())]) => Yields(vec![0]),
        }

        "realistic sparse host pattern" {
            MlxValueType::EnumArray(vec![
                Some("HOST_0".to_string()),
                None,
                None,
                Some("HOST_3".to_string()),
                None,
                None,
                Some("HOST_6".to_string()),
                None,
            ]) => Yields(vec![0, 3, 6]),
        }

        "indices stay in ascending order" {
            MlxValueType::IntegerArray(vec![
                None,
                Some(10),
                None,
                Some(30),
                None,
                Some(50),
            ]) => Yields(vec![1, 3, 5]),
        }

        "boolean scalar has no indices" {
            MlxValueType::Boolean(true) => Fails,
        }

        "integer scalar has no indices" {
            MlxValueType::Integer(42) => Fails,
        }

        "string scalar has no indices" {
            MlxValueType::String("test".to_string()) => Fails,
        }

        "enum scalar has no indices" {
            MlxValueType::Enum("option".to_string()) => Fails,
        }

        "preset has no indices" {
            MlxValueType::Preset(5) => Fails,
        }

        "binary scalar has no indices" {
            MlxValueType::Binary(vec![0x01, 0x02]) => Fails,
        }

        "bytes have no indices" {
            MlxValueType::Bytes(vec![0x01, 0x02]) => Fails,
        }

        "untyped string array has no indices" {
            MlxValueType::Array(vec!["item1".to_string(), "item2".to_string()]) => Fails,
        }

        "opaque has no indices" {
            MlxValueType::Opaque(vec![0x01, 0x02]) => Fails,
        }
    );
}
