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

//! The device reader: one sweep reads the census and by-ID records of hosts,
//! DPUs, switches, power shelves, racks and instances, the expected records
//! and the open host firmware requests, and publishes one snapshot per
//! family. A family is published only when every read it needs succeeded and
//! no unit is unresolved; otherwise it keeps its last snapshot.
//!
//! DPUs get no by-ID read: a DPU is attached when a host lists it, and takes
//! that host's state category.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use carbide_site_health_probe_api::{
    DpuFields, ExpectedFields, Family, FirmwareRequest, HostFields, InstanceFields, MismatchKind,
    RecordType, Source, Stage, Unit, UnitType,
};
use chrono::{DateTime, Utc};
use rpc::forge;
use rpc::forge::host_reprovisioning_list_response::HostReprovisioningListItem;

use crate::census::{
    CensusRead, OTHER, dpu_done_under, host_category, initiator, lifecycle_tag, read_family,
};
use crate::config::ReaderConfig;
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::{CallError, CallGate};
use crate::nico::host_read::{HostDetail, Known};
use crate::nico::{NicoApi, NicoConnector};
use crate::readers::{Attempt, Failure, incomplete};
use crate::snapshot::{Reader, SnapshotSink, SweepMeta, UnitsData, unix_millis};

/// Unlinked expected records get an explorer read for their reason only up
/// to this many; above it the reason is a bare `no_device`.
const EXPLORED_READ_LIMIT: usize = 500;

/// Explored endpoints per explorer read.
const EXPLORED_BATCH: usize = 25;

/// The families this reader publishes.
pub(crate) const DEVICE_FAMILIES: [Family; 8] = [
    Family::Host,
    Family::Dpu,
    Family::Switch,
    Family::PowerShelf,
    Family::Rack,
    Family::Instance,
    Family::Expected,
    Family::HostFirmwareRequests,
];

type Built = Result<UnitsData, Failure>;

/// Reads devices from nico-api every interval.
pub(crate) struct DevicesReader {
    cfg: ReaderConfig,
    batch: usize,
    nico: Arc<dyn NicoConnector>,
    gate: Arc<CallGate>,
    sink: Arc<dyn SnapshotSink>,
    process: u64,
    seq: AtomicU64,
    attempt: Attempt,
}

impl DevicesReader {
    pub(crate) fn new(
        cfg: ReaderConfig,
        batch: usize,
        nico: Arc<dyn NicoConnector>,
        gate: Arc<CallGate>,
        sink: Arc<dyn SnapshotSink>,
        process_start: SystemTime,
    ) -> Self {
        Self {
            cfg,
            batch,
            nico,
            gate,
            sink,
            process: unix_millis(process_start),
            seq: AtomicU64::new(0),
            attempt: Attempt::new(Reader::Devices, &DEVICE_FAMILIES),
        }
    }
}

#[async_trait::async_trait]
impl Probe for DevicesReader {
    fn name(&self) -> &'static str {
        Reader::Devices.as_str()
    }

    fn api(&self) -> &'static str {
        self.gate.api()
    }

    fn interval(&self) -> Duration {
        self.cfg.interval
    }

    fn timeout(&self) -> Duration {
        self.cfg.timeout
    }

    fn timed_out(&self) {
        self.attempt.timed_out(self.sink.as_ref());
    }

    async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
        let start = crate::clock::now();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.attempt.begin();
        let families = match self.nico.connect().await {
            Ok(nico) => {
                Sweep {
                    nico,
                    gate: &self.gate,
                    rec: recorder,
                    batch: self.batch,
                }
                .read()
                .await
            }
            Err(err) => {
                let failure = Failure::call("connect", &err);
                DEVICE_FAMILIES
                    .iter()
                    .map(|family| (*family, Err(failure.clone())))
                    .collect()
            }
        };
        let meta = SweepMeta {
            process: self.process,
            seq,
            reader: Reader::Devices,
            start,
            end: crate::clock::now(),
        };
        let mut failed = Vec::new();
        for (family, built) in families {
            if !self
                .attempt
                .deliver(self.sink.as_ref(), &meta, family, built)
            {
                failed.push(family);
            }
        }
        incomplete(&failed)
    }
}

/// One sweep's reads, through one nico-api session.
struct Sweep<'a> {
    nico: Arc<dyn NicoApi>,
    gate: &'a CallGate,
    rec: &'a ObservationRecorder,
    batch: usize,
}

/// Every read of one sweep, before the families are built.
struct Reads {
    machines: Result<(), Failure>,
    hosts: Result<CensusRead<HostDetail>, Failure>,
    dpus: Vec<String>,
    switches: Result<CensusRead<forge::Switch>, Failure>,
    shelves: Result<CensusRead<forge::PowerShelf>, Failure>,
    racks: Result<CensusRead<forge::Rack>, Failure>,
    instances: Result<CensusRead<forge::Instance>, Failure>,
    expected: Result<ExpectedReads, Failure>,
    firmware: Result<Vec<HostReprovisioningListItem>, Failure>,
}

struct ExpectedReads {
    machines: Vec<forge::LinkedExpectedMachine>,
    switches: Vec<forge::LinkedExpectedSwitch>,
    shelves: Vec<forge::LinkedExpectedPowerShelf>,
    racks: Vec<forge::ExpectedRack>,
    rack_groups: Vec<forge::ExpectedRackGroup>,
}

