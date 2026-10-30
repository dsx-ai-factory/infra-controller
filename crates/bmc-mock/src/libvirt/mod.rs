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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use nv_redfish::schema::resource::PowerState;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::actor::{Actor, ActorCallbacks, ActorMailbox, ActorResult, AlarmId};
use crate::persistence::PersistenceError;
use crate::redfish::computer_system::{SingleSystemState, SystemState};
use crate::{
    ActionError, BmcEvent, BmcState, BootOptionKind, Callbacks, ResourceResetType,
    VirtualMediaUpdate, validate_power_reset,
};

mod commands;
mod device_xml;
mod domain_xml;
mod media_lifecycle;
mod persistence;
mod virsh;
mod xml;
use commands::{DomainConfiguration, VirshCommand};
use media_lifecycle::MediaLifecycle;
use persistence::StateFile;
use virsh::Virsh;

/// Maximum duration of one virsh attempt, including output collection.
const VIRSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Additional grace period for reaping after a kill request, independent of the command deadline.
const VIRSH_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Per power request, allow one five-second poll delay, a 30-second virsh
/// attempt and five seconds of cleanup to obtain the first recognized state.
/// Expiry fails before enqueueing a power command. Backend polling continues.
const INITIALIZATION_WAIT: Duration = Duration::from_secs(40);

/// Delay after each periodic observation; the actor retains at most one polling alarm.
// Unit tests exercise polling and recovery without waiting for the production cadence.
const POWER_POLL_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(5)
};
const PERSISTENCE_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct PowerObservation {
    state: PowerState,
    cycling: bool,
}

pub struct LibvirtActor {
    actor: Actor<LibvirtMessage>,
    mailbox: ActorMailbox<LibvirtMessage>,
    config: Config,
    refresh_pending: Arc<AtomicBool>,
    power_state: Arc<RwLock<PowerObservation>>,
    initialized: watch::Sender<bool>,
}

impl LibvirtActor {
    /// Creates an unstarted libvirt actor and its callbacks.
    /// Call `run` to initialize the backend and spawn it in the owner's supervised task set.
    pub fn new(config: Config, guard: DropGuard) -> (Self, LibvirtCallbacks) {
        let refresh_pending = Arc::new(AtomicBool::new(false));
        let power_state = Arc::new(RwLock::new(PowerObservation {
            state: PowerState::Off,
            cycling: false,
        }));
        let (actor, mailbox) = Actor::new();
        let (initialized, initialization) = watch::channel(false);
        (
            LibvirtActor {
                actor,
                mailbox: mailbox.clone(),
                config,
                refresh_pending: refresh_pending.clone(),
                power_state: power_state.clone(),
                initialized,
            },
            LibvirtCallbacks {
                mailbox,
                refresh_pending,
                power_state,
                initialized: initialization,
                _stop: guard,
            },
        )
    }

