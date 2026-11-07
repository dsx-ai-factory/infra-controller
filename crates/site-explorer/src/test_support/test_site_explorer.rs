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

use std::net::IpAddr;
use std::ops::Deref;
use std::sync::Arc;

use model::site_explorer::{EndpointExplorationError, EndpointExplorationReport};

use super::mock_endpoint_explorer::MockEndpointExplorer;
use crate::errors::SiteExplorerResult;
use crate::{EndpointExplorationService, SiteExplorer, SiteIdentifiedHosts};

pub struct TestSiteExplorer {
    endpoint_explorer: Arc<MockEndpointExplorer>,
    site_explorer: SiteExplorer,
}

impl TestSiteExplorer {
    pub fn new(site_explorer: SiteExplorer, endpoint_explorer: Arc<MockEndpointExplorer>) -> Self {
        Self {
            endpoint_explorer,
            site_explorer,
        }
    }

    pub fn endpoint_explorer(&self) -> &MockEndpointExplorer {
        self.endpoint_explorer.as_ref()
    }

    pub fn endpoint_exploration_service(&self) -> Arc<EndpointExplorationService> {
        self.site_explorer.endpoint_exploration_service.clone()
    }

    pub fn insert_endpoints(&self, endpoints: Vec<(IpAddr, EndpointExplorationReport)>) {
        self.endpoint_explorer.insert_endpoints(endpoints);
    }

    pub fn insert_endpoint_result(
        &self,
        address: IpAddr,
        result: Result<EndpointExplorationReport, EndpointExplorationError>,
    ) {
        self.endpoint_explorer
            .insert_endpoint_result(address, result);
    }

    pub fn insert_endpoint_results(
        &self,
        endpoints: Vec<(
            IpAddr,
            Result<EndpointExplorationReport, EndpointExplorationError>,
        )>,
    ) {
        self.endpoint_explorer.insert_endpoint_results(endpoints);
    }

    pub async fn run_single_iteration(&self) -> SiteExplorerResult<SiteIdentifiedHosts> {
        self.site_explorer.run_single_iteration().await
    }
}

impl Deref for TestSiteExplorer {
    type Target = SiteExplorer;

    fn deref(&self) -> &Self::Target {
        &self.site_explorer
    }
}
