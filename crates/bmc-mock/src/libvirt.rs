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
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use eyre::WrapErr;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, DropGuard};
use url::Url;

use crate::actor::{Actor, ActorCallbacks, ActorMailbox, ActorResult};
use crate::redfish::computer_system::{SingleSystemState, SystemState};
use crate::{ActionError, BmcState, BootOptionKind, Callbacks, MockPowerState, ResourceResetType};

/// Maximum duration of one virsh attempt, including output collection.
const VIRSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Additional grace period for reaping after a kill request, independent of the command deadline.
const VIRSH_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Delay after each periodic observation; the actor retains at most one polling alarm.
const POWER_POLL_INTERVAL: Duration = Duration::from_secs(5);

pub struct LibvirtActor {
    actor: Actor<LibvirtMessage>,
    mailbox: ActorMailbox<LibvirtMessage>,
    backend: LibvirtBackend,
}

impl LibvirtActor {
    /// Creates an unstarted libvirt actor and its callbacks.
    /// Call `run` to initialize the backend and spawn it in the owner's supervised task set.
    pub fn new(config: Config, guard: DropGuard) -> (Self, LibvirtCallbacks) {
        let refresh_pending = Arc::new(AtomicBool::new(false));
        let power_state = Arc::new(RwLock::new(MockPowerState::Unknown));
        let backend = LibvirtBackend {
            config,
            restore_boot_after_power_on: false,
            system_state: None,
            applied_state: AppliedState::default(),
            refresh_pending: refresh_pending.clone(),
            power_state: power_state.clone(),
        };
        let (actor, mailbox) = Actor::new();
        (
            LibvirtActor {
                actor,
                mailbox: mailbox.clone(),
                backend,
            },
            LibvirtCallbacks {
                mailbox,
                refresh_pending,
                power_state,
                _stop: guard,
            },
        )
    }

    pub async fn run(
        mut self,
        bmc_state: &BmcState<LibvirtCallbacks>,
        tasks: &mut JoinSet<()>,
        stop: CancellationToken,
    ) -> eyre::Result<()> {
        self.backend.init(bmc_state.system_state.clone()).await?;
        self.mailbox
            .send_at(
                (Instant::now() + POWER_POLL_INTERVAL).into(),
                LibvirtMessage::PollPower,
            )
            .expect("unstarted actor mailbox must be open");
        let fut = self.actor.run(self.backend);
        tasks.spawn(async move {
            stop.run_until_cancelled(fut).await;
        });
        Ok(())
    }
}

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
/// Power reads return the last completed observation, initially Unknown, updated at actor startup,
/// after power commands and refresh notifications, and by polling with a five-second
/// delay after each attempt. Actor work can delay polling; each virsh attempt has a 30-second
/// deadline and up to five seconds of cleanup. Failed or unrecognized observations return Unknown.
/// Power commands return once enqueued; execution failures are logged by the actor.
/// Dropping the handle cancels the actor. Cancelling `LibvirtActor::run` during
/// initialization interrupts it before the actor task is spawned.
#[derive(Debug)]
pub struct LibvirtCallbacks {
    mailbox: ActorMailbox<LibvirtMessage>,
    refresh_pending: Arc<AtomicBool>,
    power_state: Arc<RwLock<MockPowerState>>,
    _stop: DropGuard,
}

#[derive(Debug)]
enum LibvirtMessage {
    PollPower,
    SendPowerCommand { reset_type: ResourceResetType },
    Refresh,
}

struct LibvirtBackend {
    config: Config,
    restore_boot_after_power_on: bool,
    system_state: Option<Weak<SystemState<LibvirtCallbacks>>>,
    applied_state: AppliedState,
    refresh_pending: Arc<AtomicBool>,
    power_state: Arc<RwLock<MockPowerState>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AppliedState {
    persistent_boot_selection: Option<BootOptionKind>,
    boot_source_override: serde_json::Value,
    virtual_media: BTreeMap<String, serde_json::Value>,
}

impl LibvirtBackend {
    async fn init(&mut self, state: Arc<SystemState<LibvirtCallbacks>>) -> eyre::Result<()> {
        let controlled_system = state.controlled_system().ok_or(eyre::eyre!(
            "libvirt backend has no controlled ComputerSystem"
        ))?;
        let applied = AppliedState::from(controlled_system);
        self.set_persistent_boot_selection(applied.persistent_boot_selection)
            .await?;
        self.system_state = Some(Arc::downgrade(&state));
        self.applied_state = applied;
        self.refresh_power_state().await;
        Ok(())
    }

