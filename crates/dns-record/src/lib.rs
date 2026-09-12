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

use std::fmt::Display;

use chrono::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::debug;

pub mod constants;

/// Wrapper type for time intervals in seconds
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct Seconds(pub i32);

impl From<i32> for Seconds {
    fn from(value: i32) -> Self {
        Seconds(value)
    }
}

impl From<Seconds> for i32 {
    fn from(value: Seconds) -> Self {
        value.0
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DnsResourceRecordLookup {
    pub qtype: DnsResourceRecordType,
    pub qname: String,
    pub zone_id: String,
    pub remote: Option<String>,
    pub local: Option<String>,
    #[serde(rename = "real-remote")]
    pub real_remote: Option<String>,
}
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct DnsResourceRecordReply {
    pub qtype: String,
    pub qname: String,
    pub ttl: u32,
    pub content: String,
    pub domain_id: Option<String>,
    pub scope_mask: Option<String>,
    pub auth: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, Copy, Eq, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
pub enum DnsResourceRecordType {
    #[default]
    SOA,
    NS,
    A,
    AAAA,
    CNAME,
    MX,
    TXT,
    PTR,
}

impl Display for DnsResourceRecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let record_type = match self {
            DnsResourceRecordType::SOA => constants::DNS_TYPE_SOA,
            DnsResourceRecordType::NS => constants::DNS_TYPE_NS,
            DnsResourceRecordType::A => constants::DNS_TYPE_A,
            DnsResourceRecordType::AAAA => constants::DNS_TYPE_AAAA,
            DnsResourceRecordType::CNAME => constants::DNS_TYPE_CNAME,
            DnsResourceRecordType::MX => constants::DNS_TYPE_MX,
            DnsResourceRecordType::TXT => constants::DNS_TYPE_TXT,
            DnsResourceRecordType::PTR => constants::DNS_TYPE_PTR,
        };
        write!(f, "{record_type}")
    }
}

/// Represents a Start of Authority (SOA) record for a DNS zone.
///
/// The SOA record specifies authoritative information about a DNS zone,
/// including primary nameserver, email contact, and zone update details.
/// It is a critical component in DNS configuration, as it defines zone
/// refresh intervals and update policies.
///
/// # Fields
///
/// * `primary_ns` - The primary nameserver responsible for the zone.
/// * `contact` - The email contact for the zone administrator, typically in the format `hostmaster.example.com`.
/// * `serial` - The serial number for the zone, used to track updates. This should be incremented each time the zone file is modified.
/// * `refresh` - The time (in seconds) a secondary nameserver should wait before querying for zone updates.
/// * `retry` - The time (in seconds) a secondary nameserver should wait before retrying a failed zone update query.
/// * `expire` - The time (in seconds) after which a secondary nameserver should discard the zone if no updates are received.
/// * `minimum` - The minimum TTL (time-to-live) value applied to all resource records in the zone. This specifies how long DNS resolvers should cache data from this zone.
/// * `ttl` - The default TTL (time-to-live) value for the SOA record itself, which is the time period for which DNS clients can cache the SOA record.
///
/// # Example
///
/// ```rust
/// use dns_record::{Seconds, SoaRecord};
/// let soa = SoaRecord {
///     primary_ns: "ns1.example.com".to_string(),
///     contact: "hostmaster.example.com".to_string(),
///     serial: 2024110401,
///     refresh: Seconds(3600),
///     retry: Seconds(600),
///     expire: Seconds(604800),
///     minimum: Seconds(3600),
///     ttl: Seconds(3600),
/// };
/// ```
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SoaRecord {
    /// The primary nameserver responsible for the DNS zone.
    pub primary_ns: String,
    /// The contact email address of the zone administrator.
    /// Typically formatted as `hostmaster.example.com`.
    pub contact: String,
    /// The serial number for the zone. Increment this number
    /// with each change to the zone to notify secondaries.
    pub serial: u32,
    /// The time interval (in seconds) for a secondary server to refresh the zone.
    pub refresh: Seconds,
    /// The retry interval (in seconds) for a secondary server to retry
    /// if a zone refresh fails.
    pub retry: Seconds,
    /// The expiration time (in seconds) for the zone data on a secondary server.
    /// If no refresh occurs within this time, the zone is considered expired.
    pub expire: Seconds,
    /// How long (in seconds) resolvers may cache a negative answer for a name
    /// in this zone (RFC 2308 §4). Despite the name, it no longer sets a
    /// default TTL for positive records.
    pub minimum: Seconds,
    /// The default TTL (time-to-live) for the SOA record itself.
    pub ttl: Seconds,
}

impl SoaRecord {
    pub fn increment_serial(&mut self) {
        let now = Utc::now();

        // Convert serial to string and strip the last two characters
        let serial_str = self.serial.to_string();
        let stripped_date = &serial_str[..serial_str.len() - 2];

        // Parse the stripped date to check if it's outdated
        let serial_date = stripped_date
            .parse::<u32>()
            .unwrap_or(Self::generate_new_serial());

        let current_date_str = now.format("%Y%m%d").to_string();
        let current_date = current_date_str.parse::<u32>().unwrap_or(0);

        // Check if serial date is outdated
        if serial_date < current_date {
            // Generate a new serial for the new day in `YYYYMMDD01` format
            debug!("DNS serial number is for a different date, generating a new one");
            self.serial = Self::generate_new_serial();
        } else {
            // Increment the last two digits if the date hasn't changed
            let incremented_serial = self.serial + 1;
            debug!(serial = incremented_serial, "DNS serial number incremented");
            self.serial = incremented_serial;
        }
    }
    pub fn generate_new_serial() -> u32 {
        let now = Utc::now();
        let formatted_data = now.format("%Y%m%d").to_string() + "01";
        let serial = formatted_data
            .parse::<u32>()
            .expect("Unable to generate new serial for zone");
        debug!(serial, "generated serial for DNS zone");
        serial
    }