    /// Loads saved state and runs the supervised backend. The owner retains the
    /// state Arc for the serving lifetime. Lifecycle publication uses only Weak
    /// access, so it cannot keep the callbacks and backend alive.
    pub async fn run(
        self,
        bmc_state: &Arc<BmcState<LibvirtCallbacks>>,
        tasks: &mut JoinSet<()>,
        stop: CancellationToken,
    ) -> eyre::Result<()> {
        let persistence = self
            .config
            .state_file
            .as_ref()
            .map(|path| StateFile::load(path.clone(), bmc_state))
            .transpose()?;
        let mut backend = LibvirtBackend {
            persistence,
            persistence_retry: None,
            config: self.config,
            restore_boot_after_power_on: false,
            system_state: None,
            bmc_state: Arc::downgrade(bmc_state),
            last_observed_power: None,
            boot_pending: None,
            initialized: self.initialized,
            applied_state: AppliedState::default(),
            refresh_pending: self.refresh_pending,
            power_state: self.power_state,
        };
        backend.init(bmc_state.system_state.clone()).await?;
        backend.save_state()?;
        self.mailbox
            .send_at(
                (Instant::now() + POWER_POLL_INTERVAL).into(),
                LibvirtMessage::PollPower,
            )
            .expect("unstarted actor mailbox must be open");
        let fut = self.actor.run(backend);
        tasks.spawn(async move {
            stop.run_until_cancelled(fut).await;
        });
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Optional standalone BMC snapshot file. Loaded before actor startup and saved on Refresh.
    pub state_file: Option<PathBuf>,
    pub virsh_path: PathBuf,
    pub uri: String,
    pub domain: String,
    pub virtual_media_targets: BTreeMap<String, String>,
}

/// Backend handle that sends operations to one sequential libvirt actor.
///
/// Commands use an unbounded mailbox. Refresh notifications coalesce into one pending signal.
/// Power observations are published to Redfish, initially Off, updated at actor startup,
/// after power commands and refresh notifications, and by polling with a five-second
/// delay after each attempt. Actor work can delay polling; each virsh attempt has a 30-second
/// deadline and up to five seconds of cleanup. Failed or unrecognized observations retain the last state.
/// Power commands wait up to 40 seconds for the first recognized domain observation
/// before validation, then return once enqueued. Expiry returns an internal error
/// without enqueueing. Backend exit closes the readiness channel, and the caller
/// can cancel its wait. A later request can succeed after an observation recovers.
/// Execution failures are logged by the actor.
/// Virtual-media operations await execution and publish Redfish contents only on success.
/// Resets are rejected while a power-cycle command is executing.
/// Dropping the handle cancels the actor. Cancelling `LibvirtActor::run` during
/// initialization interrupts it before the actor task is spawned.
#[derive(Debug)]
pub struct LibvirtCallbacks {
    mailbox: ActorMailbox<LibvirtMessage>,
    refresh_pending: Arc<AtomicBool>,
    power_state: Arc<RwLock<PowerObservation>>,
    initialized: watch::Receiver<bool>,
    _stop: DropGuard,
}

#[derive(Debug)]
enum LibvirtMessage {
    PollPower,
    SendPowerCommand {
        reset_type: ResourceResetType,
    },
    SetVirtualMedia {
        update: VirtualMediaUpdate,
        reply: oneshot::Sender<Result<(), ActionError>>,
    },
    Refresh,
    PersistState,
    Stop(oneshot::Sender<Result<(), PersistenceError>>),
}

struct LibvirtBackend {
    persistence: Option<StateFile>,
    persistence_retry: Option<AlarmId>,
    config: Config,
    restore_boot_after_power_on: bool,
    system_state: Option<Weak<SystemState<LibvirtCallbacks>>>,
    bmc_state: Weak<BmcState<LibvirtCallbacks>>,
    last_observed_power: Option<PowerState>,
    boot_pending: Option<BootKind>,
    initialized: watch::Sender<bool>,
    applied_state: AppliedState,
    refresh_pending: Arc<AtomicBool>,
    power_state: Arc<RwLock<PowerObservation>>,
}

#[derive(Clone, Copy)]
enum BootKind {
    Warm,
    Cold,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AppliedState {
    persistent_boot_selection: Option<BootOptionKind>,
    boot_source_override: serde_json::Value,
}

impl LibvirtBackend {
    fn save_state(&mut self) -> Result<(), PersistenceError> {
        if let Some(persistence) = &mut self.persistence {
            persistence.save()?;
        }
        Ok(())
    }

    fn persist(&mut self, mailbox: &ActorMailbox<LibvirtMessage>) {
        if let Some(alarm) = self.persistence_retry.take() {
            mailbox.cancel(alarm);
        }
        if let Err(error) = self.save_state() {
            tracing::warn!(error = %error, "BMC state save failed; retrying");
            self.persistence_retry = Some(
                mailbox
                    .send_at(
                        (Instant::now() + PERSISTENCE_RETRY_INTERVAL).into(),
                        LibvirtMessage::PersistState,
                    )
                    .expect("running actor mailbox must be open"),
            );
        }
    }

    async fn init(&mut self, state: Arc<SystemState<LibvirtCallbacks>>) -> eyre::Result<()> {
        let controlled_system = state.controlled_system().ok_or(eyre::eyre!(
            "libvirt backend has no controlled ComputerSystem"
        ))?;
        let applied = AppliedState::from(controlled_system);
        self.apply_persistent_boot_selection(applied.persistent_boot_selection)
            .await?;
        if let Some(media) = controlled_system.virtual_media() {
            let lifecycle = MediaLifecycle::new(self.virsh(), &self.config.virtual_media_targets);
            for device_id in media.device_ids() {
                lifecycle.ensure_device(device_id).await?;
            }
        }
        self.system_state = Some(Arc::downgrade(&state));
        self.applied_state = applied;
        self.publish_power_state(PowerState::Off);
        self.refresh_power_state().await;
        Ok(())
    }

    fn virsh(&self) -> Virsh<'_> {
        Virsh {
            path: &self.config.virsh_path,
            uri: &self.config.uri,
            domain: &self.config.domain,
        }
    }

