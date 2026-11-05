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

/// A virsh stand-in for vmctl's BMC shim. It records each command and keeps the
/// pending selection in a file; marker files make individual commands fail.
fn media_boot_backend(dir: &std::path::Path, power: &str, selection: &str) -> LibvirtBackend {
    use std::os::unix::fs::PermissionsExt;

    let virsh = dir.join("virsh");
    std::fs::write(
        &virsh,
        r#"#!/bin/sh
base=$(dirname "$0")
printf '%s\n' "$3" >> "$base/log"
fail() { if [ -e "$base/$1-fails" ]; then echo "$1 failed" >&2; exit 1; fi; }
case "$3" in
 domblklist) printf 'Type Device Target Source\nfile cdrom sdc vm-media://media-1\n';;
 boot-selection) cat "$base/selection";;
 domstate) fail domstate; cat "$base/power";;
 boot-default) fail boot-default; printf '{"pending":{"boot_device":"disk"}}' > "$base/selection";;
 boot-once-media) printf '{"pending":{"boot_device":"media","media_id":"%s"}}' "$5" > "$base/selection";;
 start|reset) fail "$3";;
 *) exit 1;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&virsh, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("power"), power).unwrap();
    std::fs::write(dir.join("selection"), selection).unwrap();

    let (domain, actor) = unstarted();
    LibvirtBackend {
        persistence: None,
        persistence_retry: None,
        config: Config {
            virsh_path: virsh,
            virtual_media_targets: BTreeMap::from([("ConfigCd".into(), "sdc".into())]),
            ..actor.config
        },
        restore_boot_after_power_on: false,
        system_state: None,
        bmc_state: Arc::downgrade(&domain.state),
        last_observed_power: None,
        boot_pending: None,
        initialized: actor.initialized,
        applied_state: AppliedState::default(),
        refresh_pending: actor.refresh_pending,
        power_state: actor.power_state,
    }
}

fn read(dir: &std::path::Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap()
}

#[tokio::test]
async fn media_boot_with_unknown_power_state_arms_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let network = r#"{"pending":{"boot_device":"network"}}"#;
    let mut backend = media_boot_backend(dir.path(), "shut off", network);
    std::fs::write(dir.path().join("domstate-fails"), "").unwrap();

    let result = backend.boot_media_once("ConfigCd", "media-1").await;

    assert!(
        matches!(result, Err(ActionError::BadRequest(_))),
        "{result:?}"
    );
    assert_eq!(
        read(dir.path(), "log"),
        "domblklist\nboot-selection\ndomstate\n"
    );
    assert_eq!(
        read(dir.path(), "selection"),
        network,
        "neither the media nor the network selection changes"
    );
    assert!(backend.boot_pending.is_none());
}

#[tokio::test]
async fn failed_media_boot_cancels_the_selection_and_keeps_the_power_error() {
    for (power, command) in [("shut off", "start"), ("running", "reset")] {
        for cancel_fails in [false, true] {
            let case = format!("{command}, cancel fails: {cancel_fails}");
            let dir = tempfile::tempdir().unwrap();
            let mut backend = media_boot_backend(dir.path(), power, "{}");
            std::fs::write(dir.path().join(format!("{command}-fails")), "").unwrap();
            if cancel_fails {
                std::fs::write(dir.path().join("boot-default-fails"), "").unwrap();
            }

            let result = backend.boot_media_once("ConfigCd", "media-1").await;

            let Err(ActionError::Internal(error)) = result else {
                panic!("{case}: {result:?}");
            };
            assert!(
                error.to_string().contains(&format!("{command} failed")),
                "{case}: {error}"
            );
            assert_eq!(
                read(dir.path(), "log"),
                format!(
                    "domblklist\nboot-selection\ndomstate\nboot-once-media\n{command}\nboot-default\n"
                ),
                "{case}"
            );
            if !cancel_fails {
                assert_eq!(
                    read(dir.path(), "selection"),
                    r#"{"pending":{"boot_device":"disk"}}"#,
                    "{case}"
                );
            }
            assert!(backend.boot_pending.is_none(), "{case}");
        }
    }
}

#[tokio::test]
async fn media_boot_cancels_pending_network_then_boots_without_cancelling_media() {
    for (power, command, cold) in [("shut off", "start", true), ("running", "reset", false)] {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = media_boot_backend(
            dir.path(),
            power,
            r#"{"pending":{"boot_device":"network"}}"#,
        );

        backend
            .boot_media_once("ConfigCd", "media-1")
            .await
            .unwrap();

        assert_eq!(
            read(dir.path(), "log"),
            format!(
                "domblklist\nboot-selection\ndomstate\nboot-default\nboot-once-media\n{command}\n"
            )
        );
        assert_eq!(
            read(dir.path(), "selection"),
            r#"{"pending":{"boot_device":"media","media_id":"media-1"}}"#
        );
        assert!(
            matches!(backend.boot_pending, Some(BootKind::Cold)) == cold
                && matches!(backend.boot_pending, Some(BootKind::Warm)) != cold,
            "{command}"
        );
    }
}
