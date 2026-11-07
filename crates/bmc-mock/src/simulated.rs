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

use std::sync::{Arc, Weak};

use nv_redfish::schema::resource::PowerState;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::actor::{Actor, ActorCallbacks, ActorMailbox, ActorResult};
use crate::{
    ActionError, BmcEvent, BmcResetUpdate, BmcState, Callbacks, DeviceEffect, POWER_CYCLE_DELAY,
    ResourceResetType, validate_power_reset,
};

/// Owns power transitions for a standalone generated BMC.
pub struct SimulatedActor {
    actor: Actor<Message>,
}

impl SimulatedActor {
    /// Creates an unstarted actor and its callbacks. Dropping the callbacks cancels the backend.
    pub fn new(guard: DropGuard) -> (Self, SimulatedCallbacks) {
        let (actor, mailbox) = Actor::new();
        (
            Self { actor },
            SimulatedCallbacks {
                mailbox,
                _stop: guard,
            },
        )
    }

    /// Publishes the initial On observation and starts the actor in the owner's task set.
    /// The owner must retain the state Arc while serving the BMC. Lifecycle access
    /// is weak so dropping the owner and callbacks can cancel the backend.
    /// Cancellation stops the actor and its pending power-cycle alarm.
    pub fn run(
        self,
        state: &Arc<BmcState<SimulatedCallbacks>>,
        tasks: &mut JoinSet<()>,
        stop: CancellationToken,
    ) {
        state.system_state.set_power_state(PowerState::On);
        state.on_event(&BmcEvent::PowerOn);
        state.on_event(&BmcEvent::BootCompleted);
        let backend = Backend {
            state: Arc::downgrade(state),
            power_state: PowerState::On,
            cycling: false,
        };
        tasks.spawn(async move {
            stop.run_until_cancelled(self.actor.run(backend)).await;
        });
    }
}

/// Sends resets to the sequential simulated backend; replies reflect applied transitions.
#[derive(Debug)]
pub struct SimulatedCallbacks {
    mailbox: ActorMailbox<Message>,
    _stop: DropGuard,
}

#[derive(Debug)]
enum Message {
    Reset {
        reset_type: ResourceResetType,
        reply: oneshot::Sender<Result<(), ActionError>>,
    },
    ResetBmcToDefaults {
        update: BmcResetUpdate,
        reply: oneshot::Sender<Result<(), ActionError>>,
    },
    CycleCompleted,
}

struct Backend {
    state: Weak<BmcState<SimulatedCallbacks>>,
    power_state: PowerState,
    cycling: bool,
}

impl Backend {
    fn publish(&mut self, state: PowerState) {
        self.power_state = state;
        if let Some(bmc) = self.state.upgrade() {
            bmc.system_state.set_power_state(state);
        }
    }

    // The standalone model boots immediately, but still publishes both stages.
    fn publish_boot(&self) {
        if let Some(state) = self.state.upgrade() {
            state.on_event(&BmcEvent::PowerOn);
            state.on_event(&BmcEvent::BootCompleted);
        }
    }

    fn reset(
        &mut self,
        mailbox: &ActorMailbox<Message>,
        reset_type: ResourceResetType,
    ) -> Result<(), ActionError> {
        if self.cycling {
            return Err(ActionError::BadRequest(eyre::eyre!(
                "bmc-mock: cannot reset machine, it is in the middle of power cycling"
            )));
        }
        validate_power_reset(self.power_state, reset_type)?;
        use ResourceResetType::*;
        match reset_type {
            On | ForceOn | PushPowerButton => {
                let was_off = self.power_state != PowerState::On;
                self.publish(PowerState::On);
                if was_off {
                    self.publish_boot();
                }
            }
            GracefulRestart | ForceRestart => {
                self.publish(PowerState::On);
                self.publish_boot();
            }
            Pause | Resume => self.publish(PowerState::On),
            GracefulShutdown | ForceOff | Nmi | Suspend | Sleep | Hibernate => {
                self.publish(PowerState::Off)
            }
            PowerCycle | FullPowerCycle => {
                mailbox
                    .send_at(
                        (Instant::now() + POWER_CYCLE_DELAY).into(),
                        Message::CycleCompleted,
                    )
                    .map_err(|error| ActionError::Internal(error.into()))?;
                self.cycling = true;
                self.publish(PowerState::Off);
            }
            UnsupportedValue => {}
        }
        Ok(())
    }
}

impl ActorCallbacks<Message> for Backend {
    async fn message(&mut self, mailbox: &ActorMailbox<Message>, message: Message) -> ActorResult {
        match message {
            Message::Reset { reset_type, reply } => {
                let result = self.reset(mailbox, reset_type);
                // A power cycle completes later, in CycleCompleted.
                if result.is_ok()
                    && !matches!(
                        reset_type,
                        ResourceResetType::PowerCycle | ResourceResetType::FullPowerCycle
                    )
                    && let Some(state) = self.state.upgrade()
                {
                    state.record_power_command_completed(reset_type);
                }
                reply.send(result).ok();
            }
            Message::ResetBmcToDefaults { update, reply } => {
                // Publication belongs to the actor even if the HTTP caller has gone away.
                // A standalone BMC serves no DHCP, so there is no recovery to admit.
                reply.send(update.commit().await.map(drop)).ok();
            }
            Message::CycleCompleted => {
                self.cycling = false;
                self.publish(PowerState::On);
                self.publish_boot();
                if let Some(state) = self.state.upgrade() {
                    state.record_power_command_completed(ResourceResetType::PowerCycle);
                }
            }
        }
        ActorResult::Noop
    }
}

