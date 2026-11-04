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

//! Compact golden vectors under `testdata/`, shared with the Go scale-test
//! runner (`dev/k8s/scale-test/internal/probeapi`), which reads the same
//! files. This crate owns them: each file is the one-line JSON of a value
//! built here and must read back to it, so a drift in either language's
//! view of the `/v1` wire contract fails a test.
//!
//! The vectors cover the fields the v1 runner consumes: count, units,
//! units/query, config and error bodies, plus the integer, enum, null and
//! UTF-8 boundaries both strict decoders must agree on. Additive-field cases
//! are generated inside each language's test, not stored.
//!
//! - `testdata/v1/<type>.<case>.json`: one wire value of `<type>`.
//! - `testdata/v1/invalid/<type>.<case>.json`: hand-written replies and
//!   bodies that both languages must reject.
//! - `testdata/enums.json`: every consumed enum's wire names, in order.
//!
//! Regenerate after a deliberate contract change with
//! `UPDATE_GOLDEN=1 cargo test -p carbide-site-health-probe-api --test golden_vectors`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use carbide_site_health_probe_api::{metrics, *};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn update() -> bool {
    std::env::var_os("UPDATE_GOLDEN").is_some()
}

/// Checks that `path` holds the compact JSON of `value`, or writes it under
/// `UPDATE_GOLDEN`, and returns the file text.
fn check_text<T: Serialize>(path: &Path, value: &T) -> String {
    let text = serde_json::to_string(value).unwrap() + "\n";
    if update() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &text).unwrap();
    }
    let file = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with UPDATE_GOLDEN=1", path.display()));
    assert_eq!(
        file,
        text,
        "{} is stale; run with UPDATE_GOLDEN=1",
        path.display()
    );
    file
}

/// Checks a wire vector and that it reads back to `value`.
fn check_vector<T>(written: &mut BTreeSet<String>, name: &str, value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let path = testdata().join("v1").join(name);
    let file = check_text(&path, value);
    let back: T = serde_json::from_str(&file).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(&back, value, "{name}");
    assert!(written.insert(name.to_string()), "{name} twice");
}

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

/// Text that needs every kind of JSON escape, and characters a Go encoder
/// escapes where serde_json does not.
const ESCAPES: &str = "Ünïcödé ✓ 🚀 \"quoted\" back\\slash\nline\ttab\r\u{8}\u{c}\u{1}\u{1f}\u{7f} \
                       </script>&<b> \u{2028}\u{2029} \u{feff}";

fn sweep_info() -> SweepInfo {
    SweepInfo {
        snapshot_id: "s-7".to_string(),
        family: Family::Host,
        sweep_start: t("2026-10-23T10:00:00Z"),
        read_at: t("2026-10-23T10:00:40.500Z"),
        sweep_age_seconds: 3.5,
        sweep_interval_seconds: 120.0,
    }
}

fn read_info() -> ReadInfo {
    ReadInfo {
        read_start: t("2026-10-23T10:00:00Z"),
        read_at: t("2026-10-23T10:00:02Z"),
        read_age_seconds: 5.0,
        read_interval_seconds: 120.0,
        last_attempt_error: None,
    }
}

fn unit(unit_type: UnitType, id: &str) -> Unit {
    Unit {
        id: id.to_string(),
        unit_type,
        source: Source::NicoApi,
        stage: Stage::Registered,
        done: false,
        state: "s".to_string(),
        reason: None,
        bmc_ip: None,
        host: None,
        dpu: None,
        instance: None,
        expected: None,
    }
}

fn host_unit() -> Unit {
    Unit {
        reason: s("waiting for DPU"),
        bmc_ip: s("10.0.0.5"),
        state: "HostInitializing".to_string(),
        host: Some(HostFields {
            vendor: s("NVIDIA"),
            model: s("GB200 NVL"),
            instance_type: None,
            bmc_version: s("1.2.3"),
            uefi_version: None,
            version_source: VersionSource::MachineRecord,
            report_at: None,
            report_error: None,
            allocatable: None,
            blocking_reason: None,
            firmware_autoupdate: Some(true),
            update_eligible: None,
            update_blocking_reason: None,
            update_complete: false,
            open_firmware_request: Some(FirmwareRequest {
                initiator: Initiator::Automatic,
                requested_at: Some(t("2026-10-23T09:59:00Z")),
                initiated_at: None,
            }),
            nvlink_domain: None,
            rack_id: s("r1"),
            dpu_ids: vec!["fm100d".to_string()],
        }),
        ..unit(UnitType::Host, "fm100h")
    }
}

