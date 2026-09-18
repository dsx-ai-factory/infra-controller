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
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nv_redfish::schema::computer_system::{BootSource, BootSourceOverrideEnabled};
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, DropGuard};
use url::Url;

use crate::actor::{Actor, ActorCallbacks, ActorMailbox, ActorResult};
use crate::boot::{BootSourceOverride, BootState};
use crate::{
    BmcState, BootConfigPatch, BootOptionKind, CallbackError, Callbacks, MockPowerState,
    SetSystemPowerError, SystemPowerControl, SystemStateData, VirtualMediaState,
};

#[derive(Debug, thiserror::Error)]
enum VirtualMediaError {
    #[error("invalid virtual media request: {0}")]
    BadRequest(String),
    #[error("virtual media command failed: {0}")]
    Command(String),
}

type VirtualMediaResult = Result<(), VirtualMediaError>;

impl From<VirtualMediaError> for CallbackError {
    fn from(error: VirtualMediaError) -> Self {
        match error {
            VirtualMediaError::BadRequest(message) => Self::BadRequest(message),
            error => Self::InternalError(error.into()),
        }
    }
}

impl From<SetSystemPowerError> for CallbackError {
    fn from(error: SetSystemPowerError) -> Self {
        match error {
            SetSystemPowerError::BadRequest(message) => Self::BadRequest(message),
            error => Self::InternalError(error.into()),
        }
    }
}

/// Maximum duration of one virsh attempt, including output collection.
const VIRSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Additional grace period for reaping after a kill request, independent of the command deadline.
const VIRSH_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Config {
    pub virsh_path: PathBuf,
    pub uri: String,
    pub domain: String,
    pub virtual_media_targets: BTreeMap<String, String>,
}

/// Backend handle that sends operations to one sequential libvirt actor.
///
/// Commands use an unbounded mailbox. Refresh notifications coalesce into one pending signal.
/// Power reads return the last observation, initially Off, updated at actor startup
/// and after power commands, binding, and refresh notifications. Power commands
/// return once enqueued; execution failures are logged by the actor.
/// Boot and media requests read or update actor-owned desired state. A write reply
/// acknowledges the state change, not virsh I/O. Refresh reconciles that state
/// and logs execution failures.
/// Dropping the handle cancels the actor. Once binding starts, it finishes even if
/// its caller stops awaiting the reply, to avoid interrupting XML updates.
#[derive(Debug)]
pub struct LibvirtCallbacks {
    mailbox: ActorMailbox<LibvirtMessage>,
    refresh_pending: Arc<AtomicBool>,
    power_state: Arc<RwLock<MockPowerState>>,
    _stop: DropGuard,
}

#[derive(Debug)]
enum LibvirtMessage {
    Run,
    InitializeState {
        system_id: String,
        state: SystemStateData,
    },
    Bind {
        system_id: String,
        reply: oneshot::Sender<Result<(), CallbackError>>,
    },
    SendPowerCommand {
        reset_type: SystemPowerControl,
    },
    SetBootConfig {
        system_id: String,
        patch: BootConfigPatch,
        reply: oneshot::Sender<Result<(), CallbackError>>,
    },
    SetVirtualMedia {
        system_id: String,
        state: VirtualMediaState,
        reply: oneshot::Sender<Result<(), CallbackError>>,
    },
    GetSystemState {
        system_id: String,
        reply: oneshot::Sender<Result<SystemStateData, CallbackError>>,
    },
    Refresh,
}

