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

//! Filter rules: which filters each unit type takes, which gauge and family
//! answer a count, and which unit predicate answers a units listing. Counts
//! and summaries read the same gauge as the published metrics, so a count
//! always equals the metric sum.

use carbide_site_health_probe_api::{
    ErrorCode, Family, Filter, HostFields, RecordType, RestResource, Source, Stage, Unit, UnitType,
};
use regex::Regex;
use serde::Serialize;

use crate::api::ApiError;
use crate::state_metrics::{instance_type_label, model_label};

/// What the probe can answer at all, independent of any snapshot.
#[derive(Debug, Clone)]
pub(crate) struct Capabilities {
    /// Hosts are read with the full machine record: no allocatability or
    /// update eligibility.
    pub full_host_read: bool,
}

/// A capability a filter needs from the data it is answered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Need {
    /// A REST total exists only on a site with NICo Flow for it.
    RestTotal(RestResource),
}

/// The gauge and label values a count or summary reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeriesSpec {
    /// The gauge name without its prefix.
    pub metric: &'static str,
    pub family: Family,
    /// Label equality matchers. A label may repeat: conflicting values match
    /// nothing.
    pub matchers: Vec<(&'static str, String)>,
    pub needs: Vec<Need>,
}

/// The wire name of an API enum, which is also its label value.
pub(crate) fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn unsupported(field: &str, message: String) -> ApiError {
    ApiError::unprocessable(ErrorCode::UnsupportedFilter, Some(field), message)
}

fn unavailable(field: &str, message: &str) -> ApiError {
    ApiError::unprocessable(
        ErrorCode::FieldUnavailable,
        Some(field),
        message.to_string(),
    )
}

/// The camelCase names of the filter fields that are set, `type` aside.
fn set_fields(filter: &Filter) -> Vec<String> {
    serde_json::to_value(filter)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()))
        .unwrap_or_default()
        .into_iter()
        .filter(|k| k != "type")
        .collect()
}

/// Rejects every set field outside `allowed` (`source` is always allowed).
fn only(filter: &Filter, allowed: &[&str]) -> Result<(), ApiError> {
    for field in set_fields(filter) {
        if field != "source" && !allowed.contains(&field.as_str()) {
            return Err(unsupported(
                &field,
                format!("{field} is not a filter of type={}", wire(filter.unit_type)),
            ));
        }
    }
    Ok(())
}

fn record_type_of(unit_type: UnitType) -> Option<RecordType> {
    match unit_type {
        UnitType::Host => Some(RecordType::Host),
        UnitType::Switch => Some(RecordType::Switch),
        UnitType::PowerShelf => Some(RecordType::PowerShelf),
        UnitType::Rack => Some(RecordType::Rack),
        _ => None,
    }
}

fn family_of(unit_type: UnitType) -> Family {
    match unit_type {
        UnitType::Host => Family::Host,
        UnitType::Dpu => Family::Dpu,
        UnitType::Switch => Family::Switch,
        UnitType::PowerShelf => Family::PowerShelf,
        UnitType::Rack => Family::Rack,
        UnitType::Instance => Family::Instance,
        UnitType::ExpectedRecord | UnitType::IdentityMismatch => Family::Expected,
        UnitType::HostFirmwareRequest => Family::HostFirmwareRequests,
        UnitType::RestTotal => Family::RestTotals,
    }
}

/// Host filters that only the hosts gauge carries.
const HOST_FIELDS: [&str; 4] = ["model", "instanceType", "allocatable", "updateComplete"];

fn host_read_checks(filter: &Filter, caps: &Capabilities) -> Result<(), ApiError> {
    for (set, field) in [
        (filter.bmc_version.is_some(), "bmcVersion"),
        (filter.uefi_version.is_some(), "uefiVersion"),
    ] {
        if set {
            return Err(unavailable(
                field,
                "exact host versions are not counted until the firmware tests (#8154)",
            ));
        }
    }
    if caps.full_host_read {
        if filter.allocatable.is_some() {
            return Err(unavailable(
                "allocatable",
                "allocatability is unknown until nico-api serves the compact host read",
            ));
        }
        if filter.update_eligible.is_some() {
            return Err(unavailable(
                "updateEligible",
                "update eligibility is unknown until nico-api serves the compact host read",
            ));
        }
    }
    Ok(())
}

/// `stage=expected` on a device type counts that type's expected records.
fn expected_stage_spec(filter: &Filter) -> Result<SeriesSpec, ApiError> {
    only(filter, &["stage"])?;
    let record_type = record_type_of(filter.unit_type).ok_or_else(|| {
        unsupported(
            "stage",
            format!("type={} has no expected records", wire(filter.unit_type)),
        )
    })?;
    Ok(SeriesSpec {
        metric: "expected_records",
        family: Family::Expected,
        matchers: vec![("type", wire(record_type))],
        needs: vec![],
    })
}

fn units_spec(filter: &Filter) -> SeriesSpec {
    let mut matchers = vec![
        ("type", wire(filter.unit_type)),
        ("stage", wire(filter.stage.unwrap_or(Stage::Registered))),
    ];
    if let Some(state) = &filter.state {
        matchers.push(("state", state.clone()));
    }
    SeriesSpec {
        metric: "units",
        family: family_of(filter.unit_type),
        matchers,
        needs: vec![],
    }
}

