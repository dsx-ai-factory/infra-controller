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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

//! Validates the compiled BMC drivers against a real BMC.
//!
//! The BMC is discovered, drivers are selected by the compiled rules, and
//! the connection is built by `bmc-runtime` exactly as a controller would;
//! checks then call the selected drivers the way controllers do.

mod checks;
mod connect;
mod ownership;
mod redact;
mod report;
mod selection;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use bmc_platform::{BiosSettings, Capability, PlatformIdentity};
use clap::Parser;
use mac_address::MacAddress;
use tracing_subscriber::EnvFilter;

use crate::checks::{CheckResult, Ctx, EndState, Inputs, Outcome, Planned, Scope, UefiPassword};
use crate::connect::Target;
use crate::redact::Redactor;
use crate::report::Terminal;
use crate::selection::Selection;

/// The Redfish port a BMC address without one uses.
const HTTPS_PORT: u16 = 443;

#[derive(Debug, Parser)]
#[command(
    name = "bmc-validate",
    about = "Validate the compiled BMC drivers against a real BMC.",
    after_help = "Set RUST_LOG (for example RUST_LOG=warn) to also print library logs to stderr, \
                  such as the URL and status of each failed Redfish request."
)]
struct Args {
    /// IP address of the BMC.
    bmc_ip: IpAddr,

    /// HTTPS port of the BMC's Redfish service.
    #[arg(long, default_value_t = HTTPS_PORT)]
    port: u16,

    /// BMC account to log in with.
    #[arg(long, short)]
    username: String,

    /// Password of that account; with `--new-password`, its factory password.
    /// Prefer the environment variable: command arguments are visible to
    /// other users of the machine.
    #[arg(long, env = "BMC_PASSWORD", hide_env_values = true)]
    password: String,

    /// Pin a capability to a driver, as `<capability>=<driver-id>` or a
    /// driver id alone. `<capability>=standard` and
    /// `<capability>=unsupported` are accepted. Repeatable.
    #[arg(long = "driver", value_name = "[CAPABILITY=]DRIVER")]
    drivers: Vec<String>,

    /// Take a BMC from factory state through everything NICo does to it,
    /// in production order. Without `--validate` or `--check`, a run only
    /// reads the BMC.
    #[arg(long, conflicts_with_all = ["only", "checks"], requires = "new_password")]
    validate: bool,

    /// Read only the checks of these capabilities.
    #[arg(long, value_delimiter = ',', conflicts_with = "checks")]
    only: Vec<Capability>,

    /// Run exactly these checks, including ones that change the BMC and
    /// host. A `--validate` step runs alone, without the steps it follows
    /// there.
    #[arg(long = "check", value_delimiter = ',', value_name = "CHECK_ID")]
    checks: Vec<String>,

    /// Print the driver map and the planned checks without running them.
    #[arg(long)]
    plan: bool,

    /// MAC address of the host interface boot order should put first;
    /// boot-order checks need it.
    #[arg(long)]
    boot_mac: Option<MacAddress>,

    /// JSON object of BIOS attributes to apply and check on top of the
    /// platform's own settings.
    #[arg(long)]
    bios_profile: Option<PathBuf>,

    /// Most read checks in flight at once.
    #[arg(long, default_value_t = 4)]
    jobs: usize,

    /// Print each step of the apply checks to stderr as it happens: driver
    /// calls and how they completed, job status, restarts, and status reads.
    #[arg(long, short)]
    verbose: bool,

    /// The password `--username` has in place of the factory one, which is
    /// set first if the BMC still has the factory password. Factory reset
    /// checks need it to know the factory password.
    #[arg(long, env = "BMC_NEW_PASSWORD", hide_env_values = true)]
    new_password: Option<String>,

    /// What `--validate` leaves the BMC in.
    #[arg(long, value_enum, default_value_t, requires = "validate")]
    end_state: EndState,

    /// Before `--validate`, lift lockdown and reset the BIOS and the BMC to
    /// factory defaults, then take the BMC over again from its factory
    /// password. For a BMC an earlier run left configured.
    #[arg(long, requires = "validate")]
    factory_reset_first: bool,

    /// NTP server the BMC should sync from. Repeatable.
    #[arg(long = "ntp-server", value_name = "SERVER")]
    ntp_servers: Vec<String>,

    /// UEFI setup password to set.
    #[arg(long, env = "BMC_UEFI_PASSWORD", hide_env_values = true)]
    uefi_password: Option<String>,

