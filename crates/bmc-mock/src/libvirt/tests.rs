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