/// A host with every optional field set, read from the explored report.
fn full_host_unit() -> Unit {
    Unit {
        stage: Stage::Ready,
        done: true,
        state: "Ready".to_string(),
        host: Some(HostFields {
            vendor: s("Dell Inc."),
            model: s("PowerEdge R750"),
            instance_type: s("it-1"),
            bmc_version: s("7.10.30.00"),
            uefi_version: s("1.13.2"),
            version_source: VersionSource::ExploredReport,
            report_at: Some(t("2026-10-23T09:58:00.123456Z")),
            report_error: s("timeout"),
            allocatable: Some(false),
            blocking_reason: s("in maintenance"),
            firmware_autoupdate: Some(false),
            update_eligible: Some(true),
            update_blocking_reason: s("none"),
            update_complete: true,
            open_firmware_request: Some(FirmwareRequest {
                initiator: Initiator::Other,
                requested_at: None,
                initiated_at: Some(t("2026-10-23T09:59:30.000000001Z")),
            }),
            nvlink_domain: s("nvl-1"),
            rack_id: None,
            dpu_ids: vec![],
        }),
        ..unit(UnitType::Host, "fm100r")
    }
}

#[test]
fn wire_vectors_match_the_files() {
    let mut written = BTreeSet::new();

    check_vector(
        &mut written,
        "count-response.typical.json",
        &CountResponse {
            count: 4321,
            sweep: sweep_info(),
        },
    );

    check_vector(
        &mut written,
        "units-response.every-unit.json",
        &UnitsResponse {
            sweep: sweep_info(),
            units: vec![
                host_unit(),
                full_host_unit(),
                Unit {
                    dpu: Some(DpuFields { host_id: None }),
                    ..unit(UnitType::Dpu, "fm100d")
                },
                Unit {
                    stage: Stage::Error,
                    state: "Failed".to_string(),
                    reason: s("dpu reboot"),
                    dpu: Some(DpuFields {
                        host_id: s("fm100h"),
                    }),
                    ..unit(UnitType::Dpu, "fm100e")
                },
                Unit {
                    source: Source::Rest,
                    stage: Stage::Ready,
                    done: true,
                    instance: Some(InstanceFields {
                        name: s("scale-1"),
                        tenant: s("t1"),
                        host_id: s("fm100h"),
                        status: "Ready".to_string(),
                    }),
                    ..unit(UnitType::Instance, "i-1")
                },
                Unit {
                    instance: Some(InstanceFields {
                        name: None,
                        tenant: None,
                        host_id: None,
                        status: "Provisioning".to_string(),
                    }),
                    ..unit(UnitType::Instance, "i-2")
                },
                Unit {
                    stage: Stage::Expected,
                    expected: Some(ExpectedFields {
                        record_type: RecordType::Switch,
                        linked: false,
                        device_id: None,
                        bmc_mac: s("aa:bb:cc:dd:ee:ff"),
                        explored_address: None,
                    }),
                    ..unit(UnitType::ExpectedRecord, "e-1")
                },
                Unit {
                    stage: Stage::Expected,
                    done: true,
                    expected: Some(ExpectedFields {
                        record_type: RecordType::RackGroup,
                        linked: true,
                        device_id: s("rg-1"),
                        bmc_mac: None,
                        explored_address: s("10.0.0.9"),
                    }),
                    ..unit(UnitType::ExpectedRecord, "e-2")
                },
                unit(UnitType::Rack, "r1"),
                unit(UnitType::Switch, "sw-1"),
                unit(UnitType::PowerShelf, "ps-1"),
                unit(UnitType::IdentityMismatch, "m-1"),
                unit(UnitType::HostFirmwareRequest, "fw-1"),
                unit(UnitType::RestTotal, "rt-1"),
            ],
            missing_ids: vec!["gone-1".to_string()],
            next_cursor: s("eyJz"),
        },
    );

    let mut filter = Filter::new(UnitType::Host);
    filter.source = Some(Source::NicoApi);
    filter.stage = Some(Stage::Ready);
    filter.state = s("Ready");
    filter.model = s("GB200 NVL");
    filter.instance_type = s("it-1");
    filter.bmc_version = s("1.2.3");
    filter.uefi_version = s("4.5");
    filter.allocatable = Some(true);
    filter.update_complete = Some(false);
    filter.update_eligible = Some(true);
    filter.tenant = s("t1");
    filter.status = s("Ready");
    filter.record_type = Some(RecordType::PowerShelf);
    filter.linked = Some(false);
    filter.resource = Some(RestResource::Tray);
    filter.initiator = Some(Initiator::Manual);
    filter.kind = Some(MismatchKind::AmbiguousLink);
    filter.done = Some(false);
    check_vector(
        &mut written,
        "units-query.every-field.json",
        &UnitsQuery {
            filter,
            name: s("^scale-.*$"),
            ids: vec!["i-1".to_string(), "i-2".to_string()],
            limit: Some(u32::MAX),
            cursor: s("eyJz"),
        },
    );

    let all_families = variants_family();
    check_vector(
        &mut written,
        "config-response.full.json",
        &ConfigResponse {
            nico_api: Some(ConfigNicoApi {
                read: ReadInfo {
                    last_attempt_error: s("rpc: deadline exceeded"),
                    ..read_info()
                },
                firmware_targets: [
                    Component::Bmc,
                    Component::Uefi,
                    Component::BmcUefi,
                    Component::Other,
                ]
                .into_iter()
                .enumerate()
                .map(|(i, component)| FirmwareTarget {
                    vendor: "NVIDIA".to_string(),
                    model: "GB200 NVL".to_string(),
                    component,
                    version: format!("1.{i}"),
                    explicit_start: i % 2 == 0,
                })
                .collect(),
                runtime: RuntimeSettings {
                    max_concurrent_machine_updates_absolute: Some(i64::MAX),
                    max_concurrent_machine_updates_percent: Some(0),
                    auto_host_firmware_update: Some(false),
                },
                site_explorer: Some(SiteExplorerRun {
                    started_at: Some(t("2026-10-23T09:00:00Z")),
                    finished_at: Some(t("2026-10-23T09:01:30.250Z")),
                    last_successful_finished_at: None,
                    success: false,
                    explorations_attempted: 4500,
                    explorations_succeeded: 4499,
                    explorations_failed: 1,
                }),
            }),
            rest: Some(ConfigRest {
                read: ReadInfo {
                    read_interval_seconds: 1e21,
                    ..read_info()
                },
                tenant_allocation: Some(TenantAllocation {
                    tenant: "t1".to_string(),
                    instance_type: "it-1".to_string(),
                    assigned: 4,
                    total: 5,
                    used: 3,
                    unused: -2,
                    unused_usable: i64::MIN,
                }),
            }),
            rest_site: Some(RestSiteBinding {
                read: ReadInfo {
                    last_attempt_error: s("412 Precondition Failed"),
                    ..read_info()
                },
                result: SiteMatch::Mismatch,
            }),
            metrics: Some(MetricsInfo {
                contract_version: metrics::CONTRACT_VERSION,
                selector: BTreeMap::from([
                    ("job".to_string(), "probe-metrics".to_string()),
                    ("namespace".to_string(), "nico-system".to_string()),
                    ("cluster".to_string(), "a \"b\" ✓".to_string()),
                ]),
                scrape_interval_seconds: 0.1,
                reader_intervals_seconds: all_families
                    .iter()
                    .enumerate()
                    .map(|(i, family)| (*family, 30.0 * (i as f64 + 1.0)))
                    .collect(),
                process_start_seconds: 1_761_818_400.25,
            }),
        },
    );

    check_vector(
        &mut written,
        "error-body.field-unavailable.json",
        &ErrorBody {
            error: ErrorDetail {
                code: ErrorCode::FieldUnavailable,
                message: "allocatability is unknown until nico-api serves the compact host read"
                    .to_string(),
                field: s("allocatable"),
            },
        },
    );
    let codes = variants_error_code();
    check_vector(
        &mut written,
        "error-bodies.every-code.json",
        &codes
            .iter()
            .enumerate()
            .map(|(i, code)| ErrorBody {
                error: ErrorDetail {
                    code: *code,
                    message: if i == 0 {
                        ESCAPES.to_string()
                    } else {
                        format!("{} {i}", metrics::wire(code))
                    },
                    field: (i % 2 == 0).then(|| "bmcVersion".to_string()),
                },
            })
            .collect::<Vec<_>>(),
    );

    // Every file under testdata/v1 is one of these, so a vector this test
    // stopped writing cannot linger for the Go side to read.
    let on_disk: BTreeSet<String> = fs::read_dir(testdata().join("v1"))
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect();
    assert_eq!(on_disk, written);

    // After every positive fixture is written and checked, each reply still
    // reads the same with a field a newer probe could add. Run here, not in a
    // separate test, so no reader races the UPDATE_GOLDEN writer above.
    check_additive_reply::<CountResponse>("count-response.typical.json");
    check_additive_reply::<UnitsResponse>("units-response.every-unit.json");
    check_additive_reply::<ConfigResponse>("config-response.full.json");
    check_additive_reply::<ErrorBody>("error-body.field-unavailable.json");
}