struct LibvirtBackend {
    config: Config,
    restore_boot_after_power_on: bool,
    system_states: BTreeMap<String, SystemStateData>,
    controlled_system: Option<String>,
    applied_state: AppliedState,
    refresh_pending: Arc<AtomicBool>,
    power_state: Arc<RwLock<MockPowerState>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AppliedState {
    persistent_boot_selection: Option<BootOptionKind>,
    boot_source_override: BootSourceOverride,
    virtual_media: BTreeMap<String, VirtualMediaState>,
}

impl LibvirtCallbacks {
    /// Starts a libvirt actor in the owner's supervised task set.
    /// The owner must observe task failures and shut down the set when stopping the BMC.
    pub fn new(config: Config, tasks: &mut JoinSet<()>) -> Self {
        let refresh_pending = Arc::new(AtomicBool::new(false));
        let power_state = Arc::new(RwLock::new(MockPowerState::Off));
        let (actor, mailbox) = Actor::new(
            LibvirtBackend {
                config,
                restore_boot_after_power_on: false,
                system_states: BTreeMap::new(),
                controlled_system: None,
                applied_state: AppliedState::default(),
                refresh_pending: refresh_pending.clone(),
                power_state: power_state.clone(),
            },
            LibvirtMessage::Run,
        );
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        tasks.spawn(async move {
            stop.run_until_cancelled(actor.run()).await;
        });
        Self {
            mailbox,
            refresh_pending,
            power_state,
            _stop: guard,
        }
    }

    /// Binds this backend to the generated BMC state and applies its initial
    /// persistent boot selection to the inactive libvirt domain XML.
    ///
    /// Binding succeeds at most once. An initial boot-selection failure leaves
    /// the backend unbound so the caller can retry.
    ///
    /// # Errors
    ///
    /// Returns an error when the BMC has no controlled `ComputerSystem`, this
    /// backend is already bound, or libvirt cannot apply the initial selection.
    pub async fn bind_state(&self, state: &BmcState<Self>) -> Result<(), CallbackError> {
        let system = state
            .system_state
            .controlled_system()
            .ok_or_else(|| eyre::eyre!("libvirt backend has no controlled ComputerSystem"))?;
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(LibvirtMessage::Bind {
                system_id: system.id().to_string(),
                reply,
            })
            .map_err(eyre::Error::from)?;
        response.await.map_err(eyre::Error::from)?
    }
}

impl LibvirtBackend {
    fn controlled_state(&self) -> Option<&SystemStateData> {
        self.controlled_system
            .as_ref()
            .and_then(|id| self.system_states.get(id))
    }

    async fn bind_state(&mut self, system_id: String) -> Result<(), CallbackError> {
        if self.controlled_system.is_some() {
            return Err(eyre::eyre!("libvirt backend state is already bound").into());
        }
        let state = self
            .system_states
            .get(&system_id)
            .ok_or_else(|| CallbackError::SystemNotInitialized(system_id.clone()))?;
        let applied = AppliedState::from(state);
        self.set_persistent_boot_selection(applied.persistent_boot_selection)
            .await?;
        self.controlled_system = Some(system_id);
        self.applied_state = applied;
        Ok(())
    }

    async fn virsh_output(&self, arguments: &[&str]) -> Result<Output, String> {
        let command = self.config.virsh_path.display().to_string();
        let mut child = Command::new(&self.config.virsh_path)
            .arg("--connect")
            .arg(&self.config.uri)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("could not execute {command}: {error}"))?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        // Drain both pipes while waiting: a full pipe must not deadlock the child.
        // The deadline includes output collection as well as process execution.
        let completion = tokio::time::timeout(VIRSH_COMMAND_TIMEOUT, async {
            tokio::try_join!(
                child.wait(),
                stdout.read_to_end(&mut stdout_bytes),
                stderr.read_to_end(&mut stderr_bytes),
            )
        })
        .await;
        match completion {
            Ok(Ok((status, _, _))) => Ok(Output {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            }),
            failed => {
                child
                    .start_kill()
                    .map_err(|error| format!("could not stop {command}: {error}"))?;
                // On timeout, returning drops Child and leaves reaping to Tokio's
                // best-effort cleanup so the actor can keep processing.
                tokio::time::timeout(VIRSH_CLEANUP_TIMEOUT, child.wait())
                    .await
                    .map_err(|_| {
                        format!("could not reap {command} within {VIRSH_CLEANUP_TIMEOUT:?} after kill request")
                    })?
                    .map_err(|error| format!("could not reap {command}: {error}"))?;
                Err(match failed {
                    Err(_) => format!("{command} timed out after {VIRSH_COMMAND_TIMEOUT:?}"),
                    Ok(Err(error)) => format!("could not collect {command} output: {error}"),
                    Ok(Ok(_)) => unreachable!(),
                })
            }
        }
    }