    /// The UEFI setup password the firmware has now, such as its factory
    /// default.
    #[arg(
        long,
        env = "BMC_CURRENT_UEFI_PASSWORD",
        hide_env_values = true,
        default_value = "",
        requires = "uefi_password"
    )]
    current_uefi_password: String,

    /// How many CX9 SuperNICs the host has, as its expected machine lists
    /// them; SuperNIC checks need it.
    #[arg(long, value_name = "COUNT")]
    super_nics: Option<u8>,

    /// Let the AC power cycle check cut and restore AC power to the host.
    #[arg(long)]
    allow_ac_cycle: bool,

    /// Let the storage checks erase the boot controller, wiping the host's
    /// boot disk, and create the boot volume NICo creates.
    #[arg(long)]
    allow_storage_erase: bool,

    /// PEM file of the platform key secure boot enrolls on a DPU.
    #[arg(long)]
    pk_pem: Option<PathBuf>,

    /// The host BMC of a DPU, as `IP` or `IP:PORT`; it power-cycles the
    /// host when the DPU changes NIC mode.
    #[arg(long, value_parser = bmc_address, requires_all = ["host_username", "host_password"])]
    host_bmc: Option<SocketAddr>,

    /// Account on the host BMC.
    #[arg(long, requires = "host_bmc")]
    host_username: Option<String>,

    /// Password of that account.
    #[arg(
        long,
        env = "BMC_HOST_PASSWORD",
        hide_env_values = true,
        requires = "host_bmc"
    )]
    host_password: Option<String>,
}

fn bmc_address(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .or_else(|_| value.parse().map(|ip| SocketAddr::new(ip, HTTPS_PORT)))
        .map_err(|_| format!("{value} is neither an IP address nor IP:PORT"))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let redactor = Redactor::new(
        [
            Some(args.password.as_str()),
            args.new_password.as_deref(),
            args.uefi_password.as_deref(),
            Some(args.current_uefi_password.as_str()),
            args.host_password.as_deref(),
        ]
        .into_iter()
        .flatten(),
    );
    let terminal = Arc::new(Terminal::new(redactor, args.verbose));
    match run(args, terminal.clone()).await {
        Ok(code) => code,
        Err(error) => {
            terminal.error(&error);
            ExitCode::from(2)
        }
    }
}

async fn run(args: Args, terminal: Arc<Terminal>) -> Result<ExitCode, String> {
    let jobs = args.jobs.max(1);
    let scope = if args.validate {
        Scope::Validate(args.end_state)
    } else if !args.checks.is_empty() {
        Scope::Checks(&args.checks)
    } else {
        Scope::Reads(&args.only)
    };
    let mut session = Session::open(&args, &terminal).await?;
    if args.plan {
        if args.factory_reset_first {
            terminal.print_plan(
                "Factory reset",
                &session.plan(Scope::FactoryReset)?,
                &session.selection,
            );
        }
        terminal.print_plan("Plan", &session.plan(scope)?, &session.selection);
        return Ok(ExitCode::SUCCESS);
    }

    let started = Instant::now();
    let mut results = Vec::new();
    let mut unrestored = Vec::new();
    if args.factory_reset_first {
        let plan = session.plan(Scope::FactoryReset)?;
        let (reset, left) = session.run(&terminal, plan, jobs).await?;
        let finished = !reset
            .iter()
            .any(|result| matches!(result.outcome, Outcome::Fail(_) | Outcome::Blocked(_)));
        results.extend(reset);
        unrestored.extend(left);
        if !finished {
            terminal.warn("the factory reset did not finish, so validation did not start");
            terminal.summary(&results, &unrestored, started.elapsed());
            return Ok(exit_code(&results, &unrestored));
        }
        session = Session::open(&args, &terminal).await?;
    }
    let plan = session.plan(scope)?;
    let (validated, left) = session.run(&terminal, plan, jobs).await?;
    results.extend(validated);
    unrestored.extend(left);
    terminal.summary(&results, &unrestored, started.elapsed());
    Ok(exit_code(&results, &unrestored))
}

/// A BMC taken over, discovered, and matched to drivers, with what its
/// checks use.
struct Session {
    address: SocketAddr,
    target: Target,
    identity: PlatformIdentity,
    selection: Selection,
    inputs: Inputs,
}

impl Session {
    /// Replaces the factory password when `--validate` gives a new one, then
    /// discovers the BMC and selects its drivers.
    async fn open(args: &Args, terminal: &Terminal) -> Result<Self, String> {
        let address = SocketAddr::new(args.bmc_ip, args.port);
        let given = Target::new(address, &args.username, &args.password);
        let (target, factory, factory_accounts) = match &args.new_password {
            Some(new_password) => {
                let (owned, accounts) = own(args, terminal, &given, new_password).await?;
                (owned, Some(given), accounts)
            }
            None => (given, None, None),
        };

        terminal.discovering(address);
        let identity = discover(&target, terminal).await?;
        let selection = Selection::resolve(&identity, &args.drivers)?;
        terminal.print_identity(address, &identity);
        terminal.print_selection(&selection);
        if let Some(driver) = factory_accounts
            && driver != selection.driver(Capability::Accounts)
        {
            terminal.warn(&format!(
                "the factory password was changed through {driver}, but the full identity selects {}",
                selection.driver(Capability::Accounts)
            ));
        }
        let inputs = inputs(args, factory)?;
        Ok(Self {
            address,
            target,
            identity,
            selection,
            inputs,
        })
    }