    async fn restore_boot_after_start(&mut self) -> eyre::Result<()> {
        if self.restore_boot_after_power_on {
            self.restore_persistent_boot_order().await?;
            self.restore_boot_after_power_on = false;
        }
        Ok(())
    }

    async fn restore_persistent_boot_order(&self) -> eyre::Result<()> {
        let selection = self
            .system_state
            .as_ref()
            .and_then(Weak::upgrade)
            .and_then(|state| {
                state
                    .controlled_system()
                    .and_then(SingleSystemState::resolve_persistent_boot_selection)
            });
        self.apply_persistent_boot_selection(selection).await
    }

    async fn reapply_effective_boot_order(&mut self) -> Result<(), ActionError> {
        let Some(state) = self.system_state.as_ref().and_then(Weak::upgrade) else {
            return Err(ActionError::Internal(eyre::eyre!(
                "libvirt backend is not bound to BMC mock state"
            )));
        };
        let Some(system) = state.controlled_system() else {
            return Err(ActionError::Internal(eyre::eyre!(
                "BMC mock state has no controlled ComputerSystem"
            )));
        };
        let boot_source_override = system.boot_source_override();
        if boot_source_override_is_active(&boot_source_override) {
            self.apply_boot_source_override(&boot_source_override).await
        } else {
            self.apply_persistent_boot_selection(system.resolve_persistent_boot_selection())
                .await
                .map_err(ActionError::Internal)
        }
    }

    async fn apply_persistent_boot_selection(
        &self,
        selection: Option<BootOptionKind>,
    ) -> eyre::Result<()> {
        match selection {
            Some(BootOptionKind::Disk) => self.apply_boot_devices(&["hd"]).await,
            Some(BootOptionKind::Network) => self.apply_boot_devices(&["network", "hd"]).await,
            None => Ok(()),
        }
    }

    async fn apply_boot_devices(&self, devices: &[&str]) -> eyre::Result<()> {
        let domain = self
            .virsh()
            .dumpxml(DomainConfiguration::Persistent)
            .await?;
        let command = VirshCommand::boot_order(domain, devices)?;
        self.virsh().execute(command).await.map(drop)
    }

    async fn apply_boot_source_override(
        &mut self,
        boot_source_override: &serde_json::Value,
    ) -> Result<(), ActionError> {
        let enabled = boot_source_override
            .get("BootSourceOverrideEnabled")
            .and_then(serde_json::Value::as_str);
        let target = boot_source_override
            .get("BootSourceOverrideTarget")
            .and_then(serde_json::Value::as_str);
        let devices = match (enabled, target) {
            (Some("Disabled"), _) | (_, Some("None")) => &["hd"][..],
            (_, Some("Cd")) => &["cdrom", "hd"][..],
            (_, Some("Hdd")) => &["hd"][..],
            (_, Some("Pxe" | "UefiHttp")) => &["network", "hd"][..],
            (_, Some(target)) => {
                return Err(ActionError::BadRequest(eyre::eyre!(
                    "unsupported boot source override target: {target}"
                )));
            }
            (_, None) => return Ok(()),
        };
        self.apply_boot_devices(devices)
            .await
            .map_err(ActionError::Internal)?;
        self.restore_boot_after_power_on = enabled == Some("Once");
        Ok(())
    }

