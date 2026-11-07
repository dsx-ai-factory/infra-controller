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

//! Shared health-override CRUD behaviors, written once and driven per entity.
//!
//! Power shelves, switches, and racks each expose the same health-override
//! surface — insert / list / remove an override, see it on the entity's status,
//! and roll it into the aggregation metrics — over entity-specific RPCs. Rather
//! than re-prove each behavior per entity, the behaviors live here once. An entity
//! supplies its differences as closures ([`HealthCrud`]): how to insert, list,
//! remove, and read back an override for one of its IDs, plus a representative
//! alerting report and an empty (healthy) report. Each entity's `#[sqlx_test]`
//! builds a [`HealthCrud`] over a single pool and calls the shared checks.
//!
//! Entity-unique behavior — rack override precedence and propagation to host
//! aggregate health — stays in the per-entity test module; only the behaviors that
//! are identical across all three are shared here.

use carbide_utils::test_support::test_meter::TestMeter;
use health_report::HealthReport;
use rpc::forge::{HealthReportApplyMode, HealthReportEntry, HealthSourceOrigin};
use tonic::Status;

/// The health-override fields common to every entity's status, as read back from a
/// `find` RPC: the merged health report and the per-source override origins.
pub struct HealthStatusView {
    /// The merged health report returned for the entity.
    pub health: Option<rpc::health::HealthReport>,
    /// The external health sources applied to the entity.
    pub health_sources: Vec<HealthSourceOrigin>,
}

/// One entity's health-override CRUD surface, expressed as closures over a single
/// pool. The shared checks below drive these instead of any concrete entity's RPCs.
///
/// `Id` is the entity ID type; closures take it by value so the same harness can be
/// pointed at a real ID or a freshly-minted nonexistent one.
pub struct HealthCrud<Id, Ins, Lst, Rem, Fnd> {
    /// A persisted entity's ID, used by the behaviors that operate on a live entity.
    pub real_id: Id,
    /// An ID that was never persisted, used by the "missing entity" check.
    pub nonexistent_id: Id,
    /// A representative report carrying one alert (entity-specific alert ID).
    pub alert: HealthReport,
    /// A source identifier the alert report is filed under.
    pub alert_source: &'static str,
    /// An override RPC: file `report` under its source for `id`, in `mode`.
    pub insert: Ins,
    /// A list RPC: the override entries currently held for `id`.
    pub list: Lst,
    /// A remove RPC: drop the override filed under `source` for `id`.
    pub remove: Rem,
    /// A find RPC: read back `id`'s status health view.
    pub find: Fnd,
}

