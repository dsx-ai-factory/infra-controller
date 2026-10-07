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
use std::collections::HashMap;
use std::path::PathBuf;

use carbide_uuid::machine::MachineId;

use crate::duppet::{self, FileEnsure, SyncOptions};

pub(super) fn main_sync(
    sync_options: SyncOptions,
    machine_id: &MachineId,
    host_machine_id: &MachineId,
) {
    // Sync out all duppet-managed config files. This can be called as part of
    // main_loop running if we want (and can also be called willy nilly with
    // ad-hoc sets of files, including whenever the nvue config changes if we
    // wanted to pull it in), but for now we just do this one duppet sync
    // during setup_and_run. Current files being managed are:
    //
    // - /etc/cron.daily/apt-clean
    // - /etc/dhcp/dhclient-exit-hooks.d/ntpsec
    // - /run/otelcol-contrib/machine-id
    // - /run/otelcol-contrib/host-machine-id
    //
    let duppet_files: HashMap<PathBuf, duppet::FileSpec> = HashMap::from([
        (
            "/etc/cron.daily/apt-clean".into(),
            duppet::FileSpec::new()
                .with_content(include_str!("../templates/apt-clean"))
                .with_perms(0o755),
        ),
        (
            "/etc/dhcp/dhclient-exit-hooks.d/ntpsec".into(),
            duppet::FileSpec::new()
                .with_content(include_str!("../templates/ntpsec"))
                .with_perms(0o644),
        ),
        (
            "/lib/systemd/system/update-ovs-pipe-size.service".into(),
            duppet::FileSpec::new()
                .with_content(include_str!("../templates/update-ovs-pipe-size.service"))
                .with_perms(0o644),
        ),
        (
            "/opt/forge/update-ovs-pipe-size.sh".into(),
            duppet::FileSpec::new()
                .with_content(include_str!("../templates/update-ovs-pipe-size"))
                .with_perms(0o755)
                .with_exec_on_change(),
        ),
        (
            "/run/otelcol-contrib/machine-id".into(),
            duppet::FileSpec::new().with_content(build_otel_machine_id_file_content(machine_id)),
        ),
        (
            "/run/otelcol-contrib/host-machine-id".into(),
            duppet::FileSpec::new()
                .with_content(build_otel_host_machine_id_file_content(host_machine_id)),
        ),
        // September 30, 2025.
        //
        // /etc/rc.local was added as a workaround for a bug pre-HBN 1.5,
        // which was fixed a couple of years ago. Having this hanging around
        // wasn't a problem until now: as of DOCA 2.9.3, the DPU now uses
        // networkd to manage DHCP leases, meaning we need to stop running
        // dhclient -- both are managing leases at the same time. Kind of a
        // creative way to have redundancy, but not quite what we want!
        //
        // This itself can go in some number of weeks, once the build
        // this is a part of gets deployed everywhere, and this file is
        // cleaned up.
        (
            "/etc/rc.local".into(),
            duppet::FileSpec::new().with_ensure(FileEnsure::Absent),
        ),
    ]);
    if let Err(e) = duppet::sync(duppet_files, sync_options) {
        tracing::error!(error = %e, "error during duppet run")
    }
}

// Write "machine.id=<value>" to a file so the OpenTelemetry collector can apply it as a resource
// attribute.
fn build_otel_machine_id_file_content(machine_id: &MachineId) -> String {
    format!("machine.id={machine_id}\n")
}

// Write "host.machine.id=<value>" to a file so the OpenTelemetry collector can apply it as a
// resource attribute.
fn build_otel_host_machine_id_file_content(host_machine_id: &MachineId) -> String {
    format!("host.machine.id={host_machine_id}\n")
}
