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

//! Watcher for DPU resource events.
//!
//! Uses the repository `watch()` trait method to receive DPU events.
//! The repository implementation handles retries and requeuing when
//! handlers return `Err`.
//!
//! Callbacks may fire on any update to a DPU resource, not only on
//! phase transitions. All handlers must be idempotent.
//!
//! ## Example
//!
//! ```ignore
//! let _watcher = DpuWatcherBuilder::new(repo, "dpf-operator-system")
//!     .on_dpu_event(|event| async move {
//!         println!("Phase: {:?}", event.phase);
//!         Ok(())
//!     })
//!     .on_reboot_required(|event| async move {
//!         enqueue_host_reboot(&event.host_bmc_ip).await?;
//!         Ok(())
//!     })
//!     .start()?;
//! ```

use std::future::Future;
use std::sync::Arc;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::crds::dpus_generated::{DPU, DpuStatusPhase};
use crate::error::DpfError;
use crate::repository::DpuRepository;
use crate::types::{
    DpuErrorEvent, DpuEvent, DpuPhase, DpuReadyEvent, MaintenanceEvent, RebootRequiredEvent,
};

/// Callback for DPU state changes. Implemented automatically for all `Fn(T) -> Future`.
/// Purpose is to allow for generic async callbacks without having to box and pin the closure.
pub trait DPUStateCallback<T>: Fn(T) -> Self::Fut + Send + Sync + 'static {
    type Fut: Future<Output = Result<(), DpfError>> + Send + 'static;
}

impl<T, F, Fut> DPUStateCallback<T> for F
where
    F: Fn(T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
{
    type Fut = Fut;
}

// for defaulting to no-op callbacks in the builder
type NoopFn<T> = fn(T) -> std::future::Ready<Result<(), DpfError>>;

struct Callbacks<DE, RB, RD, MN, ER> {
    dpu_event: DE,
    reboot: RB,
    ready: RD,
    maintenance: MN,
    error: ER,
}

/// The watcher only cares about how the events are translated into the callbacks,
/// not the actual event gathering. The repository implementation handles procuring
/// the events, as well as retries and requeuing when handlers return `Err`.
///
/// The watcher continues running until this struct is dropped.
pub struct DpuWatcher {
    _cancel_guard: tokio_util::sync::DropGuard,
}

/// Builder for creating a DPU watcher.
pub struct DpuWatcherBuilder<
    'a,
    R: DpuRepository,
    DE = NoopFn<DpuEvent>,
    RB = NoopFn<RebootRequiredEvent>,
    RD = NoopFn<DpuReadyEvent>,
    MN = NoopFn<MaintenanceEvent>,
    ER = NoopFn<DpuErrorEvent>,
> {
    repo: Arc<R>,
    namespace: String,
    label_selector: Option<String>,
    join_set: Option<&'a mut JoinSet<()>>,
    cbs: Callbacks<DE, RB, RD, MN, ER>,
}

impl<R: DpuRepository> DpuWatcherBuilder<'_, R> {
    pub fn new(repo: Arc<R>, namespace: impl Into<String>) -> Self {
        Self {
            repo,
            namespace: namespace.into(),
            label_selector: None,
            join_set: None,
            cbs: Callbacks {
                dpu_event: |_| std::future::ready(Ok(())),
                reboot: |_| std::future::ready(Ok(())),
                ready: |_| std::future::ready(Ok(())),
                maintenance: |_| std::future::ready(Ok(())),
                error: |_| std::future::ready(Ok(())),
            },
        }
    }
}

/// This is a type state builder pattern. It's extra boilerplate, but we get generic
/// function types for the callbacks instead of boxing and pinning the closures.
impl<'a, R: DpuRepository, DE, RB, RD, MN, ER> DpuWatcherBuilder<'a, R, DE, RB, RD, MN, ER> {
    /// Spawn the watcher task into the provided `JoinSet` instead of an
    /// internal one. Use this in production to join all background tasks
    /// via a single `JoinSet` to catch panics.
    pub fn with_join_set(mut self, join_set: &'a mut JoinSet<()>) -> Self {
        self.join_set = Some(join_set);
        self
    }

    /// Restrict the watcher to DPU resources matching the given label selector.
    pub fn with_label_selector(mut self, selector: impl Into<String>) -> Self {
        self.label_selector = Some(selector.into());
        self
    }

