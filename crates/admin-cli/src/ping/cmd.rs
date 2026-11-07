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

use std::cmp;
use std::time::{Duration, Instant};

use rpc::forge::VersionRequest;

use super::Opts;
use crate::rpc::ApiClient;

pub(super) async fn ping(client: &ApiClient, opts: &Opts) -> color_eyre::Result<()> {
    let interval = Duration::from_secs_f32(opts.interval);
    let mut total_count = 1;
    let mut err_count = 0;
    let mut success_count = 0;
    let mut rtt_min = Duration::from_secs(3600);
    let mut rtt_max = Duration::from_secs(0);
    let mut rtt_avg = Duration::from_secs(0);
    let global_start = Instant::now();
    loop {
        let start = Instant::now();
        let out = client
            .0
            .connection()
            .await?
            .version(tonic::Request::new(VersionRequest {
                display_config: false,
            }))
            .await;
        match out {
            Ok(_) => {
                let rtt = start.elapsed();
                rtt_min = cmp::min(rtt_min, rtt);
                rtt_max = cmp::max(rtt_max, rtt);
                rtt_avg = (rtt_avg * success_count + rtt) / total_count;
                println!("{total_count}. {} time={:0.2?}", client.0.url(), rtt);
                success_count += 1;
            }
            Err(status) => {
                println!(
                    "{total_count}. ERROR {}: {}",
                    status.code(),
                    status.message()
                );
                err_count += 1;
            }
        }
        total_count += 1;
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                println!();
                println!("{} requests transmitted, {} received, {:0.1}% loss, time {:0.2?}",
                         total_count - 1,
                        success_count,
                        err_count as f32 / (total_count-1) as f32 * 100.0,
                        global_start.elapsed(),
                );
                println!("rtt min/avg/max = {rtt_min:0.2?}/{rtt_avg:0.2?}/{rtt_max:0.2?}");
                break;
            }
            _ = tokio::time::sleep(interval) => {}
        }
    }
    Ok(())
}
