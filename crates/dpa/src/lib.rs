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

//! State Controller implementation for Dpa Interface

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Instant;

use carbide_instrument::emit;
use mqttea::client::MqtteaClient;

use crate::metrics::{DpaCommandSendFailed, DpaCommandSent};

mod metrics;
pub mod rpc;

pub struct DpaInfo {
    pub subnet_ip: Ipv4Addr,
    pub subnet_mask: i32,
    pub mqtt_client: Option<Arc<MqtteaClient>>,
}

// Send a SetVni command to the DPA specified by the given macaddress.
// The SetVni command to contain the given vni and revision string.
pub async fn send_dpa_command(
    client: Arc<MqtteaClient>,
    dpa_info: &Arc<DpaInfo>,
    macaddr: String,
    revision: String,
    vni: i32,
) -> Result<(), eyre::Report> {
    let pfvni = rpc::Pfvni {
        pf_id: 0,
        mac: macaddr.clone(),
        vni,
        subnet_ip: dpa_info.subnet_ip.to_string(),
        subnet_mask: dpa_info.subnet_mask,
        dhcp_ip: String::new(),
        host_ip: String::new(),
    };

    let mdata = rpc::DpaMetadata {
        dpa_id: macaddr.clone(),
        host_id: String::new(),
        revision: revision.clone(),
        transaction: String::new(),
    };

    let svni = rpc::SetVni {
        metadata: Some(mdata),
        pf_info: Some(pfvni),
    };

    let maddr = macaddr.replace(":", "");

    let topic = format!("dpa/command/{maddr}/SetVni");

    let send_started_at = Instant::now();
    let send_result = client.send_message(&topic, &svni).await;
    let send_latency = send_started_at.elapsed();

    match send_result {
        Ok(()) => {
            emit(DpaCommandSent {
                latency: send_latency,
                revision,
                vni: i64::from(vni),
            });
        }
        Err(e) => {
            emit(DpaCommandSendFailed {
                latency: send_latency,
                error: format!("{e:?}"),
                payload: format!("{svni:?}"),
                topic,
            });
            return Err(eyre::eyre!("send_message error: {e}"));
        }
    }
    Ok(())
}