/// Every variant of an enum, in declaration order. The closure is an
/// exhaustive match, so a new variant fails to compile until it is listed.
macro_rules! variants {
    ($ty:ident: $($v:ident),+ $(,)?) => {{
        let _exhaustive = |v: $ty| match v {
            $($ty::$v)|+ => (),
        };
        let all = vec![$($ty::$v),+];
        assert!(all.windows(2).all(|w| w[0] < w[1]), "{} is not in declaration order", stringify!($ty));
        all
    }};
}

fn variants_family() -> Vec<Family> {
    variants!(Family: Host, Dpu, Switch, PowerShelf, Rack, Instance, Expected,
        HostFirmwareRequests, RestInstances, RestTotals, RestSiteMatch,
        RestTenantInstances, ConfigNicoApi, ConfigRest)
}

fn variants_error_code() -> Vec<ErrorCode> {
    variants!(ErrorCode: InvalidParam, UnsupportedFilter, FieldUnavailable,
        NoCompleteSweep, SnapshotExpired, EndInFuture, Busy, StoreUnavailable,
        StoreTimeout, NotFound, Internal)
}

/// The wire names of `all`, checked to read back to the same values.
fn wire_names<T>(all: Vec<T>) -> Vec<String>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let names: Vec<String> = all.iter().map(metrics::wire).collect();
    assert_eq!(
        names.iter().collect::<BTreeSet<_>>().len(),
        names.len(),
        "{names:?}"
    );
    for (value, name) in all.iter().zip(&names) {
        assert!(!name.is_empty(), "{value:?}");
        let back: T = serde_json::from_value(serde_json::Value::from(name.as_str())).unwrap();
        assert_eq!(&back, value);
    }
    names
}