    async fn virsh(&self, arguments: &[&str]) -> Result<Output, String> {
        let output = self.virsh_output(arguments).await?;
        if output.status.success() {
            return Ok(output);
        }
        Err(format!(
            "{} exited with {}: {}",
            self.config.virsh_path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    async fn domain_command(&self, command: &str) -> Result<(), SetSystemPowerError> {
        self.virsh(&[command, &self.config.domain])
            .await
            .map(drop)
            .map_err(SetSystemPowerError::CommandSendError)
    }

    async fn start(&mut self) -> Result<(), SetSystemPowerError> {
        self.domain_command("start").await?;
        let restore_boot = std::mem::take(&mut self.restore_boot_after_power_on);
        if restore_boot {
            for state in self.system_states.values_mut() {
                state.boot.on_boot_completed();
            }
            self.restore_persistent_boot_order().await?;
        }
        Ok(())
    }

    async fn restore_persistent_boot_order(&self) -> Result<(), SetSystemPowerError> {
        let selection = self
            .controlled_state()
            .and_then(|state| state.boot.persistent_selection());
        self.set_persistent_boot_selection(selection).await
    }

    async fn reapply_effective_boot_order(&mut self) -> Result<(), SetSystemPowerError> {
        let boot = self
            .controlled_state()
            .ok_or_else(|| {
                SetSystemPowerError::CommandSendError(
                    "libvirt backend is not bound to BMC mock state".to_string(),
                )
            })?
            .boot
            .clone();
        self.apply_boot_config(&boot).await
    }

    async fn set_persistent_boot_selection(
        &self,
        selection: Option<BootOptionKind>,
    ) -> Result<(), SetSystemPowerError> {
        match selection {
            Some(BootOptionKind::Disk) => self.set_boot_devices(&["hd"]).await,
            Some(BootOptionKind::Network) => self.set_boot_devices(&["network", "hd"]).await,
            None => Ok(()),
        }
    }

    async fn set_boot_devices(&self, devices: &[&str]) -> Result<(), SetSystemPowerError> {
        let output = self
            .virsh(&["dumpxml", "--inactive", &self.config.domain])
            .await
            .map_err(SetSystemPowerError::CommandSendError)?;
        let xml = String::from_utf8(output.stdout).map_err(|error| {
            SetSystemPowerError::CommandSendError(format!(
                "virsh dumpxml returned invalid UTF-8: {error}"
            ))
        })?;
        let xml =
            set_boot_order_xml(&xml, devices).map_err(SetSystemPowerError::CommandSendError)?;
        let file = tempfile::NamedTempFile::new().map_err(|error| {
            SetSystemPowerError::CommandSendError(format!(
                "could not create temporary domain XML: {error}"
            ))
        })?;
        tokio::fs::write(file.path(), xml.as_bytes())
            .await
            .map_err(|error| {
                SetSystemPowerError::CommandSendError(format!(
                    "could not write temporary domain XML: {error}"
                ))
            })?;
        self.virsh(&["define", file.path().to_string_lossy().as_ref()])
            .await
            .map(drop)
            .map_err(SetSystemPowerError::CommandSendError)
    }

    fn target_for_device(&self, device_id: &str) -> Result<&str, VirtualMediaError> {
        self.config
            .virtual_media_targets
            .get(device_id)
            .map(String::as_str)
            .ok_or_else(|| {
                VirtualMediaError::BadRequest(format!(
                    "virtual media device {device_id} has no libvirt target"
                ))
            })
    }

    async fn target_is_attached(&self, target: &str) -> Result<bool, VirtualMediaError> {
        let output = self
            .virsh(&["domblklist", "--details", &self.config.domain])
            .await
            .map_err(VirtualMediaError::Command)?;
        let output = String::from_utf8(output.stdout).map_err(|error| {
            VirtualMediaError::Command(format!("virsh domblklist returned invalid UTF-8: {error}"))
        })?;
        Ok(output.lines().any(|line| {
            line.split_whitespace()
                .nth(2)
                .is_some_and(|value| value == target)
        }))
    }

    async fn detach_target(&self, target: &str) -> VirtualMediaResult {
        if !self.target_is_attached(target).await? {
            return Ok(());
        }
        self.virsh(&["detach-disk", &self.config.domain, target, "--persistent"])
            .await
            .map(drop)
            .map_err(VirtualMediaError::Command)
    }

    async fn set_boot_source_override(
        &mut self,
        boot_source_override: &BootSourceOverride,
    ) -> Result<(), SetSystemPowerError> {
        let enabled = boot_source_override.enabled;
        let target = boot_source_override.target;
        let devices = match (enabled, target) {
            (Some(BootSourceOverrideEnabled::Disabled), _) | (_, Some(BootSource::None)) => {
                &["hd"][..]
            }
            (_, Some(BootSource::Cd)) => &["cdrom", "hd"][..],
            (_, Some(BootSource::Hdd)) => &["hd"][..],
            (_, Some(BootSource::Pxe | BootSource::UefiHttp)) => &["network", "hd"][..],
            (_, Some(target)) => {
                return Err(SetSystemPowerError::BadRequest(format!(
                    "unsupported boot source override target: {target:?}"
                )));
            }
            (_, None) => return Ok(()),
        };
        self.set_boot_devices(devices).await?;
        self.restore_boot_after_power_on = enabled == Some(BootSourceOverrideEnabled::Once);
        Ok(())
    }

    async fn insert_virtual_media(
        &self,
        device_id: &str,
        image: &str,
        write_protected: bool,
    ) -> VirtualMediaResult {
        let target = self.target_for_device(device_id)?;
        self.detach_target(target).await?;
        let xml = virtual_media_xml(device_id, target, image, write_protected)?;
        let file = tempfile::NamedTempFile::new().map_err(|error| {
            VirtualMediaError::Command(format!("could not create temporary device XML: {error}"))
        })?;
        tokio::fs::write(file.path(), xml.as_bytes())
            .await
            .map_err(|error| {
                VirtualMediaError::Command(format!("could not write temporary device XML: {error}"))
            })?;
        self.virsh(&[
            "attach-device",
            &self.config.domain,
            file.path().to_string_lossy().as_ref(),
            "--persistent",
        ])
        .await
        .map(drop)
        .map_err(VirtualMediaError::Command)
    }

    async fn eject_virtual_media(&self, device_id: &str) -> VirtualMediaResult {
        let target = self.target_for_device(device_id)?;
        self.detach_target(target).await
    }

    async fn apply_virtual_media(&self, state: &VirtualMediaState) -> VirtualMediaResult {
        let Some(image) = state.image.as_deref() else {
            return self.eject_virtual_media(&state.device_id).await;
        };
        self.insert_virtual_media(&state.device_id, image, state.write_protected)
            .await
    }

    async fn reconcile_state(&mut self, desired: AppliedState) -> Result<(), String> {
        let desired_override_active = boot_source_override_is_active(&desired.boot_source_override);
        if desired.boot_source_override != self.applied_state.boot_source_override
            && desired_override_active
        {
            self.set_boot_source_override(&desired.boot_source_override)
                .await
                .map_err(|error| error.to_string())?;
        } else if !desired_override_active
            && (desired.boot_source_override != self.applied_state.boot_source_override
                || desired.persistent_boot_selection
                    != self.applied_state.persistent_boot_selection)
        {
            self.set_persistent_boot_selection(desired.persistent_boot_selection)
                .await
                .map_err(|error| error.to_string())?;
        }
        self.applied_state.boot_source_override = desired.boot_source_override;
        self.applied_state.persistent_boot_selection = desired.persistent_boot_selection;
        for (device_id, desired_device) in desired.virtual_media {
            if self.applied_state.virtual_media.get(&device_id) == Some(&desired_device) {
                continue;
            }
            self.apply_virtual_media(&desired_device)
                .await
                .map_err(|error| error.to_string())?;
            self.applied_state
                .virtual_media
                .insert(device_id, desired_device);
        }
        Ok(())
    }

    async fn apply_boot_config(&mut self, boot: &BootState) -> Result<(), SetSystemPowerError> {
        if boot_source_override_is_active(&boot.source) {
            self.set_boot_source_override(&boot.source).await
        } else {
            self.set_persistent_boot_selection(boot.persistent_selection())
                .await
        }
    }
}

impl From<&SystemStateData> for AppliedState {
    fn from(state: &SystemStateData) -> Self {
        Self {
            persistent_boot_selection: state.boot.persistent_selection(),
            boot_source_override: state.boot.source.clone(),
            virtual_media: state.virtual_media.clone(),
        }
    }
}

fn boot_source_override_is_active(boot_source_override: &BootSourceOverride) -> bool {
    let enabled = boot_source_override.enabled;
    let target = boot_source_override.target;
    enabled != Some(BootSourceOverrideEnabled::Disabled)
        && !matches!(target, None | Some(BootSource::None))
}

impl LibvirtBackend {
    async fn get_power_state(&self) -> MockPowerState {
        match self.virsh(&["domstate", &self.config.domain]).await {
            Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
                "running" | "idle" | "blocked" | "paused" | "in shutdown" | "pmsuspended" => {
                    MockPowerState::On
                }
                _ => MockPowerState::Off,
            },
            Err(error) => {
                tracing::warn!(
                    domain = %self.config.domain,
                    error,
                    "could not read libvirt domain power state",
                );
                MockPowerState::Off
            }
        }
    }

    async fn refresh_power_state(&self) {
        let observed = self.get_power_state().await;
        *self.power_state.write().expect("power state lock poisoned") = observed;
    }

    async fn send_power_command(
        &mut self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        use SystemPowerControl::*;
        // Only a cold start loads the saved domain XML. Reboot and reset keep
        // the running domain's boot configuration and their existing semantics.
        if matches!(reset_type, On | ForceOn | PowerCycle) {
            self.reapply_effective_boot_order().await?;
        }
        match reset_type {
            On | ForceOn => self.start().await,
            GracefulShutdown => self.domain_command("shutdown").await,
            ForceOff => self.domain_command("destroy").await,
            GracefulRestart => self.domain_command("reboot").await,
            ForceRestart => self.domain_command("reset").await,
            PowerCycle => {
                self.domain_command("destroy").await?;
                self.start().await
            }
            Pause => self.domain_command("suspend").await,
            Resume => self.domain_command("resume").await,
            Nmi => self.domain_command("inject-nmi").await,
            PushPowerButton | Suspend => Err(SetSystemPowerError::BadRequest(format!(
                "libvirt backend does not support {reset_type:?}"
            ))),
        }
    }

    async fn refresh(&mut self) {
        let Some(state) = self.controlled_state() else {
            tracing::error!(
                domain = %self.config.domain,
                "libvirt backend is not bound to BMC mock state",
            );
            return;
        };
        if let Err(error) = self.reconcile_state(AppliedState::from(state)).await {
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
        _mailbox: &ActorMailbox<LibvirtMessage>,
        message: LibvirtMessage,
    ) -> ActorResult {
        match message {
            LibvirtMessage::Run => self.refresh_power_state().await,
            LibvirtMessage::InitializeState { system_id, state } => {
                match self.system_states.entry(system_id) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(state);
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        tracing::error!(domain = %self.config.domain, system_id = %entry.key(), "libvirt system state is already initialized");
                    }
                }
            }
            LibvirtMessage::Bind { system_id, reply } => {
                if !reply.is_closed() {
                    let result = self.bind_state(system_id).await;
                    self.refresh_power_state().await;
                    // The caller can stop waiting while the operation completes.
                    reply.send(result).ok();
                }
            }
            LibvirtMessage::SendPowerCommand { reset_type } => {
                if matches!(reset_type, SystemPowerControl::PowerCycle) {
                    *self.power_state.write().expect("power state lock poisoned") =
                        MockPowerState::PowerCycling {
                            since: Instant::now(),
                        };
                }
                if let Err(error) = self.send_power_command(reset_type).await {
                    tracing::error!(domain = %self.config.domain, ?reset_type, %error, "libvirt power command failed");
                }
                self.refresh_power_state().await;
            }
            LibvirtMessage::SetBootConfig {
                system_id,
                patch,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self
                        .system_states
                        .get_mut(&system_id)
                        .ok_or(CallbackError::SystemNotInitialized(system_id))
                        .map(|state| state.boot.apply(patch));
                    reply.send(result).ok();
                }
            }
            LibvirtMessage::SetVirtualMedia {
                system_id,
                state: desired,
                reply,
            } => {
                if !reply.is_closed() {
                    let result = self
                        .system_states
                        .get_mut(&system_id)
                        .ok_or(CallbackError::SystemNotInitialized(system_id))
                        .and_then(|state| state.set_media(desired));
                    reply.send(result).ok();
                }
            }
            LibvirtMessage::GetSystemState { system_id, reply } => {
                let result = self
                    .system_states
                    .get(&system_id)
                    .cloned()
                    .ok_or(CallbackError::SystemNotInitialized(system_id));
                reply.send(result).ok();
            }
            LibvirtMessage::Refresh => {
                // Coalesce notifications until this refresh begins.
                self.refresh_pending.store(false, Ordering::SeqCst);
                self.refresh().await;
                self.refresh_power_state().await;
            }
        }
        ActorResult::Noop
    }
}

