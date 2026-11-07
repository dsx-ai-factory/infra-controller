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

/*!
 *  Common code for working with the database. Provides constants and generics
 *  for making boilerplate copy-pasta code handled in a common way.
 */

use std::collections::HashSet;
use std::convert::From;
use std::fmt;
use std::hash::Hash;
use std::vec::Vec;

use crate::records::{MeasurementBundleValueRecord, MeasurementReportValueRecord};

// PcrRange is a small struct used when parsing
// --pcr-register values from the CLI as part of
// the parse_pcr_index_input function.
#[derive(Clone, Debug)]
pub struct PcrRange {
    pub start: usize,
    pub end: usize,
}

impl fmt::Display for PcrRange {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

/// PcrSet is a list of PCR register indexes that are expected
/// to be targeted. For example: 0,1,2,5,6. With this PCR set,
/// an incoming list of PcrRegisterValues will have any values
/// whose indexes match the register numbers from the PcrSet.
///
/// This includes implementations for iterating.
#[derive(Clone, Debug)]
pub struct PcrSet(pub Vec<i16>);

impl Default for PcrSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PcrSet {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn iter(&'_ self) -> PcrSetIter<'_> {
        PcrSetIter {
            current_slice: &self.0,
        }
    }
}

impl fmt::Display for PcrSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let vals: Vec<String> = self.iter().map(|&val| val.to_string()).collect();
        write!(f, "{}", vals.join(","))
    }
}

impl IntoIterator for PcrSet {
    type Item = i16;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'p> IntoIterator for &'p PcrSet {
    type Item = &'p i16;
    type IntoIter = std::slice::Iter<'p, i16>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Clone, Debug)]
pub struct PcrSetIter<'i> {
    current_slice: &'i [i16],
}

impl<'i> Iterator for PcrSetIter<'i> {
    type Item = &'i i16;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.current_slice.is_empty() {
            let (first, rest) = self.current_slice.split_first().unwrap();
            self.current_slice = rest;
            Some(first)
        } else {
            None
        }
    }
}

pub fn parse_pcr_index_input(arg: &str) -> super::Result<PcrSet> {
    let groups: Vec<&str> = arg.split(',').collect();
    let mut index_set: HashSet<i16> = HashSet::new();
    for group in groups {
        if group.contains('-') {
            let pcr_range = parse_range(group)?;
            for index in pcr_range.start..=pcr_range.end {
                index_set.insert(index as i16);
            }
        } else {
            index_set.insert(group.parse::<i16>().map_err(|e| {
                super::Error::Parse(format!(
                    "parse_pcr_index_input group parse failed: {group}, {e}"
                ))
            })?);
        }
    }

    let mut vals: Vec<i16> = index_set.into_iter().collect();
    vals.sort();
    Ok(PcrSet(vals))
}