#[test]
fn enum_names_match_the_file() {
    let enums: BTreeMap<&str, Vec<String>> = BTreeMap::from([
        (
            "UnitType",
            wire_names(variants!(UnitType: Rack, Host, Dpu, Switch, PowerShelf,
                Instance, ExpectedRecord, IdentityMismatch, HostFirmwareRequest, RestTotal)),
        ),
        (
            "Stage",
            wire_names(variants!(Stage: Expected, Registered, Ready, Error)),
        ),
        ("Source", wire_names(variants!(Source: NicoApi, Rest))),
        (
            "SiteMatch",
            wire_names(variants!(SiteMatch: Match, Mismatch, Unknown)),
        ),
        (
            "RecordType",
            wire_names(variants!(RecordType: Host, Switch, PowerShelf, Rack, RackGroup)),
        ),
        (
            "RestResource",
            wire_names(variants!(RestResource: Machine, Instance, Rack, Tray)),
        ),
        (
            "Initiator",
            wire_names(variants!(Initiator: Automatic, Manual, Other)),
        ),
        (
            "Component",
            wire_names(variants!(Component: Bmc, Uefi, BmcUefi, Other)),
        ),
        (
            "MismatchKind",
            wire_names(variants!(MismatchKind: AmbiguousLink, NoExpectedRecord)),
        ),
        ("Direction", wire_names(variants!(Direction: Up, Down))),
        (
            "VersionSource",
            wire_names(variants!(VersionSource: MachineRecord, ExploredReport)),
        ),
        ("Family", wire_names(variants_family())),
        ("ErrorCode", wire_names(variants_error_code())),
    ]);
    check_text(&testdata().join("enums.json"), &enums);
}

/// A reply with a field a newer probe could add reads as the reply without
/// it. The extra field is injected here, so no additive file is stored.
fn check_additive_reply<T>(name: &str)
where
    T: serde::de::DeserializeOwned + Serialize + PartialEq + Debug,
{
    let text = fs::read_to_string(testdata().join("v1").join(name)).unwrap();
    let clean: T = serde_json::from_str(&text).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value.as_object_mut().unwrap().insert(
        "zzzAddedByANewerProbe".to_string(),
        serde_json::json!([1, {"nested": null}, "x"]),
    );
    let augmented = serde_json::to_string(&value).unwrap();
    let back: T = serde_json::from_str(&augmented).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(back, clean, "{name}");
}

