/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Linux supervisor qualification through Core's authenticated artifact route.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::panic::AssertUnwindSafe;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use carbide_libmlx_model::device::identity::MlxDeviceSelector;
use carbide_libmlx_model::firmware::FirmwareDeliveryMode;
use carbide_uuid::machine::MachineId;
use futures::FutureExt;
use libmlx::firmware::config::{
    FirmwareArtifactDigests, FirmwareFlasherProfile, FirmwareUpdateContext, FlashOptions, FlashSpec,
};
use libmlx::firmware::core_download::CoreFirmwareDownloadConfig;
use libmlx::firmware::operation::{
    FirmwareOperation, FirmwareOperationObservation, FirmwareOperationPhase,
    FirmwareOperationState, FirmwareSupervisorClient, FirmwareSupervisorClientError,
    ObserverExitBehavior,
};
use model::machine::nic_firmware::NicFirmwareOperation;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::time::{sleep, timeout};

use super::super::{CONFIGURATION, IMAGE, Origin, digest, seed};

const DEVICE: &str = "ffff:fe:1f.0";
const SLOT: &str = "ffff:fe:1f";
const WATCHDOG: Duration = Duration::from_secs(10);

/// Exercise the shipping Core transport through real supervisor admission and
/// owned staging. A settled vendor burn failure retains the prepared inputs
/// without claiming hardware qualification.
pub(super) async fn supervisor_stages_from_core(
    pool: &sqlx::PgPool,
    host: MachineId,
    operation: &NicFirmwareOperation,
    origin: &Origin,
    cache_namespace: &Path,
    config: CoreFirmwareDownloadConfig,
) {
    let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
        Some(root) => tempfile::tempdir_in(root),
        None => tempfile::tempdir(),
    }
    .expect("private supervisor fixture");
    let fixture = directory.path();
    for path in [
        fixture.to_path_buf(),
        fixture.join("state"),
        fixture.join("tools"),
    ] {
        fs::create_dir_all(&path).expect("create supervisor fixture directory");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("private supervisor fixture permissions");
    }
    write_vendor_tools(fixture).await;
    let mut process = Command::new(supervisor_binary())
        .args(["serve", "--device", DEVICE, "--state-directory"])
        .arg(fixture.join("state"))
        .env_clear()
        .env("PATH", fixture.join("tools"))
        .env("FIXTURE", fixture)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(fixture.join("daemon.log")).expect("supervisor log"))
        .kill_on_drop(true)
        .spawn()
        .expect("start actual firmware supervisor");

    // Reap the daemon even if an assertion fails or the workflow watchdog fires.
    let proof = AssertUnwindSafe(timeout(Duration::from_secs(45), async {
        let socket = fixture.join("state").join(SLOT).join("control.sock");
        let mut client = timeout(WATCHDOG, async {
            loop {
                if let Some(status) = process.try_wait().expect("supervisor status") {
                    panic!(
                        "supervisor exited: {status}; {}",
                        fs::read_to_string(fixture.join("daemon.log")).expect("supervisor log")
                    );
                }
                if let Ok(client) = FirmwareSupervisorClient::connect(&socket).await {
                    break client;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual supervisor became ready");
        let entry = &operation.targets[0].resolution.entry;
        let profile = FirmwareFlasherProfile {
            firmware_spec: entry.firmware.clone(),
            flash_spec: FlashSpec {
                firmware_url: entry.image.url.to_string(),
                firmware_credentials: None,
                device_conf_url: entry.device_config.as_ref().map(|artifact| artifact.url.to_string()),
                device_conf_credentials: None,
                verify_from_cache: false,
                cache_dir: None,
            },
            flash_options: FlashOptions::default(),
        };
        let context = FirmwareUpdateContext {
            phase_id: operation.operation_id.clone(),
            delivery_mode: FirmwareDeliveryMode::SiteCache,
            expected_digests: FirmwareArtifactDigests {
                image_sha256: Sha256::digest(IMAGE).into(),
                configuration_sha256: Some(Sha256::digest(CONFIGURATION).into()),
            },
            expected_device: Some(MlxDeviceSelector::Mac("02:00:00:00:00:01".parse().unwrap())),
        };
        let mut admitted = operation.clone();
        admitted.delivery_mode = FirmwareDeliveryMode::SiteCache;
        admitted.targets[0].pci_name = DEVICE.into();
        let mut untrusted = admitted.clone();
        untrusted.operation_id.push_str("-untrusted-core");
        let mut untrusted_context = context.clone();
        untrusted_context.phase_id.clone_from(&untrusted.operation_id);
        let instance = observe_missing(&mut client, &untrusted.operation_id, &profile, &untrusted_context).await;
        untrusted.targets[0].supervisor_instance_id = Some(instance.clone());
        seed(pool, host.try_into().expect("host ID"), Some(untrusted.clone())).await;
        let mut wrong_ca_params = CertificateParams::default();
        wrong_ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let wrong_ca = wrong_ca_params
            .self_signed(&KeyPair::generate().expect("untrusted CA key"))
            .expect("untrusted CA certificate");
        let mut untrusted_config = config.clone();
        untrusted_config.root_ca_path = fixture.join("wrong-ca.pem").to_string_lossy().into_owned();
        fs::write(&untrusted_config.root_ca_path, wrong_ca.pem()).expect("untrusted root fixture");
        client.submit_update_via_core(
            &untrusted.operation_id, profile.clone(), untrusted_context,
            ObserverExitBehavior::Continue, &instance, untrusted_config,
        ).await.expect("admit update with untrusted Core root");
        let rejected = terminal_operation(&mut client, &untrusted.operation_id).await;
        assert_eq!(rejected.state, FirmwareOperationState::Failed);
        assert_eq!(rejected.phase, Some(FirmwareOperationPhase::Prepare));
        assert!(!rejected.report.flashed);
        assert_eq!(
            rejected.diagnostic.as_deref(),
            Some("core firmware transfer failed during request transport"),
        );
        let rejected_record = journal_record(fixture, &untrusted.operation_id);
        assert_eq!(rejected_record["mutation_started"], false);
        assert!(rejected_record["prepared_inputs"].is_null());
        assert!(!fixture.join("mutations").exists());
        assert_eq!(origin.paths(), ["/image", "/configuration"], "TLS failure must not fall back to the source");
        timeout(WATCHDOG, async {
            while fixture.join("state/mutation.json").exists() {
                sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("failed preparation released host exclusion");

        // Losing a published cache file must refill through Core before the
        // helper can publish its own digest-checked Prepared inputs.
        fs::remove_file(cache_namespace.join(digest(IMAGE))).expect("remove cached image");
        let instance = observe_missing(&mut client, &admitted.operation_id, &profile, &context).await;
        admitted.targets[0].supervisor_instance_id = Some(instance.clone());
        seed(pool, host.try_into().expect("host ID"), Some(admitted.clone())).await;
        client.submit_update_via_core(
            &admitted.operation_id, profile.clone(), context.clone(),
            ObserverExitBehavior::Continue, &instance, config,
        ).await.expect("submit through the authenticated Core transport");
        let burned = terminal_operation(&mut client, &admitted.operation_id).await;
        assert_eq!(burned.state, FirmwareOperationState::ManualIntervention);
        assert_eq!(burned.phase, Some(FirmwareOperationPhase::Burn));
        assert!(!burned.report.flashed);
        assert!(burned.diagnostic.as_deref().is_some_and(|diagnostic| diagnostic.contains("settled fixture burn failure")));
        let record = journal_record(fixture, &admitted.operation_id);
        assert_eq!(record["operation"], serde_json::to_value(&burned).unwrap());
        assert_eq!(record["mutation_started"], true);
        assert_eq!(record["requested"]["update_context"]["delivery_mode"], "site_cache");
        for (role, bytes) in [("image", IMAGE), ("configuration", CONFIGURATION)] {
            let artifact = &record["prepared_inputs"][role];
            let relative = Path::new(artifact["path"].as_str().expect("relative prepared path"));
            assert!(!relative.as_os_str().is_empty());
            assert!(relative.components().all(|component| matches!(component, Component::Normal(_))));
            let owned = fixture.join("state").join(SLOT).join(relative);
            assert_eq!(fs::read(&owned).expect("owned prepared bytes"), bytes);
            assert_eq!(artifact["sha256"], serde_json::to_value(Sha256::digest(bytes).to_vec()).unwrap());
            assert_eq!(record["requested"]["update_context"]["expected_digests"][format!("{role}_sha256")], artifact["sha256"]);
            assert_eq!(
                fs::read_to_string(fixture.join(format!("prepared-{role}"))).expect("vendor input path"),
                owned.to_string_lossy(),
            );
            assert_eq!(fs::read(cache_namespace.join(digest(bytes))).expect("Core cached bytes"), bytes);
        }
        assert_eq!(fs::read_to_string(fixture.join("mutations")).expect("vendor mutations"), "configure\nburn\n");
        assert_eq!(origin.paths(), ["/image", "/configuration", "/image"]);

        // The fetch address is Core's digest route, but the original source
        // remains part of the immutable request identity.
        let mut changed_source = profile;
        changed_source.flash_spec.firmware_url = format!("{}/replacement", origin.url);
        let conflict = client.observe_update(
            &admitted.operation_id, changed_source, context, ObserverExitBehavior::Continue,
        ).await.expect_err("changed original source must conflict");
        assert!(matches!(conflict, FirmwareSupervisorClientError::Rpc(status) if status.code() == tonic::Code::AlreadyExists));
    }))
    .catch_unwind()
    .await;
    process.start_kill().expect("stop actual supervisor");
    timeout(WATCHDOG, process.wait())
        .await
        .expect("supervisor exited")
        .expect("reap actual supervisor");
    match proof {
        Ok(result) => result.expect("Core-to-supervisor staging exceeded its watchdog"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Locate the shipping helper built by the normal test prerequisites.
fn supervisor_binary() -> PathBuf {
    let executable = std::env::current_exe().expect("test executable");
    let helper = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mlx-firmware-supervisor");
    assert!(
        helper.is_file(),
        "run cargo make build-nic-firmware-test-helper before Core artifact TLS tests"
    );
    helper
}

/// Read missing work to obtain the instance token required for guarded admission.
async fn observe_missing(
    client: &mut FirmwareSupervisorClient,
    operation_id: &str,
    profile: &FirmwareFlasherProfile,
    context: &FirmwareUpdateContext,
) -> String {
    match client
        .observe_update(
            operation_id,
            profile.clone(),
            context.clone(),
            ObserverExitBehavior::Continue,
        )
        .await
        .expect("read before supervisor admission")
    {
        FirmwareOperationObservation::MissingReceipt {
            supervisor_instance_id,
        } => supervisor_instance_id,
        FirmwareOperationObservation::Receipt(receipt) => {
            panic!("unexpected existing receipt: {receipt:?}")
        }
    }
}

/// Read until execution settles so each caller can assert its exact terminal state.
async fn terminal_operation(
    client: &mut FirmwareSupervisorClient,
    operation_id: &str,
) -> FirmwareOperation {
    loop {
        let operation = client
            .get(operation_id)
            .await
            .expect("read supervisor receipt");
        match operation.state {
            FirmwareOperationState::Accepted
            | FirmwareOperationState::Running
            | FirmwareOperationState::CancelRequested => {
                sleep(Duration::from_millis(10)).await;
            }
            FirmwareOperationState::Complete
            | FirmwareOperationState::Canceled
            | FirmwareOperationState::Interrupted
            | FirmwareOperationState::ManualIntervention
            | FirmwareOperationState::Failed => return operation,
        }
    }
}

/// Load one durable operation record to inspect its prepared bytes and mutation intent.
fn journal_record(fixture: &Path, operation_id: &str) -> serde_json::Value {
    let ledger: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.join("state").join(SLOT).join("operations.json"))
            .expect("supervisor journal"),
    )
    .expect("decode supervisor journal");
    ledger["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["operation"]["operation_id"] == operation_id)
        .expect("durable operation record")
        .clone()
}

/// Install strict vendor fixtures that expose staged paths and stop after burn starts.
async fn write_vendor_tools(fixture: &Path) {
    let script = r#"#!/bin/sh
set -eu
if [ "${1-}" = --version ]; then exit 0; fi
case "${0##*/}" in
    mlxfwmanager)
        [ "$#" -eq 4 ] && [ "$1" = --dev ] && [ "$2" = ffff:fe:1f.0 ] &&
            [ "$3" = --query-format ] && [ "$4" = xml ] || exit 40
        printf '<Devices><Device pciName="ffff:fe:1f.0" type="BlueField3" partNumber="test-card" psid="test-psid"><Versions><FW current="1.0"/></Versions><MACs Base_Mac="020000000001"/><GUIDs Base_Guid="0200000300000001"/></Device></Devices>'
        ;;
    mlxconfig)
        [ "$#" -eq 5 ] && [ "$1" = -d ] && [ "$2" = ffff:fe:1f.0 ] &&
            [ "$3" = --yes ] && [ "$4" = apply ] && [ -s "$5" ] || exit 41
        printf '%s' "$5" > "$FIXTURE/prepared-configuration"
        printf 'configure\n' >> "$FIXTURE/mutations"
        ;;
    flint)
        [ "$#" -eq 6 ] && [ "$1" = -d ] && [ "$2" = ffff:fe:1f.0 ] &&
            [ "$3" = -y ] && [ "$4" = -i ] && [ -s "$5" ] && [ "$6" = burn ] || exit 42
        printf '%s' "$5" > "$FIXTURE/prepared-image"
        printf 'burn\n' >> "$FIXTURE/mutations"
        printf 'settled fixture burn failure\n' >&2
        exit 17
        ;;
    *) exit 43 ;;
esac
"#;
    // The writer exits before exec, avoiding ETXTBSY when other tests fork.
    let status = timeout(WATCHDOG, Command::new("/bin/sh")
        .args(["-c", "umask 077; for tool in mlxfwmanager mlxconfig flint; do printf '%s' \"$2\" > \"$1/$tool\"; done", "fixture-writer"])
        .arg(fixture.join("tools"))
        .arg(script)
        .kill_on_drop(true)
        .status()).await.expect("vendor fixture writer exited").expect("write vendor tools");
    assert!(status.success());
    for tool in ["mlxfwmanager", "mlxconfig", "flint"] {
        fs::set_permissions(
            fixture.join("tools").join(tool),
            fs::Permissions::from_mode(0o700),
        )
        .expect("executable vendor fixture");
    }
}
