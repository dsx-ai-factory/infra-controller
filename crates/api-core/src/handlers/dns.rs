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
use ::rpc::protos;
use carbide_uuid::domain::DomainId;
use db::db_read::DbReader;
use db::dns::resource_record;
use dns_record::{DnsResourceRecordType, SoaRecord};
use model::dns::{Answer, Fqdn, ResourceRecord};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

/// The zone this site holds that a queried name falls under, and what
/// `lookup_answer` needs from it to answer.
///
/// `find_site_authority` produces one per question. `None` from that lookup
/// means no held zone contains the name and the answer is
/// `NotAuthoritative`. `Some` means the site is authoritative and the
/// question is answered from inside this zone, whether or not records exist
/// at the name. Every field is read on the way to a positive or negative
/// answer, which is why the SOA is loaded here rather than fetched again when
/// a negative turns out to need it.
struct HeldAuthority {
    /// The zone apex. Names the authority in every `Answer` variant given
    /// from this zone.
    zone: Fqdn,
    /// The zone's SOA. Returned as the record for an apex SOA question, and
    /// placed in the authority section of NODATA and NXDOMAIN answers so
    /// resolvers can cache the negative.
    soa: SoaRecord,
    /// The queried name is the zone apex itself, not a name under it. An apex
    /// always exists, so it can be NODATA but never NXDOMAIN, and an apex SOA
    /// question is answered from `soa` instead of the record store.
    is_apex: bool,
}

/// Find which of our zones contains `qname`.
///
/// The candidates are the qname's own label suffixes, so `gpu.mysite.example.com`
/// can only match `gpu.mysite.example.com`, `mysite.example.com`,
/// `example.com`, or `com`, never `notmysite.example.com`. One query returns
/// the longest live `domains` row among them. Returns `None` if none match.
async fn find_site_authority(
    db: impl DbReader<'_>,
    qname: &Fqdn,
) -> Result<Option<HeldAuthority>, Status> {
    let Some(domain) = db::dns::domain::find_longest_live_zone(db, &qname.suffixes())
        .await
        .map_err(CarbideError::from)?
    else {
        return Ok(None);
    };
    // The row matched one of the qname's own suffixes, so its name is a valid
    // Fqdn unless the stored spelling is broken.
    let zone = Fqdn::parse(&domain.name).map_err(|error| CarbideError::Internal {
        message: format!(
            "domain {} has an unparsable name {:?}: {error}",
            domain.id, domain.name
        ),
    })?;
    // A row created before SOAs were stored has none. Answer with the same
    // default `CreateDomain` would have given it, so negatives keep their SOA.
    // `SoaRecord::new` appends the name to `ns1.` as given, so pass the
    // normalised zone without its root dot rather than the stored spelling.
    let soa = domain.soa.map(|soa| soa.0).unwrap_or_else(|| {
        tracing::warn!(domain_id = %domain.id, %zone, "held zone has no stored SOA; using default");
        SoaRecord::new(zone.as_str().trim_end_matches('.'))
    });
    Ok(Some(HeldAuthority {
        is_apex: qname == &zone,
        zone,
        soa,
    }))
}

/// Returns all published record types at `qname`. The caller selects the
/// requested type.
async fn lookup_records_by_qname(
    txn: impl DbReader<'_>,
    qname: &Fqdn,
) -> Result<Vec<ResourceRecord>, tonic::Status> {
    tracing::debug!(%qname, "Looking up DNS records");

    let result = resource_record::find_record(txn, qname.as_str())
        .await
        .map_err(CarbideError::from)?
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();

    Ok(result)
}

/// Resolve a reverse-DNS (PTR) query. The qname is an address in `in-addr.arpa` /
/// `ip6.arpa` form, so we parse it back to an `IpAddr` and look the holding
/// interface up by address (rather than matching a per-row arpa string in a view).
/// A name that is not a complete reverse address, or one no interface holds,
/// yields no records.
async fn lookup_ptr_record(
    txn: impl DbReader<'_>,
    qname: &Fqdn,
) -> Result<Vec<ResourceRecord>, tonic::Status> {
    tracing::debug!(%qname, "looking up PTR record");

    let Some(address) = model::dns::arpa_qname_to_ip(qname.as_str()) else {
        return Ok(vec![]);
    };

    let result = resource_record::find_ptr_record(txn, address)
        .await
        .map_err(CarbideError::from)?
        .into_iter()
        .map(|record| ResourceRecord {
            q_type: DnsResourceRecordType::PTR.to_string(),
            q_name: qname.to_string(),
            ttl: u32::try_from(record.ttl).unwrap_or(0),
            content: record.ptr_content,
            domain_id: Some(record.domain_id.to_string()),
        })
        .collect::<Vec<_>>();

    Ok(result)
}