impl Sweep<'_> {
    async fn call<T, F, Fut>(&self, call: &'static str, f: F) -> Result<T, CallError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, CallError>>,
    {
        self.gate.call_with_retries(self.rec, call, f).await
    }

    /// Reads one unit type: census, by-ID batches, and one second census
    /// for misses.
    async fn census<Id, R, C, CFut, B, BFut>(
        &self,
        what: &str,
        (census_call, census): (&'static str, C),
        (by_id_call, by_id): (&'static str, B),
        id_of: fn(&R) -> String,
    ) -> Result<CensusRead<R>, Failure>
    where
        Id: Clone + ToString,
        C: Fn() -> CFut,
        CFut: Future<Output = Result<Vec<Id>, CallError>>,
        B: Fn(Vec<Id>) -> BFut,
        BFut: Future<Output = Result<Vec<R>, CallError>>,
    {
        let ids = self
            .call(census_call, &census)
            .await
            .map_err(|e| Failure::call(census_call, &e))?;
        self.by_ids(what, ids, (census_call, census), (by_id_call, by_id), id_of)
            .await
    }

    /// The by-ID half of [`Sweep::census`], for a census already read.
    async fn by_ids<Id, R, C, CFut, B, BFut>(
        &self,
        what: &str,
        ids: Vec<Id>,
        (census_call, census): (&'static str, C),
        (by_id_call, by_id): (&'static str, B),
        id_of: fn(&R) -> String,
    ) -> Result<CensusRead<R>, Failure>
    where
        Id: Clone + ToString,
        C: Fn() -> CFut,
        CFut: Future<Output = Result<Vec<Id>, CallError>>,
        B: Fn(Vec<Id>) -> BFut,
        BFut: Future<Output = Result<Vec<R>, CallError>>,
    {
        let typed: BTreeMap<String, Id> = ids.into_iter().map(|id| (id.to_string(), id)).collect();
        let (typed_ref, by_id, census) = (&typed, &by_id, &census);
        let read = read_family(
            typed.keys().cloned().collect(),
            self.batch,
            move |batch| {
                let ids: Vec<Id> = batch.iter().map(|id| typed_ref[id].clone()).collect();
                async move { self.call(by_id_call, || by_id(ids.clone())).await }
            },
            id_of,
            move || async move {
                self.call(census_call, census)
                    .await
                    .map(|ids| ids.iter().map(ToString::to_string).collect())
            },
        )
        .await
        .map_err(|e| Failure::call(what, &e))?;
        if read.unresolved.is_empty() {
            Ok(read)
        } else {
            Err(Failure::unresolved(format!(
                "{} {what} missing from their by-ID read but still in the census",
                read.unresolved.len()
            )))
        }
    }

    async fn read_all(&self) -> Reads {
        let nico = &self.nico;
        let machine_census = self
            .call("find_machine_ids", || nico.machine_ids())
            .await
            .map_err(|e| Failure::call("find_machine_ids", &e));
        let (machines, hosts, dpus) = match machine_census {
            Ok(ids) => {
                let (hosts, dpus): (Vec<_>, Vec<_>) = ids
                    .into_iter()
                    .filter(|id| !id.machine_type().is_predicted_host())
                    .partition(|id| id.machine_type().is_host());
                let hosts = self
                    .by_ids(
                        "hosts",
                        hosts,
                        ("find_machine_ids", move || async move {
                            nico.machine_ids().await.map(|ids| {
                                ids.into_iter()
                                    .filter(|id| id.machine_type().is_host())
                                    .collect::<Vec<_>>()
                            })
                        }),
                        ("find_machines_by_ids", move |ids: Vec<_>| async move {
                            nico.hosts_by_ids(&ids).await
                        }),
                        |h: &HostDetail| h.id.to_string(),
                    )
                    .await;
                (
                    Ok(()),
                    hosts,
                    dpus.iter().map(ToString::to_string).collect(),
                )
            }
            Err(failure) => (Err(failure.clone()), Err(failure), Vec::new()),
        };
        let switches = self
            .census(
                "switches",
                ("find_switch_ids", move || nico.switch_ids()),
                ("find_switches_by_ids", move |ids: Vec<_>| async move {
                    nico.switches_by_ids(&ids).await
                }),
                |s: &forge::Switch| s.id.map(|id| id.to_string()).unwrap_or_default(),
            )
            .await
            .map(drop_deleted(|s: &forge::Switch| s.deleted.is_some()));
        let shelves = self
            .census(
                "power shelves",
                ("find_power_shelf_ids", move || nico.power_shelf_ids()),
                ("find_power_shelves_by_ids", move |ids: Vec<_>| async move {
                    nico.power_shelves_by_ids(&ids).await
                }),
                |s: &forge::PowerShelf| s.id.map(|id| id.to_string()).unwrap_or_default(),
            )
            .await
            .map(drop_deleted(|s: &forge::PowerShelf| s.deleted.is_some()));
        let racks = self
            .census(
                "racks",
                ("find_rack_ids", move || nico.rack_ids()),
                ("find_racks_by_ids", move |ids: Vec<_>| async move {
                    nico.racks_by_ids(&ids).await
                }),
                |r: &forge::Rack| r.id.as_ref().map(ToString::to_string).unwrap_or_default(),
            )
            .await
            .map(drop_deleted(|r: &forge::Rack| r.deleted.is_some()));
        let instances = self
            .census(
                "instances",
                ("find_instance_ids", move || nico.instance_ids()),
                ("find_instances_by_ids", move |ids: Vec<_>| async move {
                    nico.instances_by_ids(&ids).await
                }),
                |i: &forge::Instance| i.id.map(|id| id.to_string()).unwrap_or_default(),
            )
            .await;
        let expected = self.read_expected().await;
        let firmware = self
            .call("list_hosts_waiting_for_reprovisioning", || {
                nico.hosts_waiting_for_reprovisioning()
            })
            .await
            .map_err(|e| Failure::call("list_hosts_waiting_for_reprovisioning", &e));
        Reads {
            machines,
            hosts,
            dpus,
            switches,
            shelves,
            racks,
            instances,
            expected,
            firmware,
        }
    }

    async fn read_expected(&self) -> Result<ExpectedReads, Failure> {
        let nico = &self.nico;
        let fail = |call: &'static str| move |e: CallError| Failure::call(call, &e);
        Ok(ExpectedReads {
            machines: self
                .call("get_all_expected_machines_linked", || {
                    nico.expected_machines_linked()
                })
                .await
                .map_err(fail("get_all_expected_machines_linked"))?,
            switches: self
                .call("get_all_expected_switches_linked", || {
                    nico.expected_switches_linked()
                })
                .await
                .map_err(fail("get_all_expected_switches_linked"))?,
            shelves: self
                .call("get_all_expected_power_shelves_linked", || {
                    nico.expected_power_shelves_linked()
                })
                .await
                .map_err(fail("get_all_expected_power_shelves_linked"))?,
            racks: self
                .call("get_all_expected_racks", || nico.expected_racks())
                .await
                .map_err(fail("get_all_expected_racks"))?,
            rack_groups: self
                .call("get_all_expected_rack_groups", || {
                    nico.expected_rack_groups()
                })
                .await
                .map_err(fail("get_all_expected_rack_groups"))?,
        })
    }

    /// `last_exploration_error` per explored address, read in batches.
    async fn explored_errors(
        &self,
        addresses: Vec<String>,
    ) -> Result<BTreeMap<String, Option<String>>, Failure> {
        let mut errors = BTreeMap::new();
        for chunk in addresses.chunks(EXPLORED_BATCH) {
            let endpoints = self
                .call("find_explored_endpoints_by_ids", || {
                    self.nico.explored_endpoints(chunk)
                })
                .await
                .map_err(|e| Failure::call("find_explored_endpoints_by_ids", &e))?;
            for endpoint in endpoints {
                let error = endpoint
                    .report
                    .and_then(|report| report.last_exploration_error)
                    .filter(|e| !e.is_empty());
                errors.insert(endpoint.address, error);
            }
        }
        Ok(errors)
    }

    async fn read(&self) -> Vec<(Family, Built)> {
        let reads = self.read_all().await;
        let hosts = reads.hosts.as_ref();

        // Firmware requests join to this sweep's hosts. Host units carry
        // their open request, so without the request read a host snapshot
        // could not tell "no request" from "unknown": the host family then
        // keeps its last snapshot too.
        let firmware = match (&reads.firmware, hosts) {
            (Err(failure), _) => Err(failure.clone()),
            (_, Err(failure)) => Err(failure.needed_by(Family::HostFirmwareRequests)),
            (Ok(items), Ok(hosts)) => Ok(firmware_requests(items, &hosts.rows)),
        };
        let host_family = match (hosts, &firmware) {
            (Err(failure), _) => Err(failure.clone()),
            (Ok(_), Err(failure)) => Err(failure.needed_by(Family::Host)),
            (Ok(read), Ok(joined)) => Ok(host_units(&read.rows, &joined.open)),
        };
        let dpu_family = match (&reads.machines, hosts) {
            (Ok(()), Ok(read)) => Ok(dpu_units(&reads.dpus, &read.rows)),
            (Err(failure), _) | (_, Err(failure)) => Err(failure.needed_by(Family::Dpu)),
        };
        let firmware_family = firmware.and_then(|joined| {
            if joined.unjoinable > 0 {
                Err(Failure::unresolved(format!(
                    "{} firmware requests name no host of this sweep",
                    joined.unjoinable
                )))
            } else {
                Ok(UnitsData::new(joined.units, 0))
            }
        });
        let expected_family = self.expected_family(&reads).await;

        vec![
            (Family::Host, host_family),
            (Family::Dpu, dpu_family),
            (
                Family::Switch,
                reads
                    .switches
                    .as_ref()
                    .map(|r| lifecycle_units(UnitType::Switch, &r.rows, switch_view))
                    .map_err(Clone::clone),
            ),
            (
                Family::PowerShelf,
                reads
                    .shelves
                    .as_ref()
                    .map(|r| lifecycle_units(UnitType::PowerShelf, &r.rows, shelf_view))
                    .map_err(Clone::clone),
            ),
            (
                Family::Rack,
                reads
                    .racks
                    .as_ref()
                    .map(|r| lifecycle_units(UnitType::Rack, &r.rows, rack_view))
                    .map_err(Clone::clone),
            ),
            (
                Family::Instance,
                reads
                    .instances
                    .as_ref()
                    .map(|r| instance_units(&r.rows))
                    .map_err(Clone::clone),
            ),
            (Family::Expected, expected_family),
            (Family::HostFirmwareRequests, firmware_family),
        ]
    }

    async fn expected_family(&self, reads: &Reads) -> Built {
        let expected = reads.expected.as_ref().map_err(Clone::clone)?;
        let hosts = census_ids(&reads.hosts)?;
        let switches = census_ids(&reads.switches)?;
        let shelves = census_ids(&reads.shelves)?;
        let racks = census_ids(&reads.racks)?;

        let mut records = Vec::new();
        records.extend(link_records(
            RecordType::Host,
            expected.machines.iter().map(|row| LinkRow {
                expected_id: row.expected_machine_id.as_ref().map(|id| id.value.clone()),
                bmc_mac: &row.bmc_mac_address,
                explored: row.explored_endpoint_address.as_deref(),
                device: row.machine_id.map(|id| id.to_string()),
            }),
            &hosts,
        ));
        records.extend(link_records(
            RecordType::Switch,
            expected.switches.iter().map(|row| LinkRow {
                expected_id: row.expected_switch_id.as_ref().map(|id| id.value.clone()),
                bmc_mac: &row.bmc_mac_address,
                explored: row.explored_endpoint_address.as_deref(),
                device: row.switch_id.map(|id| id.to_string()),
            }),
            &switches,
        ));
        records.extend(link_records(
            RecordType::PowerShelf,
            expected.shelves.iter().map(|row| LinkRow {
                expected_id: row
                    .expected_power_shelf_id
                    .as_ref()
                    .map(|id| id.value.clone()),
                bmc_mac: &row.bmc_mac_address,
                explored: row.explored_endpoint_address.as_deref(),
                device: row.power_shelf_id.map(|id| id.to_string()),
            }),
            &shelves,
        ));

        // Unlinked records name why: the explorer's last error for their
        // address, read only while few records are unlinked.
        let unlinked: Vec<&Record> = records.iter().filter(|r| r.devices.is_empty()).collect();
        let explored = if unlinked.len() <= EXPLORED_READ_LIMIT {
            let addresses: Vec<String> = unlinked
                .iter()
                .filter_map(|r| r.explored.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            Some(self.explored_errors(addresses).await?)
        } else {
            None
        };

        let mut units = Vec::new();
        for record in &records {
            units.extend(record.units(explored.as_ref()));
        }
        for rack in &expected.racks {
            let Some(id) = rack.rack_id.as_ref().map(ToString::to_string) else {
                continue;
            };
            let linked = racks.contains(&id);
            units.push(plain_record(
                RecordType::Rack,
                id.clone(),
                linked,
                linked.then_some(id),
            ));
        }
        for group in &expected.rack_groups {
            let Some(id) = group.rack_group_id.as_ref().map(ToString::to_string) else {
                continue;
            };
            // Rack groups have no device of their own to link to.
            units.push(plain_record(RecordType::RackGroup, id, false, None));
        }
        for (record_type, census, linked) in [
            (
                RecordType::Host,
                &hosts,
                linked_devices(&records, RecordType::Host),
            ),
            (
                RecordType::Switch,
                &switches,
                linked_devices(&records, RecordType::Switch),
            ),
            (
                RecordType::PowerShelf,
                &shelves,
                linked_devices(&records, RecordType::PowerShelf),
            ),
        ] {
            for device in census.difference(&linked) {
                units.push(mismatch_unit(
                    record_type,
                    MismatchKind::NoExpectedRecord,
                    device.clone(),
                    Some(device.clone()),
                    None,
                    None,
                ));
            }
        }
        Ok(UnitsData::new(units, 0))
    }
}

/// The read IDs of a unit type the expected family links against.
fn census_ids<R>(read: &Result<CensusRead<R>, Failure>) -> Result<BTreeSet<String>, Failure> {
    match read {
        Ok(read) => Ok(read.rows.keys().cloned().collect()),
        Err(failure) => Err(failure.needed_by(Family::Expected)),
    }
}

/// Moves soft-deleted rows to `gone`.
fn drop_deleted<R>(deleted: fn(&R) -> bool) -> impl FnOnce(CensusRead<R>) -> CensusRead<R> {
    move |mut read| {
        let ids: Vec<String> = read
            .rows
            .iter()
            .filter(|(_, row)| deleted(row))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            read.rows.remove(&id);
            read.gone.insert(id);
        }
        read
    }
}

fn timestamp(ts: Option<rpc::Timestamp>) -> Option<DateTime<Utc>> {
    ts.and_then(|ts| DateTime::<Utc>::try_from(ts).ok())
}

/// The open firmware requests joined to one host read.
struct JoinedRequests {
    units: Vec<Unit>,
    /// Each joined host's open request.
    open: BTreeMap<String, FirmwareRequest>,
    /// Requests whose host is not in the host read: the request family is
    /// unresolved, while the joined hosts still show their requests.
    unjoinable: usize,
}

/// Firmware request units, plus each open request by host ID.
fn firmware_requests(
    items: &[HostReprovisioningListItem],
    hosts: &BTreeMap<String, HostDetail>,
) -> JoinedRequests {
    let mut units = Vec::new();
    let mut open = BTreeMap::new();
    let mut unjoinable = 0;
    for item in items {
        let Some(host) = item.id.as_ref().and_then(|id| hosts.get(&id.to_string())) else {
            unjoinable += 1;
            continue;
        };
        let request = FirmwareRequest {
            initiator: initiator(&item.initiator),
            requested_at: timestamp(item.requested_at),
            initiated_at: timestamp(item.initiated_at),
        };
        let id = host.id.to_string();
        if open.insert(id.clone(), request.clone()).is_some() {
            continue;
        }
        let (category, _) = host_state(&host.state);
        let mut fields = host_fields(host, None);
        fields.open_firmware_request = Some(request);
        units.push(Unit {
            id,
            unit_type: UnitType::HostFirmwareRequest,
            source: Source::NicoApi,
            stage: Stage::Registered,
            done: false,
            state: category.to_string(),
            reason: Some(if item.state.is_empty() {
                "open firmware request".to_string()
            } else {
                item.state.clone()
            }),
            bmc_ip: host.bmc_ip.clone(),
            host: Some(fields),
            dpu: None,
            instance: None,
            expected: None,
        });
    }
    JoinedRequests {
        units,
        open,
        unjoinable,
    }
}

/// The category and whether it is in the pinned table.
fn host_state(state: &str) -> (&'static str, bool) {
    match host_category(state) {
        Some(category) => (category, true),
        None => (OTHER, false),
    }
}

fn known_flag(known: &Known<Result<(), String>>) -> (Option<bool>, Option<String>) {
    match known {
        Known::Value(Ok(())) => (Some(true), None),
        Known::Value(Err(reason)) => (Some(false), Some(reason.clone())),
        Known::Unknown(why) => (None, Some((*why).to_string())),
    }
}

fn host_fields(host: &HostDetail, open: Option<&FirmwareRequest>) -> HostFields {
    let (allocatable, blocking_reason) = known_flag(&host.allocatable);
    let (update_eligible, update_blocking_reason) = known_flag(&host.update_eligible);
    HostFields {
        vendor: host.vendor.clone(),
        model: host.model.clone(),
        instance_type: host.instance_type.clone(),
        bmc_version: host.bmc_version.clone(),
        uefi_version: host.uefi_version.clone(),
        version_source: host.version_source,
        report_at: host.report_at,
        report_error: host.report_error.clone(),
        allocatable,
        blocking_reason,
        firmware_autoupdate: host.firmware_autoupdate,
        update_eligible,
        update_blocking_reason,
        update_complete: host.update_complete,
        open_firmware_request: open.cloned(),
        nvlink_domain: host.nvlink_domain.clone(),
        rack_id: host.rack_id.clone(),
        dpu_ids: host.dpu_ids.iter().map(ToString::to_string).collect(),
    }
}

/// Stage and done of a unit by its ready and error states.
fn stage(ready: bool, error: bool) -> Stage {
    if ready {
        Stage::Ready
    } else if error {
        Stage::Error
    } else {
        Stage::Registered
    }
}

/// The full state and the controller's message, for units that are not done.
fn not_done_reason(state: &str, message: Option<&str>) -> String {
    match message {
        Some(message) => format!("{state}: {message}"),
        None => state.to_string(),
    }
}

/// Host units, each with its open firmware request.
fn host_units(
    hosts: &BTreeMap<String, HostDetail>,
    open: &BTreeMap<String, FirmwareRequest>,
) -> UnitsData {
    let mut unknown = 0;
    let units = hosts
        .iter()
        .map(|(id, host)| {
            let (category, known) = host_state(&host.state);
            if !known {
                unknown += 1;
            }
            let done = category == "Ready";
            Unit {
                id: id.clone(),
                unit_type: UnitType::Host,
                source: Source::NicoApi,
                stage: stage(done, category == "Failed"),
                done,
                state: category.to_string(),
                reason: (!done).then(|| not_done_reason(&host.state, host.state_reason.as_deref())),
                bmc_ip: host.bmc_ip.clone(),
                host: Some(host_fields(host, open.get(id))),
                dpu: None,
                instance: None,
                expected: None,
            }
        })
        .collect();
    UnitsData::new(units, unknown)
}

/// DPU units: each census DPU takes the category of the host that lists it.
fn dpu_units(dpus: &[String], hosts: &BTreeMap<String, HostDetail>) -> UnitsData {
    let owner: BTreeMap<String, &HostDetail> = hosts
        .values()
        .flat_map(|host| host.dpu_ids.iter().map(move |dpu| (dpu.to_string(), host)))
        .collect();
    let units = dpus
        .iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| match owner.get(id) {
            Some(host) => {
                let (category, _) = host_state(&host.state);
                let done = dpu_done_under(category);
                Unit {
                    id: id.clone(),
                    unit_type: UnitType::Dpu,
                    source: Source::NicoApi,
                    stage: stage(done, category == "Failed"),
                    done,
                    state: category.to_string(),
                    reason: (!done).then(|| format!("host {}: {}", host.id, host.state)),
                    bmc_ip: None,
                    host: None,
                    dpu: Some(DpuFields {
                        host_id: Some(host.id.to_string()),
                    }),
                    instance: None,
                    expected: None,
                }
            }
            None => Unit {
                id: id.clone(),
                unit_type: UnitType::Dpu,
                source: Source::NicoApi,
                stage: Stage::Registered,
                done: false,
                state: UNATTACHED.to_string(),
                reason: Some(UNATTACHED.to_string()),
                bmc_ip: None,
                host: None,
                dpu: Some(DpuFields { host_id: None }),
                instance: None,
                expected: None,
            },
        })
        .collect();
    UnitsData::new(units, 0)
}

/// The state of a DPU no host lists.
pub(crate) const UNATTACHED: &str = "unattached";

/// What a lifecycle-tracked device (switch, shelf, rack) shows.
struct LifecycleView<'a> {
    lifecycle: Option<&'a forge::LifecycleStatus>,
    bmc_ip: Option<String>,
}

fn switch_view(s: &forge::Switch) -> LifecycleView<'_> {
    LifecycleView {
        lifecycle: s.status.as_ref().and_then(|st| st.lifecycle.as_ref()),
        bmc_ip: s.bmc_info.as_ref().and_then(|b| b.ip.clone()),
    }
}

fn shelf_view(s: &forge::PowerShelf) -> LifecycleView<'_> {
    LifecycleView {
        lifecycle: s.status.as_ref().and_then(|st| st.lifecycle.as_ref()),
        bmc_ip: s.bmc_info.as_ref().and_then(|b| b.ip.clone()),
    }
}

