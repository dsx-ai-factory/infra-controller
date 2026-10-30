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

//! An in-memory nico-api site for reader tests: per-call delay, injected
//! failures, site mutations between calls, and a log of call starts with the
//! most calls ever in flight.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_site_health_probe_api::VersionSource;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::power_shelf::{PowerShelfId, PowerShelfIdSource, PowerShelfType};
use carbide_uuid::rack::{RackGroupId, RackId};
use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
use rpc::forge::host_reprovisioning_list_response::HostReprovisioningListItem;
use rpc::{forge, site_explorer};
use tokio::time::Instant;

use crate::gate::CallError;
use crate::nico::host_read::{COMPACT_HOST_READ_MISSING, HostDetail, Known};
use crate::nico::{NicoApi, NicoConnector};

fn hash(i: u32, kind: u8) -> [u8; 32] {
    let mut hash = [0u8; 32];
    hash[0] = kind;
    hash[1..5].copy_from_slice(&i.to_be_bytes());
    hash
}

pub(crate) fn host_id(i: u32) -> MachineId {
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        hash(i, 1),
        MachineType::Host,
    )
}

pub(crate) fn dpu_id(i: u32) -> MachineId {
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        hash(i, 2),
        MachineType::Dpu,
    )
}

pub(crate) fn switch_id(i: u32) -> SwitchId {
    SwitchId::new(
        SwitchIdSource::ProductBoardChassisSerial,
        hash(i, 3),
        SwitchType::NvLink,
    )
}

pub(crate) fn shelf_id(i: u32) -> PowerShelfId {
    PowerShelfId::new(
        PowerShelfIdSource::ProductBoardChassisSerial,
        hash(i, 4),
        PowerShelfType::Rack,
    )
}

pub(crate) fn rack_id(i: u32) -> RackId {
    RackId::new(format!("rack-{i}"))
}

pub(crate) fn instance_id(i: u32) -> InstanceId {
    format!("00000000-0000-4000-8000-{i:012}")
        .parse()
        .expect("instance id")
}

/// A host in `state`, with the given DPUs.
pub(crate) fn host(i: u32, state: &str, dpus: &[u32]) -> HostDetail {
    HostDetail {
        id: host_id(i),
        state: state.to_string(),
        state_reason: None,
        vendor: Some("NVIDIA".to_string()),
        model: Some("GB200 NVL".to_string()),
        instance_type: None,
        bmc_ip: Some(format!("10.0.1.{i}")),
        rack_id: Some(rack_id(1).to_string()),
        nvlink_domain: None,
        dpu_ids: dpus.iter().map(|d| dpu_id(*d)).collect(),
        update_complete: false,
        firmware_autoupdate: None,
        bmc_version: Some("25.06-2".to_string()),
        uefi_version: Some("00000083".to_string()),
        version_source: VersionSource::MachineRecord,
        report_at: None,
        report_error: None,
        allocatable: Known::Unknown(COMPACT_HOST_READ_MISSING),
        update_eligible: Known::Unknown(COMPACT_HOST_READ_MISSING),
    }
}