    /// Register a callback for DPU events.
    ///
    /// The callback is invoked on every observed update to a DPU, not only
    /// on phase transitions. The handler must be idempotent.
    pub fn on_dpu_event<F, Fut>(self, callback: F) -> DpuWatcherBuilder<'a, R, F, RB, RD, MN, ER>
    where
        F: Fn(DpuEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        DpuWatcherBuilder {
            repo: self.repo,
            namespace: self.namespace,
            label_selector: self.label_selector,
            join_set: self.join_set,
            cbs: Callbacks {
                dpu_event: callback,
                reboot: self.cbs.reboot,
                ready: self.cbs.ready,
                maintenance: self.cbs.maintenance,
                error: self.cbs.error,
            },
        }
    }

    /// Register a callback for when a host reboot is required.
    ///
    /// Invoked on every update where the DPU is in the Rebooting phase, not
    /// only on transitions into that phase. The handler must be idempotent.
    ///
    /// Return `Ok(())` to acknowledge the event. Return `Err` to have the
    /// repository implementation retry after a backoff period.
    pub fn on_reboot_required<F, Fut>(
        self,
        callback: F,
    ) -> DpuWatcherBuilder<'a, R, DE, F, RD, MN, ER>
    where
        F: Fn(RebootRequiredEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        DpuWatcherBuilder {
            repo: self.repo,
            namespace: self.namespace,
            label_selector: self.label_selector,
            join_set: self.join_set,
            cbs: Callbacks {
                dpu_event: self.cbs.dpu_event,
                reboot: callback,
                ready: self.cbs.ready,
                maintenance: self.cbs.maintenance,
                error: self.cbs.error,
            },
        }
    }

    /// Register a callback for when a DPU is in the Ready phase.
    ///
    /// Invoked on every update where the DPU is in the Ready phase, not
    /// only on transitions into that phase. The handler must be idempotent.
    pub fn on_dpu_ready<F, Fut>(self, callback: F) -> DpuWatcherBuilder<'a, R, DE, RB, F, MN, ER>
    where
        F: Fn(DpuReadyEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        DpuWatcherBuilder {
            repo: self.repo,
            namespace: self.namespace,
            label_selector: self.label_selector,
            join_set: self.join_set,
            cbs: Callbacks {
                dpu_event: self.cbs.dpu_event,
                reboot: self.cbs.reboot,
                ready: callback,
                maintenance: self.cbs.maintenance,
                error: self.cbs.error,
            },
        }
    }

    /// Register a callback for when the DPU is in the NodeEffect phase.
    ///
    /// Invoked on every update where the DPU is in the NodeEffect phase, not
    /// only on transitions into that phase. The handler must be idempotent.
    ///
    /// Return `Ok(`)` to acknowledge the event. Return `Err` to have the
    /// repository implementation retry after a backoff period.
    pub fn on_maintenance_needed<F, Fut>(
        self,
        callback: F,
    ) -> DpuWatcherBuilder<'a, R, DE, RB, RD, F, ER>
    where
        F: Fn(MaintenanceEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        DpuWatcherBuilder {
            repo: self.repo,
            namespace: self.namespace,
            label_selector: self.label_selector,
            join_set: self.join_set,
            cbs: Callbacks {
                dpu_event: self.cbs.dpu_event,
                reboot: self.cbs.reboot,
                ready: self.cbs.ready,
                maintenance: callback,
                error: self.cbs.error,
            },
        }
    }

    /// Register a callback for when a DPU is in the Error phase.
    ///
    /// Invoked on every update where the DPU is in the Error phase, not
    /// only on transitions into that phase. The handler must be idempotent.
    ///
    /// Return `Ok(())` to acknowledge the event. Return `Err` to have the
    /// repository implementation retry after a backoff period.
    pub fn on_error<F, Fut>(self, callback: F) -> DpuWatcherBuilder<'a, R, DE, RB, RD, MN, F>
    where
        F: Fn(DpuErrorEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), DpfError>> + Send + 'static,
    {
        DpuWatcherBuilder {
            repo: self.repo,
            namespace: self.namespace,
            label_selector: self.label_selector,
            join_set: self.join_set,
            cbs: Callbacks {
                dpu_event: self.cbs.dpu_event,
                reboot: self.cbs.reboot,
                ready: self.cbs.ready,
                maintenance: self.cbs.maintenance,
                error: callback,
            },
        }
    }
}