impl Callbacks for LibvirtCallbacks {
    async fn get_system_state(&self, system_id: &str) -> Result<SystemStateData, CallbackError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(LibvirtMessage::GetSystemState {
                system_id: system_id.to_string(),
                reply,
            })
            .map_err(eyre::Error::from)?;
        response.await.map_err(eyre::Error::from)?
    }

    async fn set_boot_config(
        &self,
        system_id: &str,
        patch: BootConfigPatch,
    ) -> Result<(), CallbackError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(LibvirtMessage::SetBootConfig {
                system_id: system_id.to_string(),
                patch,
                reply,
            })
            .map_err(eyre::Error::from)?;
        response.await.map_err(eyre::Error::from)?
    }

    async fn set_virtual_media(
        &self,
        system_id: &str,
        state: VirtualMediaState,
    ) -> Result<(), CallbackError> {
        let (reply, response) = oneshot::channel();
        self.mailbox
            .send(LibvirtMessage::SetVirtualMedia {
                system_id: system_id.to_string(),
                state,
                reply,
            })
            .map_err(eyre::Error::from)?;
        response.await.map_err(eyre::Error::from)?
    }

    fn get_power_state(&self) -> MockPowerState {
        *self.power_state.read().expect("power state lock poisoned")
    }

    fn send_power_command(
        &self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        self.mailbox
            .send(LibvirtMessage::SendPowerCommand { reset_type })
            .map_err(|error| SetSystemPowerError::CommandSendError(error.to_string()))
    }

    fn initialize_system(&self, system_id: &str, initial: SystemStateData) {
        if let Err(error) = self.mailbox.send(LibvirtMessage::InitializeState {
            system_id: system_id.to_string(),
            state: initial,
        }) {
            tracing::error!(%error, "could not initialize libvirt system state");
        }
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

fn set_boot_order_xml(xml: &str, devices: &[&str]) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut inside_os = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| format!("could not parse libvirt domain XML: {error}"))?;
        match event {
            Event::Start(start) if start.name().as_ref() == b"os" => {
                inside_os = true;
                writer.write_event(Event::Start(start.into_owned()))
            }
            Event::Empty(empty) if inside_os && empty.name().as_ref() == b"boot" => Ok(()),
            Event::End(end) if end.name().as_ref() == b"os" => {
                let result: std::io::Result<()> = (|| {
                    for device in devices {
                        let mut boot = BytesStart::new("boot");
                        boot.push_attribute(("dev", *device));
                        writer.write_event(Event::Empty(boot))?;
                    }
                    inside_os = false;
                    writer.write_event(Event::End(BytesEnd::new("os")))
                })();
                result
            }
            Event::Eof => break,
            event => writer.write_event(event.into_owned()),
        }
        .map_err(|error| format!("could not write libvirt domain XML: {error}"))?;
    }
    String::from_utf8(writer.into_inner())
        .map_err(|error| format!("generated libvirt domain XML is invalid UTF-8: {error}"))
}

