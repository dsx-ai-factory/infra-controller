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
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;

use super::dhcpv6_factory::RELAY_ADDR;

const KEA_READY_TIMEOUT: Duration = Duration::from_secs(15);
const KEA_START_ATTEMPTS: usize = 5;
const KEA_EXIT_SETTLE: Duration = Duration::from_millis(100);
const KEA_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const KEA_SHUTDOWN_POLL: Duration = Duration::from_millis(50);
const METRICS_READY_CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
/// DHCPv6 DNS servers configured through hook context.
pub(super) const HOOK_DNS_SERVERS_IPV6: [&str; 1] = ["2001:db8::53"];
/// DHCPv6 NTP servers configured through hook context.
pub(super) const HOOK_NTP_SERVERS_IPV6: [&str; 1] = ["2001:db8::123"];

// Real Kea children share process-global hook/logger/metrics state through the
// loaded cdylib, so serialize them instead of running multiple daemons at once.
static KEA6_RUN_GATE: OnceLock<Kea6RunGate> = OnceLock::new();

/// Kea expired-lease processing knobs used by tests that force quick reclamation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Kea6ExpiredLeasesProcessing {
    pub(crate) reclaim_timer_wait_time: u16,
    pub(crate) flush_reclaimed_timer_wait_time: u16,
    pub(crate) hold_reclaimed_time: u32,
    pub(crate) max_reclaim_leases: u32,
    pub(crate) max_reclaim_time: u16,
    pub(crate) unwarned_reclaim_cycles: u16,
}

/// Runtime configuration overrides for the DHCPv6 integration-test Kea process.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Kea6Config {
    pub(crate) preferred_lifetime: u32,
    pub(crate) valid_lifetime: u32,
    pub(crate) renew_timer: u32,
    pub(crate) rebind_timer: u32,
    pub(crate) rapid_commit_v6: bool,
    pub(crate) kea_rapid_commit_v6: bool,
    pub(crate) mac_sources: Option<&'static [&'static str]>,
    pub(crate) expired_leases_processing: Option<Kea6ExpiredLeasesProcessing>,
}

impl Default for Kea6Config {
    fn default() -> Self {
        Self {
            preferred_lifetime: 3600,
            valid_lifetime: 7200,
            renew_timer: 1800,
            rebind_timer: 2880,
            rapid_commit_v6: false,
            kea_rapid_commit_v6: true,
            mac_sources: None,
            expired_leases_processing: None,
        }
    }
}

struct Kea6RunGate {
    state: Mutex<Kea6RunState>,
    available: Condvar,
}

#[derive(Default)]
struct Kea6RunState {
    running: bool,
}

struct Kea6RunPermit {
    gate: &'static Kea6RunGate,
}