    fn plan(&self, scope: Scope<'_>) -> Result<Vec<Planned>, String> {
        checks::plan(&self.selection, &self.inputs, scope)
    }

    /// Connects through `bmc-runtime` and runs `plan`, returning the results
    /// and the changes left unrestored.
    async fn run(
        self,
        terminal: &Arc<Terminal>,
        plan: Vec<Planned>,
        jobs: usize,
    ) -> Result<(Vec<CheckResult>, Vec<String>), String> {
        let bmc = self
            .target
            .connect(self.selection.endpoint(self.address, self.identity)?)
            .await?;
        let ctx = Ctx::new(
            bmc,
            self.selection,
            self.target,
            self.inputs,
            terminal.clone(),
        );
        terminal.checks_heading();
        let results = checks::run(&ctx, plan, jobs).await;
        Ok((results, ctx.unrestored()))
    }
}

/// Discovers `target`, trying again when an attempt fails, as site
/// exploration does on its next pass: a busy BMC can time out reading a large
/// inventory.
async fn discover(target: &Target, terminal: &Terminal) -> Result<PlatformIdentity, String> {
    const ATTEMPTS: usize = 3;
    let mut attempt = 1;
    let mut warned = false;
    loop {
        let waiting = |reason: &str| {
            if !warned {
                terminal.warn(reason);
                warned = true;
            }
        };
        match target.discover_settled(waiting).await {
            Ok((identity, true)) => return Ok(identity),
            Ok((identity, false)) => {
                terminal.warn(
                    "the ServiceRoot still reports neither a vendor nor a product; continuing",
                );
                return Ok(identity);
            }
            Err(error) if attempt < ATTEMPTS => {
                terminal.warn(&format!(
                    "discovery attempt {attempt} of {ATTEMPTS} failed; trying again: {error}"
                ));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

fn inputs(args: &Args, factory: Option<Target>) -> Result<Inputs, String> {
    Ok(Inputs {
        boot_mac: args.boot_mac,
        bios_profile: bios_profile(args.bios_profile.as_deref())?,
        ntp_servers: args.ntp_servers.clone(),
        uefi_password: args.uefi_password.clone().map(|new| UefiPassword {
            current: args.current_uefi_password.clone(),
            new,
        }),
        allow_ac_cycle: args.allow_ac_cycle,
        allow_storage_erase: args.allow_storage_erase,
        platform_key: args
            .pk_pem
            .as_deref()
            .map(|path| read(path, "the platform key"))
            .transpose()?,
        host_bmc: args.host_bmc.map(|address| {
            Target::new(
                address,
                args.host_username.as_deref().unwrap_or_default(),
                args.host_password.as_deref().unwrap_or_default(),
            )
        }),
        factory,
        super_nics: args.super_nics,
    })
}

/// 0 when nothing failed, 1 when a check failed, 3 when a change could not
/// be restored.
fn exit_code(results: &[CheckResult], unrestored: &[String]) -> ExitCode {
    if !unrestored.is_empty() {
        ExitCode::from(3)
    } else if results
        .iter()
        .any(|result| matches!(result.outcome, Outcome::Fail(_)))
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The BMC reached with `new_password`, replacing the factory password
/// first unless only planning: a plan changes nothing, so it reaches the
/// BMC with whichever password it accepts. Also returns the accounts driver
/// that replaced the password, if one did.
async fn own(
    args: &Args,
    terminal: &Terminal,
    factory: &Target,
    new_password: &str,
) -> Result<(Target, Option<String>), String> {
    let owned = factory.as_account(factory.username(), new_password);
    if args.plan {
        let reachable = match owned.log_in().await {
            Ok(()) => owned,
            Err(_) => factory.clone(),
        };
        return Ok((reachable, None));
    }
    terminal.taking_ownership(factory.address());
    let (owned, ownership) = ownership::take(factory, new_password, &args.drivers).await?;
    terminal.print_ownership(&ownership);
    Ok((owned, ownership.accounts_driver))
}

fn bios_profile(path: Option<&Path>) -> Result<BiosSettings, String> {
    let Some(path) = path else {
        return Ok(BiosSettings::default());
    };
    Ok(BiosSettings {
        attributes: serde_json::from_str::<BTreeMap<_, _>>(&read(path, "the BIOS profile")?)
            .map_err(|error| format!("{} is not a JSON object: {error}", path.display()))?,
    })
}

fn read(path: &Path, what: &str) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("reading {what} {}: {error}", path.display()))
}
