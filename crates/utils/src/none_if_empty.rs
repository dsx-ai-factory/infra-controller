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

/// Converts an empty value, or an optional empty value, to [`None`].
pub trait NoneIfEmpty {
    type Val;

    fn none_if_empty(self) -> Option<Self::Val>;
}

impl NoneIfEmpty for Option<String> {
    type Val = String;

    fn none_if_empty(self) -> Option<Self::Val> {
        self.filter(|value| !value.is_empty())
    }
}

impl NoneIfEmpty for String {
    type Val = String;

    fn none_if_empty(self) -> Option<Self::Val> {
        if self.is_empty() { None } else { Some(self) }
    }
}

impl<T> NoneIfEmpty for Option<Vec<T>> {
    type Val = Vec<T>;
    fn none_if_empty(self) -> Option<Self::Val> {
        match self {
            None => None,
            Some(v) if v.is_empty() => None,
            Some(v) => Some(v),
        }
    }
}

impl<T> NoneIfEmpty for Vec<T> {
    type Val = Vec<T>;
    fn none_if_empty(self) -> Option<Self::Val> {
        if self.is_empty() { None } else { Some(self) }
    }
}

impl<K, V> NoneIfEmpty for Option<HashMap<K, V>> {
    type Val = HashMap<K, V>;
    fn none_if_empty(self) -> Option<Self::Val> {
        match self {
            None => None,
            Some(h) if h.is_empty() => None,
            Some(h) => Some(h),
        }
    }
}

impl<K, V> NoneIfEmpty for HashMap<K, V> {
    type Val = HashMap<K, V>;
    fn none_if_empty(self) -> Option<Self::Val> {
        if self.is_empty() { None } else { Some(self) }
    }
}

impl<'a> NoneIfEmpty for &'a str {
    type Val = &'a str;
    fn none_if_empty(self) -> Option<Self::Val> {
        if self.is_empty() { None } else { Some(self) }
    }
}

impl<'a> NoneIfEmpty for Option<&'a str> {
    type Val = &'a str;
    fn none_if_empty(self) -> Option<Self::Val> {
        self.filter(|value| !value.is_empty())
    }
}

impl<'a, T> NoneIfEmpty for &'a [T] {
    type Val = &'a [T];
    fn none_if_empty(self) -> Option<Self::Val> {
        if self.is_empty() { None } else { Some(self) }
    }
}

impl<'a, T> NoneIfEmpty for Option<&'a [T]> {
    type Val = &'a [T];
    fn none_if_empty(self) -> Option<Self::Val> {
        self.filter(|value| !value.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn filters_empty_strings() {
        value_scenarios!(
            run = |value| value.none_if_empty();
            "option string" {
                None::<String> => None,
                Some(String::new()) => None,
                Some("value".to_string()) => Some("value".to_string()),
            }
        );

        value_scenarios!(
            run = |value: String| value.none_if_empty();
            "string" {
                String::new() => None,
                "value".to_string() => Some("value".to_string()),
            }
        );
    }

    #[test]
    fn filters_empty_borrowed_strings() {
        value_scenarios!(
            run = |value| value.none_if_empty();
            "optional borrowed string" {
                None::<&str> => None,
                Some("") => None,
                Some("value") => Some("value"),
            }
        );

        value_scenarios!(
            run = |value: &str| value.none_if_empty();
            "borrowed string" {
                "" => None,
                "value" => Some("value"),
            }
        );
    }

    #[test]
    fn filters_empty_vectors() {
        value_scenarios!(
            run = |value| value.none_if_empty();
            "option vector" {
                None::<Vec<u8>> => None,
                Some(Vec::new()) => None,
                Some(vec![1, 2]) => Some(vec![1, 2]),
            }
        );

        value_scenarios!(
            run = |value: Vec<u8>| value.none_if_empty();
            "vector" {
                Vec::new() => None,
                vec![1, 2] => Some(vec![1, 2]),
            }
        );
    }

    #[test]
    fn filters_empty_borrowed_slices() {
        value_scenarios!(
            run = |value| value.none_if_empty();
            "optional borrowed slice" {
                None::<&[u8]> => None,
                Some(&[] as &[u8]) => None,
                Some(&[1_u8, 2] as &[u8]) => Some(&[1_u8, 2] as &[u8]),
            }
        );

        value_scenarios!(
            run = |value: &[u8]| value.none_if_empty();
            "borrowed slice" {
                &[] as &[u8] => None,
                &[1_u8, 2] as &[u8] => Some(&[1_u8, 2] as &[u8]),
            }
        );
    }

    #[test]
    fn filters_empty_hash_maps() {
        value_scenarios!(
            run = |value| value.none_if_empty();
            "option hash map" {
                None::<HashMap<&str, u8>> => None,
                Some(HashMap::new()) => None,
                Some(HashMap::from([("key", 1)])) => Some(HashMap::from([("key", 1)])),
            }
        );

        value_scenarios!(
            run = |value: HashMap<&str, u8>| value.none_if_empty();
            "hash map" {
                HashMap::new() => None,
                HashMap::from([("key", 1)]) => Some(HashMap::from([("key", 1)])),
            }
        );
    }
}
