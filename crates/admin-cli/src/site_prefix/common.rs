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

use carbide_uuid::site_prefix::SitePrefixId;
use rpc::forge::{SitePrefix, SitePrefixSearchFilter, SitePrefixesByIdsRequest};

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn find_ids(
    api_client: &ApiClient,
    filter: SitePrefixSearchFilter,
) -> CarbideCliResult<Vec<SitePrefixId>> {
    Ok(api_client
        .0
        .find_site_prefix_ids(filter)
        .await?
        .site_prefix_ids)
}

pub(super) async fn find_by_ids(
    api_client: &ApiClient,
    page_size: usize,
    site_prefix_ids: &[SitePrefixId],
) -> CarbideCliResult<Vec<SitePrefix>> {
    let mut site_prefixes = Vec::with_capacity(site_prefix_ids.len());
    if site_prefix_ids.is_empty() {
        return Ok(site_prefixes);
    }

    let page_size = api_client.effective_chunk_size(page_size).await?;
    for page in site_prefix_ids.chunks(page_size) {
        let response = api_client
            .0
            .find_site_prefixes_by_ids(SitePrefixesByIdsRequest {
                site_prefix_ids: page.to_vec(),
            })
            .await?;
        site_prefixes.extend(response.site_prefixes);
    }

    Ok(site_prefixes)
}

pub(super) async fn find_one_by_id(
    api_client: &ApiClient,
    site_prefix_id: SitePrefixId,
) -> CarbideCliResult<SitePrefix> {
    let mut site_prefixes = find_by_ids(api_client, 1, &[site_prefix_id]).await?;
    match site_prefixes.len() {
        0 => Err(CarbideCliError::SitePrefixNotFound(site_prefix_id)),
        1 => Ok(site_prefixes
            .pop()
            .expect("one SitePrefix was returned by the API")),
        returned_count => Err(CarbideCliError::GenericError(format!(
            "the API returned {returned_count} SitePrefixes for ID {site_prefix_id}"
        ))),
    }
}
