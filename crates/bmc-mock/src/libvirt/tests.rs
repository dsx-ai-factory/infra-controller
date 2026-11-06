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
