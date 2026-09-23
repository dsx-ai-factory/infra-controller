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

use chrono::{DateTime, Utc};
use dns_record::SoaRecord;
use model::dns::domain_info::DomainInfo;
use model::dns::{Answer, Domain, DomainMetadata, NewDomain, ResourceRecord, SoaSnapshot};

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<DomainInfo> for rpc::protos::dns::DomainInfo {
    fn from(domain: DomainInfo) -> Self {
        rpc::protos::dns::DomainInfo {
            id: Some(domain.id),
            zone: domain.zone,
            kind: domain.kind,
            serial: domain.serial as i32,
            last_checked: domain.last_check.map(|v| v as i32),
            notified_serial: domain.notified_serial.map(|v| v as i32),
        }
    }
}

impl TryFrom<rpc::protos::dns::Domain> for NewDomain {
    type Error = RpcDataConversionError;

    fn try_from(proto: rpc::protos::dns::Domain) -> Result<Self, Self::Error> {
        let soa = proto
            .soa
            .map(|soa| {
                let record: SoaRecord = serde_json::from_str(&soa)
                    .map_err(|e| RpcDataConversionError::InvalidSoaRecord(e.to_string()))?;
                Ok::<SoaSnapshot, RpcDataConversionError>(SoaSnapshot(record))
            })
            .transpose()?;

        Ok(NewDomain {
            name: proto.name,
            soa,
        })
    }
}

impl From<Domain> for rpc::protos::dns::Domain {
    fn from(domain: Domain) -> Self {
        rpc::protos::dns::Domain {
            id: Some(domain.id),
            name: domain.name,
            created: Some(domain.created.into()),
            updated: Some(domain.updated.into()),
            deleted: domain.deleted.map(|d| d.into()),
            metadata: domain.metadata.map(|m| m.into()),
            soa: domain.soa.map(|s| s.0.to_string()),
        }
    }
}

impl TryFrom<rpc::protos::dns::Domain> for Domain {
    type Error = RpcDataConversionError;

    fn try_from(domain: rpc::protos::dns::Domain) -> Result<Self, Self::Error> {
        let domain_id = match domain.id {
            Some(id) => id,
            None => uuid::Uuid::new_v4().into(),
        };

        let created = match domain.created {
            Some(created) => {
                let system_time = std::time::SystemTime::try_from(created)
                    .map_err(|_| RpcDataConversionError::InvalidTimestamp(created.to_string()))?;
                DateTime::<Utc>::from(system_time)
            }
            None => Utc::now(),
        };

        let updated = match domain.updated {
            Some(updated) => {
                let system_time = std::time::SystemTime::try_from(updated)
                    .map_err(|_| RpcDataConversionError::InvalidTimestamp(updated.to_string()))?;
                DateTime::<Utc>::from(system_time)
            }
            None => Utc::now(),
        };

        let deleted: Option<DateTime<Utc>> = match domain.deleted {
            Some(deleted) => {
                let system_time = std::time::SystemTime::try_from(deleted)
                    .map_err(|_| RpcDataConversionError::InvalidTimestamp(deleted.to_string()))?;
                Some(DateTime::<Utc>::from(system_time))
            }
            None => None,
        };

        let soa: Option<SoaSnapshot> = domain
            .soa
            .map(|soa| {
                let record: SoaRecord = serde_json::from_str(&soa)
                    .map_err(|e| RpcDataConversionError::InvalidSoaRecord(e.to_string()))?;
                Ok::<SoaSnapshot, RpcDataConversionError>(SoaSnapshot(record))
            })
            .transpose()?;

        let metadata = domain.metadata.map(DomainMetadata::from);

        Ok(Domain {
            id: domain_id,
            name: domain.name,
            created,
            updated,
            deleted,
            soa,
            metadata,
        })
    }
}

impl From<rpc::protos::dns::Metadata> for DomainMetadata {
    fn from(metadata: rpc::protos::dns::Metadata) -> Self {
        DomainMetadata {
            allow_axfr_from: metadata.allow_axfr_from,
        }
    }
}

impl From<DomainMetadata> for rpc::protos::dns::Metadata {
    fn from(metadata: DomainMetadata) -> Self {
        rpc::protos::dns::Metadata {
            allow_axfr_from: vec![metadata.allow_axfr_from.join(",")],
        }
    }
}

/// Maps a classified answer onto the lookup response. `outcome` and
/// `authoritative` are both derived from the variant, so they cannot disagree.
/// `Refused` and `NotImplemented` exist on the wire for `carbide-dns` but
/// nico-api never produces them.
impl From<Answer> for rpc::protos::dns::DnsResourceRecordLookupResponse {
    fn from(answer: Answer) -> Self {
        use rpc::protos::dns::DnsLookupOutcome;

        let authoritative = answer.is_authoritative();
        let (outcome, records, authority_soa) = match answer {
            Answer::Records { records, .. } => (DnsLookupOutcome::Records, records, None),
            Answer::NoData { zone, soa } => (
                DnsLookupOutcome::NoData,
                vec![],
                Some(ResourceRecord::soa(&zone, &soa)),
            ),
            Answer::NxDomain { zone, soa } => (
                DnsLookupOutcome::NoSuchName,
                vec![],
                Some(ResourceRecord::soa(&zone, &soa)),
            ),
            Answer::NotAuthoritative => (DnsLookupOutcome::NotAuthoritative, vec![], None),
        };
        Self {
            records: records.into_iter().map(to_proto_record).collect(),
            outcome: outcome as i32,
            authority_soa: authority_soa.map(to_proto_record),
            authoritative,
        }
    }
}

fn to_proto_record(record: ResourceRecord) -> rpc::protos::dns::DnsResourceRecord {
    dns_record::DnsResourceRecordReply::from(record).into()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use dns_record::{DnsResourceRecordType, SoaRecord};
    use model::dns::{Answer, Fqdn, ResourceRecord};

    use super::*;

    fn zone() -> Fqdn {
        Fqdn::parse("mysite.example.com").expect("fixture zone")
    }

    fn soa() -> SoaRecord {
        SoaRecord::new("mysite.example.com")
    }

    fn a_record() -> ResourceRecord {
        ResourceRecord {
            q_type: DnsResourceRecordType::A.to_string(),
            q_name: "gpu.mysite.example.com.".to_string(),
            ttl: 300,
            content: "192.0.2.10".to_string(),
            domain_id: None,
        }
    }

    /// One row per matrix line: proto tag, AA, whether an authority SOA is
    /// present, and the answer count. The tags are the wire contract.
    #[test]
    fn answer_maps_onto_the_lookup_response() {
        value_scenarios!(
            run = |answer: Answer| {
                let proto = rpc::protos::dns::DnsResourceRecordLookupResponse::from(answer);
                (
                    proto.outcome,
                    proto.authoritative,
                    proto.authority_soa.is_some(),
                    proto.records.len(),
                )
            };
            "in-zone RRset" {
                Answer::Records { zone: zone(), records: vec![a_record()] } => (1, true, false, 1),
            }
            "empty Records is Records, not NXDOMAIN" {
                Answer::Records { zone: zone(), records: vec![] } => (1, true, false, 0),
            }
            "NODATA carries authority SOA" {
                Answer::NoData { zone: zone(), soa: soa() } => (2, true, true, 0),
            }
            "NXDOMAIN carries authority SOA" {
                Answer::NxDomain { zone: zone(), soa: soa() } => (3, true, true, 0),
            }
            "not authoritative" {
                Answer::NotAuthoritative => (4, false, false, 0),
            }
        );
    }
}