impl Kea6RunGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(Kea6RunState::default()),
            available: Condvar::new(),
        }
    }

    fn acquire(&'static self) -> Kea6RunPermit {
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

        Kea6RunPermit { gate: self }
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

impl Drop for Kea6RunPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

pub(crate) struct Kea6 {
    temp_conf_file: PathBuf,
    dhcp_in_port: u16,
    dhcp_out_port: u16,
    dhcp_in_port_reservation: Option<UdpSocket>,
    metrics_endpoint: SocketAddr,
    metrics_endpoint_reservation: Option<TcpListener>,
    temp_base_directory: TempDir,
    process: Option<Child>,
    logs: Arc<Mutex<Vec<String>>>,
    _run_permit: Option<Kea6RunPermit>,
}

struct Kea6Ports {
    dhcp_in_port: u16,
    dhcp_out_port: u16,
}

impl Kea6 {
    /// Reserve dynamic DHCPv6 ports, start Kea, and return a connected relay socket.
    pub(crate) fn start(
        api_server_url: &str,
        lease_file: Option<&Path>,
    ) -> Result<(Kea6, UdpSocket), eyre::Report> {
        Self::start_with_config(api_server_url, lease_file, Kea6Config::default())
    }

    /// Start Kea with launch-time overrides such as lifetimes, mac-sources, and expiry processing.
    pub(crate) fn start_with_config(
        api_server_url: &str,
        lease_file: Option<&Path>,
        config: Kea6Config,
    ) -> Result<(Kea6, UdpSocket), eyre::Report> {
        // Acquire the gate before reserving ports so blocked tests do not hold
        // loopback DHCP/metrics reservations while another Kea child runs.
        let run_permit = KEA6_RUN_GATE.get_or_init(Kea6RunGate::new).acquire();

        // Reserve both ends of the relayed test flow before writing config so
        // Kea's command-line receive port and relay reply port stay paired.
        let relay_socket = UdpSocket::bind(format!("[{RELAY_ADDR}]:0"))?;
        let dhcp_out_port = relay_socket.local_addr()?.port();
        let (dhcp_in_port, dhcp_in_port_reservation) = Self::reserve_dhcp_in_port()?;
        let (metrics_endpoint, metrics_endpoint_reservation) = Self::reserve_metrics_endpoint()?;

        let mut kea = Kea6::new_inner(
            api_server_url,
            Kea6Ports {
                dhcp_in_port,
                dhcp_out_port,
            },
            Some(dhcp_in_port_reservation),
            metrics_endpoint,
            Some(metrics_endpoint_reservation),
            lease_file,
            config,
        )?;
        kea.run(run_permit)?;
        relay_socket.connect(format!("[{RELAY_ADDR}]:{}", kea.dhcp_in_port))?;

        Ok((kea, relay_socket))
    }

    fn new_inner(
        api_server_url: &str,
        ports: Kea6Ports,
        dhcp_in_port_reservation: Option<UdpSocket>,
        metrics_endpoint: SocketAddr,
        metrics_endpoint_reservation: Option<TcpListener>,
        lease_file: Option<&Path>,
        config: Kea6Config,
    ) -> Result<Kea6, eyre::Report> {
        let temp_base_directory = tempfile::tempdir()?;
        let temp_conf_file = temp_base_directory.path().join("kea-dhcp6.conf");

        // Use a caller-supplied memfile only when a test needs to inspect
        // lease persistence across Kea restarts.
        let lease_file = lease_file
            .map(Path::to_path_buf)
            .unwrap_or_else(|| temp_base_directory.path().join("kea-leases6.csv"));

        let mut temp_conf_fd = File::create(&temp_conf_file)?;
        temp_conf_fd.write_all(
            Kea6::config(api_server_url, &lease_file, metrics_endpoint, config).as_bytes(),
        )?;

        // Close the config before spawning Kea so the child reads complete JSON.
        drop(temp_conf_fd);

        Ok(Kea6 {
            temp_conf_file,
            temp_base_directory,
            dhcp_in_port: ports.dhcp_in_port,
            dhcp_out_port: ports.dhcp_out_port,
            dhcp_in_port_reservation,
            metrics_endpoint,
            metrics_endpoint_reservation,
            process: None,
            logs: Arc::new(Mutex::new(Vec::new())),
            _run_permit: None,
        })
    }

    fn reserve_dhcp_in_port() -> Result<(u16, UdpSocket), eyre::Report> {
        // Hold the port until the child process starts so parallel local
        // activity is less likely to steal it between config generation and
        // Kea's bind.
        let dhcp_in_port_reservation = UdpSocket::bind("[::1]:0")?;
        let dhcp_in_port = dhcp_in_port_reservation.local_addr()?.port();

        Ok((dhcp_in_port, dhcp_in_port_reservation))
    }

    fn reserve_metrics_endpoint() -> Result<(SocketAddr, TcpListener), eyre::Report> {
        let reservation = TcpListener::bind("[::1]:0")?;
        let endpoint = reservation.local_addr()?;

        Ok((endpoint, reservation))
    }

    fn refresh_dhcp_in_port(&mut self) -> Result<(), eyre::Report> {
        // Kea may fail before binding a reserved dynamic port; retry with a
        // fresh receive port while keeping the same config directory/memfile.
        let (dhcp_in_port, dhcp_in_port_reservation) = Self::reserve_dhcp_in_port()?;

        self.dhcp_in_port = dhcp_in_port;
        self.dhcp_in_port_reservation = Some(dhcp_in_port_reservation);

        Ok(())
    }

    fn run(&mut self, run_permit: Kea6RunPermit) -> Result<(), eyre::Report> {
        let mut run_permit = Some(run_permit);
        let mut last_exit = None;
        for attempt in 1..=KEA_START_ATTEMPTS {
            match self.run_once()? {
                Some(status) => {
                    last_exit = Some((self.dhcp_in_port, status));
                    if attempt == KEA_START_ATTEMPTS {
                        break;
                    }

                    // A failed early start usually means a transient bind or
                    // config-load failure; report the child status and retry
                    // with a new receive port before giving up.
                    println!(
                        "KEA6 exited before binding DHCP port {} on attempt {attempt}/{KEA_START_ATTEMPTS}: {status}; retrying with a fresh DHCP receive port",
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

        let (port, status) = last_exit.expect("at least one Kea6 start attempt should have run");
        Err(eyre::eyre!(
            "kea6 exited before binding DHCP port {port} after {KEA_START_ATTEMPTS} attempts: {status}"
        ))
    }

    fn run_once(&mut self) -> Result<Option<ExitStatus>, eyre::Report> {
        // Release our reservations immediately before spawn; from this point
        // Kea must own those sockets for readiness checks to pass.
        drop(self.dhcp_in_port_reservation.take());
        drop(self.metrics_endpoint_reservation.take());

        // Point Kea runtime files at the per-test temp dir instead of system
        // paths that unprivileged integration tests cannot safely share.
        let mut process = Command::new("/usr/sbin/kea-dhcp6")
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
        // Keep child logs in memory so tests can assert warnings and failures
        // include useful diagnostics. These reader threads end at pipe EOF.
        thread::spawn(move || {
            for line in stdout.lines() {
                let line = line.unwrap();
                stdout_logs
                    .lock()
                    .unwrap()
                    .push(format!("KEA6 STDOUT: {line}"));
                println!("KEA6 STDOUT: {line}");
            }
        });
        let stderr_logs = self.logs.clone();
        thread::spawn(move || {
            for line in stderr.lines() {
                let line = line.unwrap();
                stderr_logs
                    .lock()
                    .unwrap()
                    .push(format!("KEA6 STDERR: {line}"));
                println!("KEA6 STDERR: {line}");
            }
        });

        self.process = Some(process);

        let deadline = Instant::now() + KEA_READY_TIMEOUT;
        loop {
            thread::sleep(Duration::from_millis(100));
            if let Some(status) = self.process.as_mut().unwrap().try_wait()? {
                self.process = None;
                return Ok(Some(status));
            }

            // Once binding our probe socket fails with AddrInUse, Kea owns the
            // DHCP receive port. Metrics still initializes separately below.
            match UdpSocket::bind(format!("[::1]:{}", self.dhcp_in_port)) {
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    // After the DHCP bind appears ready, give Kea a short
                    // window to fail startup before declaring the harness ready.
                    thread::sleep(KEA_EXIT_SETTLE);
                    if let Some(status) = self.process.as_mut().unwrap().try_wait()? {
                        self.process = None;
                        return Ok(Some(status));
                    }
                    if let Some(status) = self.wait_for_metrics_endpoint(deadline)? {
                        return Ok(Some(status));
                    }
                    break;
                }
                Ok(_) => {}
                Err(e) => return Err(eyre::eyre!("unexpected error probing kea6 readiness: {e}")),
            }
            if Instant::now() >= deadline {
                self.stop_process();
                return Err(eyre::eyre!(
                    "kea6 did not bind DHCP port {} within {KEA_READY_TIMEOUT:?}",
                    self.dhcp_in_port
                ));
            }
        }

        Ok(None)
    }

    fn wait_for_metrics_endpoint(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<ExitStatus>, eyre::Report> {
        loop {
            // The hook initializes metrics asynchronously after loading; wait
            // for the endpoint so metric assertions do not race initialization.
            if TcpStream::connect_timeout(&self.metrics_endpoint, METRICS_READY_CONNECT_TIMEOUT)
                .is_ok()
            {
                return Ok(None);
            }
            if let Some(status) = self.process.as_mut().unwrap().try_wait()? {
                self.process = None;
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                self.stop_process();
                return Err(eyre::eyre!(
                    "kea6 did not bind metrics endpoint {} within {KEA_READY_TIMEOUT:?}",
                    self.metrics_endpoint
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn wait_for_log(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .logs
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains(needle))
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) fn metrics_endpoint(&self) -> SocketAddr {
        self.metrics_endpoint
    }

    /// Restart the same Kea6 config and memfile, clearing process-local hook cache.
    pub(crate) fn restart(&mut self) -> Result<(), eyre::Report> {
        let run_permit = self
            ._run_permit
            .take()
            .expect("Kea6 restart requires an active run permit");

        // The cache under test lives inside the child process, not the memfile.
        self.stop_process();
        self.logs.lock().unwrap().clear();
        match self.run_once()? {
            Some(status) => Err(eyre::eyre!("kea6 exited during restart: {status}")),
            None => {
                self._run_permit = Some(run_permit);
                Ok(())
            }
        }
    }

    fn stop_process(&mut self) {
        if let Some(process) = &mut self.process {
            // Prefer Kea's shutdown path so hook-library cleanup runs; kill is
            // only a fallback for a child that ignores SIGTERM.
            // SAFETY: The unreaped Child retains its valid, unreused PID and
            // SIGTERM is a valid signal; no borrowed memory crosses the syscall.
            unsafe {
                libc::kill(process.id() as i32, libc::SIGTERM);
            }

            let deadline = Instant::now() + KEA_SHUTDOWN_GRACE;
            loop {
                match process.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => thread::sleep(KEA_SHUTDOWN_POLL),
                    Ok(None) => {
                        process.kill().unwrap();
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        self.process = None;
    }

    fn config(
        api_server_url: &str,
        lease_file: &Path,
        metrics_endpoint: SocketAddr,
        config: Kea6Config,
    ) -> String {
        let hook_lib = super::hook_library_path();
        let metrics_endpoint = metrics_endpoint.to_string();
        let mut dhcp6 = json!({
            "interfaces-config": {
                // DHCPv6 binds interface-only selectors to link-local
                // addresses; the harness sends relayed traffic to ::1.
                "interfaces": [ "lo/::1" ]
            },
            // DHCPv6 otherwise persists its generated server DUID under
            // /var/lib/kea, which unprivileged integration tests cannot write.
            "server-id": {
                "type": "LL",
                "htype": 1,
                "identifier": "000102030405",
                "persist": false
            },
            // Persist the memfile so restart and lease-inspection tests observe
            // Kea's on-disk state; tempdir cleanup still bounds artifacts.
            "lease-database": {
                "type": "memfile",
                "persist": true,
                "name": lease_file.to_string_lossy(),
                "lfc-interval": 3600
            },
            // Exercise the hook's advertised Kea multithreading compatibility.
            "multi-threading": {
                "enable-multi-threading": true,
                "thread-pool-size": 4,
                "packet-queue-size": 28
            },
            "preferred-lifetime": config.preferred_lifetime,
            "valid-lifetime": config.valid_lifetime,
            "renew-timer": config.renew_timer,
            "rebind-timer": config.rebind_timer,
            // Match the v4 test/example quarantine instead of Kea's much
            // longer DHCPv6 default, so delayed DECLINE behavior is comparable.
            "decline-probation-period": 900,
            "hooks-libraries": [
                {
                    "library": hook_lib,
                    "parameters": {
                        "carbide-api-url": api_server_url,
                        "carbide-metrics-endpoint": metrics_endpoint,
                        "hook-dns-servers-ipv6": HOOK_DNS_SERVERS_IPV6.join(","),
                        "hook-ntp-servers-ipv6": HOOK_NTP_SERVERS_IPV6.join(","),
                        // Keep optional v6 parameters explicit so loader
                        // parsing is exercised even when the option is off.
                        "hook-provisioning-server-ipv6": "",
                        "hook-rapid-commit-v6": config.rapid_commit_v6
                    }
                }
            ],
            "subnet6": [
                {
                    "subnet": "::/0",
                    // Kea owns the rapid-commit state transition; the hook
                    // remains the policy gate by stripping option 14.
                    "rapid-commit": config.kea_rapid_commit_v6,
                    "pools": [{
                        "pool": "2001:db8::1-2001:db8::ffff"
                    }]
                }
            ],
            "loggers": [
                {
                    "name": "kea-dhcp6",
                    "output_options": [{"output": "stdout"}],
                    "severity": "WARN",
                    "debuglevel": 99
                },
                {
                    "name": "kea-dhcp6.carbide-rust",
                    "output_options": [{"output": "stdout"}],
                    "severity": "WARN",
                    "debuglevel": 10
                },
                {
                    "name": "kea-dhcp6.carbide-callouts",
                    "output_options": [{"output": "stdout"}],
                    "severity": "WARN",
                    "debuglevel": 10
                }
            ]
        });
        if let Some(expiration) = config.expired_leases_processing {
            // Most tests use normal lease lifetimes; expiry tests override this
            // block to make Kea reclaim leases within the test timeout.
            dhcp6["expired-leases-processing"] = json!({
                "reclaim-timer-wait-time": expiration.reclaim_timer_wait_time,
                "flush-reclaimed-timer-wait-time": expiration.flush_reclaimed_timer_wait_time,
                "hold-reclaimed-time": expiration.hold_reclaimed_time,
                "max-reclaim-leases": expiration.max_reclaim_leases,
                "max-reclaim-time": expiration.max_reclaim_time,
                "unwarned-reclaim-cycles": expiration.unwarned_reclaim_cycles
            });
        }
        if let Some(mac_sources) = config.mac_sources {
            // Identity tests pin mac-sources to force Kea's stored hwaddr down
            // specific option79-vs-DUID paths.
            dhcp6["mac-sources"] = json!(mac_sources);
        }
        let conf = json!({ "Dhcp6": dhcp6 });
        conf.to_string()
    }
}

impl Drop for Kea6 {
    fn drop(&mut self) {
        self.stop_process();
    }
}
