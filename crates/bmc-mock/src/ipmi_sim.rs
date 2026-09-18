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

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::pipe;
use tokio::net::{TcpListener as TokioTcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::redfish::account_service::PasswordUpdater;
use crate::redfish::manager::ManagerState;
use crate::{BmcState, Callbacks, SystemPowerControl};

const START_ATTEMPTS: usize = 5;
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PASSWORD_UPDATE_TIMEOUT: Duration = Duration::from_secs(10);
const IPMI_SIM_EXECUTABLE: &str = "ipmi_sim";
const CHASSIS_CONTROL_FIFO: &str = "chassis-control.fifo";

/// A byte stream supplied to the mock SOL transport for one console connection.
pub type ConsoleOutputStream = Pin<Box<dyn Stream<Item = Bytes> + Send + 'static>>;

/// Creates an independent byte stream for every accepted mock SOL connection.
pub type ConsoleOutputStreamFactory = Box<dyn Fn() -> ConsoleOutputStream + Send + 'static>;

/// Configures one `ipmi_sim` process and its mock serial-console transport.
#[derive(Debug, Clone)]
pub struct IpmiSimConfig {
    /// Stable identity used to derive the simulator GUID.
    pub stable_id: String,
    /// Prompt written after interactive input ends with a newline.
    pub console_prompt: String,
}

pub struct IpmiSimHandle {
    child: tokio::process::Child,
    _chassis_control: ChassisControl,
    _temp_dir: TempDir,
    _console: MockConsole,
    manager: Arc<ManagerState>,
    _password_updater: Arc<dyn PasswordUpdater>,
    /// UDP port on which the simulator listens and which clients must use.
    pub port: u16,
}

impl std::fmt::Debug for IpmiSimHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IpmiSimHandle")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Drop for IpmiSimHandle {
    fn drop(&mut self) {
        self.child.start_kill().ok();
        self.manager.set_ipmi_endpoint(None);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "IPMI simulation requires {executable}, but it was not found or is not executable in PATH"
    )]
    ExecutableUnavailable { executable: &'static str },
    #[error("the BMC mock has no administrative account")]
    MissingAdministrativeAccount,
    #[error("the BMC mock has no power-control callback")]
    MissingPowerControlCallback,
    #[error("the IPMI simulator {0} contains unsupported characters")]
    UnsupportedCredentialCharacters(&'static str),
    #[error("failed to prepare IPMI simulator: {0}")]
    Io(#[from] std::io::Error),
    #[error("ipmi_sim exited during startup with status {0}")]
    EarlyExit(std::process::ExitStatus),
    #[error("ipmi_sim did not claim its ports within {READY_TIMEOUT:?}")]
    ReadinessTimeout,
    #[error("ipmi_sim failed to start after {START_ATTEMPTS} attempts: {0}")]
    AttemptsExhausted(Box<Error>),
}

pub fn validate_executable() -> Result<(), Error> {
    validate_executable_in_path(std::env::var_os("PATH").as_deref())
}

fn validate_executable_in_path(path: Option<&OsStr>) -> Result<(), Error> {
    let executable_available = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(IPMI_SIM_EXECUTABLE))
        .any(|candidate| {
            candidate.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        });

    if executable_available {
        Ok(())
    } else {
        Err(Error::ExecutableUnavailable {
            executable: IPMI_SIM_EXECUTABLE,
        })
    }
}

struct Reservations {
    ipmi_sim_lan_socket: UdpSocket,
    ipmi_sim_serial_listener: TcpListener,
}

impl Reservations {
    fn new() -> Result<Self, std::io::Error> {
        Ok(Self {
            ipmi_sim_lan_socket: UdpSocket::bind(SocketAddr::new(
                IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
                0,
            ))?,
            ipmi_sim_serial_listener: TcpListener::bind((
                IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                0,
            ))?,
        })
    }

    fn ipmi_sim_lan_port(&self) -> Result<u16, std::io::Error> {
        Ok(self.ipmi_sim_lan_socket.local_addr()?.port())
    }

    fn ipmi_sim_serial_port(&self) -> Result<u16, std::io::Error> {
        Ok(self.ipmi_sim_serial_listener.local_addr()?.port())
    }
}

/// Starts an IPMI simulator with an optional output stream factory for SOL connections.
///
/// The factory is called once per accepted console connection; its bytes are forwarded unchanged.
/// Without a factory, the console only echoes interactive input and writes the configured prompt.
pub async fn start(
    state: &BmcState,
    config: IpmiSimConfig,
    console_output: Option<ConsoleOutputStreamFactory>,
) -> Result<IpmiSimHandle, Error> {
    let (username, password) = state
        .account_service_state
        .administrator_credentials()
        .ok_or(Error::MissingAdministrativeAccount)?;
    validate_credential("username", &username)?;
    validate_credential("password", &password)?;
    let temp_dir = tempfile::Builder::new()
        .prefix("bmc-mock-ipmi-")
        .tempdir()?;
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o700))?;

    let console = MockConsole::start(config.console_prompt.clone(), console_output).await?;
    let callbacks = state
        .callbacks
        .clone()
        .ok_or(Error::MissingPowerControlCallback)?;
    let chassis_control = ChassisControl::start(temp_dir.path(), callbacks)?;
    let mut last_error = None;

    for attempt in 1..=START_ATTEMPTS {
        let reservations = Reservations::new()?;
        let ipmi_sim_lan_port = reservations.ipmi_sim_lan_port()?;
        let ipmi_sim_serial_port = reservations.ipmi_sim_serial_port()?;
        let state_dir = temp_dir.path().join(format!("state-{attempt}"));
        std::fs::create_dir(&state_dir)?;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
        write_config(
            temp_dir.path(),
            &config,
            &username,
            &password,
            ipmi_sim_lan_port,
            ipmi_sim_serial_port,
            console.bmc_mock_console_port,
        )?;

        drop(reservations);
        let mut child = tokio::process::Command::new(IPMI_SIM_EXECUTABLE)
            .current_dir(temp_dir.path())
            .arg("-c")
            .arg(temp_dir.path().join("lan.conf"))
            .arg("-f")
            .arg(temp_dir.path().join("cmd.conf"))
            .arg("-s")
            .arg(state_dir)
            .arg("--nostdio")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        match wait_until_ready(&mut child, ipmi_sim_lan_port, ipmi_sim_serial_port).await {
            Ok(()) => {
                let connect_ip = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
                let password_updater: Arc<dyn PasswordUpdater> = Arc::new(IpmiPasswordUpdater {
                    connect_ip,
                    ipmi_sim_lan_port,
                });
                state
                    .account_service_state
                    .set_password_updater(&password_updater);
                state.manager.set_ipmi_endpoint(Some(ipmi_sim_lan_port));
                return Ok(IpmiSimHandle {
                    child,
                    _chassis_control: chassis_control,
                    _temp_dir: temp_dir,
                    _console: console,
                    manager: state.manager.clone(),
                    _password_updater: password_updater,
                    port: ipmi_sim_lan_port,
                });
            }
            Err(error) => {
                child.kill().await.ok();
                last_error = Some(error);
                tracing::warn!(
                    attempt,
                    ipmi_sim_lan_port,
                    ipmi_sim_serial_port,
                    "ipmi_sim startup failed; retrying"
                );
            }
        }
    }

    Err(Error::AttemptsExhausted(Box::new(
        last_error.expect("at least one startup attempt ran"),
    )))
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ChassisControlEvent {
    Reset,
}

