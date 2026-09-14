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
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use url::Url;

use crate::redfish::computer_system::{SingleSystemState, SystemState};
use crate::{
    BmcState, BootOptionKind, Callbacks, MockPowerState, SetSystemPowerError, SystemPowerControl,
};

#[derive(Debug, thiserror::Error)]
enum VirtualMediaError {
    #[error("invalid virtual media request: {0}")]
    BadRequest(String),
    #[error("virtual media command failed: {0}")]
    Command(String),
}

type VirtualMediaResult = Result<(), VirtualMediaError>;

#[derive(Clone, Debug)]
pub struct Config {
    pub virsh_path: PathBuf,
    pub uri: String,
    pub domain: String,
    pub virtual_media_targets: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct LibvirtCallbacks {
    config: Config,
    restore_boot_after_power_on: Mutex<bool>,
    system_state: OnceLock<Weak<SystemState>>,
    applied_state: Mutex<AppliedState>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AppliedState {
    persistent_boot_selection: Option<BootOptionKind>,
    boot_source_override: serde_json::Value,
    virtual_media: BTreeMap<String, serde_json::Value>,
}

impl LibvirtCallbacks {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            restore_boot_after_power_on: Mutex::new(false),
            system_state: OnceLock::new(),
            applied_state: Mutex::new(AppliedState::default()),
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
    pub fn bind_state(&self, state: &BmcState) -> Result<(), String> {
        let controlled_system = state
            .system_state
            .controlled_system()
            .ok_or_else(|| "libvirt backend has no controlled ComputerSystem".to_string())?;
        let mut applied_state = self.applied_state.lock().unwrap();
        if self.system_state.get().is_some() {
            return Err("libvirt backend state is already bound".to_string());
        }
        let applied = AppliedState::from(controlled_system);
        self.set_persistent_boot_selection(applied.persistent_boot_selection)
            .map_err(|error| error.to_string())?;
        self.system_state
            .set(Arc::downgrade(&state.system_state))
            .map_err(|_| "libvirt backend state is already bound".to_string())?;
        *applied_state = applied;
        Ok(())
    }

    fn virsh_output(&self, arguments: &[&str]) -> Result<Output, String> {
        Command::new(&self.config.virsh_path)
            .arg("--connect")
            .arg(&self.config.uri)
            .args(arguments)
            .output()
            .map_err(|error| {
                format!(
                    "could not execute {}: {error}",
                    self.config.virsh_path.display()
                )
            })
    }

    fn virsh(&self, arguments: &[&str]) -> Result<Output, String> {
        let output = self.virsh_output(arguments)?;
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

    fn domain_command(&self, command: &str) -> Result<(), SetSystemPowerError> {
        self.virsh(&[command, &self.config.domain])
            .map(drop)
            .map_err(SetSystemPowerError::CommandSendError)
    }

    fn start(&self) -> Result<(), SetSystemPowerError> {
        self.domain_command("start")?;
        let restore_boot = {
            let mut restore_boot_after_power_on = self.restore_boot_after_power_on.lock().unwrap();
            std::mem::take(&mut *restore_boot_after_power_on)
        };
        if restore_boot {
            if let Some(system_state) = self.system_state.get().and_then(Weak::upgrade) {
                system_state.on_boot_completed();
            }
            self.restore_persistent_boot_order()?;
        }
        Ok(())
    }

    fn restore_persistent_boot_order(&self) -> Result<(), SetSystemPowerError> {
        let selection = self
            .system_state
            .get()
            .and_then(Weak::upgrade)
            .and_then(|state| {
                state
                    .controlled_system()
                    .and_then(SingleSystemState::resolve_persistent_boot_selection)
            });
        self.set_persistent_boot_selection(selection)
    }

    fn reapply_effective_boot_order(&self) -> Result<(), SetSystemPowerError> {
        let Some(state) = self.system_state.get().and_then(Weak::upgrade) else {
            return Err(SetSystemPowerError::CommandSendError(
                "libvirt backend is not bound to BMC mock state".to_string(),
            ));
        };
        let Some(system) = state.controlled_system() else {
            return Err(SetSystemPowerError::CommandSendError(
                "BMC mock state has no controlled ComputerSystem".to_string(),
            ));
        };
        let boot_source_override = system.boot_source_override();
        if boot_source_override_is_active(&boot_source_override) {
            self.set_boot_source_override(&boot_source_override)
        } else {
            self.set_persistent_boot_selection(system.resolve_persistent_boot_selection())
        }
    }

    fn set_persistent_boot_selection(
        &self,
        selection: Option<BootOptionKind>,
    ) -> Result<(), SetSystemPowerError> {
        match selection {
            Some(BootOptionKind::Disk) => self.set_boot_devices(&["hd"]),
            Some(BootOptionKind::Network) => self.set_boot_devices(&["network", "hd"]),
            None => Ok(()),
        }
    }

    fn set_boot_devices(&self, devices: &[&str]) -> Result<(), SetSystemPowerError> {
        let output = self
            .virsh(&["dumpxml", "--inactive", &self.config.domain])
            .map_err(SetSystemPowerError::CommandSendError)?;
        let xml = String::from_utf8(output.stdout).map_err(|error| {
            SetSystemPowerError::CommandSendError(format!(
                "virsh dumpxml returned invalid UTF-8: {error}"
            ))
        })?;
        let xml =
            set_boot_order_xml(&xml, devices).map_err(SetSystemPowerError::CommandSendError)?;
        let mut file = tempfile::NamedTempFile::new().map_err(|error| {
            SetSystemPowerError::CommandSendError(format!(
                "could not create temporary domain XML: {error}"
            ))
        })?;
        file.write_all(xml.as_bytes()).map_err(|error| {
            SetSystemPowerError::CommandSendError(format!(
                "could not write temporary domain XML: {error}"
            ))
        })?;
        self.virsh(&["define", file.path().to_string_lossy().as_ref()])
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

    fn target_is_attached(&self, target: &str) -> Result<bool, VirtualMediaError> {
        let output = self
            .virsh(&["domblklist", "--details", &self.config.domain])
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

    fn detach_target(&self, target: &str) -> VirtualMediaResult {
        if !self.target_is_attached(target)? {
            return Ok(());
        }
        self.virsh(&["detach-disk", &self.config.domain, target, "--persistent"])
            .map(drop)
            .map_err(VirtualMediaError::Command)
    }

    fn set_boot_source_override(
        &self,
        boot_source_override: &serde_json::Value,
    ) -> Result<(), SetSystemPowerError> {
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
                return Err(SetSystemPowerError::BadRequest(format!(
                    "unsupported boot source override target: {target}"
                )));
            }
            (_, None) => return Ok(()),
        };
        self.set_boot_devices(devices)?;
        *self.restore_boot_after_power_on.lock().unwrap() = enabled == Some("Once");
        Ok(())
    }

    fn insert_virtual_media(
        &self,
        device_id: &str,
        image: &str,
        write_protected: bool,
    ) -> VirtualMediaResult {
        let target = self.target_for_device(device_id)?;
        self.detach_target(target)?;
        let xml = virtual_media_xml(device_id, target, image, write_protected)?;
        let mut file = tempfile::NamedTempFile::new().map_err(|error| {
            VirtualMediaError::Command(format!("could not create temporary device XML: {error}"))
        })?;
        file.write_all(xml.as_bytes()).map_err(|error| {
            VirtualMediaError::Command(format!("could not write temporary device XML: {error}"))
        })?;
        self.virsh(&[
            "attach-device",
            &self.config.domain,
            file.path().to_string_lossy().as_ref(),
            "--persistent",
        ])
        .map(drop)
        .map_err(VirtualMediaError::Command)
    }

    fn eject_virtual_media(&self, device_id: &str) -> VirtualMediaResult {
        let target = self.target_for_device(device_id)?;
        self.detach_target(target)
    }

    fn apply_virtual_media(&self, state: &serde_json::Value) -> VirtualMediaResult {
        let device_id = state
            .get("Id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                VirtualMediaError::BadRequest("virtual media state has no Id".to_string())
            })?;
        let inserted = state
            .get("Inserted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if !inserted {
            return self.eject_virtual_media(device_id);
        }
        let image = state
            .get("Image")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                VirtualMediaError::BadRequest(format!(
                    "inserted virtual media device {device_id} has no Image"
                ))
            })?;
        let write_protected = state
            .get("WriteProtected")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        self.insert_virtual_media(device_id, image, write_protected)
    }