fn rack_view(r: &forge::Rack) -> LifecycleView<'_> {
    LifecycleView {
        lifecycle: r.status.as_ref().and_then(|st| st.lifecycle.as_ref()),
        bmc_ip: None,
    }
}

fn lifecycle_units<R>(
    unit_type: UnitType,
    rows: &BTreeMap<String, R>,
    view: fn(&R) -> LifecycleView<'_>,
) -> UnitsData {
    let mut unknown = 0;
    let units = rows
        .iter()
        .map(|(id, row)| {
            let view = view(row);
            let tag = view.lifecycle.and_then(|l| lifecycle_tag(&l.state));
            if tag.is_none() {
                unknown += 1;
            }
            let state = tag.unwrap_or_else(|| OTHER.to_string());
            let done = state == "ready";
            let message = view
                .lifecycle
                .and_then(|l| l.state_reason.as_ref())
                .and_then(|r| r.outcome_msg.as_deref())
                .filter(|m| !m.is_empty());
            Unit {
                id: id.clone(),
                unit_type,
                source: Source::NicoApi,
                stage: stage(done, state == "error"),
                done,
                reason: (!done).then(|| not_done_reason(&state, message)),
                state,
                bmc_ip: view.bmc_ip.filter(|ip| !ip.is_empty()),
                host: None,
                dpu: None,
                instance: None,
                expected: None,
            }
        })
        .collect();
    UnitsData::new(units, unknown)
}