impl FromStr for ChassisControlEvent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "chassis-reset" => Ok(Self::Reset),
            _ => Err(()),
        }
    }
}

struct ChassisControl {
    task: JoinHandle<()>,
    _keepalive_sender: pipe::Sender,
}

impl Drop for ChassisControl {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ChassisControl {
    fn start(base: &Path, callbacks: Arc<dyn Callbacks>) -> Result<Self, std::io::Error> {
        let fifo_path = base.join(CHASSIS_CONTROL_FIFO);
        mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR).map_err(std::io::Error::from)?;
        let receiver = pipe::OpenOptions::new().open_receiver(&fifo_path)?;
        let keepalive_sender = pipe::OpenOptions::new().open_sender(fifo_path)?;
        let mut lines = BufReader::new(receiver).lines();
        let task = tokio::spawn(async move {
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => match line.parse::<ChassisControlEvent>() {
                        Ok(ChassisControlEvent::Reset) => {
                            if let Err(error) =
                                callbacks.send_power_command(SystemPowerControl::ForceRestart)
                            {
                                tracing::warn!(
                                    error = %error,
                                    "Failed to deliver IPMI chassis reset",
                                );
                            }
                        }
                        Err(()) => {
                            tracing::warn!(event = %line, "Ignoring unknown IPMI chassis event");
                        }
                    },
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "Stopped receiving IPMI chassis events",
                        );
                        break;
                    }
                }
            }
        });
        Ok(Self {
            task,
            _keepalive_sender: keepalive_sender,
        })
    }
}

struct IpmiPasswordUpdater {
    connect_ip: IpAddr,
    ipmi_sim_lan_port: u16,
}