    fn reconcile_state(&self, desired: AppliedState) -> Result<(), String> {
        let mut applied = self.applied_state.lock().unwrap();
        let desired_override_active = boot_source_override_is_active(&desired.boot_source_override);
        if desired.boot_source_override != applied.boot_source_override && desired_override_active {
            self.set_boot_source_override(&desired.boot_source_override)
                .map_err(|error| error.to_string())?;
        } else if !desired_override_active
            && (desired.boot_source_override != applied.boot_source_override
                || desired.persistent_boot_selection != applied.persistent_boot_selection)
        {
            self.set_persistent_boot_selection(desired.persistent_boot_selection)
                .map_err(|error| error.to_string())?;
        }
        applied.boot_source_override = desired.boot_source_override;
        applied.persistent_boot_selection = desired.persistent_boot_selection;
        for (device_id, desired_device) in desired.virtual_media {
            if applied.virtual_media.get(&device_id) == Some(&desired_device) {
                continue;
            }
            self.apply_virtual_media(&desired_device)
                .map_err(|error| error.to_string())?;
            applied.virtual_media.insert(device_id, desired_device);
        }
        Ok(())
    }
}

impl From<&SingleSystemState> for AppliedState {
    fn from(system: &SingleSystemState) -> Self {
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

impl Callbacks for LibvirtCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        match self.virsh(&["domstate", &self.config.domain]) {
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

    fn send_power_command(
        &self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        use SystemPowerControl::*;
        // Only a cold start loads the saved domain XML. Reboot and reset keep
        // the running domain's boot configuration and their existing semantics.
        if matches!(reset_type, On | ForceOn | PowerCycle) {
            self.reapply_effective_boot_order()?;
        }
        match reset_type {
            On | ForceOn => self.start(),
            GracefulShutdown => self.domain_command("shutdown"),
            ForceOff => self.domain_command("destroy"),
            GracefulRestart => self.domain_command("reboot"),
            ForceRestart => self.domain_command("reset"),
            PowerCycle => {
                self.domain_command("destroy")?;
                self.start()
            }
            Pause => self.domain_command("suspend"),
            Resume => self.domain_command("resume"),
            Nmi => self.domain_command("inject-nmi"),
            PushPowerButton | Suspend => Err(SetSystemPowerError::BadRequest(format!(
                "libvirt backend does not support {reset_type:?}"
            ))),
        }
    }

    fn state_refresh_indication(&self) {
        let Some(system_state) = self.system_state.get().and_then(Weak::upgrade) else {
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
        if let Err(error) = self.reconcile_state(AppliedState::from(controlled_system)) {
            tracing::error!(
                domain = %self.config.domain,
                error = %error,
                "could not reconcile libvirt domain with BMC mock state",
            );
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
                let host = url.host_str().ok_or_else(|| {
                    VirtualMediaError::BadRequest(format!("virtual media URL has no host: {image}"))
                })?;
                Ok(Self::Network {
                    protocol: url.scheme().to_string(),
                    host: host.to_string(),
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
    use std::borrow::Cow;
    use std::fs;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::host_info;
    use crate::{HardwareType, MachineRouterOptions, VirtualMediaDeviceConfig, machine_router};

    #[tokio::test]
    async fn hpe_persistent_boot_settings_reconcile_libvirt() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let virsh_path = directory.path().join("virsh");
        let defined_xml_path = directory.path().join("defined.xml");
        fs::write(
            &virsh_path,
            format!(
                r#"#!/bin/sh
case "$3" in
  dumpxml) printf '<domain><os><type>hvm</type><boot dev="hd"/></os><devices/></domain>\n' ;;
  define) cp "$4" '{}' ;;
  *) exit 1 ;;
esac
"#,
                defined_xml_path.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&virsh_path, fs::Permissions::from_mode(0o755)).unwrap();
        let callbacks = Arc::new(LibvirtCallbacks::new(Config {
            virsh_path,
            uri: "qemu:///system".to_string(),
            domain: "hpe-node".to_string(),
            virtual_media_targets: BTreeMap::new(),
        }));
        let (router, state) = machine_router(
            &host_info(HardwareType::HpeProliantDl380aGen11),
            callbacks.clone(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        callbacks.bind_state(&state).unwrap();
        assert!(
            fs::read_to_string(&defined_xml_path)
                .unwrap()
                .contains("<boot dev=\"network\"/><boot dev=\"hd\"/>")
        );

        for (order, expected_xml, expected_selection) in [
            (
                ["HD.BootOption.Boot0001", "NIC.BootOption.Boot0000"],
                "<boot dev=\"hd\"/>",
                BootOptionKind::Disk,
            ),
            (
                ["NIC.BootOption.Boot0000", "HD.BootOption.Boot0001"],
                "<boot dev=\"network\"/><boot dev=\"hd\"/>",
                BootOptionKind::Network,
            ),
            (
                ["HD.BootOption.Boot9999", "HD.BootOption.Boot0001"],
                "<boot dev=\"hd\"/>",
                BootOptionKind::Disk,
            ),
        ] {
            let status = request(
                &router,
                Method::PATCH,
                "/redfish/v1/Systems/1/Bios/oem/hpe/boot/settings",
                json!({"PersistentBootConfigOrder": order}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                state.system_state.resolve_current_boot_selection(),
                Some(expected_selection)
            );
            let xml = fs::read_to_string(&defined_xml_path).unwrap();
            assert!(xml.contains(expected_xml), "unexpected domain XML: {xml}");
            if expected_selection == BootOptionKind::Disk {
                assert!(!xml.contains("<boot dev=\"network\"/>"));
            }
        }
    }

    async fn request(
        router: &Router,
        method: Method,
        uri: &str,
        body: serde_json::Value,
    ) -> StatusCode {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        response.status()
    }

    #[cfg(unix)]
    #[test]
    fn bind_state_can_retry_after_initial_boot_sync_failure() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let virsh_path = directory.path().join("virsh");
        let fail_define_path = directory.path().join("fail-define");
        let defined_xml_path = directory.path().join("defined.xml");
        fs::write(&fail_define_path, "").unwrap();
        fs::write(
            &virsh_path,
            format!(
                r#"#!/bin/sh
case "$3" in
  dumpxml) printf '<domain><os><type>hvm</type><boot dev="hd"/></os><devices/></domain>\n' ;;
  define)
    if [ -e '{}' ]; then
      rm '{}'
      exit 1
    fi
    cp "$4" '{}'
    ;;
  *) exit 1 ;;
esac
"#,
                fail_define_path.display(),
                fail_define_path.display(),
                defined_xml_path.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&virsh_path, fs::Permissions::from_mode(0o755)).unwrap();
        let callbacks = Arc::new(LibvirtCallbacks::new(Config {
            virsh_path,
            uri: "qemu:///system".to_string(),
            domain: "retry-node".to_string(),
            virtual_media_targets: BTreeMap::new(),
        }));
        let (_router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            callbacks.clone(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );

        assert!(callbacks.bind_state(&state).is_err());
        callbacks.bind_state(&state).unwrap();
        assert_eq!(
            callbacks.bind_state(&state).unwrap_err(),
            "libvirt backend state is already bound"
        );
        assert!(
            fs::read_to_string(&defined_xml_path)
                .unwrap()
                .contains("<boot dev=\"network\"/><boot dev=\"hd\"/>")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn successful_start_consumes_once_when_boot_restoration_fails() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let virsh_path = directory.path().join("virsh");
        let state_path = directory.path().join("domain-state");
        let fail_restore_path = directory.path().join("fail-restore");
        let defined_xml_path = directory.path().join("defined.xml");
        let active_xml_path = directory.path().join("active.xml");
        fs::write(&state_path, "shut off\n").unwrap();
        fs::write(
            &virsh_path,
            format!(
                r#"#!/bin/sh
case "$3" in
  domstate) cat '{}' ;;
  start) printf 'running\n' > '{}'; cp '{}' '{}' ;;
  dumpxml) printf '<domain><os><type>hvm</type><boot dev="hd"/></os><devices/></domain>\n' ;;
  define)
    if [ -e '{}' ] && [ "$(cat '{}')" = running ]; then
      exit 1
    fi
    cp "$4" '{}'
    ;;
  *) exit 1 ;;
esac
"#,
                state_path.display(),
                state_path.display(),
                defined_xml_path.display(),
                active_xml_path.display(),
                fail_restore_path.display(),
                state_path.display(),
                defined_xml_path.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&virsh_path, fs::Permissions::from_mode(0o755)).unwrap();
        let callbacks = Arc::new(LibvirtCallbacks::new(Config {
            virsh_path,
            uri: "qemu:///system".to_string(),
            domain: "restore-failure-node".to_string(),
            virtual_media_targets: BTreeMap::new(),
        }));
        let (router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            callbacks.clone(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        callbacks.bind_state(&state).unwrap();
        let system = "/redfish/v1/Systems/System.Embedded.1";
        let status = request(
            &router,
            Method::PATCH,
            system,
            json!({
                "Boot": {
                    "BootSourceOverrideEnabled": "Once",
                    "BootSourceOverrideTarget": "Hdd",
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        fs::write(&fail_restore_path, "").unwrap();

        let status = request(
            &router,
            Method::POST,
            &format!("{system}/Actions/ComputerSystem.Reset"),
            json!({"ResetType": "On"}),
        )
        .await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let active_xml = fs::read_to_string(&active_xml_path).unwrap();
        assert!(active_xml.contains("<boot dev=\"hd\"/>"));
        assert!(!active_xml.contains("<boot dev=\"network\"/>"));
        assert_eq!(
            state
                .system_state
                .controlled_system()
                .unwrap()
                .boot_source_override()["BootSourceOverrideEnabled"],
            "Disabled"
        );
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
    fn builds_http_virtual_media_device() {
        let actual =
            virtual_media_xml("Cd", "sdb", "http://127.0.0.1:8080/installer.iso", true).unwrap();

        assert!(actual.contains("<disk type=\"network\" device=\"cdrom\">"));
        assert!(actual.contains("<source protocol=\"http\" name=\"/installer.iso\">"));
        assert!(actual.contains("<host name=\"127.0.0.1\" port=\"8080\"/>"));
        assert!(actual.contains("<target dev=\"sdb\" bus=\"sata\"/>"));
        assert!(actual.contains("<readonly/>"));
    }

    #[test]
    fn builds_file_virtual_media_device() {
        let actual = virtual_media_xml("ConfigCd", "sdc", "/tmp/config.iso", false).unwrap();

        assert!(actual.contains("<disk type=\"file\" device=\"cdrom\">"));
        assert!(actual.contains("<source file=\"/tmp/config.iso\"/>"));
        assert!(actual.contains("<target dev=\"sdc\" bus=\"sata\"/>"));
        assert!(!actual.contains("<readonly/>"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn state_refresh_drives_the_configured_domain() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let virsh_path = directory.path().join("virsh");
        let log_path = directory.path().join("virsh.log");
        let defined_xml_path = directory.path().join("defined.xml");
        let attached_xml_path = directory.path().join("attached.xml");
        let active_xml_path = directory.path().join("active.xml");
        let state_path = directory.path().join("domain-state");
        fs::write(&state_path, "shut off\n").unwrap();
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$3" in
  domstate) cat '{}' ;;
  start) printf 'running\n' > '{}'; cp '{}' '{}' ;;
  reset|reboot) [ "$(cat '{}')" = running ] ;;
  destroy) printf 'shut off\n' > '{}' ;;
  dumpxml) printf '<domain><os><type>hvm</type><boot dev="hd"/></os><devices/></domain>\n' ;;
  domblklist) printf 'Type Device Target Source\nnetwork cdrom sdb http://example/old.iso\n' ;;
  define) cp "$4" '{}' ;;
  attach-device) cp "$5" '{}' ;;
esac
"#,
            log_path.display(),
            state_path.display(),
            state_path.display(),
            defined_xml_path.display(),
            active_xml_path.display(),
            state_path.display(),
            state_path.display(),
            defined_xml_path.display(),
            attached_xml_path.display(),
        );
        fs::write(&virsh_path, script).unwrap();
        let mut permissions = fs::metadata(&virsh_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&virsh_path, permissions).unwrap();

        let callbacks = Arc::new(LibvirtCallbacks::new(Config {
            virsh_path,
            uri: "qemu:///system".to_string(),
            domain: "dsx-node".to_string(),
            virtual_media_targets: BTreeMap::from([("Cd".to_string(), "sdb".to_string())]),
        }));
        let (router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            callbacks.clone(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions {
                event_service: crate::EventServiceOverride::Profile,
                bmc_reset_duration: None,
                virtual_media_devices: Some(vec![VirtualMediaDeviceConfig {
                    id: Cow::Borrowed("Cd"),
                    name: Cow::Borrowed("Operating System Virtual CD"),
                    media_types: vec![Cow::Borrowed("CD"), Cow::Borrowed("DVD")],
                }]),
            },
        );
        callbacks.bind_state(&state).unwrap();
        let system = "/redfish/v1/Systems/System.Embedded.1";

        let defined_xml = fs::read_to_string(&defined_xml_path).unwrap();
        assert!(defined_xml.contains("<boot dev=\"network\"/><boot dev=\"hd\"/>"));
        let bind_log = fs::read_to_string(&log_path).unwrap();
        assert!(!bind_log.contains("domblklist"));

        fs::write(&log_path, "").unwrap();
        let status = request(
            &router,
            Method::POST,
            &format!("{system}/Actions/ComputerSystem.Reset"),
            json!({"ResetType": "On"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let power_log = fs::read_to_string(&log_path).unwrap();
        let define = power_log
            .find(" define ")
            .expect("persistent boot order was not re-applied before power on");
        let start = power_log.find(" start ").expect("start was not sent");
        assert!(define < start);

        let status = request(
            &router,
            Method::PATCH,
            system,
            json!({"Boot": {"BootOrder": ["Boot0001", "Boot0000"]}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let defined_xml = fs::read_to_string(&defined_xml_path).unwrap();
        assert!(defined_xml.contains("<boot dev=\"hd\"/>"));
        assert!(!defined_xml.contains("<boot dev=\"network\"/>"));

        // The saved disk-first selection must not turn a warm restart into a
        // cold start. The fixture loads saved XML only when start is called.
        for (reset_type, command) in [("ForceRestart", "reset"), ("GracefulRestart", "reboot")] {
            fs::write(&log_path, "").unwrap();
            let status = request(
                &router,
                Method::POST,
                &format!("{system}/Actions/ComputerSystem.Reset"),
                json!({"ResetType": reset_type}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let power_log = fs::read_to_string(&log_path).unwrap();
            assert!(power_log.contains(&format!(" {command} dsx-node")));
            for unexpected in [" define ", " destroy ", " start "] {
                assert!(
                    !power_log.contains(unexpected),
                    "unexpected {unexpected} during {reset_type}"
                );
            }
            assert!(
                fs::read_to_string(&active_xml_path)
                    .unwrap()
                    .contains("<boot dev=\"network\"/><boot dev=\"hd\"/>")
            );
            assert!(
                !fs::read_to_string(&defined_xml_path)
                    .unwrap()
                    .contains("<boot dev=\"network\"/>")
            );
        }

        fs::write(&log_path, "").unwrap();
        let status = request(
            &router,
            Method::POST,
            &format!("{system}/Actions/ComputerSystem.Reset"),
            json!({"ResetType": "PowerCycle"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let power_log = fs::read_to_string(&log_path).unwrap();
        let define = power_log
            .find(" define ")
            .expect("boot order was not reapplied");
        let destroy = power_log.find(" destroy ").expect("power off was not sent");
        let start = power_log.find(" start ").expect("power on was not sent");
        assert!(define < destroy && destroy < start);
        let active_xml = fs::read_to_string(&active_xml_path).unwrap();
        assert!(active_xml.contains("<boot dev=\"hd\"/>"));
        assert!(!active_xml.contains("<boot dev=\"network\"/>"));

        let status = request(
            &router,
            Method::PATCH,
            system,
            json!({"Boot": {"BootOrder": ["Boot0000", "Boot0001"]}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let defined_xml = fs::read_to_string(&defined_xml_path).unwrap();
        assert!(defined_xml.contains("<boot dev=\"network\"/><boot dev=\"hd\"/>"));

        let status = request(
            &router,
            Method::PATCH,
            system,
            json!({
                "Boot": {
                    "BootSourceOverrideEnabled": "Once",
                    "BootSourceOverrideTarget": "Cd",
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let status = request(
            &router,
            Method::POST,
            &format!("{system}/Actions/ComputerSystem.Reset"),
            json!({"ResetType": "PowerCycle"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let active_xml = fs::read_to_string(&active_xml_path).unwrap();
        assert!(active_xml.contains("<boot dev=\"cdrom\"/><boot dev=\"hd\"/>"));
        let defined_xml = fs::read_to_string(&defined_xml_path).unwrap();
        assert!(defined_xml.contains("<boot dev=\"network\"/><boot dev=\"hd\"/>"));
        assert!(!defined_xml.contains("<boot dev=\"cdrom\"/>"));

        // Disk can also be a one-shot override when the persistent selection
        // is network-first. The guest starts disk-first, but the saved domain
        // must be restored for the next cold start.
        let status = request(
            &router,
            Method::PATCH,
            system,
            json!({
                "Boot": {
                    "BootSourceOverrideEnabled": "Once",
                    "BootSourceOverrideTarget": "Hdd",
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let status = request(
            &router,
            Method::POST,
            &format!("{system}/Actions/ComputerSystem.Reset"),
            json!({"ResetType": "PowerCycle"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let active_xml = fs::read_to_string(&active_xml_path).unwrap();
        assert!(active_xml.contains("<boot dev=\"hd\"/>"));
        assert!(!active_xml.contains("<boot dev=\"network\"/>"));
        let defined_xml = fs::read_to_string(&defined_xml_path).unwrap();
        assert!(defined_xml.contains("<boot dev=\"network\"/><boot dev=\"hd\"/>"));

        // A one-shot override is consumed by the successful start. A second
        // cold start without another PATCH must use the persistent selection.
        let status = request(
            &router,
            Method::POST,
            &format!("{system}/Actions/ComputerSystem.Reset"),
            json!({"ResetType": "PowerCycle"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let active_xml = fs::read_to_string(&active_xml_path).unwrap();
        assert!(
            active_xml.contains("<boot dev=\"network\"/><boot dev=\"hd\"/>"),
            "one-shot disk override was reapplied: {active_xml}"
        );
        assert_eq!(
            state
                .system_state
                .controlled_system()
                .unwrap()
                .boot_source_override()["BootSourceOverrideEnabled"],
            "Disabled"
        );

        let status = request(
            &router,
            Method::POST,
            &format!("{system}/VirtualMedia/Cd/Actions/VirtualMedia.InsertMedia"),
            json!({
                "Image": "http://127.0.0.1:8080/installer.iso",
                "WriteProtected": true,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let status = request(
            &router,
            Method::POST,
            &format!("{system}/VirtualMedia/Cd/Actions/VirtualMedia.EjectMedia"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let status = request(
            &router,
            Method::PATCH,
            system,
            json!({
                "Boot": {
                    "BootSourceOverrideMode": "UEFI",
                    "BootSourceOverrideEnabled": "Disabled",
                    "BootSourceOverrideTarget": "None",
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let log = fs::read_to_string(log_path).unwrap();
        assert!(log.contains("--connect qemu:///system dumpxml --inactive dsx-node"));
        assert!(log.contains("--connect qemu:///system start dsx-node"));
        assert!(log.contains("--connect qemu:///system detach-disk dsx-node sdb --persistent"));
        assert!(log.contains("--connect qemu:///system attach-device dsx-node"));
        let attached_xml = fs::read_to_string(attached_xml_path).unwrap();
        assert!(attached_xml.contains("protocol=\"http\""));
        assert!(attached_xml.contains("dev=\"sdb\""));
        let defined_xml = fs::read_to_string(defined_xml_path).unwrap();
        assert!(defined_xml.contains("<boot dev=\"network\"/><boot dev=\"hd\"/>"));
        assert!(!defined_xml.contains("<boot dev=\"cdrom\"/>"));
    }
}
