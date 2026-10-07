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
// Apache License 2.0 - https://github.com/maxlambrecht/rust-spiffe/blob/main/LICENSE
//
// This is a copy of https://github.com/maxlambrecht/rust-spiffe/blob/main/src/spiffe_id/mod.rs at commit 57604e9

//! SPIFFE-ID and TrustDomain types compliant with the SPIFFE standard.

use std::fmt;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use thiserror::Error;

const SPIFFE_SCHEME: &str = "spiffe";
const SCHEME_PREFIX: &str = "spiffe://";

/// Represents a [SPIFFE ID](https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE-ID.md#2-spiffe-identity).
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SpiffeId {
    trust_domain: TrustDomain,
    path: String,
}

/// Represents a [SPIFFE Trust domain](https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE-ID.md#21-trust-domain)
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TrustDomain {
    name: String,
}

/// An error that can arise parsing a SPIFFE ID.
#[derive(Debug, Error, PartialEq, Clone)]
#[non_exhaustive]
pub enum SpiffeIdError {
    /// An empty string cannot be parsed as a SPIFFE ID.
    #[error("cannot be empty")]
    Empty,

    /// The trust domain name of SPIFFE ID cannot be empty.
    #[error("trust domain is missing")]
    MissingTrustDomain,

    /// A SPIFFE ID must have a scheme 'spiffe'.
    #[error("scheme is missing or invalid")]
    WrongScheme,

    /// A trust domain name can only contain chars in a limited char set.
    #[error(
        "trust domain characters are limited to lowercase letters, numbers, dots, dashes, and \
         underscores"
    )]
    BadTrustDomainChar,

    /// A path segment can only contain chars in a limited char set.
    #[error(
        "path segment characters are limited to letters, numbers, dots, dashes, and underscores"
    )]
    BadPathSegmentChar,

    /// Path cannot contain empty segments, e.g '//'
    #[error("path cannot contain empty segments")]
    EmptySegment,

    /// Path cannot contain dot segments, e.g '/.', '/..'
    #[error("path cannot contain dot segments")]
    DotSegment,

    /// Path cannot have a trailing slash.
    #[error("path cannot have a trailing slash")]
    TrailingSlash,
}

impl SpiffeId {
    /// Attempts to parse a SPIFFE ID from the given id string.
    ///
    /// # Arguments
    ///
    /// * `id` - A SPIFFE ID, e.g. 'spiffe://trustdomain/path/other'
    ///
    /// # Errors
    ///
    /// If the function cannot parse the input as a SPIFFE ID, a [`SpiffeIdError`] variant will be returned.
    pub fn new(id: &str) -> Result<Self, SpiffeIdError> {
        if id.is_empty() {
            return Err(SpiffeIdError::Empty);
        }
        if id.len() < SCHEME_PREFIX.len() {
            return Err(SpiffeIdError::WrongScheme);
        }

        if &id[..SCHEME_PREFIX.len()] != SCHEME_PREFIX {
            return Err(SpiffeIdError::WrongScheme);
        }

        let rest = &id[SCHEME_PREFIX.len()..];

        let mut i = 0;

        for c in rest.chars() {
            if c == '/' {
                break;
            }

            if !is_valid_trust_domain_char(c) {
                return Err(SpiffeIdError::BadTrustDomainChar);
            }
            i += 1;
        }

        if i == 0 {
            return Err(SpiffeIdError::MissingTrustDomain);
        }

        let td = &rest[0..i];
        let path = &rest[i..];

        if !path.is_empty() {
            validate_path(path)?;
        }

        let trust_domain = TrustDomain {
            name: td.to_string(),
        };
        let path = path.to_string();
        Ok(SpiffeId { trust_domain, path })
    }

    /// Returns a new SPIFFE ID in the given trust domain with joined
    /// path segments. The path segments must be valid according to the SPIFFE
    /// specification and must not contain path separators.
    /// See <https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE-ID.md#22-path>
    ///
    /// # Arguments
    ///
    /// * `trust_domain` - A [`TrustDomain`] object.
    /// * `segments` - A slice of path segments.
    ///
    /// # Errors
    ///
    /// If the segments contain not allowed characters, a [`SpiffeIdError`] variant will be returned.
    #[cfg(test)] // currently only used by tests
    pub fn from_segments(
        trust_domain: TrustDomain,
        segments: &[&str],
    ) -> Result<Self, SpiffeIdError> {
        let mut path = String::new();
        for p in segments {
            validate_path(p)?;
            path = format!("{path}/{p}");
        }

        Ok(SpiffeId { trust_domain, path })
    }