fn host_spec(filter: &Filter, caps: &Capabilities) -> Result<SeriesSpec, ApiError> {
    host_read_checks(filter, caps)?;
    if filter.stage == Some(Stage::Expected) {
        return expected_stage_spec(filter);
    }
    if filter.update_eligible.is_some() {
        only(filter, &["updateEligible", "model"])?;
        let mut matchers = vec![(
            "eligible",
            filter
                .update_eligible
                .map(|e| e.to_string())
                .unwrap_or_default(),
        )];
        if let Some(model) = &filter.model {
            matchers.push(("model", model.clone()));
        }
        return Ok(SeriesSpec {
            metric: "hosts_update_eligible",
            family: Family::Host,
            matchers,
            needs: vec![],
        });
    }
    let mut allowed = vec!["stage", "state"];
    allowed.extend(HOST_FIELDS);
    only(filter, &allowed)?;
    if set_fields(filter)
        .iter()
        .all(|f| ["stage", "state", "source"].contains(&f.as_str()))
    {
        return Ok(units_spec(filter));
    }
    let mut matchers = Vec::new();
    match filter.stage {
        Some(Stage::Ready) => matchers.push(("state", "Ready".to_string())),
        Some(Stage::Error) => matchers.push(("state", "Failed".to_string())),
        _ => {}
    }
    for (label, value) in [
        ("state", filter.state.clone()),
        ("model", filter.model.clone()),
        ("instance_type", filter.instance_type.clone()),
        ("allocatable", filter.allocatable.map(|v| v.to_string())),
        (
            "update_complete",
            filter.update_complete.map(|v| v.to_string()),
        ),
    ] {
        if let Some(value) = value {
            matchers.push((label, value));
        }
    }
    Ok(SeriesSpec {
        metric: "hosts",
        family: Family::Host,
        matchers,
        needs: vec![],
    })
}

/// The gauge a count or summary of `filter` reads. Anything the table does
/// not name is 422.
pub(crate) fn series_spec(filter: &Filter, caps: &Capabilities) -> Result<SeriesSpec, ApiError> {
    if filter.done.is_some() {
        return Err(unsupported(
            "done",
            "done is a /v1/units filter".to_string(),
        ));
    }
    rest_source(filter)?;
    match filter.unit_type {
        UnitType::Host => host_spec(filter, caps),
        UnitType::Dpu
        | UnitType::Switch
        | UnitType::PowerShelf
        | UnitType::Rack
        | UnitType::Instance => {
            if filter.stage == Some(Stage::Expected) {
                return expected_stage_spec(filter);
            }
            only(filter, &["stage", "state"])?;
            Ok(units_spec(filter))
        }
        UnitType::ExpectedRecord => {
            only(filter, &["recordType", "linked"])?;
            let record_type = required_record_type(filter)?;
            let mut matchers = vec![("type", wire(record_type))];
            if let Some(linked) = filter.linked {
                matchers.push(("linked", linked.to_string()));
            }
            Ok(SeriesSpec {
                metric: "expected_records",
                family: Family::Expected,
                matchers,
                needs: vec![],
            })
        }
        UnitType::IdentityMismatch => {
            only(filter, &["recordType", "kind"])?;
            let mut matchers = Vec::new();
            if let Some(record_type) = mismatch_record_type(filter)? {
                matchers.push(("type", wire(record_type)));
            }
            if let Some(kind) = filter.kind {
                matchers.push(("kind", wire(kind)));
            }
            Ok(SeriesSpec {
                metric: "identity_mismatches",
                family: Family::Expected,
                matchers,
                needs: vec![],
            })
        }
        UnitType::HostFirmwareRequest => {
            only(filter, &["model", "initiator"])?;
            let mut matchers = Vec::new();
            if let Some(model) = &filter.model {
                matchers.push(("model", model.clone()));
            }
            if let Some(initiator) = filter.initiator {
                matchers.push(("initiator", wire(initiator)));
            }
            Ok(SeriesSpec {
                metric: "host_firmware_requests",
                family: Family::HostFirmwareRequests,
                matchers,
                needs: vec![],
            })
        }
        UnitType::RestTotal => {
            only(filter, &["resource"])?;
            let resource = filter.resource.ok_or_else(|| {
                unsupported("resource", "type=rest_total needs resource".to_string())
            })?;
            Ok(SeriesSpec {
                metric: "rest_totals",
                family: Family::RestTotals,
                matchers: vec![("resource", wire(resource))],
                needs: vec![Need::RestTotal(resource)],
            })
        }
    }
}

/// The tenant's REST instances, per status and one by one, wait for the
/// instance tests (#8152). Site totals are `type=rest_total`.
fn rest_source(filter: &Filter) -> Result<(), ApiError> {
    match filter.source {
        Some(Source::Rest) if filter.unit_type == UnitType::Instance => Err(unavailable(
            "source",
            "the tenant's REST instances are not read until the instance tests (#8152)",
        )),
        Some(Source::Rest) => Err(unsupported(
            "source",
            format!("type={} has no REST source", wire(filter.unit_type)),
        )),
        _ => Ok(()),
    }
}