fn instance_units(rows: &BTreeMap<String, forge::Instance>) -> UnitsData {
    let mut unknown = 0;
    let units = rows
        .iter()
        .map(|(id, instance)| {
            let tenant_status = instance.status.as_ref().and_then(|s| s.tenant.as_ref());
            let state = match tenant_status.map(|t| forge::TenantState::try_from(t.state)) {
                Some(Ok(state)) => state.as_str_name().to_lowercase(),
                _ => {
                    unknown += 1;
                    OTHER.to_string()
                }
            };
            let done = state == "ready";
            let details = tenant_status
                .map(|t| t.state_details.as_str())
                .filter(|d| !d.is_empty());
            Unit {
                id: id.clone(),
                unit_type: UnitType::Instance,
                source: Source::NicoApi,
                stage: stage(done, state == "failed"),
                done,
                reason: (!done).then(|| not_done_reason(&state, details)),
                bmc_ip: None,
                host: None,
                dpu: None,
                instance: Some(InstanceFields {
                    name: instance
                        .metadata
                        .as_ref()
                        .map(|m| m.name.clone())
                        .filter(|n| !n.is_empty()),
                    tenant: instance
                        .config
                        .as_ref()
                        .and_then(|c| c.tenant.as_ref())
                        .map(|t| t.tenant_organization_id.clone())
                        .filter(|t| !t.is_empty()),
                    host_id: instance.machine_id.map(|id| id.to_string()),
                    status: state.clone(),
                }),
                state,
                expected: None,
            }
        })
        .collect();
    UnitsData::new(units, unknown)
}

