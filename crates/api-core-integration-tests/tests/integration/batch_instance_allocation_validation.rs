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

use carbide_test_harness::prelude::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// Send an empty batch request with no instances.
/// Expect an error indicating at least one instance is required.
#[sqlx_test]
async fn test_batch_allocate_instances_empty_request(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = TestHarness::builder(pool).build().await;

    let batch_request = rpc::forge::BatchInstanceAllocationRequest {
        instance_requests: vec![],
    };

    let result = env
        .api()
        .allocate_instances(tonic::Request::new(batch_request))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message().contains("at least one instance"),
        "Expected error about empty request, got: {}",
        err.message()
    );
}
