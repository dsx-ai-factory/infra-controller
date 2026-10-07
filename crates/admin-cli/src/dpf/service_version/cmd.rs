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

use prettytable::row;

use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn service_version(api_client: &ApiClient) -> CarbideCliResult<()> {
    let services = api_client.get_dpf_service_versions().await?;

    if services.is_empty() {
        println!("No DPF services configured");
        return Ok(());
    }

    let mut table = prettytable::Table::new();
    table.set_titles(row![
        "Service",
        "Config Helm Version",
        "Live Helm Version",
        "Config Docker Tag",
        "Live Docker Tag",
    ]);
    for svc in services {
        let live_helm = if svc.live_helm_version.is_empty() {
            "n/a".to_string()
        } else if svc.live_helm_version == svc.config_helm_version {
            format!("{} (match)", svc.live_helm_version)
        } else {
            format!("{} (DIFFERS)", svc.live_helm_version)
        };
        let config_docker = if svc.config_docker_image_tag.is_empty() {
            "-".to_string()
        } else {
            svc.config_docker_image_tag.clone()
        };
        let live_docker = if svc.live_docker_image_tag.is_empty() {
            "n/a".to_string()
        } else if svc.live_docker_image_tag == svc.config_docker_image_tag {
            format!("{} (match)", svc.live_docker_image_tag)
        } else {
            format!("{} (DIFFERS)", svc.live_docker_image_tag)
        };
        table.add_row(row![
            svc.service,
            svc.config_helm_version,
            live_helm,
            config_docker,
            live_docker,
        ]);
    }
    table.printstd();
    Ok(())
}
