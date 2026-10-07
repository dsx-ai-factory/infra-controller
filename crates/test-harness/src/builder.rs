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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use carbide_api_core::test_support::ApiMetricsEmitter;
use carbide_api_core::test_support::builder::TestApiBuilder;
use carbide_utils::test_support::test_meter::TestMeter;
use db::work_lock_manager;
use model::resource_pool::ResourcePoolDef;
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::resource_pool::ResourcePoolBuilder;
use crate::{ApiHandle, TestHarness};

type ApiBuilderFn = Box<dyn FnOnce(TestApiBuilder) -> TestApiBuilder + 'static>;

pub struct TestHarnessBuilder {
    pub(crate) db_pool: PgPool,
    pub(crate) test_meter: Option<TestMeter>,
    pub(crate) api_builder_fn: Option<ApiBuilderFn>,
    pub(crate) pools: Option<HashMap<String, ResourcePoolDef>>,
}

impl TestHarnessBuilder {
    pub fn with_resource_pools(self, pools: HashMap<String, ResourcePoolDef>) -> Self {
        Self {
            pools: Some(pools),
            ..self
        }
    }

    pub fn with_api_builder_fn(
        self,
        f: impl FnOnce(TestApiBuilder) -> TestApiBuilder + 'static,
    ) -> TestHarnessBuilder {
        Self {
            api_builder_fn: Some(Box::new(f)),
            ..self
        }
    }

    pub async fn build(self) -> TestHarness {
        let test_meter = self.test_meter.unwrap_or_default();
        let api = match self.api_builder_fn {
            Some(f) => Self::build_api(self.pools, self.db_pool, &test_meter, f).await,
            None => Self::build_api(self.pools, self.db_pool, &test_meter, |b| b).await,
        };

        TestHarness {
            api: api.into(),
            test_meter,
            processor_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    async fn build_api(
        resource_pools: Option<HashMap<String, ResourcePoolDef>>,
        db_pool: PgPool,
        test_meter: &TestMeter,
        f: impl FnOnce(TestApiBuilder) -> TestApiBuilder,
    ) -> ApiHandle {
        let cancel_token = CancellationToken::new();
        let mut join_set = JoinSet::new();

        let pools = resource_pools.unwrap_or_else(|| ResourcePoolBuilder::default().build());
        let mut txn = db_pool.begin().await.unwrap();
        db::resource_pool::define_all_from(&mut txn, &pools.into_iter().collect())
            .await
            .unwrap();
        txn.commit().await.unwrap();

        let ib_fabric_ids = ["default"];
        let common_pools = db::resource_pool::create_common_pools(
            db_pool.clone(),
            HashSet::from_iter(ib_fabric_ids.into_iter().map(ToString::to_string)),
        )
        .await
        .expect("common pool creation must succeed");

        let work_lock_manager_handle = db::work_lock_manager::start(
            &mut join_set,
            db_pool.clone(),
            work_lock_manager::KeepaliveConfig::default(),
        )
        .await
        .expect("work_lock_manager failed to start: no available connections?");

        let api = f(TestApiBuilder::new(
            db_pool,
            common_pools,
            work_lock_manager_handle,
        ))
        .with_metric_emitter(ApiMetricsEmitter::new(&test_meter.meter()))
        .build();
        ApiHandle {
            api: Arc::new(api),
            _drop_guard: cancel_token.clone().drop_guard(),
            cancel_token,
            _js: join_set,
        }
    }
}
