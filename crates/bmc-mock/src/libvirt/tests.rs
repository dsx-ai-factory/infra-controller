// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::FutureExt;
use tower::ServiceExt;

use super::*;
use crate::test_support::host_info;
use crate::{HardwareType, MachineRouterOptions};

struct TestDomain {
    router: Router,
    state: Arc<BmcState<LibvirtCallbacks>>,
    callbacks: Arc<LibvirtCallbacks>,
}

fn unstarted() -> (TestDomain, LibvirtActor) {
    let stop = CancellationToken::new();
    let (actor, callbacks) = LibvirtActor::new(
        Config {
            state_file: None,
            virsh_path: PathBuf::from("unused"),
            uri: "test:///default".into(),
            domain: "test-domain".into(),
            virtual_media_targets: Default::default(),
            restart_loads_persistent_definition: false,
        },
        stop.drop_guard(),
    );
    let callbacks = Arc::new(callbacks);
    let (router, state) = crate::machine_router(
        &host_info(HardwareType::GenericAmi),
        callbacks.clone(),
        "test".into(),
        false,
        MachineRouterOptions::default(),
    );
    (
        TestDomain {
            router,
            state: Arc::new(state),
            callbacks,
        },
        actor,
    )
}

#[tokio::test]
async fn libvirt_initialization_exit_releases_waiting_power_requests() {
    let (domain, actor) = unstarted();
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
}