    async fn virsh_output(&self, arguments: &[&str]) -> eyre::Result<Output> {
        let command = self.config.virsh_path.display().to_string();
        let mut child = Command::new(&self.config.virsh_path)
            .env("LC_ALL", "C")
            .arg("--connect")
            .arg(&self.config.uri)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("could not execute {command}"))?;
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
                    .with_context(|| format!("could not stop {command}"))?;
                // On timeout, returning drops Child and leaves reaping to Tokio's
                // best-effort cleanup so the actor can keep processing.
                tokio::time::timeout(VIRSH_CLEANUP_TIMEOUT, child.wait())
                    .await
                    .with_context(|| {
                        format!("could not reap {command} within {VIRSH_CLEANUP_TIMEOUT:?} after kill request")
                    })?
                    .with_context(|| format!("could not reap {command}"))?;
                Err(match failed {
                    Err(_) => eyre::eyre!("{command} timed out after {VIRSH_COMMAND_TIMEOUT:?}"),
                    Ok(Err(error)) => {
                        eyre::eyre!("could not collect {command} output: {error}")
                    }
                    Ok(Ok(_)) => unreachable!(),
                })
            }
        }
    }

    async fn virsh(&self, arguments: &[&str]) -> eyre::Result<Output> {
        let output = self.virsh_output(arguments).await?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(eyre::eyre!(
                "{} exited with {}: {}",
                self.config.virsh_path.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    async fn domain_command(&self, command: &str) -> eyre::Result<()> {
        self.virsh(&[command, &self.config.domain]).await.map(drop)
    }

    async fn start(&mut self) -> eyre::Result<()> {
        self.domain_command("start").await?;
        let restore_boot = std::mem::take(&mut self.restore_boot_after_power_on);
        if restore_boot {
            if let Some(system_state) = self.system_state.as_ref().and_then(Weak::upgrade) {
                system_state.on_boot_completed();
            }
            self.restore_persistent_boot_order().await?;
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
        self.set_persistent_boot_selection(selection).await
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
            self.set_boot_source_override(&boot_source_override).await
        } else {
            self.set_persistent_boot_selection(system.resolve_persistent_boot_selection())
                .await
                .map_err(ActionError::Internal)
        }
    }

    async fn set_persistent_boot_selection(
        &self,
        selection: Option<BootOptionKind>,
    ) -> eyre::Result<()> {
        match selection {
            Some(BootOptionKind::Disk) => self.set_boot_devices(&["hd"]).await,
            Some(BootOptionKind::Network) => self.set_boot_devices(&["network", "hd"]).await,
            None => Ok(()),
        }
    }

    async fn set_boot_devices(&self, devices: &[&str]) -> eyre::Result<()> {
        let output = self
            .virsh(&["dumpxml", "--inactive", &self.config.domain])
            .await?;
        let xml =
            String::from_utf8(output.stdout).context("virsh dumpxml returned invalid UTF-8")?;
        let xml = set_boot_order_xml(&xml, devices)?;
        let file =
            tempfile::NamedTempFile::new().context("could not create temporary domain XML")?;
        tokio::fs::write(file.path(), xml.as_bytes())
            .await
            .context("could not write temporary domain XML")?;
        self.virsh(&["define", file.path().to_string_lossy().as_ref()])
            .await
            .map(drop)
    }

    fn target_for_device(&self, device_id: &str) -> Result<&str, ActionError> {
        self.config
            .virtual_media_targets
            .get(device_id)
            .map(String::as_str)
            .ok_or_else(|| {
                ActionError::BadRequest(eyre::eyre!(
                    "virtual media device {} has no libvirt target",
                    device_id
                ))
            })
    }

    async fn target_is_attached(&self, target: &str) -> eyre::Result<bool> {
        let output = self
            .virsh(&["domblklist", "--details", &self.config.domain])
            .await?;
        let output =
            String::from_utf8(output.stdout).context("virsh domblklist returned invalid UTF-8")?;
        Ok(output.lines().any(|line| {
            line.split_whitespace()
                .nth(2)
                .is_some_and(|value| value == target)
        }))
    }

    async fn detach_target(&self, target: &str) -> eyre::Result<()> {
        if !self.target_is_attached(target).await? {
            return Ok(());
        }
        self.virsh(&["detach-disk", &self.config.domain, target, "--persistent"])
            .await
            .map(drop)
    }

    async fn set_boot_source_override(
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
        self.set_boot_devices(devices)
            .await
            .map_err(ActionError::Internal)?;
        self.restore_boot_after_power_on = enabled == Some("Once");
        Ok(())
    }

    async fn insert_virtual_media(
        &self,
        device_id: &str,
        image: &str,
        write_protected: bool,
    ) -> eyre::Result<()> {
        let target = self.target_for_device(device_id)?;
        self.detach_target(target).await?;
        let xml = virtual_media_xml(device_id, target, image, write_protected)?;
        let file =
            tempfile::NamedTempFile::new().context("could not create temporary device XML")?;
        tokio::fs::write(file.path(), xml.as_bytes())
            .await
            .context("could not write temporary device XML")?;
        self.virsh(&[
            "attach-device",
            &self.config.domain,
            file.path().to_string_lossy().as_ref(),
            "--persistent",
        ])
        .await
        .map(drop)
    }

    async fn eject_virtual_media(&self, device_id: &str) -> eyre::Result<()> {
        let target = self.target_for_device(device_id)?;
        self.detach_target(target).await
    }

    async fn apply_virtual_media(&self, state: &serde_json::Value) -> Result<(), ActionError> {
        let device_id = state
            .get("Id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ActionError::BadRequest(eyre::eyre!("virtual media state has no id")))?;
        let inserted = state
            .get("Inserted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if !inserted {
            return self
                .eject_virtual_media(device_id)
                .await
                .map_err(ActionError::Internal);
        }
        let image = state
            .get("Image")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ActionError::BadRequest(eyre::eyre!(
                    "inserted virtual media device {} has no image",
                    device_id
                ))
            })?;
        let write_protected = state
            .get("WriteProtected")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        self.insert_virtual_media(device_id, image, write_protected)
            .await
            .map_err(ActionError::Internal)
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
}

impl<C: Callbacks> From<&SingleSystemState<C>> for AppliedState {
    fn from(system: &SingleSystemState<C>) -> Self {
        let virtual_media = system
            .virtual_media()
            .into_iter()
            .flat_map(|virtual_media| virtual_media.desired_state())
            .filter_map(|state| {
                let device_id = state
                    .get("Id")
                    .and_then(serde_json::Value::as_str)?
                    .to_string();
                Some((device_id, state))
            })
            .collect();
        Self {
            persistent_boot_selection: system.resolve_persistent_boot_selection(),
            boot_source_override: system.boot_source_override(),
            virtual_media,
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
    async fn get_power_state(&self) -> MockPowerState {
        match self.virsh(&["domstate", &self.config.domain]).await {
            Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
                "running" | "idle" | "blocked" | "paused" | "in shutdown" | "pmsuspended" => {
                    MockPowerState::On
                }
                "shut off" | "crashed" => MockPowerState::Off,
                state => {
                    tracing::warn!(domain = %self.config.domain, state, "unrecognized libvirt domain power state");
                    MockPowerState::Unknown
                }
            },
            Err(error) => {
                tracing::warn!(
                    domain = %self.config.domain,
                    error = ?error,
                    "could not read libvirt domain power state",
                );
                MockPowerState::Unknown
            }
        }
    }

    async fn refresh_power_state(&self) {
        let observed = self.get_power_state().await;
        *self.power_state.write().expect("power state lock poisoned") = observed;
    }

    async fn send_power_command(
        &mut self,
        reset_type: ResourceResetType,
    ) -> Result<(), ActionError> {
        use ResourceResetType::*;
        // Only a cold start loads the saved domain XML. Reboot and reset keep
        // the running domain's boot configuration and their existing semantics.
        if matches!(reset_type, On | ForceOn | PowerCycle) {
            self.reapply_effective_boot_order().await?;
        }
        match reset_type {
            On | ForceOn => self.start().await.map_err(ActionError::Internal),
            GracefulShutdown => self
                .domain_command("shutdown")
                .await
                .map_err(ActionError::Internal),
            ForceOff => self
                .domain_command("destroy")
                .await
                .map_err(ActionError::Internal),
            GracefulRestart => self
                .domain_command("reboot")
                .await
                .map_err(ActionError::Internal),
            ForceRestart => self
                .domain_command("reset")
                .await
                .map_err(ActionError::Internal),
            PowerCycle | FullPowerCycle => {
                self.domain_command("destroy")
                    .await
                    .map_err(ActionError::Internal)?;
                self.start().await.map_err(ActionError::Internal)
            }
            Pause => self
                .domain_command("suspend")
                .await
                .map_err(ActionError::Internal),
            Resume => self
                .domain_command("resume")
                .await
                .map_err(ActionError::Internal),
            Nmi => self
                .domain_command("inject-nmi")
                .await
                .map_err(ActionError::Internal),
            Sleep | Hibernate | PushPowerButton | Suspend | UnsupportedValue => {
                Err(ActionError::BadRequest(eyre::eyre!(
                    "libvirt backend does not support {reset_type:?}"
                )))
            }
        }
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
                    *self.power_state.write().expect("power state lock poisoned") =
                        MockPowerState::PowerCycling {
                            since: Instant::now(),
                        };
                }
                if let Err(error) = self.send_power_command(reset_type).await {
                    tracing::error!(domain = %self.config.domain, ?reset_type, %error, "libvirt power command failed");
                }
                self.refresh_power_state().await;
                ActorResult::Noop
            }
            LibvirtMessage::Refresh => {
                // Clear before reading state so changes during reconciliation queue another refresh.
                self.refresh_pending.store(false, Ordering::SeqCst);
                self.refresh().await;
                self.refresh_power_state().await;
                ActorResult::Noop
            }
        }
    }
}