/// Each file under `testdata/v1/invalid`, then a part of the serde error it
/// must fail with, so a vector cannot pass by failing for another reason.
const INVALID: &[&str] = &[
    "config-response.fractional-i64.json => expected i64",
    "config-response.i64-above-max.json => expected i64",
    "config-response.i64-below-min.json => expected i64",
    "config-response.null-runtime.json => invalid type: null",
    "config-response.target-missing-component.json => missing field `component`",
    "config-response.metrics-missing-selector.json => missing field `selector`",
    "config-response.metrics-negative-version.json => expected u32",
    "config-response.null-firmware-targets.json => invalid type: null",
    "config-response.site-missing-read-at.json => missing field `readAt`",
    "config-response.unknown-family-key.json => unknown variant `hosts`",
    "config-response.unknown-site-match.json => unknown variant `matched`",
    "count-response.array.json => invalid type: map",
    "count-response.comma-fraction.json => invalid characters",
    "count-response.count-over-u64.json => expected u64",
    "count-response.duplicate-key.json => duplicate field `count`",
    "count-response.float-count.json => invalid type: floating point",
    "count-response.float-out-of-range.json => number out of range",
    "count-response.lone-high-surrogate.json => unexpected end of hex escape",
    "count-response.lone-low-surrogate.json => lone leading surrogate",
    "count-response.missing-count.json => missing field `count`",
    "count-response.missing-sweep-age.json => missing field `sweepAgeSeconds`",
    "count-response.negative-count.json => expected u64",
    "count-response.null-count.json => invalid type: null, expected u64",
    "count-response.null-sweep-age.json => invalid type: null, expected f64",
    "count-response.string-count.json => invalid type: string",
    "count-response.time-without-zone.json => premature end of input",
    "count-response.time-offset-hour-24.json => input is out of range",
    "count-response.time-offset-minute-60.json => input is out of range",
    "count-response.trailing-value.json => trailing characters",
    "count-response.unknown-family.json => unknown variant `hosts`",
    "count-response.wrong-case-family.json => unknown variant `Host`",
    "count-response.wrong-case-key.json => missing field `count`",
    "error-body.missing-error.json => missing field `error`",
    "error-body.missing-message.json => missing field `message`",
    "error-body.unknown-code.json => unknown variant `teapot`",
    "units-query.limit-over-u32.json => expected u32",
    "units-query.missing-type.json => missing field `type`",
    "units-query.null-ids.json => invalid type: null",
    "units-query.string-bool.json => expected a boolean",
    "units-query.unknown-field.json => unknown field `bogus`",
    "units-query.unknown-stage.json => unknown variant `done`",
    "units-response.missing-missing-ids.json => missing field `missingIds`",
    "units-response.missing-update-complete.json => missing field `updateComplete`",
    "units-response.null-done.json => invalid type: null, expected a boolean",
    "units-response.null-instance-status.json => invalid type: null, expected a string",
    "units-response.null-in-missing-ids.json => invalid type: null, expected a string",
    "units-response.null-units.json => invalid type: null",
    "units-response.unknown-source.json => unknown variant `nico_api`",
];

fn read_error<T: DeserializeOwned>(text: &str) -> String {
    match serde_json::from_str::<T>(text) {
        Ok(_) => "accepted".to_string(),
        Err(e) => e.to_string(),
    }
}

#[test]
fn invalid_vectors_are_rejected() {
    let dir = testdata().join("v1").join("invalid");
    let on_disk: BTreeSet<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let rows: Vec<(&str, &str)> = INVALID
        .iter()
        .map(|row| row.split_once(" => ").unwrap())
        .collect();
    let listed: BTreeSet<String> = rows.iter().map(|(name, _)| name.to_string()).collect();
    assert_eq!(on_disk, listed);
    let mut wrong = Vec::new();
    for (name, want) in rows {
        let text = fs::read_to_string(dir.join(name)).unwrap();
        let (ty, _) = name.split_once('.').unwrap();
        let error = match ty {
            "count-response" => read_error::<CountResponse>(&text),
            "units-response" => read_error::<UnitsResponse>(&text),
            "units-query" => read_error::<UnitsQuery>(&text),
            "config-response" => read_error::<ConfigResponse>(&text),
            "error-body" => read_error::<ErrorBody>(&text),
            other => panic!("{name}: no type named {other}"),
        };
        if !error.contains(want) {
            wrong.push(format!("{name}: {error}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
