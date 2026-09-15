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
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;

use super::dhcp_factory::RELAY_IP;

const KEA_READY_TIMEOUT: Duration = Duration::from_secs(15);
const KEA_START_ATTEMPTS: usize = 5;
const KEA_EXIT_SETTLE: Duration = Duration::from_millis(100);

// Kea binds loopback DHCP ports and loads the same hook library in each test.
// Within one test process, keep one Kea child alive at a time to avoid
// CI-only loopback/startup flakes. Separate test binaries can still run Kea
// concurrently; dynamic ports and startup retries handle that case. The condvar
// keeps waiters asleep without holding the mutex for a full test.
static KEA_RUN_GATE: OnceLock<KeaRunGate> = OnceLock::new();

struct KeaRunGate {
    state: Mutex<KeaRunState>,
    available: Condvar,
}

#[derive(Default)]
struct KeaRunState {
    running: bool,
}

struct KeaRunPermit {
    gate: &'static KeaRunGate,
}

impl KeaRunGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(KeaRunState::default()),
            available: Condvar::new(),
        }
    }

    fn acquire(&'static self) -> KeaRunPermit {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.running {
            state = self
                .available
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        state.running = true;

        KeaRunPermit { gate: self }
    }

    fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.running = false;
        self.available.notify_one();
    }
}

impl Drop for KeaRunPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

pub(crate) struct Kea {
    temp_conf_file: PathBuf,

    dhcp_in_port: u16,
    dhcp_out_port: u16,
    dhcp_in_port_reservation: Option<UdpSocket>,

    // Hold this around so that when Kea is dropped, TempDir is dropped and cleaned up
    temp_base_directory: TempDir,

    process: Option<Child>,
    logs: Arc<Mutex<Vec<String>>>,
    log_readers: Vec<thread::JoinHandle<()>>,
    _run_permit: Option<KeaRunPermit>,
}

impl Kea {
    /// Reserve dynamic DHCP ports, start Kea, and return it with a connected
    /// relay socket. The Kea process is stopped when the harness drops. Tests
    /// that inspect Kea's memfile can pass an externally-owned lease path.
    pub(crate) fn start(
        api_server_url: &str,
        lease_file: Option<&Path>,
    ) -> Result<(Kea, UdpSocket), eyre::Report> {
        // Acquire before reserving ports so waiters do not hold loopback port
        // reservations while another Kea test is running in this process.
        let run_permit = KEA_RUN_GATE.get_or_init(KeaRunGate::new).acquire();
        let relay_socket = UdpSocket::bind(format!("{RELAY_IP}:0"))?;
        let dhcp_out_port = relay_socket.local_addr()?.port();
        let (dhcp_in_port, dhcp_in_port_reservation) = Self::reserve_dhcp_in_port()?;

        let mut kea = Kea::new_inner(
            api_server_url,
            dhcp_in_port,
            dhcp_out_port,
            Some(dhcp_in_port_reservation),
            lease_file,
        )?;
        kea.run(run_permit)?;
        relay_socket.connect(format!("127.0.0.1:{}", kea.dhcp_in_port))?;

        Ok((kea, relay_socket))
    }

    fn new_inner(
        api_server_url: &str,
        dhcp_in_port: u16,
        dhcp_out_port: u16,
        dhcp_in_port_reservation: Option<UdpSocket>,
        lease_file: Option<&Path>,
    ) -> Result<Kea, eyre::Report> {
        let temp_base_directory = tempfile::tempdir()?;

        let temp_conf_file = temp_base_directory.path().join("kea-dhcp4.conf");
        let lease_file = lease_file
            .map(Path::to_path_buf)
            .unwrap_or_else(|| temp_base_directory.path().join("kea-leases4.csv"));

        let mut temp_conf_fd = File::create(&temp_conf_file)?;
        temp_conf_fd.write_all(Kea::config(api_server_url, &lease_file).as_bytes())?;

        // Close the file so it's updated for Kea.
        drop(temp_conf_fd);

        Ok(Kea {
            temp_conf_file,
            temp_base_directory,
            dhcp_in_port,
            dhcp_out_port,
            dhcp_in_port_reservation,
            process: None,
            logs: Arc::new(Mutex::new(Vec::new())),
            log_readers: Vec::new(),
            _run_permit: None,
        })
    }