    /// The SOA a new zone starts with.
    ///
    /// `minimum` is the negative-caching TTL (RFC 2308 §4), so it is kept
    /// short: names in these zones appear when hardware boots and instances
    /// are allocated, and a resolver that cached NXDOMAIN for a name a minute
    /// before it was published should not keep denying it for an hour.
    pub fn new(domain_name: &str) -> SoaRecord {
        SoaRecord {
            primary_ns: format!("ns1.{domain_name}"),
            contact: format!("hostmaster.{domain_name}"),
            serial: Self::generate_new_serial(),
            refresh: Seconds(3600),
            retry: Seconds(3600),
            expire: Seconds(604800),
            minimum: Seconds(120),
            ttl: Seconds(3600),
        }
    }
}

impl Display for SoaRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}. {}. {} {} {} {} {}",
            self.primary_ns,
            self.contact,
            self.serial,
            self.refresh.0,
            self.retry.0,
            self.expire.0,
            self.minimum.0,
        )
    }
}

impl TryFrom<&str> for DnsResourceRecordType {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            constants::DNS_TYPE_SOA => Ok(DnsResourceRecordType::SOA),
            constants::DNS_TYPE_NS => Ok(DnsResourceRecordType::NS),
            constants::DNS_TYPE_A => Ok(DnsResourceRecordType::A),
            constants::DNS_TYPE_AAAA => Ok(DnsResourceRecordType::AAAA),
            constants::DNS_TYPE_CNAME => Ok(DnsResourceRecordType::CNAME),
            constants::DNS_TYPE_MX => Ok(DnsResourceRecordType::MX),
            constants::DNS_TYPE_TXT => Ok(DnsResourceRecordType::TXT),
            constants::DNS_TYPE_PTR => Ok(DnsResourceRecordType::PTR),
            _ => Err(format!("RecordType {value} not implement")),
        }
    }
}

impl TryFrom<String> for DnsResourceRecordType {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            constants::DNS_TYPE_SOA => Ok(DnsResourceRecordType::SOA),
            constants::DNS_TYPE_NS => Ok(DnsResourceRecordType::NS),
            constants::DNS_TYPE_A => Ok(DnsResourceRecordType::A),
            constants::DNS_TYPE_AAAA => Ok(DnsResourceRecordType::AAAA),
            constants::DNS_TYPE_CNAME => Ok(DnsResourceRecordType::CNAME),
            constants::DNS_TYPE_MX => Ok(DnsResourceRecordType::MX),
            constants::DNS_TYPE_TXT => Ok(DnsResourceRecordType::TXT),
            constants::DNS_TYPE_PTR => Ok(DnsResourceRecordType::PTR),
            _ => Err(format!("RecordType {value} not implement")),
        }
    }
}