    /// Returns the trust domain of the SPIFFE ID.
    pub fn trust_domain(&self) -> &TrustDomain {
        &self.trust_domain
    }

    /// Returns the path of the SPIFFE ID.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns `true` if this SPIFFE ID has the given TrustDomain.
    pub fn is_member_of(&self, trust_domain: &TrustDomain) -> bool {
        &self.trust_domain == trust_domain
    }
}

#[test]
fn test_new() {
    use crate::spiffe_id::SpiffeId;

    let spiffe_id = SpiffeId::new("spiffe://trustdomain/path").unwrap();
    assert_eq!("trustdomain", spiffe_id.trust_domain().to_string());
    assert_eq!("/path", spiffe_id.path());
}

#[test]
fn test_from_segments() {
    use crate::spiffe_id::{SpiffeId, TrustDomain};

    let trust_domain = TrustDomain::new("trustdomain").unwrap();
    let spiffe_id = SpiffeId::from_segments(trust_domain, &["path1", "path2", "path3"]).unwrap();
    assert_eq!(
        "spiffe://trustdomain/path1/path2/path3",
        spiffe_id.to_string()
    );
}

impl Display for SpiffeId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}{}", SPIFFE_SCHEME, self.trust_domain, self.path)
    }
}

impl FromStr for SpiffeId {
    type Err = SpiffeIdError;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        Self::new(id)
    }
}

impl TryFrom<String> for SpiffeId {
    type Error = SpiffeIdError;
    fn try_from(s: String) -> Result<SpiffeId, Self::Error> {
        Self::new(s.as_ref())
    }
}

impl TryFrom<&str> for SpiffeId {
    type Error = SpiffeIdError;
    fn try_from(s: &str) -> Result<SpiffeId, Self::Error> {
        Self::new(s)
    }
}

/// Validates that a path string is a conformant path for a SPIFFE ID.
/// See <https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE-ID.md#22-path>
pub fn validate_path(path: &str) -> Result<(), SpiffeIdError> {
    if path.is_empty() {
        return Err(SpiffeIdError::Empty);
    }

    let mut segment_start = 0;
    let mut segment_end = 0;

    while segment_end < path.len() {
        let c = match path.chars().nth(segment_end) {
            None => break,
            Some(c) => c,
        };
        if c == '/' {
            match &path[segment_start..segment_end] {
                "/" => return Err(SpiffeIdError::EmptySegment),
                "/." | "/.." => return Err(SpiffeIdError::DotSegment),
                _ => {}
            }
            segment_start = segment_end;
            segment_end += 1;
            continue;
        }

        if !is_valid_path_segment_char(c) {
            return Err(SpiffeIdError::BadPathSegmentChar);
        }
        segment_end += 1;
    }

    match &path[segment_start..segment_end] {
        "/" => return Err(SpiffeIdError::TrailingSlash),
        "/." | "/.." => return Err(SpiffeIdError::DotSegment),
        _ => {}
    }

    Ok(())
}

fn is_valid_path_segment_char(c: char) -> bool {
    matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '.' | '_')
}

impl TrustDomain {
    /// Attempts to parse a TrustDomain instance from the given name or spiffe_id string.
    ///
    /// # Arguments
    ///
    /// * `id_or_name` - Name of a trust domain, it also can be a SPIFFE ID string from which the domain name
    ///   is extracted.
    ///
    /// # Errors
    ///
    /// If the function cannot parse the input as a Trust domain, a [`SpiffeIdError`] variant will be returned.
    pub fn new(id_or_name: &str) -> Result<Self, SpiffeIdError> {
        if id_or_name.is_empty() {
            return Err(SpiffeIdError::MissingTrustDomain);
        }

        // Something looks kinda like a scheme separator, let's try to parse as
        // an ID. We use :/ instead of :// since the diagnostics are better for
        // a bad input like spiffe:/trustdomain.
        if id_or_name.contains(":/") {
            let spiffe_id = SpiffeId::try_from(id_or_name)?;
            return Ok(spiffe_id.trust_domain);
        }

        validate_trust_domain_name(id_or_name)?;
        Ok(TrustDomain {
            name: id_or_name.to_string(),
        })
    }