enum MediaSource {
    File(PathBuf),
    Network {
        protocol: String,
        host: String,
        port: u16,
        path: String,
    },
}

impl MediaSource {
    fn parse(image: &str) -> Result<Self, VirtualMediaError> {
        let Ok(url) = Url::parse(image) else {
            return Ok(Self::File(PathBuf::from(image)));
        };
        match url.scheme() {
            "file" => url
                .to_file_path()
                .map(Self::File)
                .map_err(|()| VirtualMediaError::BadRequest(format!("invalid file URL: {image}"))),
            "http" | "https" => {
                if url.username() != "" || url.password().is_some() || url.query().is_some() {
                    return Err(VirtualMediaError::BadRequest(
                        "virtual media URLs must not contain credentials or a query".to_string(),
                    ));
                }
                let host = url.host().ok_or_else(|| {
                    VirtualMediaError::BadRequest(format!("virtual media URL has no host: {image}"))
                })?;
                // `Host::Display` brackets IPv6, but libvirt needs the bare address.
                let host = match host {
                    url::Host::Ipv6(address) => address.to_string(),
                    host => host.to_string(),
                };
                Ok(Self::Network {
                    protocol: url.scheme().to_string(),
                    host,
                    port: url
                        .port_or_known_default()
                        .expect("HTTP(S) has a default port"),
                    path: url.path().to_string(),
                })
            }
            scheme => Err(VirtualMediaError::BadRequest(format!(
                "unsupported virtual media URL scheme: {scheme}"
            ))),
        }
    }
}