fn lifecycle(tag: &str) -> forge::LifecycleStatus {
    forge::LifecycleStatus {
        state: format!(r#"{{"state":"{tag}"}}"#),
        ..Default::default()
    }
}

/// A switch whose lifecycle state is `tag`.
pub(crate) fn switch(i: u32, tag: &str) -> forge::Switch {
    forge::Switch {
        id: Some(switch_id(i)),
        status: Some(forge::SwitchStatus {
            lifecycle: Some(lifecycle(tag)),
            ..Default::default()
        }),
        bmc_info: Some(forge::BmcInfo {
            ip: Some(format!("10.0.2.{i}")),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A power shelf whose lifecycle state is `tag`.
pub(crate) fn shelf(i: u32, tag: &str) -> forge::PowerShelf {
    forge::PowerShelf {
        id: Some(shelf_id(i)),
        status: Some(forge::PowerShelfStatus {
            lifecycle: Some(lifecycle(tag)),
            ..Default::default()
        }),
        bmc_info: Some(forge::BmcInfo {
            ip: Some(format!("10.0.3.{i}")),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A rack whose lifecycle state is `tag`.
pub(crate) fn rack(i: u32, tag: &str) -> forge::Rack {
    forge::Rack {
        id: Some(rack_id(i)),
        status: Some(forge::RackStatus {
            lifecycle: Some(lifecycle(tag)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// An instance on host `on_host` in tenant state `state`.
pub(crate) fn instance(i: u32, on_host: u32, state: forge::TenantState) -> forge::Instance {
    forge::Instance {
        id: Some(instance_id(i)),
        machine_id: Some(host_id(on_host).try_into().expect("stable host id")),
        metadata: Some(forge::Metadata {
            name: format!("scale-{i}"),
            ..Default::default()
        }),
        config: Some(forge::InstanceConfig {
            tenant: Some(forge::TenantConfig {
                tenant_organization_id: "tenant-org".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        status: Some(forge::InstanceStatus {
            tenant: Some(forge::InstanceTenantStatus {
                state: state as i32,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// An expected machine record `i`, linked to host `linked_to` when given.
pub(crate) fn expected_machine(i: u32, linked_to: Option<u32>) -> forge::LinkedExpectedMachine {
    forge::LinkedExpectedMachine {
        bmc_mac_address: format!("aa:00:00:00:00:{i:02x}"),
        explored_endpoint_address: linked_to.map(|h| format!("10.0.1.{h}")),
        machine_id: linked_to.map(host_id),
        expected_machine_id: Some(rpc::common::Uuid {
            value: format!("em-{i}"),
        }),
        ..Default::default()
    }
}

/// An expected switch record `i`, linked to switch `linked_to` when given.
pub(crate) fn expected_switch(i: u32, linked_to: Option<u32>) -> forge::LinkedExpectedSwitch {
    forge::LinkedExpectedSwitch {
        bmc_mac_address: format!("bb:00:00:00:00:{i:02x}"),
        explored_endpoint_address: linked_to.map(|s| format!("10.0.2.{s}")),
        switch_id: linked_to.map(switch_id),
        expected_switch_id: Some(rpc::common::Uuid {
            value: format!("es-{i}"),
        }),
        ..Default::default()
    }
}

/// An expected power shelf record `i`, linked to shelf `linked_to` when
/// given.
pub(crate) fn expected_shelf(i: u32, linked_to: Option<u32>) -> forge::LinkedExpectedPowerShelf {
    forge::LinkedExpectedPowerShelf {
        bmc_mac_address: format!("cc:00:00:00:00:{i:02x}"),
        explored_endpoint_address: linked_to.map(|s| format!("10.0.3.{s}")),
        power_shelf_id: linked_to.map(shelf_id),
        expected_power_shelf_id: Some(rpc::common::Uuid {
            value: format!("ep-{i}"),
        }),
        ..Default::default()
    }
}

pub(crate) fn expected_rack(i: u32) -> forge::ExpectedRack {
    forge::ExpectedRack {
        rack_id: Some(rack_id(i)),
        ..Default::default()
    }
}

pub(crate) fn expected_rack_group(name: &str) -> forge::ExpectedRackGroup {
    forge::ExpectedRackGroup {
        rack_group_id: Some(RackGroupId::new(name)),
        ..Default::default()
    }
}

/// An open firmware request on host `i`.
pub(crate) fn firmware_request(i: u32, initiator: &str) -> HostReprovisioningListItem {
    HostReprovisioningListItem {
        id: Some(host_id(i).try_into().expect("host id")),
        state: "HostReprovisioning/CheckingFirmware".to_string(),
        initiator: initiator.to_string(),
        ..Default::default()
    }
}

/// The site's records. Tests edit it directly or through mutation hooks.
#[derive(Default)]
pub(crate) struct FakeSite {
    pub hosts: BTreeMap<String, HostDetail>,
    pub dpus: BTreeMap<String, MachineId>,
    pub switches: BTreeMap<String, forge::Switch>,
    pub shelves: BTreeMap<String, forge::PowerShelf>,
    pub racks: BTreeMap<String, forge::Rack>,
    pub instances: BTreeMap<String, forge::Instance>,
    pub expected_machines: Vec<forge::LinkedExpectedMachine>,
    pub expected_switches: Vec<forge::LinkedExpectedSwitch>,
    pub expected_shelves: Vec<forge::LinkedExpectedPowerShelf>,
    pub expected_racks: Vec<forge::ExpectedRack>,
    pub expected_rack_groups: Vec<forge::ExpectedRackGroup>,
    pub firmware_requests: Vec<HostReprovisioningListItem>,
    pub explored: BTreeMap<String, site_explorer::ExploredEndpoint>,
    /// IDs every by-ID read leaves out although the census lists them.
    pub withheld: BTreeSet<String>,
}

impl FakeSite {
    pub(crate) fn add_host(&mut self, host: HostDetail) {
        for dpu in &host.dpu_ids {
            self.dpus.insert(dpu.to_string(), *dpu);
        }
        self.hosts.insert(host.id.to_string(), host);
    }

    pub(crate) fn add_switch(&mut self, switch: forge::Switch) {
        let id = switch.id.expect("switch id").to_string();
        self.switches.insert(id, switch);
    }

    pub(crate) fn add_shelf(&mut self, shelf: forge::PowerShelf) {
        let id = shelf.id.expect("shelf id").to_string();
        self.shelves.insert(id, shelf);
    }

    pub(crate) fn add_rack(&mut self, rack: forge::Rack) {
        let id = rack.id.clone().expect("rack id").to_string();
        self.racks.insert(id, rack);
    }

    pub(crate) fn add_instance(&mut self, instance: forge::Instance) {
        let id = instance.id.expect("instance id").to_string();
        self.instances.insert(id, instance);
    }
}

type Hook = Box<dyn FnOnce(&mut FakeSite) + Send>;

/// The fake site behind [`NicoApi`].
#[derive(Default)]
pub(crate) struct FakeNico {
    pub site: Mutex<FakeSite>,
    delay: Mutex<Duration>,
    blocks: Mutex<BTreeMap<&'static str, Duration>>,
    failures: Mutex<BTreeMap<&'static str, VecDeque<CallError>>>,
    hooks: Mutex<Vec<(&'static str, Hook)>>,
    starts: Mutex<Vec<(&'static str, Instant)>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl FakeNico {
    pub(crate) fn new(site: FakeSite) -> Arc<Self> {
        Arc::new(Self {
            site: Mutex::new(site),
            ..Self::default()
        })
    }

    /// Every call takes `delay` before it answers.
    pub(crate) fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    /// The next call named `call` blocks its thread for `duration`, like a
    /// synchronous read on a wedged volume, so no timer of that task fires.
    pub(crate) fn block_next(&self, call: &'static str, duration: Duration) {
        self.blocks.lock().unwrap().insert(call, duration);
    }

    /// The next call named `call` fails with `err` (queued per call).
    pub(crate) fn fail_next(&self, call: &'static str, err: CallError) {
        self.failures
            .lock()
            .unwrap()
            .entry(call)
            .or_default()
            .push_back(err);
    }

    /// Runs `hook` on the site right after the next call named `call`
    /// answers, before the reader's next call.
    pub(crate) fn after(
        &self,
        call: &'static str,
        hook: impl FnOnce(&mut FakeSite) + Send + 'static,
    ) {
        self.hooks.lock().unwrap().push((call, Box::new(hook)));
    }

    /// Calls named `call` so far.
    pub(crate) fn calls(&self, call: &str) -> usize {
        self.starts
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| *name == call)
            .count()
    }

    /// Start times of every call so far.
    pub(crate) fn starts(&self) -> Vec<(&'static str, Instant)> {
        self.starts.lock().unwrap().clone()
    }

    /// Forgets the calls logged so far, for long tests that only need the
    /// log of the latest sweep.
    pub(crate) fn clear_calls(&self) {
        self.starts.lock().unwrap().clear();
    }

    pub(crate) fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// Answers one call: logs it, waits the delay, then either fails with an
    /// injected error or reads the site, then runs the call's hooks.
    async fn answer<T>(
        &self,
        call: &'static str,
        read: impl FnOnce(&FakeSite) -> T,
    ) -> Result<T, CallError> {
        self.starts.lock().unwrap().push((call, Instant::now()));
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let block = self.blocks.lock().unwrap().remove(call);
        if let Some(block) = block {
            std::thread::sleep(block);
        }
        let injected = self
            .failures
            .lock()
            .unwrap()
            .get_mut(call)
            .and_then(VecDeque::pop_front);
        let result = match injected {
            Some(err) => Err(err),
            None => Ok(read(&self.site.lock().unwrap())),
        };
        let hooks: Vec<Hook> = {
            let mut all = self.hooks.lock().unwrap();
            let (mine, rest) = std::mem::take(&mut *all)
                .into_iter()
                .partition(|(name, _)| *name == call);
            *all = rest;
            mine.into_iter().map(|(_, hook)| hook).collect()
        };
        for hook in hooks {
            hook(&mut self.site.lock().unwrap());
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

/// Picks the asked rows of `rows` that the site does not withhold.
fn by_ids<T: Clone>(
    site: &FakeSite,
    rows: &BTreeMap<String, T>,
    ids: impl Iterator<Item = String>,
) -> Vec<T> {
    ids.filter(|id| !site.withheld.contains(id))
        .filter_map(|id| rows.get(&id).cloned())
        .collect()
}

#[async_trait::async_trait]
impl NicoApi for FakeNico {
    async fn machine_ids(&self) -> Result<Vec<MachineId>, CallError> {
        self.answer("find_machine_ids", |site| {
            site.hosts
                .values()
                .map(|h| h.id)
                .chain(site.dpus.values().copied())
                .collect()
        })
        .await
    }

    async fn hosts_by_ids(&self, ids: &[MachineId]) -> Result<Vec<HostDetail>, CallError> {
        self.answer("find_machines_by_ids", |site| {
            by_ids(site, &site.hosts, ids.iter().map(ToString::to_string))
        })
        .await
    }

    async fn switch_ids(&self) -> Result<Vec<SwitchId>, CallError> {
        self.answer("find_switch_ids", |site| {
            site.switches.values().filter_map(|s| s.id).collect()
        })
        .await
    }

    async fn switches_by_ids(&self, ids: &[SwitchId]) -> Result<Vec<forge::Switch>, CallError> {
        self.answer("find_switches_by_ids", |site| {
            by_ids(site, &site.switches, ids.iter().map(ToString::to_string))
        })
        .await
    }

    async fn power_shelf_ids(&self) -> Result<Vec<PowerShelfId>, CallError> {
        self.answer("find_power_shelf_ids", |site| {
            site.shelves.values().filter_map(|s| s.id).collect()
        })
        .await
    }

    async fn power_shelves_by_ids(
        &self,
        ids: &[PowerShelfId],
    ) -> Result<Vec<forge::PowerShelf>, CallError> {
        self.answer("find_power_shelves_by_ids", |site| {
            by_ids(site, &site.shelves, ids.iter().map(ToString::to_string))
        })
        .await
    }

    async fn rack_ids(&self) -> Result<Vec<RackId>, CallError> {
        // Like nico-api, the census leaves soft-deleted racks out.
        self.answer("find_rack_ids", |site| {
            site.racks
                .values()
                .filter(|r| r.deleted.is_none())
                .filter_map(|r| r.id.clone())
                .collect()
        })
        .await
    }

    async fn racks_by_ids(&self, ids: &[RackId]) -> Result<Vec<forge::Rack>, CallError> {
        self.answer("find_racks_by_ids", |site| {
            by_ids(site, &site.racks, ids.iter().map(ToString::to_string))
        })
        .await
    }

    async fn instance_ids(&self) -> Result<Vec<InstanceId>, CallError> {
        self.answer("find_instance_ids", |site| {
            site.instances.values().filter_map(|i| i.id).collect()
        })
        .await
    }

    async fn instances_by_ids(
        &self,
        ids: &[InstanceId],
    ) -> Result<Vec<forge::Instance>, CallError> {
        self.answer("find_instances_by_ids", |site| {
            by_ids(site, &site.instances, ids.iter().map(ToString::to_string))
        })
        .await
    }

    async fn expected_machines_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedMachine>, CallError> {
        self.answer("get_all_expected_machines_linked", |site| {
            site.expected_machines.clone()
        })
        .await
    }

    async fn expected_switches_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedSwitch>, CallError> {
        self.answer("get_all_expected_switches_linked", |site| {
            site.expected_switches.clone()
        })
        .await
    }

    async fn expected_power_shelves_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedPowerShelf>, CallError> {
        self.answer("get_all_expected_power_shelves_linked", |site| {
            site.expected_shelves.clone()
        })
        .await
    }

    async fn expected_racks(&self) -> Result<Vec<forge::ExpectedRack>, CallError> {
        self.answer("get_all_expected_racks", |site| site.expected_racks.clone())
            .await
    }

    async fn expected_rack_groups(&self) -> Result<Vec<forge::ExpectedRackGroup>, CallError> {
        self.answer("get_all_expected_rack_groups", |site| {
            site.expected_rack_groups.clone()
        })
        .await
    }

    async fn hosts_waiting_for_reprovisioning(
        &self,
    ) -> Result<Vec<HostReprovisioningListItem>, CallError> {
        self.answer("list_hosts_waiting_for_reprovisioning", |site| {
            site.firmware_requests.clone()
        })
        .await
    }

    async fn explored_endpoints(
        &self,
        addresses: &[String],
    ) -> Result<Vec<site_explorer::ExploredEndpoint>, CallError> {
        self.answer("find_explored_endpoints_by_ids", |site| {
            addresses
                .iter()
                .filter_map(|a| site.explored.get(a).cloned())
                .collect()
        })
        .await
    }
}

/// Hands out the shared fake as every sweep's session.
pub(crate) struct FakeConnector(pub Arc<FakeNico>);

#[async_trait::async_trait]
impl NicoConnector for FakeConnector {
    async fn connect(&self) -> Result<Arc<dyn NicoApi>, CallError> {
        Ok(Arc::clone(&self.0) as Arc<dyn NicoApi>)
    }
}