    async fn reconcile_state(&mut self, desired: AppliedState) -> Result<(), String> {
        let desired_override_active = boot_source_override_is_active(&desired.boot_source_override);
        if desired.boot_source_override != self.applied_state.boot_source_override
            && desired_override_active
        {
            self.apply_boot_source_override(&desired.boot_source_override)
                .await
                .map_err(|error| error.to_string())?;
        } else if !desired_override_active
            && (desired.boot_source_override != self.applied_state.boot_source_override
                || desired.persistent_boot_selection
                    != self.applied_state.persistent_boot_selection)
        {
            self.apply_persistent_boot_selection(desired.persistent_boot_selection)
                .await
                .map_err(|error| error.to_string())?;
        }
        self.applied_state.boot_source_override = desired.boot_source_override;
        self.applied_state.persistent_boot_selection = desired.persistent_boot_selection;
        Ok(())
    }
}

impl<C: Callbacks> From<&SingleSystemState<C>> for AppliedState {
    fn from(system: &SingleSystemState<C>) -> Self {
        Self {
            persistent_boot_selection: system.resolve_persistent_boot_selection(),
            boot_source_override: system.boot_source_override(),
        }
    }
}

fn boot_source_override_is_active(boot_source_override: &serde_json::Value) -> bool {
    let enabled = boot_source_override
        .get("BootSourceOverrideEnabled")
        .and_then(serde_json::Value::as_str);
    let target = boot_source_override
        .get("BootSourceOverrideTarget")
        .and_then(serde_json::Value::as_str);
    enabled != Some("Disabled") && !matches!(target, None | Some("None"))
}

impl LibvirtBackend {
    async fn observe_power_state(&self) -> Option<PowerState> {
        match self.virsh().execute(VirshCommand::DomainState).await {
            Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
                "running" | "idle" | "blocked" | "paused" | "in shutdown" | "pmsuspended" => {
                    Some(PowerState::On)
                }
                "shut off" | "crashed" => Some(PowerState::Off),
                state => {
                    tracing::warn!(domain = %self.config.domain, state, "unrecognized libvirt domain power state");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(
                    domain = %self.config.domain,
                    error = ?error,
                    "could not read libvirt domain power state",
                );
                None
            }
        }
    }

    fn publish_power_state(&self, state: PowerState) {
        self.power_state
            .write()
            .expect("power state lock poisoned")
            .state = state;
        if let Some(system_state) = self.system_state.as_ref().and_then(Weak::upgrade) {
            system_state.set_power_state(state);
        }
    }

    async fn refresh_power_state(&mut self) {
        if let Some(observed) = self.observe_power_state().await {
            // Synthetic Off during a cycle is not an observed edge. Initial On
            // seeds state without inventing a boot for a restored BFB task.
            let boot = if observed == PowerState::On {
                self.boot_pending.take().or_else(|| {
                    (self.last_observed_power == Some(PowerState::Off)).then_some(BootKind::Cold)
                })
            } else {
                None
            };
            self.last_observed_power = Some(observed);
            self.publish_power_state(observed);
            self.initialized.send_replace(true);
            if let Some(boot) = boot {
                let used_saved_boot_configuration = matches!(boot, BootKind::Cold);
                if let Some(state) = self.bmc_state.upgrade() {
                    state.on_event(&BmcEvent::PowerOn);
                    // Domain On is the standalone backend's boot-completion
                    // inference, as for one-time boot-policy restoration.
                    state.on_boot_completed(used_saved_boot_configuration);
                }
                // Publish this boot before yielding to XML cleanup. A later
                // upload admitted during cleanup must wait for its own boot.
                if used_saved_boot_configuration
                    && let Err(error) = self.restore_boot_after_start().await
                {
                    tracing::error!(domain = %self.config.domain, %error, "could not restore persistent boot order");
                }
            }
        }
    }

    async fn send_power_command(
        &mut self,
        reset_type: ResourceResetType,
    ) -> Result<(), ActionError> {
        use ResourceResetType::*;
        // Only a cold start loads the saved domain XML. Reboot and reset keep
        // the running domain's boot configuration and their existing semantics.
        if matches!(reset_type, On | ForceOn | PowerCycle | FullPowerCycle) {
            self.reapply_effective_boot_order().await?;
        }
        for command in VirshCommand::power(reset_type)? {
            let boot = match command {
                VirshCommand::Start => Some(BootKind::Cold),
                VirshCommand::Reset | VirshCommand::Reboot => Some(BootKind::Warm),
                _ => None,
            };
            self.virsh()
                .execute(command)
                .await
                .map_err(ActionError::Internal)?;
            // The boot happened even if later boot-policy cleanup fails.
            // Only confirmed On consumes this marker and publishes lifecycle.
            if boot.is_some() {
                self.boot_pending = boot;
            }
        }
        Ok(())
    }