impl PasswordUpdater for IpmiPasswordUpdater {
    fn update_password<'a>(
        &'a self,
        username: &'a str,
        current_password: &'a str,
        new_password: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let connect_ip = self.connect_ip.to_string();
            let ipmi_sim_lan_port = self.ipmi_sim_lan_port.to_string();
            let mut command = [
                "-I",
                "lanplus",
                "-H",
                connect_ip.as_str(),
                "-p",
                ipmi_sim_lan_port.as_str(),
                "-U",
                username,
                "-E",
                "-C",
                "3",
                "user",
                "set",
                "password",
                "3",
                new_password,
                "20",
            ]
            .into_iter()
            .fold(
                tokio::process::Command::new("ipmitool"),
                |mut command, argument| {
                    command.arg(argument);
                    command
                },
            );
            command
                .env("IPMI_PASSWORD", current_password)
                .kill_on_drop(true);
            let output = tokio::time::timeout(PASSWORD_UPDATE_TIMEOUT, command.output())
                .await
                .map_err(|_| format!("ipmitool timed out after {PASSWORD_UPDATE_TIMEOUT:?}"))?
                .map_err(|error| format!("failed to execute ipmitool: {error}"))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "ipmitool exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            }
        })
    }
}

fn validate_credential(field: &'static str, value: &str) -> Result<(), Error> {
    if value
        .chars()
        .any(|character| character == '"' || character == '\\' || character.is_control())
    {
        return Err(Error::UnsupportedCredentialCharacters(field));
    }
    Ok(())
}

fn write_config(
    base: &Path,
    config: &IpmiSimConfig,
    username: &str,
    password: &str,
    ipmi_sim_lan_port: u16,
    ipmi_sim_serial_port: u16,
    bmc_mock_console_port: u16,
) -> Result<(), std::io::Error> {
    let lan_config = format!(
        r#"name "ManagedHostBmc"
set_working_mc 0x20

startlan 1
  addr 0.0.0.0 {ipmi_sim_lan_port}
  priv_limit admin
  allowed_auths_admin none md2 md5 straight none
  guid {}
endlan

user 1 true "" "" user 10 none md2 md5 straight none
user 2 true "admin" "admin" admin 10 none md2 md5 straight none
user 3 true "{username}" "{password}" admin 10 none md2 md5 straight none

chassis_control "./chassis-control.sh 0x20"
serial 15 127.0.0.1 {ipmi_sim_serial_port} codec VM ipmb 0x20
sol "telnet:127.0.0.1:{bmc_mock_console_port}" 115200
"#,
        stable_guid(&config.stable_id),
    );

    write_private_file(&base.join("lan.conf"), lan_config.as_bytes(), 0o600)?;
    write_private_file(
        &base.join("cmd.conf"),
        include_bytes!("../../../dev/ipmi/cmd.conf"),
        0o600,
    )?;
    write_private_file(
        &base.join("chassis-control.sh"),
        include_bytes!("../../../dev/ipmi/ipmi_sim_chassiscontrol.sh"),
        0o700,
    )?;
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8], mode: u32) -> Result<(), std::io::Error> {
    std::fs::write(path, contents)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

fn stable_guid(stable_id: &str) -> String {
    let mut bytes = [0_u8; 16];
    for (index, value) in stable_id.bytes().enumerate() {
        let slot = index % bytes.len();
        bytes[slot] = bytes[slot]
            .wrapping_mul(31)
            .wrapping_add(value)
            .wrapping_add(index as u8);
    }
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}

async fn wait_until_ready(
    child: &mut tokio::process::Child,
    ipmi_sim_lan_port: u16,
    ipmi_sim_serial_port: u16,
) -> Result<(), Error> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(Error::EarlyExit(status));
        }

        if udp_port_is_claimed(ipmi_sim_lan_port)? && tcp_port_is_claimed(ipmi_sim_serial_port)? {
            tokio::time::sleep(READY_POLL_INTERVAL).await;
            if let Some(status) = child.try_wait()? {
                return Err(Error::EarlyExit(status));
            }
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(Error::ReadinessTimeout);
        }
        tokio::time::sleep(READY_POLL_INTERVAL).await;
    }
}

fn udp_port_is_claimed(port: u16) -> Result<bool, std::io::Error> {
    port_is_claimed(UdpSocket::bind(SocketAddr::new(
        IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        port,
    )))
}

fn tcp_port_is_claimed(port: u16) -> Result<bool, std::io::Error> {
    port_is_claimed(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)))
}