fn virtual_media_xml(
    device_id: &str,
    target: &str,
    image: &str,
    write_protected: bool,
) -> Result<String, VirtualMediaError> {
    let mut writer = Writer::new(Vec::new());
    let mut disk = BytesStart::new("disk");
    let source = MediaSource::parse(image)?;
    disk.push_attribute((
        "type",
        match &source {
            MediaSource::File(_) => "file",
            MediaSource::Network { .. } => "network",
        },
    ));
    disk.push_attribute(("device", "cdrom"));
    writer.write_event(Event::Start(disk)).unwrap();

    let mut driver = BytesStart::new("driver");
    driver.push_attribute(("name", "qemu"));
    driver.push_attribute(("type", "raw"));
    writer.write_event(Event::Empty(driver)).unwrap();

    match source {
        MediaSource::File(path) => {
            let mut source = BytesStart::new("source");
            let path = path.to_string_lossy();
            source.push_attribute(("file", path.as_ref()));
            writer.write_event(Event::Empty(source)).unwrap();
        }
        MediaSource::Network {
            protocol,
            host,
            port,
            path,
        } => {
            let mut source = BytesStart::new("source");
            source.push_attribute(("protocol", protocol.as_str()));
            source.push_attribute(("name", path.as_str()));
            writer.write_event(Event::Start(source)).unwrap();
            let mut host_element = BytesStart::new("host");
            let port = port.to_string();
            host_element.push_attribute(("name", host.as_str()));
            host_element.push_attribute(("port", port.as_str()));
            writer.write_event(Event::Empty(host_element)).unwrap();
            writer
                .write_event(Event::End(BytesEnd::new("source")))
                .unwrap();
        }
    }

    let mut target_element = BytesStart::new("target");
    target_element.push_attribute(("dev", target));
    target_element.push_attribute(("bus", "sata"));
    writer.write_event(Event::Empty(target_element)).unwrap();
    if write_protected {
        writer
            .write_event(Event::Empty(BytesStart::new("readonly")))
            .unwrap();
    }
    let mut alias = BytesStart::new("alias");
    let alias_name = format!("ua-bmc-mock-vmedia-{device_id}");
    alias.push_attribute(("name", alias_name.as_str()));
    writer.write_event(Event::Empty(alias)).unwrap();
    writer
        .write_event(Event::End(BytesEnd::new("disk")))
        .unwrap();

    String::from_utf8(writer.into_inner()).map_err(|error| {
        VirtualMediaError::Command(format!("generated device XML is invalid UTF-8: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases};

    use super::*;

    #[test]
    fn replaces_domain_boot_order() {
        let xml = "<domain><os><type>hvm</type><boot dev='hd'/></os><devices/></domain>";
        let actual = set_boot_order_xml(xml, &["cdrom", "hd"]).unwrap();

        assert!(actual.contains("<type>hvm</type>"));
        assert!(actual.contains("<boot dev=\"cdrom\"/><boot dev=\"hd\"/>"));
        assert!(!actual.contains("<boot dev='hd'/>"));
    }

    #[test]
    fn builds_network_virtual_media_device() {
        let device = |source| {
            format!(
                "<disk type=\"network\" device=\"cdrom\"><driver name=\"qemu\" type=\"raw\"/>\
                {source}</source><target dev=\"sdb\" bus=\"sata\"/><readonly/>\
                <alias name=\"ua-bmc-mock-vmedia-Cd\"/></disk>"
            )
        };
        check_cases(
            [
                Case {
                    scenario: "IPv4 with an explicit port",
                    input: "http://127.0.0.1:8080/installer.iso",
                    expect: Yields(device(
                        "<source protocol=\"http\" name=\"/installer.iso\"><host name=\"127.0.0.1\" port=\"8080\"/>",
                    )),
                },
                Case {
                    scenario: "IPv6 with an explicit port",
                    input: "http://[2001:db8::10]:8080/installer.iso",
                    expect: Yields(device(
                        "<source protocol=\"http\" name=\"/installer.iso\"><host name=\"2001:db8::10\" port=\"8080\"/>",
                    )),
                },
                Case {
                    scenario: "domain with the default HTTPS port",
                    input: "https://images.example.com/installer.iso",
                    expect: Yields(device(
                        "<source protocol=\"https\" name=\"/installer.iso\"><host name=\"images.example.com\" port=\"443\"/>",
                    )),
                },
            ],
            |image| virtual_media_xml("Cd", "sdb", image, true).map_err(|error| error.to_string()),
        );
    }

    #[test]
    fn builds_file_virtual_media_device() {
        let actual = virtual_media_xml("ConfigCd", "sdc", "/tmp/config.iso", false).unwrap();

        assert!(actual.contains("<disk type=\"file\" device=\"cdrom\">"));
        assert!(actual.contains("<source file=\"/tmp/config.iso\"/>"));
        assert!(actual.contains("<target dev=\"sdc\" bus=\"sata\"/>"));
        assert!(!actual.contains("<readonly/>"));
    }
}
