// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::fs::PermissionsExt;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::FutureExt;
use tower::ServiceExt;

use super::*;
use crate::{
    DpuMachineInfo, DpuOsImage, DpuSettings, HardwareType, MachineInfo, MachineRouterOptions,
};

// Allow process scheduling under the full parallel suite without busy polling.
// These bounds apply only to test coordination, not reset readiness.
const TEST_IO_WAIT: Duration = Duration::from_secs(10);
const TEST_IO_POLL: Duration = Duration::from_millis(1);

// Exercise the actual actor and virsh executor. Only the executable is fake,
// so no installed libvirt daemon or guest domain is needed on the test host.
struct TestDomain {
    files: tempfile::TempDir,
    router: Router,
    state: Arc<BmcState<LibvirtCallbacks>>,
    callbacks: Arc<LibvirtCallbacks>,
    tasks: JoinSet<()>,
    stop: CancellationToken,
}

impl TestDomain {
    fn unstarted(initial: &str, delay: Duration) -> (Self, LibvirtActor) {
        let files = tempfile::tempdir().unwrap();
        std::fs::write(files.path().join("power"), initial).unwrap();
        std::fs::write(files.path().join("calls"), "").unwrap();
        std::fs::write(
            files.path().join("domain.xml"),
            "<domain><os/><devices/></domain>",
        )
        .unwrap();
        let executable = files.path().join("virsh");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
set -eu
fixture_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
shift 2
operation=$1
printf '%s\n' "$operation" >> "$fixture_dir/calls"
if [ -f "$fixture_dir/fail-$operation" ]; then
    printf 'injected %s failure\n' "$operation" >&2
    exit 1
fi
if [ -f "$fixture_dir/hold-$operation" ]; then
    : > "$fixture_dir/entered-$operation"
    while [ -f "$fixture_dir/hold-$operation" ]; do sleep 0.01; done
fi
if [ "$operation" = define ] && [ -f "$fixture_dir/hold-cleanup" ] && [ "$(cat "$fixture_dir/power")" = running ]; then
    : > "$fixture_dir/entered-cleanup"
    while [ -f "$fixture_dir/hold-cleanup" ]; do sleep 0.01; done
fi
case "$operation" in
    domstate) cat "$fixture_dir/power" ;;
    dumpxml) cat "$fixture_dir/domain.xml" ;;
    define) cat > "$fixture_dir/domain.xml" ;;
    start)
        if [ "$(cat "$fixture_dir/power")" = running ]; then exit 1; fi
        printf 'running\n' > "$fixture_dir/power" ;;
    reset|reboot) printf 'running\n' > "$fixture_dir/power" ;;
    destroy|shutdown) printf 'shut off\n' > "$fixture_dir/power" ;;
    *) printf 'unexpected command: %s\n' "$operation" >&2; exit 2 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let stop = CancellationToken::new();
        let (actor, callbacks) = LibvirtActor::new(
            Config {
                state_file: None,
                virsh_path: executable,
                uri: "test:///fixture".into(),
                domain: "test-dpu".into(),
                virtual_media_targets: Default::default(),
            },
            stop.clone().drop_guard(),
        );
        let callbacks = Arc::new(callbacks);
        let info = {
            let mut pool = crate::test_support::TEST_MAC_POOL.lock().unwrap();
            MachineInfo::Dpu(DpuMachineInfo::new(
                HardwareType::DellPowerEdgeR750,
                &mut pool,
                DpuSettings::default(),
            ))
        };
        let (router, state) = crate::machine_router(
            &info,
            callbacks.clone(),
            "libvirt-dpu".into(),
            false,
            MachineRouterOptions {
                firmware_upgrade_duration: Some(delay),
                ..Default::default()
            },
        );
        let state = Arc::new(state);
        (
            Self {
                files,
                router,
                state,
                callbacks,
                tasks: JoinSet::new(),
                stop,
            },
            actor,
        )
    }

    async fn start(initial: &str) -> Self {
        let (mut domain, actor) = Self::unstarted(initial, Duration::ZERO);
        actor
            .run(&domain.state, &mut domain.tasks, domain.stop.clone())
            .await
            .unwrap();
        domain
    }

    async fn install(&self, image: &str) -> String {
        let response = self.router.clone().oneshot(Request::post("/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({"ImageURI": image, "Targets": ["redfish/v1/UpdateService/FirmwareInventory/DPU_OS"]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        response.headers()["Location"].to_str().unwrap().to_owned()
    }

    async fn task(&self, path: &str) -> serde_json::Value {
        let response = self
            .router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.files.path().join("calls"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    async fn wait_for_polls_after(&self, operation: &str, count: usize) {
        tokio::time::timeout(TEST_IO_WAIT, async {
            loop {
                let calls = self.calls();
                if let Some(index) = calls.iter().rposition(|call| call == operation)
                    && calls[index + 1..]
                        .iter()
                        .filter(|call| call.as_str() == "domstate")
                        .count()
                        >= count
                {
                    break;
                }
                tokio::time::sleep(TEST_IO_POLL).await;
            }
        })
        .await
        .expect("libvirt actor did not resume polling after the command");
    }

    async fn finish(&mut self) {
        self.callbacks.finish().await.unwrap();
        self.tasks.join_next().await.unwrap().unwrap();
    }
}

impl Drop for TestDomain {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[tokio::test]
async fn libvirt_bfb_installs_complete_from_real_backend_boot_events() {
    for initial in ["running", "shut off"] {
        let mut domain = TestDomain::start(initial).await;
        for (index, (image, expected)) in [
            ("http://bmc/managed.bfb", DpuOsImage::Managed),
            ("http://bmc/preingestion.bfb", DpuOsImage::PreIngestion),
        ]
        .into_iter()
        .enumerate()
        {
            let path = domain.install(image).await;
            tokio::time::timeout(TEST_IO_WAIT, async {
                loop {
                    if domain.task(&path).await["TaskState"] == "Completed" {
                        break;
                    }
                    tokio::time::sleep(TEST_IO_POLL).await;
                }
            })
            .await
            .expect("libvirt BFB install never completed after a successful reset");
            let status = domain.state.update_service_state.dpu_os_status().unwrap();
            assert_eq!(status.image, Some(expected));
            assert_eq!(status.generation, index as u64 + 1);
            assert_eq!(
                domain
                    .state
                    .update_service_state
                    .dpu_boot_progress(true)
                    .unwrap()["LastState"],
                "OSRunning"
            );
        }
        assert!(domain.calls().iter().any(|call| call
            == if initial == "running" {
                "reset"
            } else {
                "start"
            }));
        domain.finish().await;
    }
}

#[tokio::test]
async fn libvirt_failed_resets_do_not_turn_running_polls_into_boots() {
    let mut domain = TestDomain::start("running").await;
    for operation in ["reset", "destroy"] {
        std::fs::write(domain.files.path().join(format!("fail-{operation}")), "").unwrap();
    }
    let path = domain.install("http://bmc/preingestion.bfb").await;
    domain.wait_for_polls_after("reset", 2).await;
    assert_eq!(domain.task(&path).await["TaskState"], "Running");
    assert_eq!(
        domain
            .state
            .update_service_state
            .dpu_os_status()
            .unwrap()
            .generation,
        0
    );
    domain
        .callbacks
        .computer_system_reset(ResourceResetType::PowerCycle)
        .await
        .unwrap();
    domain.wait_for_polls_after("destroy", 2).await;
    assert_eq!(domain.task(&path).await["TaskState"], "Running");
    assert_eq!(
        domain
            .state
            .update_service_state
            .dpu_os_status()
            .unwrap()
            .image,
        None
    );
    domain.finish().await;
}

async fn wait_for_file(path: &std::path::Path) {
    tokio::time::timeout(TEST_IO_WAIT, async {
        while !path.exists() {
            tokio::time::sleep(TEST_IO_POLL).await;
        }
    })
    .await
    .expect("fake virsh did not reach its controlled query");
}

#[tokio::test]
async fn libvirt_power_requests_wait_for_a_real_initial_observation() {
    let (mut domain, actor) = TestDomain::unstarted("running", Duration::ZERO);
    let files = domain.files.path().to_owned();
    std::fs::write(files.join("hold-domstate"), "").unwrap();
    let callbacks = domain.callbacks.clone();
    let mut startup = JoinSet::new();
    startup.spawn(async move {
        actor
            .run(&domain.state, &mut domain.tasks, domain.stop.clone())
            .await
            .unwrap();
        domain
    });
    wait_for_file(&files.join("entered-domstate")).await;
    let mut request = Box::pin(callbacks.computer_system_reset(ResourceResetType::ForceRestart));
    assert!(
        request.as_mut().now_or_never().is_none(),
        "synthetic initial Off must not reject the request"
    );
    std::fs::remove_file(files.join("hold-domstate")).unwrap();
    let mut domain = tokio::time::timeout(TEST_IO_WAIT, startup.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    request.await.unwrap();
    domain.wait_for_polls_after("reset", 1).await;
    assert!(!domain.calls().iter().any(|call| call == "start"));
    domain.finish().await;
}

#[tokio::test]
async fn libvirt_initialization_exit_releases_waiting_power_requests() {
    let (domain, actor) = TestDomain::unstarted("running", Duration::ZERO);
    let mut request = Box::pin(
        domain
            .callbacks
            .computer_system_reset(ResourceResetType::ForceRestart),
    );
    assert!(request.as_mut().now_or_never().is_none());
    drop(actor);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap(),
        Err(ActionError::Internal(_))
    ));
    assert!(domain.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn libvirt_unready_power_request_times_out_without_a_late_reset() {
    // Hold the readiness seam without starting virsh. Advancing Tokio time
    // must not race an external process or its own command deadline.
    let (domain, actor) = TestDomain::unstarted("running", Duration::ZERO);
    let system = domain.state.system_state.primary_system_odata_id().unwrap();
    let mut request = Box::pin(
        domain.router.clone().oneshot(
            Request::post(format!("{system}/Actions/ComputerSystem.Reset"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"ResetType":"ForceRestart"}"#))
                .unwrap(),
        ),
    );
    assert!(request.as_mut().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(40) - Duration::from_millis(1)).await;
    assert!(request.as_mut().now_or_never().is_none());
    tokio::time::advance(Duration::from_millis(1)).await;
    let response = request
        .now_or_never()
        .expect("power request did not finish at its readiness deadline")
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    actor.power_state.write().unwrap().state = PowerState::On;
    actor.initialized.send_replace(true);
    domain
        .callbacks
        .computer_system_reset(ResourceResetType::GracefulRestart)
        .await
        .unwrap();

    struct PowerCommands(tokio::sync::mpsc::UnboundedSender<ResourceResetType>);
    impl ActorCallbacks<LibvirtMessage> for PowerCommands {
        async fn message(
            &mut self,
            _mailbox: &ActorMailbox<LibvirtMessage>,
            message: LibvirtMessage,
        ) -> ActorResult {
            match message {
                LibvirtMessage::SendPowerCommand { reset_type } => {
                    self.0.send(reset_type).unwrap();
                }
                LibvirtMessage::Stop(reply) => {
                    reply.send(Ok(())).unwrap();
                    return ActorResult::Stop;
                }
                _ => {}
            }
            ActorResult::Noop
        }
    }
    let (stop, stopped) = oneshot::channel();
    actor.mailbox.send(LibvirtMessage::Stop(stop)).unwrap();
    let (commands, mut received) = tokio::sync::mpsc::unbounded_channel();
    actor.actor.run(PowerCommands(commands)).await;
    stopped.await.unwrap().unwrap();
    let mut observed = Vec::new();
    while let Ok(command) = received.try_recv() {
        observed.push(command);
    }
    assert_eq!(observed, [ResourceResetType::GracefulRestart]);
}

#[tokio::test]
async fn libvirt_restores_an_unfinished_install_before_initial_power_discovery() {
    let (original, _actor) = TestDomain::unstarted("running", Duration::from_secs(600));
    let path = original.install("http://bmc/preingestion.bfb").await;
    let snapshot = original.state.persisted();
    drop(original);

    let (mut domain, mut actor) = TestDomain::unstarted("running", Duration::ZERO);
    let files = domain.files.path().to_owned();
    let saved = files.join("state.json");
    std::fs::write(&saved, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    actor.config.state_file = Some(saved);
    std::fs::write(files.join("hold-domstate"), "").unwrap();
    let state = domain.state.clone();
    let router = domain.router.clone();
    let mut startup = JoinSet::new();
    startup.spawn(async move {
        actor
            .run(&domain.state, &mut domain.tasks, domain.stop.clone())
            .await
            .unwrap();
        domain
    });
    wait_for_file(&files.join("entered-domstate")).await;
    tokio::time::timeout(TEST_IO_WAIT, async {
        while !state.update_service_state.dpu_restart_admission_pending() {
            tokio::time::sleep(TEST_IO_POLL).await;
        }
    })
    .await
    .expect("restored install did not request its recovery restart");
    let response = router
        .oneshot(Request::get(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let task: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(task["TaskState"], "Running");
    let disk = state.update_service_state.dpu_os_status().unwrap();
    assert_eq!(disk.image, None);
    assert_eq!(disk.generation, 0);
    std::fs::remove_file(files.join("hold-domstate")).unwrap();
    let mut domain = tokio::time::timeout(TEST_IO_WAIT, startup.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(TEST_IO_WAIT, async {
        while domain.task(&path).await["TaskState"] != "Completed" {
            tokio::time::sleep(TEST_IO_POLL).await;
        }
    })
    .await
    .expect("restored task did not complete through the real libvirt restart");
    let disk = state.update_service_state.dpu_os_status().unwrap();
    assert_eq!(disk.image, Some(DpuOsImage::PreIngestion));
    assert_eq!(disk.generation, 1);
    assert!(domain.calls().iter().any(|call| call == "reset"));
    assert!(!domain.calls().iter().any(|call| call == "start"));
    domain.finish().await;
}

#[tokio::test]
async fn libvirt_warm_resets_preserve_the_next_cold_boot_override() {
    for reset in [
        ResourceResetType::ForceRestart,
        ResourceResetType::GracefulRestart,
    ] {
        let mut domain = TestDomain::start("running").await;
        let system = domain.state.system_state.primary_system_odata_id().unwrap();
        let response = domain
            .router
            .clone()
            .oneshot(
                Request::patch(&system)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"Boot": {
                            "BootSourceOverrideEnabled": "Once",
                            "BootSourceOverrideTarget": "Pxe"
                        }})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        tokio::time::timeout(TEST_IO_WAIT, async {
            while !std::fs::read_to_string(domain.files.path().join("domain.xml"))
                .unwrap()
                .contains("dev=\"network\"")
            {
                tokio::time::sleep(TEST_IO_POLL).await;
            }
        })
        .await
        .expect("PXE override was not staged in saved XML");
        domain.callbacks.computer_system_reset(reset).await.unwrap();
        domain
            .wait_for_polls_after(
                if reset == ResourceResetType::ForceRestart {
                    "reset"
                } else {
                    "reboot"
                },
                2,
            )
            .await;
        assert_eq!(
            domain
                .state
                .system_state
                .controlled_system()
                .unwrap()
                .boot_source_override()["BootSourceOverrideEnabled"],
            "Once",
            "a warm reset did not use the pending saved override"
        );
        domain
            .callbacks
            .computer_system_reset(ResourceResetType::PowerCycle)
            .await
            .unwrap();
        domain.wait_for_polls_after("start", 2).await;
        assert_eq!(
            domain
                .state
                .system_state
                .controlled_system()
                .unwrap()
                .boot_source_override()["BootSourceOverrideEnabled"],
            "Disabled"
        );
        let xml = std::fs::read_to_string(domain.files.path().join("domain.xml")).unwrap();
        assert!(xml.contains("dev=\"hd\""), "{xml}");
        assert!(!xml.contains("dev=\"network\""), "{xml}");
        domain.finish().await;
    }
}

#[tokio::test]
async fn libvirt_boot_cleanup_cannot_complete_a_later_bfb_install() {
    let mut domain = TestDomain::start("shut off").await;
    let system = domain.state.system_state.primary_system_odata_id().unwrap();
    let response = domain
        .router
        .clone()
        .oneshot(
            Request::patch(&system)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"Boot": {
                        "BootSourceOverrideEnabled": "Once",
                        "BootSourceOverrideTarget": "Pxe"
                    }})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let files = domain.files.path();
    std::fs::write(files.join("hold-cleanup"), "").unwrap();
    std::fs::write(files.join("fail-reset"), "").unwrap();
    domain
        .callbacks
        .computer_system_reset(ResourceResetType::On)
        .await
        .unwrap();
    wait_for_file(&files.join("entered-cleanup")).await;

    let path = domain.install("http://bmc/preingestion.bfb").await;
    tokio::time::timeout(TEST_IO_WAIT, async {
        loop {
            let saved = serde_json::to_value(domain.state.persisted()).unwrap();
            if saved["dpu_update"]["os"]["install"]["rebooting"] == true
                && !domain
                    .state
                    .update_service_state
                    .dpu_restart_admission_pending()
            {
                break;
            }
            tokio::time::sleep(TEST_IO_POLL).await;
        }
    })
    .await
    .expect("new BFB restart was not admitted while the old boot cleanup was held");
    assert_eq!(domain.task(&path).await["TaskState"], "Running");
    std::fs::remove_file(files.join("hold-cleanup")).unwrap();
    domain.wait_for_polls_after("reset", 2).await;
    assert_eq!(
        domain.task(&path).await["TaskState"],
        "Running",
        "an earlier boot must not complete an install whose own reset failed"
    );
    let disk = domain.state.update_service_state.dpu_os_status().unwrap();
    assert_eq!(disk.image, None);
    assert_eq!(disk.generation, 0);
    domain.finish().await;
}
