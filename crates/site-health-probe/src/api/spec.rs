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

//! Filter rules: the shared metric contract
//! (`carbide_site_health_probe_api::metrics`) names the gauge and family of a
//! count and validates a units listing. The probe compiles a listing's name
//! and matches its units here. Counts read the same gauge as the published
//! metrics, so a count always equals the metric sum.

use carbide_site_health_probe_api::metrics::{
    self, FilterError, instance_type_label, model_label, name_filter,
};
pub(crate) use carbide_site_health_probe_api::metrics::{Capabilities, Need, SeriesSpec, wire};
use carbide_site_health_probe_api::{Family, Filter, HostFields, Stage, Unit};
use regex::Regex;

use crate::api::ApiError;

impl From<FilterError> for ApiError {
    fn from(e: FilterError) -> Self {
        ApiError::unprocessable(e.code, Some(&e.field), e.reason)
    }
}

/// The gauge a count of `filter` reads. Anything the table does not name is
/// 422.
pub(crate) fn series_spec(filter: &Filter, caps: &Capabilities) -> Result<SeriesSpec, ApiError> {
    Ok(metrics::series_spec(filter, caps)?)
}

/// The family and predicate of a units listing.
#[derive(Debug)]
pub(crate) struct UnitsSpec {
    pub family: Family,
    pub needs: Vec<Need>,
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
    if name.is_some() {
        name_filter(filter)?;
    }
    let name = name
        .map(|raw| {
            regex::RegexBuilder::new(raw)
                .size_limit(NAME_REGEX_LIMIT)
                .build()
                .map_err(|e| ApiError::bad_param("name", format!("name is not a regex: {e}")))
        })
        .transpose()?;
    let listing = metrics::units_listing(filter, caps)?;
    Ok(UnitsSpec {
        family: listing.family,
        needs: listing.needs,
        filter: filter.clone(),
        name,
    })
}