pub fn parse_range(arg: &str) -> super::Result<PcrRange> {
    let range: Vec<usize> = arg
        .split('-')
        .map(|s| {
            s.parse::<usize>()
                .map_err(|_| super::Error::Parse(format!("parse_range failed on {arg}")))
        })
        .collect::<super::Result<Vec<usize>>>()?;

    if range.len() != 2 {
        return Err(super::Error::Parse(String::from(
            "parse_range range expected 2 values",
        )));
    }

    for endpoint in &range {
        i16::try_from(*endpoint).map_err(|_| {
            super::Error::Parse(format!("pcr range endpoint is out of bounds: {endpoint}"))
        })?;
    }

    if range[0] > range[1] {
        return Err(super::Error::Parse(String::from(
            "end must be greater than start",
        )));
    }

    Ok(PcrRange {
        start: range[0],
        end: range[1],
    })
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct PcrRegisterValue {
    pub pcr_register: i16,
    pub sha_any: String,
}

pub struct PcrRegisterValueVec(pub Vec<PcrRegisterValue>);

impl From<MeasurementBundleValueRecord> for PcrRegisterValue {
    fn from(val: MeasurementBundleValueRecord) -> Self {
        PcrRegisterValue {
            pcr_register: val.pcr_register,
            sha_any: val.sha_any,
        }
    }
}

impl From<MeasurementReportValueRecord> for PcrRegisterValue {
    fn from(val: MeasurementReportValueRecord) -> Self {
        Self {
            pcr_register: val.pcr_register,
            sha_any: val.sha_any,
        }
    }
}

impl From<Vec<String>> for PcrRegisterValueVec {
    fn from(pcr_strings: Vec<String>) -> Self {
        let pcr_register_values = pcr_strings
            .into_iter()
            .enumerate()
            .map(|(pcr_index, pcr_val)| PcrRegisterValue {
                pcr_register: pcr_index as i16,
                sha_any: pcr_val,
            })
            .collect();
        PcrRegisterValueVec(pcr_register_values)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};

    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct RangeSummary {
        start: usize,
        end: usize,
        display: String,
    }

    #[test]
    fn parse_range_cases() {
        check_cases(
            [
                Case {
                    scenario: "equal endpoints form a singleton range",
                    input: "4-4",
                    expect: Yields(RangeSummary {
                        start: 4,
                        end: 4,
                        display: "4-4".to_string(),
                    }),
                },
                Case {
                    scenario: "ascending endpoints form an inclusive range",
                    input: "0-23",
                    expect: Yields(RangeSummary {
                        start: 0,
                        end: 23,
                        display: "0-23".to_string(),
                    }),
                },
                Case {
                    scenario: "maximum i16 endpoints are accepted",
                    input: "32767-32767",
                    expect: Yields(RangeSummary {
                        start: 32767,
                        end: 32767,
                        display: "32767-32767".to_string(),
                    }),
                },
                Case {
                    scenario: "reversed endpoints are rejected",
                    input: "5-4",
                    expect: Fails,
                },
                Case {
                    scenario: "more than two endpoints are rejected",
                    input: "1-2-3",
                    expect: Fails,
                },
                Case {
                    scenario: "nonnumeric endpoints are rejected",
                    input: "one-2",
                    expect: Fails,
                },
                Case {
                    scenario: "overflowing start endpoint is rejected",
                    input: "32768-32768",
                    expect: Fails,
                },
                Case {
                    scenario: "overflowing end endpoint is rejected",
                    input: "0-32768",
                    expect: Fails,
                },
                Case {
                    scenario: "wrapped zero endpoint is rejected",
                    input: "65536-65536",
                    expect: Fails,
                },
            ],
            |input| {
                parse_range(input)
                    .map(|range| {
                        let display = range.to_string();
                        RangeSummary {
                            start: range.start,
                            end: range.end,
                            display,
                        }
                    })
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn parse_pcr_index_input_cases() {
        check_cases(
            [
                Case {
                    scenario: "singleton",
                    input: "7",
                    expect: Yields(vec![7]),
                },
                Case {
                    scenario: "inclusive range",
                    input: "2-5",
                    expect: Yields(vec![2, 3, 4, 5]),
                },
                Case {
                    scenario: "mixed singletons and range",
                    input: "0,2-4,7",
                    expect: Yields(vec![0, 2, 3, 4, 7]),
                },
                Case {
                    scenario: "values are sorted and deduplicated",
                    input: "4,2,4,2-3",
                    expect: Yields(vec![2, 3, 4]),
                },
                Case {
                    scenario: "maximum i16 singleton",
                    input: "32767",
                    expect: Yields(vec![32767]),
                },
                Case {
                    scenario: "empty input is rejected",
                    input: "",
                    expect: Fails,
                },
                Case {
                    scenario: "malformed range is rejected",
                    input: "1-2-3",
                    expect: Fails,
                },
                Case {
                    scenario: "nonnumeric singleton is rejected",
                    input: "one",
                    expect: Fails,
                },
                Case {
                    scenario: "overflowing singleton is rejected",
                    input: "32768",
                    expect: Fails,
                },
                Case {
                    scenario: "overflowing range is rejected before expansion",
                    input: "32768-32768",
                    expect: Fails,
                },
            ],
            |input| parse_pcr_index_input(input).map(|set| set.0).map_err(drop),
        );
    }

    enum SetConstruction {
        New,
        Default,
        Values(Vec<i16>),
    }

    #[derive(Debug, Eq, PartialEq)]
    struct SetSummary {
        iter: Vec<i16>,
        borrowed: Vec<i16>,
        owned: Vec<i16>,
        display: String,
    }

    #[test]
    fn pcr_set_iteration_and_display_cases() {
        check_values(
            [
                Check {
                    scenario: "new set is empty",
                    input: SetConstruction::New,
                    expect: SetSummary {
                        iter: vec![],
                        borrowed: vec![],
                        owned: vec![],
                        display: String::new(),
                    },
                },
                Check {
                    scenario: "default set is empty",
                    input: SetConstruction::Default,
                    expect: SetSummary {
                        iter: vec![],
                        borrowed: vec![],
                        owned: vec![],
                        display: String::new(),
                    },
                },
                Check {
                    scenario: "custom set retains order in every iterator",
                    input: SetConstruction::Values(vec![0, 7, 23]),
                    expect: SetSummary {
                        iter: vec![0, 7, 23],
                        borrowed: vec![0, 7, 23],
                        owned: vec![0, 7, 23],
                        display: "0,7,23".to_string(),
                    },
                },
            ],
            |construction| {
                let set = match construction {
                    SetConstruction::New => PcrSet::new(),
                    SetConstruction::Default => PcrSet::default(),
                    SetConstruction::Values(values) => PcrSet(values),
                };
                SetSummary {
                    iter: set.iter().copied().collect(),
                    borrowed: (&set).into_iter().copied().collect(),
                    owned: set.clone().into_iter().collect(),
                    display: set.to_string(),
                }
            },
        );
    }

    #[test]
    fn string_vector_projection_cases() {
        check_values(
            [
                Check {
                    scenario: "empty values",
                    input: vec![],
                    expect: vec![],
                },
                Check {
                    scenario: "values receive sequential PCR indexes",
                    input: vec!["sha-0".to_string(), "sha-1".to_string()],
                    expect: vec![
                        PcrRegisterValue {
                            pcr_register: 0,
                            sha_any: "sha-0".to_string(),
                        },
                        PcrRegisterValue {
                            pcr_register: 1,
                            sha_any: "sha-1".to_string(),
                        },
                    ],
                },
            ],
            |values| PcrRegisterValueVec::from(values).0,
        );
    }
}