impl<Id, Ins, Lst, Rem, Fnd> HealthCrud<Id, Ins, Lst, Rem, Fnd>
where
    Id: Clone,
    Ins: AsyncFn(Id, HealthReport, HealthReportApplyMode) -> Result<(), Status>,
    Lst: AsyncFn(Id) -> Result<Vec<HealthReportEntry>, Status>,
    Rem: AsyncFn(Id, String) -> Result<(), Status>,
    Fnd: AsyncFn(Id) -> Result<HealthStatusView, Status>,
{
    /// Insert an override, confirm exactly one entry lists with the expected source
    /// and one alert, remove it, and confirm the list is empty again.
    pub async fn check_insert_list_remove(&self) {
        (self.insert)(
            self.real_id.clone(),
            self.alert.clone(),
            HealthReportApplyMode::Merge,
        )
        .await
        .expect("insert override");

        let entries = (self.list)(self.real_id.clone())
            .await
            .expect("list overrides");
        assert_eq!(entries.len(), 1, "exactly one override should be listed");
        let listed: HealthReport = entries[0]
            .report
            .clone()
            .unwrap()
            .try_into()
            .expect("listed report converts back");
        assert_eq!(listed.source, self.alert_source);
        assert_eq!(listed.alerts.len(), 1);

        (self.remove)(self.real_id.clone(), self.alert_source.to_string())
            .await
            .expect("remove override");

        let entries = (self.list)(self.real_id.clone())
            .await
            .expect("list overrides after remove");
        assert_eq!(entries.len(), 0, "override should be gone after remove");
    }

    /// Inserting the same override repeatedly is idempotent: still one entry.
    pub async fn check_idempotent_insert(&self) {
        for _ in 0..3 {
            (self.insert)(
                self.real_id.clone(),
                self.alert.clone(),
                HealthReportApplyMode::Merge,
            )
            .await
            .expect("repeated insert override");
        }

        let entries = (self.list)(self.real_id.clone())
            .await
            .expect("list overrides");
        assert_eq!(entries.len(), 1, "repeated inserts should collapse to one");
    }

    /// A continuously-firing alert keeps the `in_alert_since` of its first
    /// report, so operators can tell how long the condition has lasted. Without
    /// this, every re-report restamps the alert and it always looks brand new.
    pub async fn check_retains_in_alert_since(&self) {
        let in_alert_since = async || {
            let entries = (self.list)(self.real_id.clone())
                .await
                .expect("list overrides");
            assert_eq!(entries.len(), 1, "exactly one override should be listed");
            let listed: HealthReport = entries[0]
                .report
                .clone()
                .unwrap()
                .try_into()
                .expect("listed report converts back");
            assert_eq!(listed.alerts.len(), 1);
            listed.alerts[0]
                .in_alert_since
                .expect("in_alert_since is stamped on insert")
        };

        (self.insert)(
            self.real_id.clone(),
            self.alert.clone(),
            HealthReportApplyMode::Merge,
        )
        .await
        .expect("insert override");
        let first_seen = in_alert_since().await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The same alert reported again must not be restamped.
        (self.insert)(
            self.real_id.clone(),
            self.alert.clone(),
            HealthReportApplyMode::Merge,
        )
        .await
        .expect("re-insert override");
        assert_eq!(
            in_alert_since().await,
            first_seen,
            "re-reported alert must retain its original in_alert_since"
        );
    }

    /// Removing a source that was never filed is a `NotFound`.
    pub async fn check_remove_nonexistent_source(&self) {
        let status = (self.remove)(self.real_id.clone(), "nonexistent-source".to_string())
            .await
            .expect_err("removing an unknown source should error");
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    /// Inserting an override for an entity that does not exist is an error.
    pub async fn check_missing_entity(&self) {
        let status = (self.insert)(
            self.nonexistent_id.clone(),
            self.alert.clone(),
            HealthReportApplyMode::Merge,
        )
        .await
        .expect_err("inserting an override for a nonexistent entity should error");
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    /// A `Replace`-mode override lists with `Replace` mode and clears on remove.
    pub async fn check_replace_mode(&self, replace_report: HealthReport, replace_source: &str) {
        (self.insert)(
            self.real_id.clone(),
            replace_report,
            HealthReportApplyMode::Replace,
        )
        .await
        .expect("insert replace override");

        let entries = (self.list)(self.real_id.clone())
            .await
            .expect("list overrides");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mode, HealthReportApplyMode::Replace as i32);

        (self.remove)(self.real_id.clone(), replace_source.to_string())
            .await
            .expect("remove replace override");

        let entries = (self.list)(self.real_id.clone())
            .await
            .expect("list overrides after remove");
        assert_eq!(entries.len(), 0);
    }

    /// An inserted override is visible on the entity's status from a `find` RPC:
    /// the merged health carries alerts, and the override source is recorded.
    pub async fn check_visible_in_find(&self) {
        (self.insert)(
            self.real_id.clone(),
            self.alert.clone(),
            HealthReportApplyMode::Merge,
        )
        .await
        .expect("insert override");

        let view = (self.find)(self.real_id.clone())
            .await
            .expect("find entity");

        let health = view.health.expect("status should carry a health report");
        let health: HealthReport = health.try_into().expect("health converts back");
        assert!(
            !health.alerts.is_empty(),
            "status health should contain alerts"
        );

        assert_eq!(view.health_sources.len(), 1);
        assert_eq!(view.health_sources[0].source, self.alert_source);
        assert_eq!(
            view.health_sources[0].mode,
            HealthReportApplyMode::Merge as i32
        );
    }
}

/// Drives the shared health-override aggregation metrics for one entity: an
/// override (merge, then replace) flips the per-entity override and status gauges,
/// and the entity never emits the legacy `alerts_suppressed` alias.
///
/// `metric_entity` is the entity token in the metric names (e.g. `power_shelves`),
/// `run_iteration` advances that entity's controller, and `insert`/`alert`/`empty`
/// supply the entity's override RPC and reports.
pub async fn check_health_aggregation<Id, Ins, Iter>(
    metric_entity: &str,
    real_id: Id,
    alert: HealthReport,
    empty: HealthReport,
    meter: &TestMeter,
    insert: Ins,
    run_iteration: Iter,
) where
    Id: Clone,
    Ins: AsyncFn(Id, HealthReport, HealthReportApplyMode) -> Result<(), Status>,
    Iter: AsyncFn(),
{
    let overrides_metric = format!("carbide_{metric_entity}_health_overrides_count");
    let status_metric = format!("carbide_{metric_entity}_health_status_count");

    let sorted = |name: &str| {
        let mut m = meter.formatted_metrics(name);
        m.sort();
        m
    };

    run_iteration().await;
    assert_eq!(
        sorted(&overrides_metric),
        vec![
            "{fresh=\"true\",override_type=\"merge\"} 0".to_string(),
            "{fresh=\"true\",override_type=\"replace\"} 0".to_string(),
        ]
    );
    assert_eq!(
        sorted(&status_metric),
        vec![
            "{fresh=\"true\",healthy=\"false\"} 0".to_string(),
            "{fresh=\"true\",healthy=\"true\"} 1".to_string(),
        ]
    );

    insert(real_id.clone(), alert, HealthReportApplyMode::Merge)
        .await
        .expect("insert merge override");
    run_iteration().await;
    assert_eq!(
        sorted(&overrides_metric),
        vec![
            "{fresh=\"true\",override_type=\"merge\"} 1".to_string(),
            "{fresh=\"true\",override_type=\"replace\"} 0".to_string(),
        ]
    );
    assert_eq!(
        sorted(&status_metric),
        vec![
            "{fresh=\"true\",healthy=\"false\"} 1".to_string(),
            "{fresh=\"true\",healthy=\"true\"} 0".to_string(),
        ]
    );

    insert(real_id, empty, HealthReportApplyMode::Replace)
        .await
        .expect("insert replace override");
    run_iteration().await;
    assert_eq!(
        sorted(&overrides_metric),
        vec![
            "{fresh=\"true\",override_type=\"merge\"} 1".to_string(),
            "{fresh=\"true\",override_type=\"replace\"} 1".to_string(),
        ]
    );
    assert_eq!(
        sorted(&status_metric),
        vec![
            "{fresh=\"true\",healthy=\"false\"} 0".to_string(),
            "{fresh=\"true\",healthy=\"true\"} 1".to_string(),
        ]
    );

    assert!(
        meter
            .formatted_metrics("carbide_alerts_suppressed_count")
            .is_empty(),
        "{metric_entity} should not emit the legacy alerts_suppressed alias"
    );
}