fn port_is_claimed<T>(result: Result<T, std::io::Error>) -> Result<bool, std::io::Error> {
    match result {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::AddrInUse => Ok(true),
        Err(error) => Err(error),
    }
}

struct MockConsole {
    bmc_mock_console_port: u16,
    task: JoinHandle<()>,
}

impl Drop for MockConsole {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockConsole {
    async fn start(
        prompt: String,
        output_factory: Option<ConsoleOutputStreamFactory>,
    ) -> Result<Self, std::io::Error> {
        let listener = TokioTcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let bmc_mock_console_port = listener.local_addr()?.port();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let prompt = prompt.clone();
                let output = output_factory.as_ref().map(|factory| factory());
                tokio::spawn(async move {
                    if let Err(error) = serve_console(stream, &prompt, output).await {
                        tracing::debug!(%error, "mock SOL console connection closed with error");
                    }
                });
            }
        });
        Ok(Self {
            bmc_mock_console_port,
            task,
        })
    }
}

async fn serve_console(
    mut stream: TcpStream,
    prompt: &str,
    mut output: Option<ConsoleOutputStream>,
) -> Result<(), std::io::Error> {
    let mut input = Vec::new();
    let mut buffer = [0_u8; 32];
    loop {
        tokio::select! {
            biased;
            length = stream.read(&mut buffer) => {
                let length = length?;
                if length == 0 {
                    return Ok(());
                }
                input.extend_from_slice(&buffer[..length]);
                stream.write_all(&buffer[..length]).await?;
                if input.ends_with(b"\n") || input.ends_with(b"\r") {
                    input.clear();
                    stream.write_all(format!("\r\n{prompt}").as_bytes()).await?;
                }
            }
            next = async { output.as_mut().expect("branch guard requires output").next().await },
                if output.is_some() =>
            {
                match next {
                    Some(bytes) => stream.write_all(&bytes).await?,
                    None => output = None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use bytes::Bytes;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::Notify;

    use super::{
        ChassisControlEvent, ConsoleOutputStream, ConsoleOutputStreamFactory, Error,
        IPMI_SIM_EXECUTABLE, IpmiSimConfig, MockConsole, stable_guid, start, validate_credential,
        validate_executable_in_path,
    };
    use crate::{Callbacks, MockPowerState, SetSystemPowerError, SystemPowerControl};

    #[derive(Debug, Default)]
    struct RecordingCallbacks {
        commands: Mutex<Vec<SystemPowerControl>>,
        command_received: Notify,
    }

    impl RecordingCallbacks {
        async fn wait_for_command_count(&self, expected_count: usize) {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let command_received = self.command_received.notified();
                    if self.commands.lock().unwrap().len() >= expected_count {
                        return;
                    }
                    command_received.await;
                }
            })
            .await
            .expect("timed out waiting for chassis reset callback");
        }
    }

    impl Callbacks for RecordingCallbacks {
        fn get_power_state(&self) -> MockPowerState {
            MockPowerState::On
        }

        fn send_power_command(
            &self,
            request: SystemPowerControl,
        ) -> Result<(), SetSystemPowerError> {
            self.commands.lock().unwrap().push(request);
            self.command_received.notify_one();
            Ok(())
        }

        fn state_refresh_indication(&self) {}
    }

    #[test]
    fn ipmi_sim_executable_is_required() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_dir = temp_dir.path().join("missing");
        let non_executable_dir = temp_dir.path().join("non-executable");
        let executable_dir = temp_dir.path().join("executable");
        fs::create_dir_all(&missing_dir).unwrap();
        fs::create_dir_all(&non_executable_dir).unwrap();
        fs::create_dir_all(&executable_dir).unwrap();

        fs::write(non_executable_dir.join(IPMI_SIM_EXECUTABLE), []).unwrap();

        let executable = executable_dir.join(IPMI_SIM_EXECUTABLE);
        fs::write(&executable, []).unwrap();
        let mut permissions = executable.metadata().unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();

        for (scenario, path, expected) in [
            ("missing executable", missing_dir, false),
            ("non-executable file", non_executable_dir, false),
            ("executable file", executable_dir, true),
        ] {
            assert_eq!(
                validate_executable_in_path(Some(path.as_os_str())).is_ok(),
                expected,
                "{scenario}"
            );
        }
    }

    #[test]
    fn stable_guid_is_stable_and_has_ipmi_length() {
        assert_eq!(stable_guid("machine-1"), stable_guid("machine-1"));
        assert_ne!(stable_guid("machine-1"), stable_guid("machine-2"));
        assert_eq!(stable_guid("machine-1").len(), 32);
    }

    #[test]
    fn chassis_control_event_parsing() {
        for (value, expected) in [
            ("chassis-reset", Ok(ChassisControlEvent::Reset)),
            ("", Err(())),
            ("reset", Err(())),
        ] {
            assert_eq!(value.parse(), expected, "{value:?}");
        }
    }

    #[tokio::test]
    async fn starting_without_power_control_callback_fails() {
        let bmc = crate::test_support::generic_supermicro_bmc().await;
        let mut state = bmc.state;
        state.callbacks = None;
        state
            .account_service_state
            .change_factory_default_password("password");

        let error = start(
            &state,
            IpmiSimConfig {
                stable_id: "missing-callback".to_string(),
                console_prompt: "root@bmc-mock # ".to_string(),
            },
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::MissingPowerControlCallback));
    }

    #[tokio::test]
    async fn real_ipmitool_resets_chassis() {
        let bmc = crate::test_support::generic_supermicro_bmc().await;
        let mut state = bmc.state;
        state
            .account_service_state
            .change_factory_default_password("password");
        let callbacks = Arc::new(RecordingCallbacks::default());
        state.callbacks = Some(callbacks.clone());
        let simulator = start(
            &state,
            IpmiSimConfig {
                stable_id: "chassis-reset".to_string(),
                console_prompt: "root@bmc-mock # ".to_string(),
            },
            None,
        )
        .await
        .unwrap();
        let port = simulator.port.to_string();
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("ipmitool")
                .args([
                    "-I",
                    "lanplus",
                    "-C",
                    "3",
                    "-H",
                    "127.0.0.1",
                    "-p",
                    &port,
                    "-U",
                    "root",
                    "-E",
                    "chassis",
                    "power",
                    "reset",
                ])
                .env("IPMI_PASSWORD", "password")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("ipmitool timed out")
        .expect("failed to execute ipmitool");
        assert!(
            output.status.success(),
            "ipmitool failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        callbacks.wait_for_command_count(1).await;

        assert_eq!(
            *callbacks.commands.lock().unwrap(),
            vec![SystemPowerControl::ForceRestart]
        );
    }

    #[test]
    fn credential_validation_rejects_ipmi_sim_syntax_characters() {
        for value in [
            "double\"quote",
            "back\\slash",
            "line\nfeed",
            "carriage\rreturn",
            "nul\0byte",
        ] {
            assert!(matches!(
                validate_credential("password", value),
                Err(Error::UnsupportedCredentialCharacters("password"))
            ));
        }
    }

    #[test]
    fn credential_validation_accepts_plain_credentials() {
        assert!(validate_credential("username", "root-admin").is_ok());
        assert!(validate_credential("password", "Welcome123! @$%^&*()").is_ok());
    }

    #[tokio::test]
    async fn dropping_mock_console_releases_listener() {
        let console = MockConsole::start("prompt".to_string(), None)
            .await
            .unwrap();
        let port = console.bmc_mock_console_port;

        drop(console);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)) {
                    Ok(_) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("failed to probe console listener: {error}"),
                }
            }
        })
        .await
        .expect("mock console listener was not released");
    }

    #[tokio::test]
    async fn mock_console_forwards_supplied_bytes_and_preserves_interaction() {
        let output_factory: ConsoleOutputStreamFactory = Box::new(|| {
            Box::pin(futures::stream::iter([Bytes::from_static(
                b"machine-owned output\r\n",
            )])) as ConsoleOutputStream
        });
        let console = MockConsole::start("prompt>".to_string(), Some(output_factory))
            .await
            .unwrap();
        let mut connection =
            TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, console.bmc_mock_console_port))
                .await
                .unwrap();
        connection.write_all(b"probe\r").await.unwrap();

        let output = tokio::time::timeout(Duration::from_secs(1), async {
            let mut output = Vec::new();
            let mut buffer = [0; 128];
            loop {
                let length = connection.read(&mut buffer).await.unwrap();
                assert_ne!(length, 0, "mock console disconnected early");
                output.extend_from_slice(&buffer[..length]);
                if output
                    .windows(b"machine-owned output\r\n".len())
                    .any(|window| window == b"machine-owned output\r\n")
                    && output
                        .windows(b"probe\r\r\nprompt>".len())
                        .any(|window| window == b"probe\r\r\nprompt>")
                {
                    return output;
                }
            }
        })
        .await
        .expect("mock console did not forward output and prompt");

        assert!(
            output
                .windows(b"machine-owned output\r\n".len())
                .any(|window| window == b"machine-owned output\r\n")
        );
    }
}