    fn reserve_dhcp_in_port() -> Result<(u16, UdpSocket), eyre::Report> {
        let dhcp_in_port_reservation = UdpSocket::bind("0.0.0.0:0")?;
        let dhcp_in_port = dhcp_in_port_reservation.local_addr()?.port();

        Ok((dhcp_in_port, dhcp_in_port_reservation))
    }

    fn refresh_dhcp_in_port(&mut self) -> Result<(), eyre::Report> {
        // The relay/output port stays fixed because `start` keeps its
        // socket bound; only Kea's receive port needs a fresh try.
        let (dhcp_in_port, dhcp_in_port_reservation) = Self::reserve_dhcp_in_port()?;

        self.dhcp_in_port = dhcp_in_port;
        self.dhcp_in_port_reservation = Some(dhcp_in_port_reservation);

        Ok(())
    }

    fn run(&mut self, run_permit: KeaRunPermit) -> Result<(), eyre::Report> {
        let mut run_permit = Some(run_permit);
        let mut last_exit = None;
        for attempt in 1..=KEA_START_ATTEMPTS {
            match self.run_once()? {
                Some(status) => {
                    last_exit = Some((self.dhcp_in_port, status));
                    if attempt == KEA_START_ATTEMPTS {
                        break;
                    }

                    println!(
                        "KEA exited before binding DHCP port {} on attempt {attempt}/{KEA_START_ATTEMPTS}: {status}; retrying with a fresh DHCP receive port",
                        self.dhcp_in_port
                    );
                    self.refresh_dhcp_in_port()?;
                }
                None => {
                    self._run_permit = run_permit.take();
                    return Ok(());
                }
            }
        }

        let (port, status) = last_exit.expect("at least one Kea start attempt should have run");
        Err(eyre::eyre!(
            "kea exited before binding DHCP port {port} after {KEA_START_ATTEMPTS} attempts: {status}"
        ))
    }

    fn run_once(&mut self) -> Result<Option<ExitStatus>, eyre::Report> {
        drop(self.dhcp_in_port_reservation.take());

        let mut process = Command::new("/usr/sbin/kea-dhcp4")
            .env("KEA_PIDFILE_DIR", self.temp_base_directory.path())
            .env("KEA_LOCKFILE_DIR", self.temp_base_directory.path())
            .arg("-c")
            .arg(self.temp_conf_file.as_os_str())
            .arg("-p")
            .arg(self.dhcp_in_port.to_string())
            .arg("-P")
            .arg(self.dhcp_out_port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = BufReader::new(process.stdout.take().unwrap());
        let stderr = BufReader::new(process.stderr.take().unwrap());
        let stdout_logs = self.logs.clone();
        self.log_readers.push(thread::spawn(move || {
            for line in stdout.lines() {
                match line {
                    Ok(line) => {
                        println!("KEA STDOUT: {line}");
                        stdout_logs.lock().unwrap().push(line);
                    }
                    Err(error) => {
                        eprintln!("failed to read Kea stdout: {error}");
                        break;
                    }
                }
            }
        }));
        let stderr_logs = self.logs.clone();
        self.log_readers.push(thread::spawn(move || {
            for line in stderr.lines() {
                match line {
                    Ok(line) => {
                        println!("KEA STDERR: {line}");
                        stderr_logs.lock().unwrap().push(line);
                    }
                    Err(error) => {
                        eprintln!("failed to read Kea stderr: {error}");
                        break;
                    }
                }
            }
        }));

        self.process = Some(process);

        // Poll until Kea binds its DHCP receive port, so the test doesn't race.
        // Trying to bind the same port ourselves: success means Kea hasn't taken it yet;
        // AddrInUse means Kea is listening and we're ready to proceed.
        let deadline = Instant::now() + KEA_READY_TIMEOUT;
        loop {
            thread::sleep(Duration::from_millis(100));
            if let Some(status) = self.process.as_mut().unwrap().try_wait()? {
                self.process = None;
                return Ok(Some(status));
            }
            match UdpSocket::bind(format!("0.0.0.0:{}", self.dhcp_in_port)) {
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    thread::sleep(KEA_EXIT_SETTLE);
                    if let Some(status) = self.process.as_mut().unwrap().try_wait()? {
                        self.process = None;
                        return Ok(Some(status));
                    }
                    break;
                }
                Ok(_) => {}
                Err(e) => return Err(eyre::eyre!("unexpected error probing kea readiness: {e}")),
            }
            if Instant::now() >= deadline {
                self.stop_process();
                return Err(eyre::eyre!(
                    "kea did not bind DHCP port {} within {KEA_READY_TIMEOUT:?}",
                    self.dhcp_in_port
                ));
            }
        }

        Ok(None)
    }