    async fn refresh(&mut self) {
        let Some(system_state) = self.system_state.as_ref().and_then(Weak::upgrade) else {
            tracing::error!(
                domain = %self.config.domain,
                "libvirt backend is not bound to BMC mock state",
            );
            return;
        };
        let Some(controlled_system) = system_state.controlled_system() else {
            tracing::error!(
                domain = %self.config.domain,
                "BMC mock state has no controlled ComputerSystem",
            );
            return;
        };
        if let Err(error) = self
            .reconcile_state(AppliedState::from(controlled_system))
            .await
        {
            tracing::error!(
                domain = %self.config.domain,
                error = %error,
                "could not reconcile libvirt domain with BMC mock state",
            );
        }
    }
}

impl ActorCallbacks<LibvirtMessage> for LibvirtBackend {
    async fn message(
        &mut self,
        mailbox: &ActorMailbox<LibvirtMessage>,
        message: LibvirtMessage,
    ) -> ActorResult {
        match message {
            LibvirtMessage::SetVirtualMedia { update, reply } => {
                let media = MediaLifecycle::new(self.virsh(), &self.config.virtual_media_targets);
                let result = media.apply(&update.device_id, &update.contents).await;
                if result.is_ok() {
                    update.commit();
                }
                // Publication belongs to the actor even if the HTTP caller has gone away.
                reply.send(result).ok();
                ActorResult::Noop
            }
            LibvirtMessage::Stop(reply) => {
                reply.send(self.save_state()).ok();
                ActorResult::Stop
            }
            LibvirtMessage::PersistState => {
                self.persistence_retry = None;
                self.persist(mailbox);
                ActorResult::Noop
            }

            LibvirtMessage::PollPower => {
                self.refresh_power_state().await;
                mailbox
                    .send_at(
                        (Instant::now() + POWER_POLL_INTERVAL).into(),
                        LibvirtMessage::PollPower,
                    )
                    .expect("running actor mailbox must be open");
                ActorResult::Noop
            }
            LibvirtMessage::SendPowerCommand { reset_type } => {
                if matches!(reset_type, ResourceResetType::PowerCycle) {
                    self.power_state
                        .write()
                        .expect("power state lock poisoned")
                        .cycling = true;
                    self.publish_power_state(PowerState::Off);
                }
                if let Err(error) = self.send_power_command(reset_type).await {
                    tracing::error!(domain = %self.config.domain, ?reset_type, %error, "libvirt power command failed");
                }
                self.refresh_power_state().await;
                self.power_state
                    .write()
                    .expect("power state lock poisoned")
                    .cycling = false;
                ActorResult::Noop
            }
            LibvirtMessage::Refresh => {
                // Clear before reading state so changes during reconciliation queue another refresh.
                self.refresh_pending.store(false, Ordering::SeqCst);
                self.persist(mailbox);
                self.refresh().await;
                self.refresh_power_state().await;
                ActorResult::Noop
            }
        }
    }
}

impl LibvirtCallbacks {
    /// Flushes the latest BMC state and stops the actor after the application stops HTTP requests.
    /// The owner must still join its supervised task. A failed final save is returned to the owner.
    pub async fn finish(&self) -> eyre::Result<()> {
        let (reply, result) = oneshot::channel();
        self.mailbox.send(LibvirtMessage::Stop(reply))?;
        result.await??;
        Ok(())
    }
}

impl Callbacks for LibvirtCallbacks {
    async fn set_virtual_media(&self, update: VirtualMediaUpdate) -> Result<(), ActionError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(LibvirtMessage::SetVirtualMedia { update, reply })
            .map_err(|error| ActionError::Internal(error.into()))?;
        response
            .await
            .map_err(|error| ActionError::Internal(error.into()))?
    }

    async fn computer_system_reset(
        &self,
        reset_type: ResourceResetType,
    ) -> Result<(), ActionError> {
        {
            let mut initialized = self.initialized.clone();
            drop(
                tokio::time::timeout(INITIALIZATION_WAIT, initialized.wait_for(|ready| *ready))
                    .await
                    .map_err(|error| {
                        ActionError::Internal(
                            eyre::Report::new(error).wrap_err(
                                "timed out waiting for the first libvirt power observation",
                            ),
                        )
                    })?
                    .map_err(|error| ActionError::Internal(error.into()))?,
            );
        }
        {
            let observation = self.power_state.read().expect("power state lock poisoned");
            if observation.cycling {
                return Err(ActionError::BadRequest(eyre::eyre!(
                    "libvirt backend is in the middle of power cycling"
                )));
            }
            validate_power_reset(observation.state, reset_type)?;
        }
        self.mailbox
            .send(LibvirtMessage::SendPowerCommand { reset_type })
            .map_err(|err| ActionError::Internal(err.into()))
    }

    fn state_refresh_indication(&self) {
        if !self.refresh_pending.swap(true, Ordering::SeqCst)
            && let Err(error) = self.mailbox.send(LibvirtMessage::Refresh)
        {
            self.refresh_pending.store(false, Ordering::SeqCst);
            tracing::warn!(%error, "could not notify libvirt actor of changed BMC state");
        }
    }
}

#[cfg(test)]
mod tests;