/// Pick an authority for a PTR record when no reverse zone contains it.
///
/// Reverse zones are only created for prefixes that end on an octet or nibble
/// boundary (/8, /16, /24, /32 for IPv4). An address on a /25 has a PTR record
/// but no `in-addr.arpa` row above it, so `find_site_authority` finds nothing.
/// Rather than drop the record, use the forward domain that owns the hostname
/// the PTR points at. Allocated prefixes cannot overlap across tenants
/// (site-wide exclusion constraints on `network_prefixes` and
/// `network_vpc_prefixes`), so every address has at most one owner and no
/// tie-break is needed.
///
/// The record was just read from a live domain, so a missing or unparsable
/// domain here is an inconsistency in our own data, not a missing name;
/// both are reported as internal errors (ServFail downstream), never as
/// NotFound (which the DNS server would turn into NXDOMAIN).
// TODO: remove once reverse authority is derived from live network prefixes
// instead of stored `domains` rows; every live address then has a held zone.
async fn ptr_forward_authority(
    db: impl DbReader<'_>,
    record: &ResourceRecord,
) -> Result<Fqdn, Status> {
    let domain_id = record
        .domain_id
        .as_deref()
        .and_then(|id| id.parse::<DomainId>().ok())
        .ok_or_else(|| CarbideError::Internal {
            message: "PTR record is missing its owning domain id".to_string(),
        })?;
    let domain = db::dns::domain::find_by_uuid(db, domain_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::Internal {
            message: format!("PTR record refers to missing domain {domain_id}"),
        })?;
    let zone = Fqdn::parse(&domain.name).map_err(|error| CarbideError::Internal {
        message: format!(
            "domain {domain_id} has an unparsable name {:?}: {error}",
            domain.name
        ),
    })?;
    Ok(zone)
}

/// Does anything exist below `qname`?
///
/// A name with records under it exists even if it has none of its own
/// (RFC 8020 §2). If we answer NXDOMAIN for `rack1.example.com` while
/// `gpu1.rack1.example.com` exists, a resolver may cache that and stop looking
/// up anything under `rack1`.
///
/// Reverse names are checked by address range, since PTR records are keyed by
/// address rather than stored under their arpa name.
async fn name_has_descendants(db: impl DbReader<'_>, qname: &Fqdn) -> Result<bool, Status> {
    let exists = match model::dns::arpa_qname_to_prefix(qname.as_str()) {
        Some(prefix) => resource_record::any_ptr_published_within(db, prefix).await,
        None => resource_record::any_record_below(db, qname.as_str()).await,
    };
    Ok(exists.map_err(CarbideError::from)?)
}

/// Answer one DNS question from the zones this site holds.
///
/// The result is one of:
///
/// - `Records`: the name has records of the requested type.
/// - `NoData`: the name exists but has no records of that type. Includes the
///   zone apex, and names that only have records below them.
/// - `NxDomain`: the name is inside one of our zones and nothing exists at or
///   below it.
/// - `NotAuthoritative`: the name is not inside any zone we hold. We never say
///   NXDOMAIN for those, because the name may well exist somewhere else.
///
/// `NoData` and `NxDomain` carry the zone SOA for the authority section.
async fn lookup_answer(
    db: impl DbReader<'_> + Copy,
    qname: &str,
    qtype: DnsResourceRecordType,
) -> Result<Answer, Status> {
    let qname =
        Fqdn::parse(qname).map_err(|error| CarbideError::InvalidArgument(error.to_string()))?;
    let held = find_site_authority(db, &qname).await?;

    // PTR data is keyed by address rather than by zone, and the address may
    // sit under a prefix that has no reverse zone row (see
    // `ptr_forward_authority`). Resolve it before the zone gate so those
    // records keep being served.
    if qtype == DnsResourceRecordType::PTR {
        let records = lookup_ptr_record(db, &qname).await?;
        if let Some(first) = records.first() {
            let zone = match held {
                Some(held) => held.zone,
                None => ptr_forward_authority(db, first).await?,
            };
            return Ok(Answer::Records { zone, records });
        }
    }

    let Some(held) = held else {
        return Ok(Answer::NotAuthoritative);
    };

    // The apex SOA is the only record synthesised from the zone row rather than
    // read from inventory. NS is not published, so it falls through to NODATA.
    if held.is_apex && qtype == DnsResourceRecordType::SOA {
        let record = ResourceRecord::soa(&held.zone, &held.soa);
        return Ok(Answer::Records {
            zone: held.zone,
            records: vec![record],
        });
    }

    // Everything published at the exact name, any type. Reverse names have
    // no rows in `dns_records`; the PTR lookup above is the only data at them,
    // and it already missed. A PTR question for a forward name still has to
    // check the forward records, or an existing name would look like
    // NXDOMAIN.
    let published = if model::dns::arpa_qname_to_prefix(qname.as_str()).is_some() {
        vec![]
    } else {
        lookup_records_by_qname(db, &qname).await?
    };
    let name_exists =
        held.is_apex || !published.is_empty() || name_has_descendants(db, &qname).await?;

    let wanted = qtype.to_string();
    let records: Vec<ResourceRecord> = published
        .into_iter()
        .filter(|record| record.q_type == wanted)
        .collect();
    if !records.is_empty() {
        return Ok(Answer::Records {
            zone: held.zone,
            records,
        });
    }

    Ok(if name_exists {
        Answer::NoData {
            zone: held.zone,
            soa: held.soa,
        }
    } else {
        Answer::NxDomain {
            zone: held.zone,
            soa: held.soa,
        }
    })
}