    /// Return whether the captured stdout or stderr contains `needle`.
    pub(crate) fn has_log(&self, needle: &str) -> bool {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(needle))
    }

    /// Stop Kea and wait until its captured stdout and stderr have been drained.
    pub(crate) fn stop_process(&mut self) {
        if let Some(process) = &mut self.process {
            // Rust stdlib can only send a KILL (9) to sub-process. Thankfully dhcp already depends on
            // libc so we can use that.
            // SAFETY: The unreaped Child retains its valid, unreused PID and
            // SIGTERM is a valid signal; no borrowed memory crosses the syscall.
            unsafe {
                libc::kill(process.id() as i32, libc::SIGTERM);
            }
            thread::sleep(Duration::from_millis(100));
            if let Ok(None) = process.try_wait() {
                process.kill().unwrap(); // -9
                process.wait().unwrap();
            }
        }
        self.process = None;
        for reader in self.log_readers.drain(..) {
            if reader.join().is_err() {
                eprintln!("Kea log-reader thread panicked");
            }
        }
    }

    fn config(api_server_url: &str, lease_file: &Path) -> String {
        let hook_lib = super::hook_library_path();

        let conf = json!({
        "Dhcp4": {
            "interfaces-config": {
                "interfaces": [ "lo" ],
                "dhcp-socket-type": "udp"
            },
            "lease-database": {
                "type": "memfile",
                "persist": true,
                "name": lease_file.to_string_lossy(),
                "lfc-interval": 3600
            },
            "multi-threading": {
                "enable-multi-threading": true,
                "thread-pool-size": 4,
                "packet-queue-size": 28,
                "user-context": {
                    "comment": "Values above are Kea recommendations for memfile backend",
                    "url": "https://kea.readthedocs.io/en/kea-2.2.0/arm/dhcp4-srv.html#multi-threading-settings-with-different-database-backends"
                }
            },
            "renew-timer": 900,
            "rebind-timer": 1800,
            "valid-lifetime": 3600,
            "decline-probation-period": 900,
            "hooks-libraries": [
                {
                        "library": hook_lib,
                        "parameters": {
                            "carbide-api-url": api_server_url,
                        "carbide-metrics-endpoint": "[::]:0",
                            "carbide-nameservers": "1.1.1.1,8.8.8.8",
                            "carbide-provisioning-server-ipv4": "127.0.0.1"
                        }
                }
            ],
            "subnet4": [
                {
                    "subnet": "0.0.0.0/0",
                    "pools": [{
                        "pool": "0.0.0.1-255.255.255.254"
                    }]
                }
            ],
            "user-context": {
                "comment": "Change severity below to DEBUG and run 'cargo test -- --nocapture' for verbose test output",
            },
            "loggers": [
                {
                    "name": "kea-dhcp4",
                    "output_options": [{"output": "stdout"}],
                    "severity": "WARN",
                    "debuglevel": 99
                },
                {
                    "name": "kea-dhcp4.carbide-rust",
                    "output_options": [{"output": "stdout"}],
                    "severity": "WARN",
                    "debuglevel": 10
                },
                {
                    "name": "kea-dhcp4.carbide-callouts",
                    "output_options": [{"output": "stdout"}],
                    "severity": "ERROR",
                    "debuglevel": 10
                }
            ]
        }
        });
        conf.to_string()
    }
}

impl Drop for Kea {
    fn drop(&mut self) {
        self.stop_process();
    }
}
