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

//! Tests for DpfSdkBuilder JoinSet integration.

use std::time::Duration;

use tokio::task::JoinSet;

use crate::sdk::DpfSdkBuilder;

const TEST_NS: &str = "sdk-joinset-ns";

#[tokio::test]
async fn test_with_join_set_spawns_refresh_into_provided_set() {
    let mut join_set = JoinSet::new();
    assert_eq!(join_set.len(), 0);

    let _sdk = DpfSdkBuilder::new(super::helpers::ConfigMock, TEST_NS, String::new())
        .with_bmc_password_refresh_interval(Duration::from_secs(3600))
        .with_join_set(&mut join_set)
        .build_without_resources()
        .await
        .unwrap();

    assert_eq!(join_set.len(), 1);
}

#[tokio::test]
async fn test_without_refresh_interval_no_task_spawned() {
    let mut join_set = JoinSet::new();

    let _sdk = DpfSdkBuilder::new(super::helpers::ConfigMock, TEST_NS, String::new())
        .with_join_set(&mut join_set)
        .build_without_resources()
        .await
        .unwrap();

    assert_eq!(join_set.len(), 0);
}

#[tokio::test]
async fn test_with_join_set_refresh_task_completes_after_sdk_drop() {
    let mut join_set = JoinSet::new();

    let sdk = DpfSdkBuilder::new(super::helpers::ConfigMock, TEST_NS, String::new())
        .with_bmc_password_refresh_interval(Duration::from_secs(3600))
        .with_join_set(&mut join_set)
        .build_without_resources()
        .await
        .unwrap();

    assert_eq!(join_set.len(), 1);
    drop(sdk);

    let result = tokio::time::timeout(Duration::from_secs(5), join_set.join_next()).await;
    assert!(
        result.is_ok(),
        "refresh task should complete after SDK is dropped"
    );
    assert_eq!(join_set.len(), 0);
}