pub(crate) async fn get_all_domains(
    api: &Api,
    _request: Request<protos::dns::GetAllDomainsRequest>,
) -> Result<Response<protos::dns::GetAllDomainsResponse>, Status> {
    log_request_data(&_request);

    let domains = db::dns::domain::find_by(
        &api.database_connection,
        db::ObjectColumnFilter::<db::dns::domain::IdColumn>::All,
    )
    .await?;

    tracing::debug!(domain_count = domains.len(), "Found domains");
    for domain in &domains {
        tracing::debug!(
            domain_id = %domain.id,
            domain_name = %domain.name,
            "Domain"
        );
    }

    let result: Vec<protos::dns::DomainInfo> = domains
        .into_iter()
        .map(model::dns::DomainInfo::from)
        .map(protos::dns::DomainInfo::from)
        .collect();

    let response = protos::dns::GetAllDomainsResponse { result };

    tracing::debug!(
        domain_info_count = response.result.len(),
        "Formatted DomainInfo response"
    );
    Ok(Response::new(response))
}

pub(crate) async fn get_all_domain_metadata(
    api: &Api,
    request: Request<protos::dns::DomainMetadataRequest>,
) -> Result<Response<protos::dns::DomainMetadataResponse>, Status> {
    log_request_data(&request);

    let metadata_request = request.into_inner();

    let domain_name = db::dns::normalize_domain(&metadata_request.domain);

    // Reverse zones may be stored with or without the trailing root dot, so
    // resolve their normalized identity. Forward domains retain the existing
    // exact lookup after the request normalization above.
    let domains = db::dns::domain::find_by_name(&api.database_connection, &domain_name).await?;

    let domain = domains.first().ok_or_else(|| CarbideError::NotFoundError {
        kind: "domain",
        id: metadata_request.domain.clone(),
    })?;

    let proto_metadata = domain
        .metadata
        .as_ref()
        .map(|m| protos::dns::Metadata::from(m.clone()));

    Ok(Response::new(protos::dns::DomainMetadataResponse {
        result: proto_metadata,
    }))
}
pub(crate) async fn lookup_record(
    api: &Api,
    request: Request<protos::dns::DnsResourceRecordLookupRequest>,
) -> Result<Response<protos::dns::DnsResourceRecordLookupResponse>, Status> {
    log_request_data(&request);

    let lookup_request = request.into_inner();

    // Log the full incoming request for debugging
    tracing::debug!(
        qtype = %lookup_request.qtype,
        qname = %lookup_request.qname,
        zone_id = %lookup_request.zone_id,
        "Processing DNS lookup request"
    );

    let rrtype = DnsResourceRecordType::try_from(lookup_request.qtype)
        .map_err(|e| CarbideError::InvalidArgument(format!("invalid qtype supplied: {}", e)))?;

    let qname = lookup_request.qname;

    if qname.is_empty() {
        return Err(CarbideError::InvalidArgument("qname cannot be empty".to_string()).into());
    }

    let answer = lookup_answer(&api.database_connection, &qname, rrtype).await?;
    Ok(Response::new(answer.into()))
}