impl<R, DE, RB, RD, MN, ER> DpuWatcherBuilder<'_, R, DE, RB, RD, MN, ER>
where
    R: DpuRepository,
    DE: DPUStateCallback<DpuEvent>,
    RB: DPUStateCallback<RebootRequiredEvent>,
    RD: DPUStateCallback<DpuReadyEvent>,
    MN: DPUStateCallback<MaintenanceEvent>,
    ER: DPUStateCallback<DpuErrorEvent>,
{
    /// Start watching for events.
    ///
    /// Returns a handle that gracefully stops the watcher when dropped.
    pub fn start(self) -> std::io::Result<DpuWatcher> {
        spawn_watcher(
            self.repo,
            self.namespace,
            self.label_selector,
            self.cbs,
            self.join_set,
        )
    }
}

// separate function to drop the 'a lifetime from the builder
fn spawn_watcher<R, DE, RB, RD, MN, ER>(
    repo: Arc<R>,
    namespace: String,
    label_selector: Option<String>,
    cbs: Callbacks<DE, RB, RD, MN, ER>,
    join_set: Option<&mut JoinSet<()>>,
) -> std::io::Result<DpuWatcher>
where
    R: DpuRepository,
    DE: DPUStateCallback<DpuEvent>,
    RB: DPUStateCallback<RebootRequiredEvent>,
    RD: DPUStateCallback<DpuReadyEvent>,
    MN: DPUStateCallback<MaintenanceEvent>,
    ER: DPUStateCallback<DpuErrorEvent>,
{
    let cancel_token = CancellationToken::new();
    let guard = cancel_token.clone().drop_guard();

    let cbs = Arc::new(cbs);

    let handler = move |dpu: Arc<DPU>| {
        let cbs = cbs.clone();
        async move {
            let Some(status) = &dpu.status else {
                return Ok(());
            };
            let Some(dpu_name) = &dpu.metadata.name else {
                return Ok(());
            };

            let device_name = dpu.spec.dpu_device_name.clone();
            let phase = DpuPhase::from(status.phase.clone());
            let node_name = dpu.spec.dpu_node_name.clone();

            (cbs.dpu_event)(DpuEvent {
                dpu_name: dpu_name.clone(),
                device_name: device_name.clone(),
                node_name: node_name.clone(),
                phase,
            })
            .await?;

            if matches!(status.phase, DpuStatusPhase::NodeEffect) {
                (cbs.maintenance)(MaintenanceEvent {
                    dpu_name: dpu_name.clone(),
                    node_name: node_name.clone(),
                })
                .await?;
            }

            if matches!(status.phase, DpuStatusPhase::Ready) {
                (cbs.ready)(DpuReadyEvent {
                    dpu_name: dpu_name.clone(),
                    device_name: device_name.clone(),
                    node_name: node_name.clone(),
                })
                .await?;
            }

            if matches!(status.phase, DpuStatusPhase::Error) {
                (cbs.error)(DpuErrorEvent {
                    dpu_name: dpu_name.clone(),
                    device_name: device_name.clone(),
                    node_name: node_name.clone(),
                })
                .await?;
            }

            if matches!(status.phase, DpuStatusPhase::Rebooting) {
                let Some(host_bmc_ip) = dpu.spec.bmc_ip.as_deref().and_then(|ip| ip.parse().ok())
                else {
                    tracing::warn!(dpu_name = %dpu_name, "Skipping reboot event with missing or invalid BMC IP");
                    return Ok(());
                };

                (cbs.reboot)(RebootRequiredEvent {
                    dpu_name: dpu_name.clone(),
                    node_name: node_name.clone(),
                    host_bmc_ip,
                })
                .await?;
            }

            Ok(())
        }
    };

    let watch_future = repo.watch(&namespace, label_selector.as_deref(), handler);
    let task = async move {
        cancel_token.run_until_cancelled(watch_future).await;
    };

    if let Some(js) = join_set {
        js.build_task().name("dpf_dpu_watcher").spawn(task)?;
    } else {
        tokio::task::Builder::new()
            .name("dpf_dpu_watcher")
            .spawn(task)?;
    }

    Ok(DpuWatcher {
        _cancel_guard: guard,
    })
}