impl Callbacks for SimulatedCallbacks {
    async fn computer_system_reset(
        &self,
        reset_type: ResourceResetType,
    ) -> Result<(), ActionError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(Message::Reset { reset_type, reply })
            .map_err(|error| ActionError::Internal(error.into()))?;
        response
            .await
            .map_err(|error| ActionError::Internal(error.into()))?
    }

    /// A factory reset changes only BMC state; other effects are not modeled.
    async fn device_effect(&self, effect: DeviceEffect) -> Result<(), ActionError> {
        let DeviceEffect::ResetBmcToDefaults(update) = effect else {
            return Err(ActionError::BadRequest(eyre::eyre!(
                "bmc-mock: backend does not support device effect {effect:?}"
            )));
        };
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(Message::ResetBmcToDefaults { update, reply })
            .map_err(|error| ActionError::Internal(error.into()))?;
        response
            .await
            .map_err(|error| ActionError::Internal(error.into()))?
    }

    fn state_refresh_indication(&self) {}
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::host_info;
    use crate::{HardwareType, MachineRouterOptions, machine_router};

    async fn integrated_event_log_messages(router: &axum::Router) -> Vec<String> {
        let response = router
            .clone()
            .oneshot(
                Request::get("/redfish/v1/Managers/1/LogServices/IEL/Entries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let entries: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        entries["Members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["Message"].as_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn hpe_integrated_event_log_records_only_completed_commands() {
        let stop = CancellationToken::new();
        let (actor, callbacks) = SimulatedActor::new(stop.clone().drop_guard());
        let callbacks = Arc::new(callbacks);
        let (router, state) = machine_router(
            &host_info(HardwareType::HpeProliantDl380aGen11),
            callbacks.clone(),
            "standalone-hpe".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = Arc::new(state);
        let mut tasks = JoinSet::new();
        actor.run(&state, &mut tasks, stop.clone());
        let system = state.system_state.primary_system_odata_id().unwrap();
        let reset = |reset_type: &'static str| {
            router.clone().oneshot(
                Request::post(format!("{system}/Actions/ComputerSystem.Reset"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"ResetType": reset_type}).to_string(),
                    ))
                    .unwrap(),
            )
        };

        assert_eq!(
            reset("ForceRestart").await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(reset("ForceOff").await.unwrap().status(), StatusCode::OK);
        // Rejected: the host is already off. A refused command is not logged.
        assert_eq!(
            reset("ForceOff").await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(reset("On").await.unwrap().status(), StatusCode::OK);
        assert_eq!(
            integrated_event_log_messages(&router).await,
            [
                "Server reset.",
                "Server power removed.",
                "Server power restored."
            ]
        );

        // A power cycle is logged when it completes, not when it is accepted.
        assert_eq!(reset("PowerCycle").await.unwrap().status(), StatusCode::OK);
        assert_eq!(integrated_event_log_messages(&router).await.len(), 3);
        tokio::time::timeout(POWER_CYCLE_DELAY * 2, async {
            while integrated_event_log_messages(&router).await.len() < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("power cycle did not complete");
        assert_eq!(
            integrated_event_log_messages(&router).await.last().unwrap(),
            "Server reset."
        );
        stop.cancel();
        tasks.shutdown().await;
    }

    #[tokio::test]
    async fn standalone_bfb_installs_complete_through_backend_lifecycle() {
        for initially_off in [false, true] {
            let info = {
                let mut pool = crate::test_support::TEST_MAC_POOL.lock().unwrap();
                crate::MachineInfo::Dpu(crate::DpuMachineInfo::new(
                    HardwareType::DellPowerEdgeR750,
                    &mut pool,
                    crate::DpuSettings::default(),
                ))
            };
            let stop = CancellationToken::new();
            let (actor, callbacks) = SimulatedActor::new(stop.clone().drop_guard());
            let callbacks = Arc::new(callbacks);
            let (router, state) = machine_router(
                &info,
                callbacks.clone(),
                "standalone-dpu".into(),
                false,
                MachineRouterOptions {
                    firmware_upgrade_duration: Some(std::time::Duration::ZERO),
                    ..Default::default()
                },
            );
            let state = Arc::new(state);
            let mut tasks = JoinSet::new();
            actor.run(&state, &mut tasks, stop.clone());
            if initially_off {
                callbacks
                    .computer_system_reset(ResourceResetType::ForceOff)
                    .await
                    .unwrap();
            }
            for (image, expected) in [
                ("http://bmc/managed.bfb", crate::DpuOsImage::Managed),
                (
                    "http://bmc/preingestion.bfb",
                    crate::DpuOsImage::PreIngestion,
                ),
            ] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::post(
                            "/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate",
                        )
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({
                                "ImageURI": image,
                                "Targets": ["redfish/v1/UpdateService/FirmwareInventory/DPU_OS"],
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::ACCEPTED);
                let path = response.headers()["Location"].to_str().unwrap().to_owned();
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    loop {
                        let response = router
                            .clone()
                            .oneshot(Request::get(&path).body(Body::empty()).unwrap())
                            .await
                            .unwrap();
                        assert_eq!(response.status(), StatusCode::OK);
                        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        if body["TaskState"] == "Completed" {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("standalone BFB task never completed");
                assert_eq!(
                    state.update_service_state.dpu_os_status().unwrap().image,
                    Some(expected)
                );
                assert_eq!(
                    state.update_service_state.dpu_boot_progress(true).unwrap()["LastState"],
                    "OSRunning"
                );
            }
            assert_eq!(
                state
                    .update_service_state
                    .dpu_os_status()
                    .unwrap()
                    .generation,
                2
            );
            stop.cancel();
            tasks.join_next().await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn standalone_manager_reset_to_defaults_restores_factory_accounts() {
        let stop = CancellationToken::new();
        let (actor, callbacks) = SimulatedActor::new(stop.clone().drop_guard());
        let (router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(callbacks),
            "standalone-reset-defaults".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = Arc::new(state);
        let mut tasks = JoinSet::new();
        actor.run(&state, &mut tasks, stop.clone());
        let factory_accounts = state.account_service_state.accounts();
        let account_id = serde_json::to_value(state.persisted()).unwrap()["accounts"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            state
                .account_service_state
                .update_password(&account_id, "rotated")
                .await
                .unwrap()
        );
        assert_ne!(state.account_service_state.accounts(), factory_accounts);

        let target = format!(
            "{}/Actions/Manager.ResetToDefaults",
            state.manager.primary_odata_id().unwrap()
        );
        let response = router
            .clone()
            .oneshot(
                Request::post(&target)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"ResetType":"ResetAll"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.account_service_state.accounts(), factory_accounts);
        assert_eq!(state.factory_reset_generation(), 1);

        stop.cancel();
        tasks.join_next().await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn standalone_backend_does_not_keep_its_owner_alive() {
        let stop = CancellationToken::new();
        let (actor, callbacks) = SimulatedActor::new(stop.clone().drop_guard());
        let (router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(callbacks),
            "drop-owner".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = Arc::new(state);
        let weak = Arc::downgrade(&state);
        let mut tasks = JoinSet::new();
        actor.run(&state, &mut tasks, stop.clone());
        drop(router);
        drop(state);
        assert!(weak.upgrade().is_none());
        tokio::time::timeout(std::time::Duration::from_secs(2), tasks.join_next())
            .await
            .expect("dropping the owner did not stop the backend")
            .unwrap()
            .unwrap();
        assert!(stop.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn power_cycles_publish_off_until_completion_and_reject_resets() {
        let stop = CancellationToken::new();
        let (actor, callbacks) = SimulatedActor::new(stop.clone().drop_guard());
        let callbacks = Arc::new(callbacks);
        let (router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            callbacks.clone(),
            "simulated".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = Arc::new(state);
        let mut tasks = JoinSet::new();
        actor.run(&state, &mut tasks, stop.clone());
        for cycle in [
            ResourceResetType::PowerCycle,
            ResourceResetType::FullPowerCycle,
        ] {
            callbacks.computer_system_reset(cycle).await.unwrap();
            for (delay, expected) in [(0, "Off"), (4, "Off"), (1, "On")] {
                tokio::time::advance(std::time::Duration::from_secs(delay)).await;
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    loop {
                        let response = router
                            .clone()
                            .oneshot(
                                Request::builder()
                                    .uri("/redfish/v1/Systems/System.Embedded.1")
                                    .body(Body::empty())
                                    .unwrap(),
                            )
                            .await
                            .unwrap();
                        assert_eq!(response.status(), StatusCode::OK);
                        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        if body["PowerState"] == expected {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("simulated power state did not converge");
                if expected == "Off" {
                    assert!(matches!(
                        callbacks.computer_system_reset(ResourceResetType::On).await,
                        Err(ActionError::BadRequest(_))
                    ));
                    assert!(matches!(
                        callbacks.computer_system_reset(cycle).await,
                        Err(ActionError::BadRequest(_))
                    ));
                }
            }
        }
        callbacks
            .computer_system_reset(ResourceResetType::ForceOff)
            .await
            .unwrap();
        callbacks
            .computer_system_reset(ResourceResetType::On)
            .await
            .unwrap();
        stop.cancel();
        tasks.join_next().await.unwrap().unwrap();
        assert!(matches!(
            callbacks
                .computer_system_reset(ResourceResetType::ForceOff)
                .await,
            Err(ActionError::Internal(_))
        ));
    }
}