impl From<DnsResourceRecordType> for String {
    fn from(value: DnsResourceRecordType) -> Self {
        match value {
            DnsResourceRecordType::SOA => constants::DNS_TYPE_SOA.to_string(),
            DnsResourceRecordType::NS => constants::DNS_TYPE_NS.to_string(),
            DnsResourceRecordType::A => constants::DNS_TYPE_A.to_string(),
            DnsResourceRecordType::AAAA => constants::DNS_TYPE_AAAA.to_string(),
            DnsResourceRecordType::CNAME => constants::DNS_TYPE_CNAME.to_string(),
            DnsResourceRecordType::MX => constants::DNS_TYPE_MX.to_string(),
            DnsResourceRecordType::TXT => constants::DNS_TYPE_TXT.to_string(),
            DnsResourceRecordType::PTR => constants::DNS_TYPE_PTR.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use assert_json_diff::assert_json_eq;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use serde_json::json;

    use super::*;

    #[derive(Clone, Copy)]
    enum RecordTypeInput {
        Borrowed(&'static str),
        Owned(&'static str),
    }

    #[derive(Debug, PartialEq, Eq)]
    struct RecordTypeSummary {
        record_type: DnsResourceRecordType,
        display: String,
        string: String,
    }

    fn soa_record_with_serial(serial: u32) -> SoaRecord {
        SoaRecord {
            primary_ns: "ns1.example.com".to_string(),
            contact: "hostmaster.example.com".to_string(),
            serial,
            refresh: Seconds(3600),
            retry: Seconds(600),
            expire: Seconds(604800),
            minimum: Seconds(3600),
            ttl: Seconds(3600),
        }
    }

    fn assert_current_serial(serial: u32, before: u32, after: u32) {
        assert!(
            serial == before || serial == after,
            "serial {serial} was not generated within the current-date window {before}..={after}"
        );
    }

    fn parse_record_type(input: RecordTypeInput) -> Result<RecordTypeSummary, String> {
        let record_type = match input {
            RecordTypeInput::Borrowed(value) => DnsResourceRecordType::try_from(value)?,
            RecordTypeInput::Owned(value) => DnsResourceRecordType::try_from(value.to_string())?,
        };

        Ok(RecordTypeSummary {
            record_type,
            display: record_type.to_string(),
            string: String::from(record_type),
        })
    }

    #[test]
    fn test_dns_resource_record_lookup_as_json() {
        let domain_uuid = uuid::Uuid::new_v4();

        let request = DnsResourceRecordLookup {
            qtype: DnsResourceRecordType::A,
            qname: "foo.example.com".to_string(),
            zone_id: uuid::Uuid::to_string(&domain_uuid),
            remote: None,
            local: None,
            real_remote: None,
        };

        let serialized = serde_json::to_value(&request).unwrap();
        let expected_json = json!({
            "qtype": "A",
            "qname": "foo.example.com",
            "zone_id": domain_uuid.to_string(),
            "remote": null,
            "local": null,
            "real-remote": null,
        });
        assert_json_eq!(serialized, expected_json);
    }

    #[test]
    fn test_dns_resource_record_reply_as_json() {
        let domain_uuid = uuid::Uuid::new_v4();

        let reply = DnsResourceRecordReply {
            qtype: DnsResourceRecordType::A.to_string(),
            qname: "example.com".to_string(),
            ttl: 3600,
            content: "192.168.1.1".to_string(),
            domain_id: Some(domain_uuid.to_string()),
            scope_mask: None,
            auth: None,
        };

        let serialized_record = serde_json::to_value(&reply).unwrap();

        let expected_json = json!({
            "qtype": "A",
            "qname": "example.com",
            "ttl": 3600,
            "content": "192.168.1.1",
            "domain_id": domain_uuid.to_string(),
            "scope_mask": null,
            "auth": null,
        });

        assert_json_eq!(serialized_record, expected_json);
    }

    #[test]
    fn test_soa_record_dns_lookup_record_reply_as_json() {
        let soa = SoaRecord {
            primary_ns: "ns1.example.com".to_string(),
            contact: "hostmaster.example.com".to_string(),
            serial: 2024110401,
            refresh: Seconds(3600),
            retry: Seconds(600),
            expire: Seconds(604800),
            minimum: Seconds(3600),
            ttl: Seconds(3600),
        };
        let reply = DnsResourceRecordReply {
            qtype: DnsResourceRecordType::SOA.to_string(),
            qname: "example.com".to_string(),
            ttl: 3600,
            content: soa.to_string(),
            domain_id: None,
            scope_mask: None,
            auth: None,
        };

        let serialized = serde_json::to_value(&reply).unwrap();
        let expected_json = json!({
            "qtype": "SOA",
            "qname": "example.com",
            "ttl": 3600,
            "content": "ns1.example.com. hostmaster.example.com. 2024110401 3600 600 604800 3600",
            "domain_id": null,
            "scope_mask": null,
            "auth": null,
        });
        assert_json_eq!(serialized, expected_json);
    }

    #[test]
    fn test_soa_record_as_string() {
        let soa = SoaRecord {
            primary_ns: "ns1.example.com".to_string(),
            contact: "hostmaster.example.com".to_string(),
            serial: 2024110401,
            refresh: Seconds(3600),
            retry: Seconds(600),
            expire: Seconds(604800),
            minimum: Seconds(3600),
            ttl: Seconds(3600),
        };

        let soa_str = soa.to_string();
        assert_eq!(
            soa_str,
            "ns1.example.com. hostmaster.example.com. 2024110401 3600 600 604800 3600"
        );
    }

    #[test]
    fn test_generate_domain_serial_format() {
        let before = SoaRecord::generate_new_serial();
        let serial = SoaRecord::generate_new_serial();
        let after = SoaRecord::generate_new_serial();

        assert_current_serial(serial, before, after);
    }

    #[test]
    fn test_seconds_conversions() {
        value_scenarios!(
            run = |value| {
                let seconds = Seconds::from(value);
                (seconds, i32::from(seconds))
            };
            "seconds conversions" {
                0 => (Seconds(0), 0),
                3600 => (Seconds(3600), 3600),
                -1 => (Seconds(-1), -1),
            }
        );
    }

    #[test]
    fn test_dns_resource_record_type_conversions() {
        scenarios!(parse_record_type:
            "borrowed record types" {
                RecordTypeInput::Borrowed("SOA") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::SOA,
                    display: "SOA".to_string(),
                    string: "SOA".to_string(),
                }),
                RecordTypeInput::Borrowed("A") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::A,
                    display: "A".to_string(),
                    string: "A".to_string(),
                }),
                RecordTypeInput::Borrowed("CNAME") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::CNAME,
                    display: "CNAME".to_string(),
                    string: "CNAME".to_string(),
                }),
                RecordTypeInput::Borrowed("TXT") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::TXT,
                    display: "TXT".to_string(),
                    string: "TXT".to_string(),
                }),
            }

            "owned record types" {
                RecordTypeInput::Owned("NS") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::NS,
                    display: "NS".to_string(),
                    string: "NS".to_string(),
                }),
                RecordTypeInput::Owned("AAAA") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::AAAA,
                    display: "AAAA".to_string(),
                    string: "AAAA".to_string(),
                }),
                RecordTypeInput::Owned("MX") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::MX,
                    display: "MX".to_string(),
                    string: "MX".to_string(),
                }),
                RecordTypeInput::Owned("PTR") => Yields(RecordTypeSummary {
                    record_type: DnsResourceRecordType::PTR,
                    display: "PTR".to_string(),
                    string: "PTR".to_string(),
                }),
            }

            "unknown record types" {
                RecordTypeInput::Borrowed("FAKE") => {
                    FailsWith("RecordType FAKE not implement".to_string())
                },
                RecordTypeInput::Owned("FAKE") => {
                    FailsWith("RecordType FAKE not implement".to_string())
                },
            }
        );
    }

    #[test]
    fn test_dns_resource_record_type_default() {
        assert_eq!(DnsResourceRecordType::default(), DnsResourceRecordType::SOA);
    }

    #[test]
    fn test_soa_record_new() {
        let before = SoaRecord::generate_new_serial();
        let soa = SoaRecord::new("example.com");
        let after = SoaRecord::generate_new_serial();

        assert_eq!(soa.primary_ns, "ns1.example.com");
        assert_eq!(soa.contact, "hostmaster.example.com");
        assert_current_serial(soa.serial, before, after);
        assert_eq!(soa.refresh, Seconds(3600));
        assert_eq!(soa.retry, Seconds(3600));
        assert_eq!(soa.expire, Seconds(604800));
        assert_eq!(soa.minimum, Seconds(120));
        assert_eq!(soa.ttl, Seconds(3600));
    }

    #[test]
    fn test_soa_record_increment_serial_cases() {
        value_scenarios!(
            run = |serial| {
                let mut soa = soa_record_with_serial(serial);
                soa.increment_serial();
                soa.serial
            };
            "future serials increment last two digits" {
                2099010101 => 2099010102,
            }
        );
    }

    #[test]
    fn test_soa_record_increment_old_serial_resets_to_current_date() {
        let before = SoaRecord::generate_new_serial();
        let mut soa = soa_record_with_serial(2000010101);
        soa.increment_serial();
        let after = SoaRecord::generate_new_serial();

        assert_current_serial(soa.serial, before, after);
    }
}
