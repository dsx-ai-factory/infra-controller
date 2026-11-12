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

/// Authority and negative-cache data for a forward-zone question.
///
/// Loaded together so positive and negative answers use the same zone snapshot.
/// Reverse questions bypass this lookup: a published PTR does not establish
/// authority over its enclosing reverse zone.
struct HeldAuthority {
    /// The enclosing zone's apex.
    zone: Fqdn,
    /// The forward zone's SOA: returned in the answer section when a query asks
    /// for SOA at the zone's exact name (apex). Included in the authority section
    /// when an existing name lacks the requested record type (NODATA) or the
    /// name does not exist (NXDOMAIN), providing the lifetime for caching that
    /// negative answer.
    soa: SoaRecord,
    /// Whether the queried name equals the zone apex. An apex can yield NODATA,
    /// but never NXDOMAIN, even when it has no records of the requested type.
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

/// Resolve a reverse name by address against machine and instance inventory,
/// independently of stored reverse zones. Incomplete reverse names, addresses
/// without a publishable name, and ambiguous ownership yield no records.
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

/// Identifies a published PTR by its owning forward domain without granting
/// reverse-zone authority or supplying an SOA for negative answers.
///
/// PTR selection requires a live forward domain. Missing or invalid domain
/// metadata here is an internal inconsistency, not evidence that the queried
/// name does not exist. Report it as an internal error so the DNS server returns
/// SERVFAIL rather than caching an NXDOMAIN from a NotFound response.
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
    Fqdn::parse(&domain.name).map_err(|error| {
        CarbideError::Internal {
            message: format!(
                "domain {domain_id} has an unparsable name {:?}: {error}",
                domain.name
            ),
        }
        .into()
    })
}

/// Does any forward record exist below `qname`?
///
/// A name with records under it exists even if it has none of its own
/// (RFC 8020 §2). If we answer NXDOMAIN for `rack1.example.com` while
/// `gpu1.rack1.example.com` exists, a resolver may cache that and stop looking
/// up anything under `rack1`.
async fn name_has_descendants(db: impl DbReader<'_>, qname: &Fqdn) -> Result<bool, Status> {
    Ok(resource_record::any_record_below(db, qname.as_str())
        .await
        .map_err(CarbideError::from)?)
}

/// Answer a forward-zone question or an inventory-derived reverse PTR query.
///
/// The result is one of:
///
/// - `Records`: the name has records of the requested type.
/// - `NoData`: the name exists but has no records of that type. Includes the
///   zone apex, and names that only have records below them.
/// - `NxDomain`: the name is inside one of our zones and nothing exists at or
///   below it.
/// - `NotAuthoritative`: a forward name is outside held zones, or a reverse
///   question has no supported answer. Neither proves the name does not exist,
///   so these queries must not produce NXDOMAIN.
///
/// `NoData` and `NxDomain` carry the zone SOA for the authority section.
/// Reverse DNS serves only published, unambiguous PTRs, identified by their
/// owning forward domain. All other reverse questions are `NotAuthoritative`.
async fn lookup_answer(
    db: impl DbReader<'_> + Copy,
    qname: &str,
    qtype: DnsResourceRecordType,
) -> Result<Answer, Status> {
    let qname =
        Fqdn::parse(qname).map_err(|error| CarbideError::InvalidArgument(error.to_string()))?;

    // Address ownership supplies a PTR, not authority over the enclosing zone.
    // Include both roots and non-address labels so rollback-compatible zone
    // maintenance cannot enable reverse SOAs or authoritative negatives.
    let is_reverse = qname
        .suffixes()
        .iter()
        .any(|name| matches!(name.as_str(), "in-addr.arpa" | "ip6.arpa"));
    if is_reverse {
        if qtype == DnsResourceRecordType::PTR {
            let records = lookup_ptr_record(db, &qname).await?;
            if let Some(first) = records.first() {
                let zone = ptr_forward_authority(db, first).await?;
                return Ok(Answer::Records { zone, records });
            }
        }
        return Ok(Answer::NotAuthoritative);
    }

    let Some(held) = find_site_authority(db, &qname).await? else {
        return Ok(Answer::NotAuthoritative);
    };

    // The apex SOA is the only record synthesised from the held zone rather
    // than read from inventory. NS is not published, so it falls through to
    // NODATA.
    if held.is_apex && qtype == DnsResourceRecordType::SOA {
        let record = ResourceRecord::soa(&held.zone, &held.soa);
        return Ok(Answer::Records {
            zone: held.zone,
            records: vec![record],
        });
    }

    // Check all forward record types even for a PTR question: an existing A
    // record must yield NODATA, not an NXDOMAIN that would hide the A record.
    let published = lookup_records_by_qname(db, &qname).await?;
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