    /// Returns a string representation of the SPIFFE ID of the trust domain,
    /// e.g. "spiffe://example.org".
    pub fn id_string(&self) -> String {
        format!("{}://{}", SPIFFE_SCHEME, self.name)
    }
}

#[test]
fn test_new_trust_domain() {
    use crate::spiffe_id::TrustDomain;

    let trust_domain = TrustDomain::new("domain.test").unwrap();
    assert_eq!("domain.test", trust_domain.to_string());
    assert_eq!("spiffe://domain.test", trust_domain.id_string());

    let trust_domain = TrustDomain::new("spiffe://example.org/path").unwrap();
    assert_eq!("example.org", trust_domain.to_string());
    assert_eq!("spiffe://example.org", trust_domain.id_string());
}

impl Display for TrustDomain {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl AsRef<str> for TrustDomain {
    fn as_ref(&self) -> &str {
        self.name.as_str()
    }
}

impl FromStr for TrustDomain {
    type Err = SpiffeIdError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        TrustDomain::new(name)
    }
}

impl TryFrom<&str> for TrustDomain {
    type Error = SpiffeIdError;

    fn try_from(name: &str) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}

impl TryFrom<String> for TrustDomain {
    type Error = SpiffeIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value.as_ref())
    }
}

fn validate_trust_domain_name(name: &str) -> Result<(), SpiffeIdError> {
    for c in name.chars() {
        if !is_valid_trust_domain_char(c) {
            return Err(SpiffeIdError::BadTrustDomainChar);
        }
    }
    Ok(())
}