fn required_record_type(filter: &Filter) -> Result<RecordType, ApiError> {
    filter.record_type.ok_or_else(|| {
        unsupported(
            "recordType",
            "type=expected_record needs recordType".to_string(),
        )
    })
}

fn mismatch_record_type(filter: &Filter) -> Result<Option<RecordType>, ApiError> {
    match filter.record_type {
        Some(RecordType::Rack | RecordType::RackGroup) => Err(unsupported(
            "recordType",
            "identity mismatches exist for host, switch and power_shelf records".to_string(),
        )),
        other => Ok(other),
    }
}

/// The family and predicate of a units listing.
#[derive(Debug)]
pub(crate) struct UnitsSpec {
    pub family: Family,
    filter: Filter,
    name: Option<Regex>,
}

impl UnitsSpec {
    /// Whether `unit` passes every filter.
    pub(crate) fn matches(&self, unit: &Unit) -> bool {
        let f = &self.filter;
        let host = unit.host.as_ref();
        let instance = unit.instance.as_ref();
        let expected = unit.expected.as_ref();
        let eq = |want: &Option<String>, have: Option<&String>| {
            want.as_ref().is_none_or(|w| have == Some(w))
        };
        // A host without a model or instance type is the metric's `unknown`
        // and `none`, so those values filter the listing as they do a count.
        let label = |want: &Option<String>, normalize: fn(&HostFields) -> String| {
            want.as_ref()
                .is_none_or(|w| host.is_some_and(|h| normalize(h) == *w))
        };
        unit.unit_type == f.unit_type
            && f.source.is_none_or(|s| s == unit.source)
            && match f.stage {
                None | Some(Stage::Registered) => true,
                Some(stage) => unit.stage == stage,
            }
            && eq(&f.state, Some(&unit.state))
            && label(&f.model, model_label)
            && label(&f.instance_type, instance_type_label)
            && f.allocatable
                .is_none_or(|a| host.and_then(|h| h.allocatable) == Some(a))
            && f.update_complete
                .is_none_or(|u| host.is_some_and(|h| h.update_complete == u))
            && f.update_eligible
                .is_none_or(|e| host.and_then(|h| h.update_eligible) == Some(e))
            && f.record_type
                .is_none_or(|r| expected.is_some_and(|e| e.record_type == r))
            && f.linked
                .is_none_or(|l| expected.is_some_and(|e| e.linked == l))
            && f.initiator.is_none_or(|i| {
                host.and_then(|h| h.open_firmware_request.as_ref())
                    .is_some_and(|r| r.initiator == i)
            })
            && f.kind.is_none_or(|k| unit.state == wire(k))
            && f.done.is_none_or(|d| unit.done == d)
            && self.name.as_ref().is_none_or(|re| {
                instance
                    .and_then(|i| i.name.as_deref())
                    .is_some_and(|n| re.is_match(n))
            })
    }
}

/// The largest compiled `name` regex.
const NAME_REGEX_LIMIT: usize = 1 << 16;

/// Validates a units listing of `filter` and names its family.
pub(crate) fn units_listing(
    filter: &Filter,
    name: Option<&str>,
    caps: &Capabilities,
) -> Result<UnitsSpec, ApiError> {
    if name.is_some() && filter.unit_type != UnitType::Instance {
        return Err(unsupported(
            "name",
            "name is a filter of type=instance".to_string(),
        ));
    }
    let name = name
        .map(|raw| {
            regex::RegexBuilder::new(raw)
                .size_limit(NAME_REGEX_LIMIT)
                .build()
                .map_err(|e| ApiError::bad_param("name", format!("name is not a regex: {e}")))
        })
        .transpose()?;
    rest_source(filter)?;
    let family = match filter.unit_type {
        UnitType::RestTotal => {
            return Err(unsupported(
                "type",
                "type=rest_total is a count, not units".to_string(),
            ));
        }
        unit_type => {
            if filter.stage == Some(Stage::Expected) {
                return Err(unsupported(
                    "stage",
                    "list expected records with type=expected_record".to_string(),
                ));
            }
            let mut allowed = vec!["done"];
            match unit_type {
                UnitType::Host => {
                    host_read_checks(filter, caps)?;
                    allowed.extend(["stage", "state", "updateEligible"]);
                    allowed.extend(HOST_FIELDS);
                }
                UnitType::ExpectedRecord => {
                    allowed.extend(["recordType", "linked"]);
                    required_record_type(filter)?;
                }
                UnitType::IdentityMismatch => {
                    allowed.extend(["recordType", "kind"]);
                    mismatch_record_type(filter)?;
                }
                UnitType::HostFirmwareRequest => allowed.extend(["model", "initiator"]),
                _ => allowed.extend(["stage", "state"]),
            }
            only(filter, &allowed)?;
            family_of(unit_type)
        }
    };
    Ok(UnitsSpec {
        family,
        filter: filter.clone(),
        name,
    })
}
