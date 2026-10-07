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

use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn re_wrap(
    api_client: &ApiClient,
    batch_size: Option<u32>,
) -> CarbideCliResult<()> {
    let request = ::rpc::forge::ReWrapSecretsRequest { batch_size };

    let resp = api_client.0.re_wrap_secrets(request).await?;

    println!(
        "Re-wrap complete: {} re-wrapped, {} already current",
        resp.re_wrapped, resp.already_current
    );
    if resp.stale_remaining == 0 {
        println!(
            "No rows remain on KEKs outside the routing config; unrouted KEKs can be retired."
        );
    } else {
        println!(
            "{} rows are still wrapped by KEKs outside the routing config -- \
             concurrent writers likely landed rows mid-walk; run re-wrap again.",
            resp.stale_remaining
        );
    }
    Ok(())
}