/// One row of a linked expected-record read. Rows repeat when a record joins
/// several interfaces.
struct LinkRow<'a> {
    expected_id: Option<String>,
    bmc_mac: &'a str,
    explored: Option<&'a str>,
    device: Option<String>,
}

/// One expected host, switch or shelf record and the census devices it
/// links to.
struct Record {
    record_type: RecordType,
    id: String,
    bmc_mac: Option<String>,
    explored: Option<String>,
    devices: BTreeSet<String>,
}

impl Record {
    /// Linked to exactly one device. Two or more is an ambiguous link, which
    /// does not count as linked.
    fn linked(&self) -> bool {
        self.devices.len() == 1
    }

    /// Linked to two or more devices.
    fn ambiguous(&self) -> bool {
        self.devices.len() > 1
    }

    /// The record's unit, and for an ambiguous link also its mismatch unit:
    /// the record stays an expected record, unlinked.
    fn units(&self, explored: Option<&BTreeMap<String, Option<String>>>) -> Vec<Unit> {
        let mut units = vec![self.record_unit(explored)];
        if self.ambiguous() {
            units.push(mismatch_unit(
                self.record_type,
                MismatchKind::AmbiguousLink,
                self.id.clone(),
                None,
                self.bmc_mac.clone(),
                self.explored.clone(),
            ));
        }
        units
    }

    fn record_unit(&self, explored: Option<&BTreeMap<String, Option<String>>>) -> Unit {
        let linked = self.linked();
        let reason = (!linked).then(|| match (&self.explored, explored) {
            _ if self.ambiguous() => {
                format!("ambiguous_link: linked to {} devices", self.devices.len())
            }
            (_, None) => "no_device".to_string(),
            (None, Some(_)) => "no_device: not_explored".to_string(),
            (Some(address), Some(errors)) => match errors.get(address) {
                Some(Some(error)) => format!("no_device: {error}"),
                Some(None) => "no_device".to_string(),
                None => "no_device: not_explored".to_string(),
            },
        });
        Unit {
            id: self.id.clone(),
            unit_type: UnitType::ExpectedRecord,
            source: Source::NicoApi,
            stage: Stage::Expected,
            done: linked,
            state: if linked { "linked" } else { "unlinked" }.to_string(),
            reason,
            bmc_ip: self.explored.clone(),
            host: None,
            dpu: None,
            instance: None,
            expected: Some(ExpectedFields {
                record_type: self.record_type,
                linked,
                device_id: linked
                    .then(|| self.devices.iter().next().cloned())
                    .flatten(),
                bmc_mac: self.bmc_mac.clone(),
                explored_address: self.explored.clone(),
            }),
        }
    }
}

/// Groups linked rows by expected record. Only devices in `census` count as
/// links.
fn link_records<'a>(
    record_type: RecordType,
    rows: impl Iterator<Item = LinkRow<'a>>,
    census: &BTreeSet<String>,
) -> Vec<Record> {
    let mut records: BTreeMap<String, Record> = BTreeMap::new();
    for row in rows {
        let id = row
            .expected_id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| format!("bmc-mac:{}", row.bmc_mac));
        let record = records.entry(id.clone()).or_insert_with(|| Record {
            record_type,
            id,
            bmc_mac: Some(row.bmc_mac.to_string()).filter(|m| !m.is_empty()),
            explored: None,
            devices: BTreeSet::new(),
        });
        if record.explored.is_none() {
            record.explored = row.explored.filter(|a| !a.is_empty()).map(str::to_string);
        }
        if let Some(device) = row.device.filter(|d| census.contains(d)) {
            record.devices.insert(device);
        }
    }
    records.into_values().collect()
}

/// Census devices some record of `record_type` names, linked or ambiguous.
fn linked_devices(records: &[Record], record_type: RecordType) -> BTreeSet<String> {
    records
        .iter()
        .filter(|r| r.record_type == record_type)
        .flat_map(|r| r.devices.iter().cloned())
        .collect()
}

/// An expected rack or rack group record.
fn plain_record(record_type: RecordType, id: String, linked: bool, device: Option<String>) -> Unit {
    Unit {
        id,
        unit_type: UnitType::ExpectedRecord,
        source: Source::NicoApi,
        stage: Stage::Expected,
        done: linked,
        state: if linked { "linked" } else { "unlinked" }.to_string(),
        reason: (!linked).then(|| "no_device".to_string()),
        bmc_ip: None,
        host: None,
        dpu: None,
        instance: None,
        expected: Some(ExpectedFields {
            record_type,
            linked,
            device_id: device,
            bmc_mac: None,
            explored_address: None,
        }),
    }
}