#[tokio::test(start_paused = true)]
async fn libvirt_unready_power_request_times_out_without_a_late_reset() {
    // Hold the readiness seam without starting virsh. Advancing Tokio time
    // must not race an external process or its own command deadline.
    let (domain, actor) = unstarted();
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
async fn libvirt_manager_reset_to_defaults_restores_and_saves_factory_accounts() {
    let (domain, actor) = unstarted();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bmc-state.json");
    // Run the real backend without virsh: the reset touches only BMC state.
    let backend = LibvirtBackend {
        persistence: Some(StateFile::load(path.clone(), &domain.state).unwrap()),
        persistence_retry: None,
        config: actor.config,
        restore_boot_after_power_on: false,
        system_state: None,
        bmc_state: Arc::downgrade(&domain.state),
        last_observed_power: None,
        boot_pending: None,
        initialized: actor.initialized,
        applied_state: AppliedState::default(),
        refresh_pending: actor.refresh_pending,
        power_state: actor.power_state,
    };
    let running = tokio::spawn(actor.actor.run(backend));

    let accounts = &domain.state.account_service_state;
    let factory_accounts = accounts.accounts();
    let account_id = serde_json::to_value(domain.state.persisted()).unwrap()["accounts"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        accounts
            .update_password(&account_id, "rotated")
            .await
            .unwrap()
    );
    assert_ne!(accounts.accounts(), factory_accounts);

    let manager = domain.state.manager.primary_odata_id().unwrap();
    let response = domain
        .router
        .clone()
        .oneshot(
            Request::post(format!("{manager}/Actions/Manager.ResetToDefaults"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"ResetType":"ResetAll"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(accounts.accounts(), factory_accounts);
    assert_eq!(domain.state.factory_reset_generation(), 1);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        saved,
        serde_json::to_value(domain.state.persisted()).unwrap(),
        "the reset state is saved before the action reports success"
    );

    let (stop, stopped) = oneshot::channel();
    actor.mailbox.send(LibvirtMessage::Stop(stop)).unwrap();
    stopped.await.unwrap().unwrap();
    running.await.unwrap();
}

#[tokio::test]
async fn hpe_integrated_event_log_records_only_successful_backend_commands() {
    const ENTRIES: &str = "/redfish/v1/Managers/1/LogServices/IEL/Entries";

    async fn entry_count(router: &Router) -> u64 {
        let response = router
            .clone()
            .oneshot(Request::get(ENTRIES).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["Members@odata.count"]
            .as_u64()
            .unwrap()
    }

    async fn wait_for(path: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("backend command did not reach its checkpoint");
    }

    for (label, exit_code) in [("success", 0), ("failure", 1)] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        std::fs::write(path.join("exit-code"), exit_code.to_string()).unwrap();
        // The reset blocks until released, so the test can observe the log
        // while the command is in flight.
        let virsh =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/libvirt/testdata/fake-virsh");
        let stop = CancellationToken::new();
        let (actor, callbacks) = LibvirtActor::new(
            Config {
                state_file: None,
                virsh_path: virsh,
                uri: format!("test://{}", path.display()),
                domain: "test-domain".into(),
                virtual_media_targets: Default::default(),
                restart_loads_persistent_definition: false,
            },
            stop.clone().drop_guard(),
        );
        let callbacks = Arc::new(callbacks);
        let (router, state) = crate::machine_router(
            &host_info(HardwareType::HpeProliantDl380aGen11),
            callbacks.clone(),
            "test".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = Arc::new(state);
        let mut tasks = JoinSet::new();
        actor.run(&state, &mut tasks, stop.clone()).await.unwrap();
        let system = state.system_state.primary_system_odata_id().unwrap();
        let response = router
            .clone()
            .oneshot(
                Request::post(format!("{system}/Actions/ComputerSystem.Reset"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"ResetType":"ForceRestart"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{label}: request accepted"
        );
        wait_for(&path.join("started")).await;
        assert_eq!(
            entry_count(&router).await,
            0,
            "{label}: an accepted command that has not completed is not logged"
        );
        std::fs::write(path.join("release"), "").unwrap();
        // The actor observes power after it has handled the command's result.
        wait_for(&path.join("observed-after")).await;
        assert_eq!(
            entry_count(&router).await,
            u64::from(exit_code == 0),
            "{label}: only a successful backend command is logged"
        );
        stop.cancel();
        tasks.shutdown().await;
    }
}

/// A started backend driven by the checked-in virsh stub. Each reset exits with
/// `exit_code` as soon as it starts.
struct StubDomain {
    directory: tempfile::TempDir,
    router: Router,
    state: Arc<BmcState<LibvirtCallbacks>>,
    callbacks: Arc<LibvirtCallbacks>,
    stop: CancellationToken,
    tasks: JoinSet<()>,
}

impl StubDomain {
    async fn start(restart_loads_persistent_definition: bool, exit_code: i32) -> Self {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("exit-code"), exit_code.to_string()).unwrap();
        std::fs::write(directory.path().join("release"), "").unwrap();
        let stop = CancellationToken::new();
        let (actor, callbacks) = LibvirtActor::new(
            Config {
                state_file: None,
                virsh_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/libvirt/testdata/fake-virsh"),
                uri: format!("test://{}", directory.path().display()),
                domain: "test-domain".into(),
                virtual_media_targets: Default::default(),
                restart_loads_persistent_definition,
            },
            stop.clone().drop_guard(),
        );
        let callbacks = Arc::new(callbacks);
        let (router, state) = crate::machine_router(
            &host_info(HardwareType::GenericAmi),
            callbacks.clone(),
            "test".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = Arc::new(state);
        let mut tasks = JoinSet::new();
        actor.run(&state, &mut tasks, stop.clone()).await.unwrap();
        Self {
            directory,
            router,
            state,
            callbacks,
            stop,
            tasks,
        }
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.directory.path().join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn stop(mut self) {
        self.stop.cancel();
        self.tasks.shutdown().await;
    }
}

#[tokio::test]
async fn libvirt_session_runs_its_requests_before_queued_backend_work() {
    let domain = StubDomain::start(false, 0).await;
    let system = domain.state.system_state.primary_system_odata_id().unwrap();
    let mut session = domain.callbacks.session().await.unwrap();
    let output = session.virsh(["domstate", "test-domain"]).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "running");
    assert_eq!(session.power_state().await.unwrap(), Some(PowerState::On));

    // A Redfish reset is accepted, then waits for the session to end.
    let (status, _) = domain
        .request(
            "POST",
            &format!("{system}/Actions/ComputerSystem.Reset"),
            r#"{"ResetType":"ForceRestart"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    session.virsh(["domstate", "test-domain"]).await.unwrap();
    assert!(
        !domain.log().iter().any(|line| line == "reset test-domain"),
        "queued backend work does not run during a session"
    );
    drop(session);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !domain.log().iter().any(|line| line == "reset test-domain") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued reset runs after the session ends");
    domain.stop().await;
}

#[tokio::test]
async fn libvirt_session_reports_power_command_and_virsh_failures() {
    let domain = StubDomain::start(false, 1).await;
    let mut session = domain.callbacks.session().await.unwrap();
    assert!(matches!(
        session
            .power(ResourceResetType::ForceRestart, SessionBootOrder::Apply)
            .await,
        Err(ActionError::Internal(_))
    ));
    assert!(matches!(
        session.virsh(["reset", "test-domain"]).await,
        Err(ActionError::Internal(_))
    ));
    drop(session);
    domain.stop().await;
}

#[tokio::test]
async fn restart_consumes_a_one_time_override_only_when_it_loads_the_saved_definition() {
    for restart_loads_persistent_definition in [false, true] {
        let domain = StubDomain::start(restart_loads_persistent_definition, 0).await;
        let system = domain.state.system_state.primary_system_odata_id().unwrap();
        let (status, _) = domain
            .request(
                "PATCH",
                &system,
                r#"{"Boot":{"BootSourceOverrideEnabled":"Once","BootSourceOverrideTarget":"Pxe"}}"#,
            )
            .await;
        assert!(status.is_success(), "{status}");
        let mut session = domain.callbacks.session().await.unwrap();
        let before = domain.log().len();
        session
            .power(ResourceResetType::ForceRestart, SessionBootOrder::Apply)
            .await
            .unwrap();
        drop(session);
        let restart = domain.log()[before..].to_vec();
        let reset = restart
            .iter()
            .position(|line| line == "reset test-domain")
            .expect("the restart ran");
        assert_eq!(
            restart[..reset]
                .iter()
                .any(|line| line.starts_with("define")),
            restart_loads_persistent_definition,
            "the override is applied before the restart only when the restart loads it: {restart:?}"
        );
        let (_, body) = domain.request("GET", &system, "").await;
        assert_eq!(
            body["Boot"]["BootSourceOverrideEnabled"],
            if restart_loads_persistent_definition {
                "Disabled"
            } else {
                "Once"
            },
            "a warm restart keeps the override for the next cold start"
        );
        domain.stop().await;
    }
}

#[tokio::test]
async fn session_power_can_keep_the_boot_configuration_it_prepared() {
    let domain = StubDomain::start(true, 0).await;
    let system = domain.state.system_state.primary_system_odata_id().unwrap();
    let (status, _) = domain
        .request(
            "PATCH",
            &system,
            r#"{"Boot":{"BootSourceOverrideEnabled":"Once","BootSourceOverrideTarget":"Pxe"}}"#,
        )
        .await;
    assert!(status.is_success(), "{status}");
    let mut session = domain.callbacks.session().await.unwrap();
    let before = domain.log().len();
    session
        .power(ResourceResetType::ForceRestart, SessionBootOrder::Keep)
        .await
        .unwrap();
    drop(session);
    let restart = domain.log()[before..].to_vec();
    assert!(
        restart.iter().any(|line| line == "reset test-domain"),
        "{restart:?}"
    );
    assert!(
        !restart.iter().any(|line| line.starts_with("define")),
        "the session's boot configuration is not replaced: {restart:?}"
    );
    let (_, body) = domain.request("GET", &system, "").await;
    assert_eq!(
        body["Boot"]["BootSourceOverrideEnabled"], "Once",
        "a boot that did not use the BMC's boot configuration leaves its one-time override pending"
    );
    domain.stop().await;
}