impl Callbacks for LibvirtCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        *self.power_state.read().expect("power state lock poisoned")
    }

    async fn computer_system_reset(
        &self,
        reset_type: ResourceResetType,
    ) -> Result<(), ActionError> {
        self.get_power_state().validate_reset_type(reset_type)?;
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

fn set_boot_order_xml(xml: &str, devices: &[&str]) -> eyre::Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut inside_os = false;
    loop {
        let event = reader
            .read_event()
            .context("could not parse libvirt domain XML")?;
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
        .context("could not write libvirt domain XML")?;
    }
    String::from_utf8(writer.into_inner()).context("generated libvirt domain XML is invalid UTF-8")
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
    fn parse(image: &str) -> Result<Self, ActionError> {
        let Ok(url) = Url::parse(image) else {
            return Ok(Self::File(PathBuf::from(image)));
        };
        match url.scheme() {
            "file" => url
                .to_file_path()
                .map(Self::File)
                .map_err(|()| ActionError::BadRequest(eyre::eyre!("invalid file URL: {}", image))),
            "http" | "https" => {
                if url.username() != "" || url.password().is_some() || url.query().is_some() {
                    return Err(ActionError::BadRequest(eyre::eyre!(
                        "virtual media URLs must not contain credentials or a query",
                    )));
                }
                let host = url.host().ok_or_else(|| {
                    ActionError::BadRequest(eyre::eyre!("virtual media URL has no host: {}", image))
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
            scheme => Err(ActionError::BadRequest(eyre::eyre!(
                "unsupported virtual media URL scheme: {}",
                scheme
            ))),
        }
    }
}

fn virtual_media_xml(
    device_id: &str,
    target: &str,
    image: &str,
    write_protected: bool,
) -> eyre::Result<String> {
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

    String::from_utf8(writer.into_inner()).context("generated device XML is invalid UTF-8")
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases};
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::host_info;
    use crate::{HardwareType, MachineRouterOptions, machine_router};

    #[tokio::test]
    async fn observes_external_power_changes_and_recovers_from_unavailable_state() {
        let directory = tempfile::tempdir().unwrap();
        let virsh = directory.path().join("virsh");
        std::fs::write(
            &virsh,
            r#"#!/bin/sh
case "$3" in
    domstate)
        IFS= read -r state < "$0.state"
        [ "$state" != error ] || exit 1
        printf '%s\n' "$state"
        ;;
    dumpxml) printf '%s\n' '<domain><os><type>hvm</type></os><devices/></domain>' ;;
    define) exit 0 ;;
    *) exit 2 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&virsh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let state_file = directory.path().join("virsh.state");
        std::fs::write(&state_file, "running").unwrap();
        let mut tasks = JoinSet::new();
        let stop = CancellationToken::new();
        let (actor, callbacks) = LibvirtActor::new(
            Config {
                virsh_path: virsh,
                uri: "test:///default".to_string(),
                domain: "test-domain".to_string(),
                virtual_media_targets: BTreeMap::new(),
            },
            stop.clone().drop_guard(),
        );
        let callbacks = Arc::new(callbacks);
        assert!(matches!(
            callbacks.get_power_state(),
            MockPowerState::Unknown
        ));
        let (router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            callbacks.clone(),
            "test-host".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        actor.run(&state, &mut tasks, stop).await.unwrap();
        // These changes happen outside the BMC: no reset or refresh is sent.
        for (observation, expected) in [
            ("running", serde_json::json!("On")),
            ("shut off", serde_json::json!("Off")),
            ("error", serde_json::Value::Null),
            ("running", serde_json::json!("On")),
            ("unrecognized", serde_json::Value::Null),
        ] {
            let replacement = directory.path().join("next-state");
            std::fs::write(&replacement, observation).unwrap();
            std::fs::rename(replacement, &state_file).unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
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
                    if body.get("PowerState") == Some(&expected) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("observation {observation:?} did not become {expected}"));
            if expected.is_null() {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset")
                            .header("content-type", "application/json")
                            .body(Body::from(r#"{"ResetType":"ForceOff"}"#))
                            .unwrap(),
                    )
                    .await;
                assert_eq!(
                    response.unwrap().status(),
                    StatusCode::INTERNAL_SERVER_ERROR
                );
            }
        }
        drop(router);
        drop(state);
        drop(callbacks);
        tokio::time::timeout(Duration::from_secs(1), tasks.join_all())
            .await
            .expect("dropping the backend must stop polling");
    }

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
