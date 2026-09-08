/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Absolute, lowercased DNS name. The trailing dot is part of the value.
///
/// Labels follow RFC 2181 §11, which places no syntax restriction on a label
/// beyond its length; this type additionally requires printable ASCII because
/// names reach it in presentation form. Questions arrive with labels such as
/// `_dmarc` that are legal DNS names but not hostnames, and those must classify
/// as NODATA/NXDOMAIN inside a held zone rather than fail to parse.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fqdn(String);

/// Why a string is not a valid [`Fqdn`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum NameError {
    /// The name has no labels (empty string or a lone `.`).
    #[error("empty DNS name")]
    Empty,
    /// The name exceeds 253 presentation characters.
    #[error("name exceeds 253 characters")]
    TooLong,
    /// A label is empty, longer than 63 octets, or contains a character that
    /// is not printable ASCII.
    #[error("invalid DNS label")]
    InvalidLabel,
}

/// Presentation-form limit that keeps the wire form (one length octet per
/// label plus the root) within 255 octets.
const MAX_NAME_CHARS: usize = 253;

impl Fqdn {
    /// Parse a name in presentation form. Surrounding whitespace, case, and one
    /// trailing root dot are normalised away; the result always ends in `.`.
    /// A second trailing dot is an empty label and fails.
    pub fn parse(name: &str) -> Result<Self, NameError> {
        let trimmed = name.trim();
        let stripped = trimmed
            .strip_suffix('.')
            .unwrap_or(trimmed)
            .to_ascii_lowercase();
        if stripped.is_empty() {
            return Err(NameError::Empty);
        }
        if stripped.len() > MAX_NAME_CHARS {
            return Err(NameError::TooLong);
        }
        for label in stripped.split('.') {
            parse_label(label)?;
        }
        Ok(Self(format!("{stripped}.")))
    }

    /// The normalised name, including its trailing dot.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Every zone name this name could fall under, longest first, without the
    /// trailing dot: `gpu.mysite.example.com.` gives `gpu.mysite.example.com`,
    /// `mysite.example.com`, `example.com`, `com`. The spelling matches
    /// `domains.name` after `lower(rtrim(name, '.'))`.
    pub fn suffixes(&self) -> Vec<String> {
        let labels = self.labels();
        (0..labels.len())
            .map(|start| labels[start..].join("."))
            .collect()
    }

    fn labels(&self) -> Vec<&str> {
        self.0.trim_end_matches('.').split('.').collect()
    }
}

impl fmt::Display for Fqdn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Fqdn {
    type Err = NameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::parse(name)
    }
}

/// RFC 2181 label: non-empty, at most 63 octets, no dot. Printable ASCII only,
/// since names reach us in presentation form.
fn parse_label(label: &str) -> Result<&str, NameError> {
    if label.is_empty() || label.len() > 63 {
        return Err(NameError::InvalidLabel);
    }
    if !label.bytes().all(|b| b.is_ascii_graphic() && b != b'.') {
        return Err(NameError::InvalidLabel);
    }
    Ok(label)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn fqdn_parse_normalizes_and_rejects() {
        scenarios!(
            run = Fqdn::parse;
            "absolute form" {
                "Example.COM." => Yields(Fqdn("example.com.".to_string())),
                "example.com" => Yields(Fqdn("example.com.".to_string())),
            }
            "non-hostname labels are still names (RFC 2181)" {
                "_dmarc.example.com." => Yields(Fqdn("_dmarc.example.com.".to_string())),
            }
            "invalid" {
                "" => FailsWith(NameError::Empty),
                "." => FailsWith(NameError::Empty),
                "a..b" => FailsWith(NameError::InvalidLabel),
                "example.com.." => FailsWith(NameError::InvalidLabel),
                "a b.example.com" => FailsWith(NameError::InvalidLabel),
            }
        );
    }

    #[test]
    fn fqdn_display_and_from_str_round_trip() {
        let name: Fqdn = "Example.COM".parse().unwrap();
        assert_eq!(name.to_string(), "example.com.");
        assert_eq!(name.to_string().parse::<Fqdn>().unwrap(), name);
    }

    #[test]
    fn suffixes_lists_every_enclosing_zone_longest_first() {
        let name = Fqdn::parse("Gpu.mysite.example.com.").unwrap();
        assert_eq!(
            name.suffixes(),
            [
                "gpu.mysite.example.com",
                "mysite.example.com",
                "example.com",
                "com"
            ]
        );
    }
}