fn is_valid_trust_domain_char(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '-' | '.' | '_')
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    pub(crate) const TD_CHARS: &[char] = &[
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r',
        's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
        '.', '-', '_',
    ];

    const PATH_CHARS: &[char] = &[
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r',
        's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J',
        'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '0', '1',
        '2', '3', '4', '5', '6', '7', '8', '9', '.', '-', '_',
    ];

    #[test]
    fn spiffe_id_parses_valid_inputs() {
        scenarios!(
            run = SpiffeId::from_str;
            "valid SPIFFE ID without a path" {
                "spiffe://trustdomain" => Yields(SpiffeId {
                    trust_domain: TrustDomain::from_str("trustdomain").unwrap(),
                    path: "".to_string(),
                }),
            }

            "valid URI with a path" {
                "spiffe://trustdomain/path/element" => Yields(SpiffeId {
                    trust_domain: TrustDomain::from_str("trustdomain").unwrap(),
                    path: "/path/element".to_string(),
                }),
            }
        );
    }

    #[test]
    fn test_is_member_of() {
        let spiffe_id = SpiffeId::from_str("spiffe://example.org").unwrap();
        let trust_domain = TrustDomain::from_str("example.org").unwrap();

        assert!(spiffe_id.is_member_of(&trust_domain));
    }

    #[test]
    fn test_new_from_string() {
        let id_string = String::from("spiffe://example.org/path/element");
        let spiffe_id = SpiffeId::try_from(id_string).unwrap();

        let expected_trust_domain = TrustDomain::from_str("example.org").unwrap();

        assert_eq!(spiffe_id.trust_domain, expected_trust_domain);
        assert_eq!(spiffe_id.path(), "/path/element");
    }

    #[test]
    fn test_to_string() {
        let spiffe_id = SpiffeId::from_str("spiffe://example.org/path/element").unwrap();
        assert_eq!(spiffe_id.to_string(), "spiffe://example.org/path/element");
    }

    #[test]
    fn test_try_from_str() {
        let spiffe_id = SpiffeId::try_from("spiffe://example.org/path").unwrap();

        assert_eq!(
            spiffe_id.trust_domain,
            TrustDomain::from_str("example.org").unwrap()
        );
        assert_eq!(spiffe_id.path, "/path");
    }

    #[test]
    fn test_try_from_string() {
        let spiffe_id = SpiffeId::try_from(String::from("spiffe://example.org/path")).unwrap();

        assert_eq!(
            spiffe_id.trust_domain,
            TrustDomain::from_str("example.org").unwrap()
        );
        assert_eq!(spiffe_id.path, "/path");
    }

    #[test]
    fn spiffe_id_rejects_invalid_inputs() {
        scenarios!(
            run = SpiffeId::from_str;
            "empty string" {
                "" => FailsWith(SpiffeIdError::Empty),
            }

            "too short to hold a scheme" {
                "spif" => FailsWith(SpiffeIdError::WrongScheme),
            }

            "bare IP address" {
                "192.168.2.2:6688" => FailsWith(SpiffeIdError::WrongScheme),
            }

            "non-spiffe scheme" {
                "http://domain.test/path/element" => FailsWith(SpiffeIdError::WrongScheme),
            }

            "single-slash authority" {
                "spiffe:/path/element" => FailsWith(SpiffeIdError::WrongScheme),
            }

            "empty authority after slashes" {
                "spiffe:///path/element" => FailsWith(SpiffeIdError::MissingTrustDomain),
            }

            "no authority slashes" {
                "spiffe:path/element" => FailsWith(SpiffeIdError::WrongScheme),
            }

            "query string in path" {
                "spiffe://domain.test/path/element?query=1" => {
                    FailsWith(SpiffeIdError::BadPathSegmentChar)
                }
            }

            "fragment in path" {
                "spiffe://domain.test/path/element#fragment-1" => {
                    FailsWith(SpiffeIdError::BadPathSegmentChar)
                }
            }

            "port in trust domain" {
                "spiffe://domain.test:8080/path/element" => {
                    FailsWith(SpiffeIdError::BadTrustDomainChar)
                }
            }

            "user info in trust domain" {
                "spiffe://user:password@test.org/path/element" => {
                    FailsWith(SpiffeIdError::BadTrustDomainChar)
                }
            }

            "trailing slash, no path" {
                "spiffe://test.org/" => FailsWith(SpiffeIdError::TrailingSlash),
            }

            "empty segment" {
                "spiffe://test.org//" => FailsWith(SpiffeIdError::EmptySegment),
            }

            "trailing slash after a path" {
                "spiffe://test.org/path/other/" => FailsWith(SpiffeIdError::TrailingSlash),
            }

            "dot segment" {
                "spiffe://test.org/./other" => FailsWith(SpiffeIdError::DotSegment),
            }

            "double-dot segment" {
                "spiffe://test.org/../other" => FailsWith(SpiffeIdError::DotSegment),
            }
        );
    }

    #[test]
    fn test_parse_with_all_chars() {
        // Go all the way through 255, which ensures we reject UTF-8 appropriately
        for i in 0..=255_u8 {
            let c = i as char;

            // Don't test '/' since it is the delimiter between path segments
            if c == '/' {
                continue;
            }

            let path = format!("/path{c}");
            let id = format!("spiffe://trustdomain{path}");

            if PATH_CHARS.contains(&c) {
                let spiffe_id = SpiffeId::new(&id).unwrap();
                assert_eq!(spiffe_id.to_string(), id)
            } else {
                assert_eq!(
                    SpiffeId::new(&id).unwrap_err(),
                    SpiffeIdError::BadPathSegmentChar
                );
            }

            let td = format!("spiffe://trustdomain{c}");

            if TD_CHARS.contains(&c) {
                let spiffe_id = SpiffeId::new(&td).unwrap();
                assert_eq!(spiffe_id.to_string(), td)
            } else {
                assert_eq!(
                    SpiffeId::new(&td).unwrap_err(),
                    SpiffeIdError::BadTrustDomainChar
                );
            }
        }
    }

    #[test]
    fn test_from_segments_with_all_chars() {
        // Go all the way through 255, which ensures we reject UTF-8 appropriately
        for i in 0..=255_u8 {
            let c = i as char;

            let path = format!("path{c}");
            let trust_domain = TrustDomain::new("trustdomain").unwrap();

            if PATH_CHARS.contains(&c) {
                let spiffe_id = SpiffeId::from_segments(trust_domain, &[path.as_str()]).unwrap();
                assert_eq!(
                    spiffe_id.to_string(),
                    format!("spiffe://trustdomain/{path}")
                )
            } else if c == '/' {
                assert_eq!(
                    SpiffeId::from_segments(trust_domain, &[path.as_str()]).unwrap_err(),
                    SpiffeIdError::TrailingSlash
                );
            } else {
                assert_eq!(
                    SpiffeId::from_segments(trust_domain, &[path.as_str()]).unwrap_err(),
                    SpiffeIdError::BadPathSegmentChar
                );
            }
        }
    }

    #[test]
    fn trust_domain_parses_valid_inputs() {
        scenarios!(
            run = TrustDomain::new;
            "bare domain name" {
                "trustdomain" => Yields(TrustDomain {
                    name: "trustdomain".to_string(),
                }),
            }

            "SPIFFE ID without a path" {
                "spiffe://other.test" => Yields(TrustDomain {
                    name: "other.test".to_string(),
                }),
            }

            "SPIFFE ID with a path" {
                "spiffe://domain.test/path/element" => Yields(TrustDomain {
                    name: "domain.test".to_string(),
                }),
            }
        );
    }

    #[test]
    fn trust_domain_rejects_invalid_inputs() {
        scenarios!(
            run = TrustDomain::new;
            "empty string" {
                "" => FailsWith(SpiffeIdError::MissingTrustDomain),
            }

            "non-spiffe scheme" {
                "other://domain.test" => FailsWith(SpiffeIdError::WrongScheme),
            }

            "port in the domain" {
                "spiffe://domain.test:80" => FailsWith(SpiffeIdError::BadTrustDomainChar),
            }

            "user info in the domain" {
                "spiffe://user:pass@domain.test" => {
                    FailsWith(SpiffeIdError::BadTrustDomainChar)
                }
            }

            "space in the domain" {
                "spiffe:// domain.test" => FailsWith(SpiffeIdError::BadTrustDomainChar),
            }

            "empty scheme" {
                "://domain.test" => FailsWith(SpiffeIdError::WrongScheme),
            }

            "empty domain" {
                "spiffe:///path" => FailsWith(SpiffeIdError::MissingTrustDomain),
            }
        );
    }

    #[test]
    fn test_trust_domain_equals() {
        let td_1 = TrustDomain::new("domain.test").unwrap();
        let td_2 = TrustDomain::new("domain.test").unwrap();
        assert_eq!(td_1, td_2);
    }

    #[test]
    fn test_trust_domain_not_equals() {
        let td_1 = TrustDomain::new("domain.test").unwrap();
        let td_2 = TrustDomain::new("other.test").unwrap();
        assert_ne!(td_1, td_2);
    }

    #[test]
    fn test_trust_domain_to_string() {
        let trust_domain = TrustDomain::from_str("spiffe://example.org").unwrap();
        assert_eq!(trust_domain.to_string(), "example.org");
    }

    #[test]
    fn test_trust_domain_to_id_string() {
        let trust_domain = TrustDomain::from_str("example.org").unwrap();
        assert_eq!(trust_domain.id_string(), "spiffe://example.org");
    }

    #[test]
    fn test_trust_domain_try_from_str() {
        let trust_domain = TrustDomain::try_from("example.org").unwrap();
        assert_eq!(trust_domain.to_string(), "example.org");
    }

    #[test]
    fn test_trust_domain_try_from_string() {
        let trust_domain = TrustDomain::try_from(String::from("example.org")).unwrap();
        assert_eq!(trust_domain.to_string(), "example.org");
    }

    #[test]
    fn test_trust_domain_parse_with_all_chars() {
        // Go all the way through 255, which ensures we reject UTF-8 appropriately
        for i in 0..=255_u8 {
            let c = i as char;
            let td = format!("trustdomain{c}");

            if TD_CHARS.contains(&c) {
                let trust_domain = TrustDomain::new(&td).unwrap();
                assert_eq!(trust_domain.to_string(), td)
            } else {
                assert_eq!(
                    TrustDomain::new(&td).unwrap_err(),
                    SpiffeIdError::BadTrustDomainChar
                );
            }
        }
    }
}