fn mismatch_unit(
    record_type: RecordType,
    kind: MismatchKind,
    id: String,
    device: Option<String>,
    bmc_mac: Option<String>,
    explored: Option<String>,
) -> Unit {
    let state = match kind {
        MismatchKind::AmbiguousLink => "ambiguous_link",
        MismatchKind::NoExpectedRecord => "no_expected_record",
    };
    Unit {
        id,
        unit_type: UnitType::IdentityMismatch,
        source: Source::NicoApi,
        stage: if device.is_some() {
            Stage::Registered
        } else {
            Stage::Expected
        },
        done: false,
        state: state.to_string(),
        reason: Some(state.to_string()),
        bmc_ip: explored.clone(),
        host: None,
        dpu: None,
        instance: None,
        expected: Some(ExpectedFields {
            record_type,
            linked: false,
            device_id: device,
            bmc_mac,
            explored_address: explored,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nico::fake::{self, FakeConnector, FakeNico, FakeSite};
    use crate::snapshot::FailureKind;
    use crate::snapshot::testing::FakeSink;

    struct Harness {
        nico: Arc<FakeNico>,
        sink: Arc<FakeSink>,
        reader: DevicesReader,
    }

    fn harness(site: FakeSite) -> Harness {
        let nico = FakeNico::new(site);
        let sink = Arc::new(FakeSink::default());
        let reader = DevicesReader::new(
            ReaderConfig {
                enabled: true,
                interval: Duration::from_secs(120),
                timeout: Duration::from_secs(600),
            },
            2,
            Arc::new(FakeConnector(Arc::clone(&nico))),
            Arc::new(CallGate::new(
                "nico-api",
                2.0,
                Duration::from_secs(5),
                Duration::from_secs(30),
            )),
            Arc::clone(&sink) as Arc<dyn SnapshotSink>,
            SystemTime::now(),
        );
        Harness { nico, sink, reader }
    }

    impl Harness {
        async fn sweep(&self) -> eyre::Result<()> {
            self.reader.run(&ObservationRecorder::default()).await
        }

        fn units(&self, family: Family) -> Vec<Unit> {
            self.sink
                .latest::<UnitsData>(family)
                .map(|(_, data)| data.units)
                .unwrap_or_default()
        }

        fn unit(&self, family: Family, id: &str) -> Unit {
            self.units(family)
                .into_iter()
                .find(|u| u.id == id)
                .unwrap_or_else(|| panic!("{family:?} has no unit {id}"))
        }
    }

    /// One rack: hosts 1 to 4 with two DPUs each, two switches, one shelf,
    /// all with expected records, and one instance.
    fn small_site() -> FakeSite {
        let mut site = FakeSite::default();
        for i in 1..=4 {
            site.add_host(fake::host(i, "Ready", &[2 * i - 1, 2 * i]));
            site.expected_machines
                .push(fake::expected_machine(i, Some(i)));
        }
        for i in 1..=2 {
            site.add_switch(fake::switch(i, "ready"));
            site.expected_switches
                .push(fake::expected_switch(i, Some(i)));
        }
        site.add_shelf(fake::shelf(1, "ready"));
        site.expected_shelves.push(fake::expected_shelf(1, Some(1)));
        site.add_rack(fake::rack(1, "ready"));
        site.expected_racks.push(fake::expected_rack(1));
        site.add_instance(fake::instance(1, 1, forge::TenantState::Ready));
        site
    }

    fn count_linked(units: &[Unit], record_type: RecordType) -> usize {
        units
            .iter()
            .filter(|u| {
                u.unit_type == UnitType::ExpectedRecord
                    && u.expected
                        .as_ref()
                        .is_some_and(|e| e.record_type == record_type && e.linked)
            })
            .count()
    }

    /// The reader on the framework's scheduler, publishing into a fake
    /// sink.
    fn scheduled(
        nico: &Arc<FakeNico>,
        interval: Duration,
        timeout: Duration,
        calls_per_second: f64,
    ) -> (Arc<FakeSink>, Arc<dyn Probe>) {
        let sink = Arc::new(FakeSink::default());
        let reader = Arc::new(DevicesReader::new(
            ReaderConfig {
                enabled: true,
                interval,
                timeout,
            },
            100,
            Arc::new(FakeConnector(Arc::clone(nico))),
            Arc::new(CallGate::new(
                "nico-api",
                calls_per_second,
                Duration::from_secs(5),
                Duration::from_secs(30),
            )),
            Arc::clone(&sink) as Arc<dyn SnapshotSink>,
            SystemTime::now(),
        ));
        (sink, reader)
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_timeout_records_timeout_and_keeps_snapshot() {
        let nico = FakeNico::new(small_site());
        let (sink, reader) = scheduled(
            &nico,
            Duration::from_secs(60),
            Duration::from_secs(10),
            100.0,
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let (mut results, loops) = crate::framework::start(vec![reader], cancel.clone());
        let drain = tokio::spawn(async move { while results.recv().await.is_some() {} });

        // The first sweep completes.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(sink.count(Family::Host), 1, "first sweep");
        assert!(sink.failure_kinds(Family::Host).is_empty());

        // The next sweep stalls on slow calls and the scheduler drops it at
        // its 10 s timeout.
        nico.set_delay(Duration::from_secs(20));
        tokio::time::sleep(Duration::from_secs(70)).await;
        assert_eq!(
            sink.count(Family::Host),
            1,
            "the host family keeps its snapshot"
        );
        for family in DEVICE_FAMILIES {
            assert_eq!(
                sink.failure_kinds(family),
                vec![FailureKind::Timeout],
                "{family:?}"
            );
        }

        // Shutdown in the middle of the next sweep records nothing.
        tokio::time::sleep(Duration::from_secs(50)).await;
        cancel.cancel();
        crate::framework::join_loops(loops)
            .await
            .expect("loops end");
        drain.await.expect("drain");
        assert_eq!(
            sink.failure_kinds(Family::Host),
            vec![FailureKind::Timeout],
            "a cancelled sweep is not a measured failure"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_timeout_records_each_family_once() {
        let nico = FakeNico::new(small_site());
        let (sink, reader) = scheduled(
            &nico,
            Duration::from_secs(2),
            Duration::from_millis(100),
            1000.0,
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let (mut results, loops) = crate::framework::start(vec![reader], cancel.clone());
        let drain = tokio::spawn(async move { while results.recv().await.is_some() {} });

        while sink.count(Family::Host) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The next sweep (at 2 s) blocks its worker thread in the census
        // past the watchdog (twice the 100 ms timeout), so the run's own
        // timeout cannot fire until the poll returns at 2.7 s. Check before
        // the sweep at 4 s. A blocked worker can strand tokio's timers; other
        // traffic (in production the metrics and API listeners) wakes the
        // other worker to drive them, which a pinger stands in for here.
        let pinging = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pinger = std::thread::spawn({
            let (handle, pinging) = (tokio::runtime::Handle::current(), Arc::clone(&pinging));
            move || {
                while pinging.load(std::sync::atomic::Ordering::SeqCst) {
                    handle.spawn(async {});
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        });
        nico.block_next("find_machine_ids", Duration::from_millis(700));
        // At 2.45 s the run is still blocked: only the watchdog can have
        // recorded the timeout.
        tokio::time::sleep(Duration::from_millis(2_410)).await;
        assert_eq!(
            sink.failure_kinds(Family::Host),
            vec![FailureKind::Timeout],
            "the watchdog records the timeout while the poll is blocked"
        );
        tokio::time::sleep(Duration::from_millis(740)).await;
        pinging.store(false, std::sync::atomic::Ordering::SeqCst);
        pinger.join().expect("pinger");
        cancel.cancel();
        crate::framework::join_loops(loops)
            .await
            .expect("loops end");
        drain.await.expect("drain");

        assert_eq!(sink.count(Family::Host), 1, "the first snapshot stays");
        for family in DEVICE_FAMILIES {
            assert_eq!(
                sink.failure_kinds(family),
                vec![FailureKind::Timeout],
                "{family:?}: one timeout from the watchdog, none from the late run"
            );
        }
    }

    #[test]
    fn known_flags_map_to_value_and_reason() {
        assert_eq!(known_flag(&Known::Value(Ok(()))), (Some(true), None));
        assert_eq!(
            known_flag(&Known::Value(Err("in maintenance".to_string()))),
            (Some(false), Some("in maintenance".to_string()))
        );
        assert_eq!(
            known_flag(&Known::Unknown("compact_host_read_missing")),
            (None, Some("compact_host_read_missing".to_string())),
            "unknown is null with its reason, never false"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_publishes_every_family() {
        let h = harness(small_site());
        h.nico.set_delay(Duration::from_secs(1));
        h.sweep().await.expect("complete sweep");
        assert_eq!(h.nico.calls("find_machine_ids"), 1, "no second census");
        assert_eq!(
            h.nico.calls("find_machines_by_ids"),
            2,
            "4 hosts, 2 per batch"
        );
        let starts: Vec<_> = h.nico.starts().into_iter().map(|(_, at)| at).collect();
        for pair in starts.windows(2) {
            assert!(
                pair[1] - pair[0] >= Duration::from_secs(1),
                "one call at a time"
            );
        }
        for family in DEVICE_FAMILIES {
            assert_eq!(h.sink.count(family), 1, "{family:?}");
        }
        assert_eq!(h.units(Family::Host).len(), 4);
        assert_eq!(h.units(Family::Dpu).len(), 8);
        assert!(h.units(Family::Dpu).iter().all(|u| u.done));
        assert_eq!(h.units(Family::Switch).len(), 2);
        assert_eq!(h.units(Family::PowerShelf).len(), 1);
        assert_eq!(h.units(Family::Rack).len(), 1);
        let instance = &h.units(Family::Instance)[0];
        assert_eq!((instance.state.as_str(), instance.done), ("ready", true));
        let expected = h.units(Family::Expected);
        assert_eq!(count_linked(&expected, RecordType::Host), 4);
        assert_eq!(count_linked(&expected, RecordType::Switch), 2);
        assert_eq!(count_linked(&expected, RecordType::PowerShelf), 1);
        assert_eq!(count_linked(&expected, RecordType::Rack), 1);
        assert!(
            expected
                .iter()
                .all(|u| u.unit_type == UnitType::ExpectedRecord),
            "no identity mismatch on a clean site"
        );
        assert!(h.units(Family::HostFirmwareRequests).is_empty());
        assert_eq!(h.nico.max_in_flight(), 1, "one call in flight");
        let (meta, _) = h.sink.latest::<UnitsData>(Family::Host).expect("host");
        assert!(meta.end >= meta.start);
        assert_eq!(meta.seq, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn mid_sweep_create_delete_no_double_count_no_partial() {
        let h = harness(small_site());
        // Between the census and the by-ID reads: host 2 is deleted and host
        // 9 is created.
        h.nico.after("find_machine_ids", |site| {
            site.hosts.remove(&fake::host_id(2).to_string());
            site.add_host(fake::host(9, "Ready", &[]));
        });
        h.sweep().await.expect("complete sweep");
        let hosts = h.units(Family::Host);
        let ids: Vec<&str> = hosts.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<BTreeSet<_>>().len(),
            "no ID twice"
        );
        assert_eq!(
            hosts.len(),
            3,
            "the deleted host is gone, the new one not yet read"
        );
        assert!(!ids.contains(&fake::host_id(9).to_string().as_str()));

        // A host the by-ID read leaves out while the census still lists it is
        // unresolved: the host family is not published, the others are.
        let before = h.sink.count(Family::Host);
        h.nico
            .site
            .lock()
            .unwrap()
            .withheld
            .insert(fake::host_id(3).to_string());
        assert!(h.sweep().await.is_err(), "incomplete sweep");
        assert_eq!(
            h.sink.count(Family::Host),
            before,
            "no partial host snapshot"
        );
        assert_eq!(
            h.sink.failure_kinds(Family::Host),
            vec![FailureKind::Unresolved]
        );
        assert_eq!(
            h.sink.count(Family::Switch),
            2,
            "other families still publish"
        );
        let census_now = h.nico.site.lock().unwrap().hosts.len();
        assert!(h.units(Family::Host).len() <= census_now);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_batch_keeps_last_family_snapshot() {
        let h = harness(small_site());
        h.sweep().await.expect("first sweep");
        let (first, hosts_before) = h.sink.latest::<UnitsData>(Family::Host).expect("hosts");

        // A host batch fails with an error answer, which is not retried.
        h.nico.fail_next(
            "find_machines_by_ids",
            CallError::Status("Internal: db".to_string()),
        );
        assert!(h.sweep().await.is_err(), "incomplete sweep");
        let (kept, hosts_after) = h.sink.latest::<UnitsData>(Family::Host).expect("hosts");
        assert_eq!(kept, first, "the host family keeps its last snapshot");
        assert_eq!(hosts_after, hosts_before);
        assert_eq!(h.sink.failure_kinds(Family::Host), vec![FailureKind::Rpc]);
        // DPUs, expected records and firmware requests need the host read.
        assert_eq!(h.sink.count(Family::Dpu), 1);
        assert_eq!(h.sink.count(Family::Expected), 1);
        assert_eq!(h.sink.count(Family::Switch), 2);
        assert_eq!(h.sink.count(Family::Instance), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn soft_deleted_rack_is_gone() {
        let mut site = small_site();
        site.add_rack(fake::rack(2, "ready"));
        let h = harness(site);
        // Rack 2 is soft-deleted between the census and its by-ID read; the
        // by-ID read still returns it, with `deleted` set.
        h.nico.after("find_rack_ids", |site| {
            let rack = site.racks.get_mut(&fake::rack_id(2).to_string()).unwrap();
            rack.deleted = Some(SystemTime::now().into());
        });
        h.sweep().await.expect("complete sweep");
        let racks = h.units(Family::Rack);
        assert_eq!(racks.len(), 1);
        assert_eq!(racks[0].id, fake::rack_id(1).to_string());
    }

    #[tokio::test(start_paused = true)]
    async fn dpu_ready_follows_host_category() {
        let mut site = FakeSite::default();
        site.add_host(fake::host(1, "Assigned/Ready", &[1]));
        site.add_host(fake::host(
            2,
            "DPUInitializing/WaitingForNetworkConfig",
            &[2],
        ));
        site.add_host(fake::host(3, "Failed/MachineValidation", &[3]));
        site.add_host(fake::host(4, "Maintenance(PowerCycle)", &[4]));
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        let dpu = |i| h.unit(Family::Dpu, &fake::dpu_id(i).to_string());
        assert_eq!((dpu(1).state.as_str(), dpu(1).done), ("Assigned", true));
        assert_eq!(dpu(1).stage, Stage::Ready);
        assert_eq!(
            (dpu(2).state.as_str(), dpu(2).done),
            ("DPUInitializing", false)
        );
        assert_eq!(dpu(2).stage, Stage::Registered);
        assert_eq!(
            (dpu(3).state.as_str(), dpu(3).stage),
            ("Failed", Stage::Error)
        );
        assert!(!dpu(3).done);
        assert!(dpu(4).done, "maintenance comes after DPU setup");
        assert_eq!(
            dpu(1).dpu.unwrap().host_id,
            Some(fake::host_id(1).to_string())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dpu_not_done_under_configure_astra_created_or_unknown() {
        let mut site = FakeSite::default();
        site.add_host(fake::host(1, "ConfigureAstra/Configuring", &[1]));
        site.add_host(fake::host(2, "Created", &[2]));
        site.add_host(fake::host(3, "BrandNewState/Sub", &[3]));
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        for (i, state) in [(1, "ConfigureAstra"), (2, "Created"), (3, OTHER)] {
            let dpu = h.unit(Family::Dpu, &fake::dpu_id(i).to_string());
            assert_eq!(dpu.state, state);
            assert!(!dpu.done, "{state}");
        }
        let (_, hosts) = h.sink.latest::<UnitsData>(Family::Host).unwrap();
        assert_eq!(hosts.unknown_states, 1, "the unknown host state is counted");
        assert_eq!(
            h.unit(Family::Host, &fake::host_id(3).to_string()).state,
            OTHER
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unattached_dpu_is_unresolved_unit() {
        let mut site = small_site();
        let orphan = fake::dpu_id(99);
        site.dpus.insert(orphan.to_string(), orphan);
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        let dpu = h.unit(Family::Dpu, &orphan.to_string());
        assert_eq!(dpu.state, UNATTACHED);
        assert!(!dpu.done);
        assert_eq!(dpu.dpu.unwrap().host_id, None);
        assert_eq!(dpu.reason.as_deref(), Some(UNATTACHED));
        assert_eq!(h.units(Family::Dpu).len(), 9, "still counted as registered");
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_link_not_counted() {
        let mut site = small_site();
        // Expected host 5 links to hosts 3 and 4 (a BMC MAC on two
        // interfaces), repeating rows.
        let mut row = fake::expected_machine(5, Some(3));
        site.expected_machines.push(row.clone());
        site.expected_machines.push(row.clone());
        row.machine_id = Some(fake::host_id(4));
        site.expected_machines.push(row);
        // Host 6 has no expected record.
        site.add_host(fake::host(6, "Ready", &[]));
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        let expected = h.units(Family::Expected);
        assert_eq!(
            count_linked(&expected, RecordType::Host),
            4,
            "em-5 is not linked"
        );
        let host_records: Vec<&Unit> = expected
            .iter()
            .filter(|u| {
                u.unit_type == UnitType::ExpectedRecord
                    && u.expected.as_ref().unwrap().record_type == RecordType::Host
            })
            .collect();
        assert_eq!(host_records.len(), 5, "em-5 stays an expected record");
        let record = host_records.iter().find(|u| u.id == "em-5").unwrap();
        assert!(!record.done && !record.expected.as_ref().unwrap().linked);
        assert_eq!(record.expected.as_ref().unwrap().device_id, None);
        assert_eq!(
            record.reason.as_deref(),
            Some("ambiguous_link: linked to 2 devices")
        );
        let mismatch: Vec<&Unit> = expected
            .iter()
            .filter(|u| u.unit_type == UnitType::IdentityMismatch && u.id == "em-5")
            .collect();
        assert_eq!(mismatch.len(), 1, "plus one mismatch diagnostic");
        assert_eq!(mismatch[0].state, "ambiguous_link");
        let orphan = h.unit(Family::Expected, &fake::host_id(6).to_string());
        assert_eq!(orphan.unit_type, UnitType::IdentityMismatch);
        assert_eq!(orphan.state, "no_expected_record");
        assert_eq!(orphan.expected.unwrap().record_type, RecordType::Host);
    }

    #[tokio::test(start_paused = true)]
    async fn rack_linked_by_census() {
        let mut site = small_site();
        site.expected_racks.push(fake::expected_rack(2));
        site.expected_rack_groups
            .push(fake::expected_rack_group("group-a"));
        // Expected switch 3 has no switch and an explorer error; expected
        // shelf 2 has never been explored.
        site.expected_switches.push(forge::LinkedExpectedSwitch {
            explored_endpoint_address: Some("10.9.9.9".to_string()),
            ..fake::expected_switch(3, None)
        });
        site.explored.insert(
            "10.9.9.9".to_string(),
            rpc::site_explorer::ExploredEndpoint {
                address: "10.9.9.9".to_string(),
                report: Some(rpc::site_explorer::EndpointExplorationReport {
                    last_exploration_error: Some("Unauthorized".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        site.expected_shelves.push(fake::expected_shelf(2, None));
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        let rack = |id: &str| h.unit(Family::Expected, id);
        let linked = rack(fake::rack_id(1).as_str());
        assert!(linked.done && linked.expected.unwrap().linked);
        let missing = rack(fake::rack_id(2).as_str());
        assert!(!missing.done);
        assert_eq!(missing.expected.unwrap().record_type, RecordType::Rack);
        let group = rack("group-a");
        assert_eq!(group.expected.unwrap().record_type, RecordType::RackGroup);
        assert!(!group.done, "rack groups never link");
        assert_eq!(
            h.unit(Family::Expected, "es-3").reason.as_deref(),
            Some("no_device: Unauthorized")
        );
        assert_eq!(
            h.unit(Family::Expected, "es-3").bmc_ip.as_deref(),
            Some("10.9.9.9")
        );
        assert_eq!(
            h.unit(Family::Expected, "ep-2").reason.as_deref(),
            Some("no_device: not_explored")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unjoinable_firmware_request_keeps_last() {
        let mut site = small_site();
        site.firmware_requests
            .push(fake::firmware_request(1, "Automatic"));
        site.firmware_requests
            .push(fake::firmware_request(2, "AdminCli"));
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        let request = h.unit(Family::HostFirmwareRequests, &fake::host_id(2).to_string());
        assert_eq!(
            request
                .host
                .unwrap()
                .open_firmware_request
                .unwrap()
                .initiator,
            carbide_site_health_probe_api::Initiator::Manual
        );
        let host = h.unit(Family::Host, &fake::host_id(1).to_string());
        assert_eq!(
            host.host.unwrap().open_firmware_request.unwrap().initiator,
            carbide_site_health_probe_api::Initiator::Automatic
        );

        // A request for a host this sweep did not read.
        h.nico
            .site
            .lock()
            .unwrap()
            .firmware_requests
            .push(fake::firmware_request(42, "Automatic"));
        assert!(h.sweep().await.is_err(), "incomplete sweep");
        assert_eq!(h.sink.count(Family::HostFirmwareRequests), 1, "kept");
        assert_eq!(
            h.sink.failure_kinds(Family::HostFirmwareRequests),
            vec![FailureKind::Unresolved]
        );
        assert_eq!(h.sink.count(Family::Host), 2, "hosts still publish");
        let host = h.unit(Family::Host, &fake::host_id(2).to_string());
        assert_eq!(
            host.host.unwrap().open_firmware_request.unwrap().initiator,
            carbide_site_health_probe_api::Initiator::Manual,
            "joined hosts keep their requests"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn manager_request_is_automatic() {
        let mut site = small_site();
        // The machine update manager's own spelling.
        site.firmware_requests
            .push(fake::firmware_request(1, "Automated"));
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        let id = fake::host_id(1).to_string();
        for family in [Family::HostFirmwareRequests, Family::Host] {
            assert_eq!(
                h.unit(family, &id)
                    .host
                    .unwrap()
                    .open_firmware_request
                    .unwrap()
                    .initiator,
                carbide_site_health_probe_api::Initiator::Automatic,
                "{family:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn failed_firmware_read_never_shows_no_request() {
        let mut site = small_site();
        site.firmware_requests
            .push(fake::firmware_request(1, "Automatic"));
        let h = harness(site);
        h.sweep().await.expect("complete sweep");
        let (first, _) = h.sink.latest::<UnitsData>(Family::Host).expect("hosts");

        h.nico.fail_next(
            "list_hosts_waiting_for_reprovisioning",
            CallError::Status("Internal: db".to_string()),
        );
        assert!(h.sweep().await.is_err(), "incomplete sweep");
        let (kept, _) = h.sink.latest::<UnitsData>(Family::Host).expect("hosts");
        assert_eq!(kept, first, "hosts keep their last snapshot");
        assert_eq!(h.sink.failure_kinds(Family::Host), vec![FailureKind::Rpc]);
        let published = h.sink.published.lock().unwrap();
        for (family, snap) in published.iter() {
            if *family != Family::Host {
                continue;
            }
            let units = &snap
                .as_any()
                .downcast_ref::<crate::snapshot::FamilySnapshot<UnitsData>>()
                .unwrap()
                .data
                .units;
            let host = units
                .iter()
                .find(|u| u.id == fake::host_id(1).to_string())
                .unwrap();
            assert!(
                host.host.as_ref().unwrap().open_firmware_request.is_some(),
                "no host snapshot shows the open request as absent"
            );
        }
    }
}
