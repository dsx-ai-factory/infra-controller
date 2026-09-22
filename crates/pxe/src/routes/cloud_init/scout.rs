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

//! The NoCloud datasource served to a host booting the Scout discovery image.
//!
//! A site drops cloud-config files into a directory this service serves; those
//! files are listed here as a cloud-init `#include` document, applied before the
//! Scout service starts.
//!
//! Unlike its sibling prefixes, nothing here answers with the generic error
//! template. A document that fails to parse stops NoCloud bringing the
//! datasource up at all.

use std::path::Path;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use carbide_instrument::emit;
use serde::Serialize;

use crate::STATIC_URL_PREFIX;
use crate::common::{AppState, Machine};
use crate::metrics::{
    BootEndpoint, CloudInitConsumer, OutcomeReason, PxeBootOutcome, PxeSnippetDirectoryUnreadable,
};

const ENDPOINT: BootEndpoint = BootEndpoint::CloudInit(CloudInitConsumer::Scout);

/// Relative to the static-file directory, so the feature needs no mount point of
/// its own and follows whatever the deployment configures.
const SNIPPET_SUBDIR: &str = "blobs/internal/cloud-init.d/scout";

/// NoCloud needs an `instance-id` to bring the datasource up, so a machine the
/// API can identify by neither machine nor interface still gets a valid one.
const FALLBACK_INSTANCE_ID: &str = "nico-discovery";

#[derive(Serialize)]
struct ScoutUserData {
    /// Empty renders the no-op document instead of an `#include` list.
    snippet_urls: Vec<String>,
}

#[derive(Serialize)]
struct ScoutMetaData {
    instance_id: String,
    /// Omitted rather than sent empty, so cloud-init falls back to its own
    /// hostname derivation.
    #[serde(skip_serializing_if = "Option::is_none")]
    local_hostname: Option<String>,
}

#[derive(Debug, PartialEq)]
enum SnippetScan {
    Found(Vec<String>),
    /// No directory, or nothing servable in it. The supported default, not a
    /// fault: the mechanism is unconditional, configuring it is optional.
    NotConfigured,
    /// The directory exists but could not be listed, so snippets a site did
    /// configure are silently not applied. The one case here worth alerting on.
    Unreadable(String),
}

/// Names outside the conventional `10-auth.yaml` shape are skipped rather than
/// percent-encoded, so a name cannot change meaning between the listing and the
/// URL a machine fetches.
fn is_url_safe_snippet_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Lists the snippet directory in filename order.
///
/// Metadata is read through symlinks so that ConfigMap mounts work, since they
/// present each key as a symlink into a hidden `..data` directory; dotted names
/// are skipped so that directory itself is not served.
fn scan_snippet_dir(static_dir: &str) -> SnippetScan {
    let dir = Path::new(static_dir).join(SNIPPET_SUBDIR);

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No directory is the unconfigured site, not a failure.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SnippetScan::NotConfigured;
        }
        Err(error) => {
            return SnippetScan::Unreadable(format!("{}: {error}", dir.display()));
        }
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    directory = %dir.display(),
                    error = %error,
                    "skipping unreadable entry in the Scout snippet directory"
                );
                continue;
            }
        };

        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            tracing::warn!(
                directory = %dir.display(),
                file_name = ?file_name,
                "skipping Scout snippet with a non-UTF-8 name"
            );
            continue;
        };

        if name.starts_with('.') {
            continue;
        }

        if !is_url_safe_snippet_name(name) {
            tracing::warn!(
                directory = %dir.display(),
                file_name = %name,
                "skipping Scout snippet whose name is not URL-safe; use only \
                 letters, digits, '.', '-' and '_'"
            );
            continue;
        }

        match std::fs::metadata(entry.path()) {
            Ok(metadata) if metadata.is_file() => names.push(name.to_string()),
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(
                    directory = %dir.display(),
                    file_name = %name,
                    error = %error,
                    "skipping Scout snippet that could not be stat'd"
                );
                continue;
            }
        }
    }

    if names.is_empty() {
        return SnippetScan::NotConfigured;
    }

    names.sort();
    SnippetScan::Found(names)
}

/// The base URL a machine should fetch snippets from: its own override when it
/// has one, otherwise the configured PXE URL.
///
/// Deliberately not `static_pxe_url`, which names an nginx cache in front of
/// this service. Snippets are small and change whenever a site edits them, so
/// they are served from the origin rather than through a cache.
fn snippet_base_url(machine: &Machine, state: &AppState) -> String {
    let url = machine
        .instructions
        .pxe_url_override
        .as_deref()
        .unwrap_or(&state.runtime_config.pxe_url);
    url.trim_end_matches('/').to_string()
}

/// The hostname on the machine interface the API resolved the caller to. Needs
/// no validation: it is the record the DNS entry is built from. Without it every
/// machine in discovery calls itself `scout`.
fn interface_hostname(machine: &Machine) -> Option<String> {
    let hostname = machine
        .instructions
        .discovery_instructions
        .as_ref()?
        .machine_interface
        .as_ref()?
        .hostname
        .clone();
    (!hostname.is_empty()).then_some(hostname)
}

/// The machine interface ID, when the API resolved the caller to one.
fn interface_id(machine: &Machine) -> Option<String> {
    machine
        .instructions
        .discovery_instructions
        .as_ref()?
        .machine_interface
        .as_ref()?
        .id
        .map(|id| id.to_string())
}

/// Serves the `#include` list of the site's snippets, or a no-op
/// `#cloud-config` when none are configured.
///
/// An unreadable directory gets the no-op document too, rather than failing the
/// request and taking the datasource down over a fault the machine cannot act
/// on; the emitted Event is the signal that the snippets did not arrive.
async fn user_data(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let snippet_urls = match scan_snippet_dir(&state.static_dir) {
        SnippetScan::Found(names) => {
            let base_url = snippet_base_url(&machine, &state);
            emit(PxeBootOutcome {
                endpoint: ENDPOINT,
                reason: OutcomeReason::Ok,
            });
            names
                .iter()
                .map(|name| format!("{base_url}{STATIC_URL_PREFIX}/{SNIPPET_SUBDIR}/{name}"))
                .collect()
        }
        SnippetScan::NotConfigured => {
            emit(PxeBootOutcome {
                endpoint: ENDPOINT,
                reason: OutcomeReason::InstructionsEmpty,
            });
            Vec::new()
        }
        SnippetScan::Unreadable(error) => {
            emit(PxeSnippetDirectoryUnreadable {
                endpoint: ENDPOINT,
                reason: OutcomeReason::SnippetDirectoryUnreadable,
                error,
            });
            Vec::new()
        }
    };

    axum_template::Render(
        "scout-user-data",
        state.engine.clone(),
        ScoutUserData { snippet_urls },
    )
}

/// Serves NoCloud's `meta-data`, whose `instance-id` carries the machine ID so
/// a snippet can read machine identity without scraping `/proc/cmdline`.
///
/// The field is optional, so it falls back to the interface ID and then to a
/// placeholder; this document must always be valid.
async fn meta_data(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let instance_id = match machine.instructions.metadata.as_ref() {
        Some(metadata) => metadata.instance_id.clone(),
        None => match interface_id(&machine) {
            Some(interface_id) => {
                tracing::debug!(
                    %interface_id,
                    "no machine ID for this caller; serving the interface ID as instance-id"
                );
                interface_id
            }
            None => {
                tracing::warn!(
                    "caller resolved to neither a machine nor an interface; \
                     serving a placeholder instance-id"
                );
                FALLBACK_INSTANCE_ID.to_string()
            }
        },
    };

    emit(PxeBootOutcome {
        endpoint: ENDPOINT,
        reason: OutcomeReason::Ok,
    });

    axum_template::Render(
        "scout-meta-data",
        state.engine.clone(),
        ScoutMetaData {
            instance_id,
            local_hostname: interface_hostname(&machine),
        },
    )
}

/// Builds the route table for the Scout discovery datasource served under
/// `path_prefix`: `user-data` and `meta-data`. `vendor-data` is absent because
/// NoCloud treats it as optional and this path has nothing to put in it.
pub(crate) fn get_router(path_prefix: &str) -> Router<AppState> {
    Router::new()
        .route(
            format!("{}/{}", path_prefix, "user-data").as_str(),
            get(user_data),
        )
        .route(
            format!("{}/{}", path_prefix, "meta-data").as_str(),
            get(meta_data),
        )
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::{Check, check_values};
    use rpc::forge;

    use super::*;
    use crate::common::{test_app_state, test_app_state_with_templates};

    const BOOT_OUTCOMES_METRIC: &str = "carbide_pxe_boot_outcomes_total";

    fn machine_with_hostname(hostname: &str) -> Machine {
        Machine {
            instructions: forge::CloudInitInstructions {
                discovery_instructions: Some(forge::CloudInitDiscoveryInstructions {
                    machine_interface: Some(forge::MachineInterface {
                        hostname: hostname.to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
    }

    fn machine_with_interface(interface_id: Option<&str>) -> Machine {
        Machine {
            instructions: forge::CloudInitInstructions {
                discovery_instructions: Some(forge::CloudInitDiscoveryInstructions {
                    machine_interface: Some(forge::MachineInterface {
                        id: interface_id.map(|id| id.parse().unwrap()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
    }

    #[test]
    fn snippet_names_are_screened_for_url_safety() {
        check_values(
            [
                Check {
                    scenario: "conventional name",
                    input: "10-auth.yaml",
                    expect: true,
                },
                Check {
                    scenario: "underscores",
                    input: "20_machine_validation.yaml",
                    expect: true,
                },
                Check {
                    scenario: "space",
                    input: "10 auth.yaml",
                    expect: false,
                },
                Check {
                    scenario: "path separator",
                    input: "nested/10-auth.yaml",
                    expect: false,
                },
                Check {
                    scenario: "query separator",
                    input: "10-auth.yaml?x=1",
                    expect: false,
                },
                Check {
                    scenario: "non-ascii",
                    input: "10-authé.yaml",
                    expect: false,
                },
                Check {
                    scenario: "empty",
                    input: "",
                    expect: false,
                },
            ],
            is_url_safe_snippet_name,
        );
    }

    /// A missing directory and an empty one are the same unconfigured site.
    #[test]
    fn scan_reports_not_configured_without_snippets() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            scan_snippet_dir(root.path().to_str().unwrap()),
            SnippetScan::NotConfigured,
        );

        std::fs::create_dir_all(root.path().join(SNIPPET_SUBDIR)).unwrap();
        assert_eq!(
            scan_snippet_dir(root.path().to_str().unwrap()),
            SnippetScan::NotConfigured,
        );
    }

    /// Files come back in filename order; subdirectories, dotfiles, and
    /// unsafe names do not come back at all.
    #[test]
    fn scan_sorts_files_and_skips_everything_else() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(SNIPPET_SUBDIR);
        std::fs::create_dir_all(&dir).unwrap();

        for name in ["20-second.yaml", "10-first.yaml", "..data", "bad name.yaml"] {
            std::fs::write(dir.join(name), "#cloud-config\n").unwrap();
        }
        std::fs::create_dir(dir.join("30-a-directory")).unwrap();

        assert_eq!(
            scan_snippet_dir(root.path().to_str().unwrap()),
            SnippetScan::Found(vec![
                "10-first.yaml".to_string(),
                "20-second.yaml".to_string(),
            ]),
        );
    }

    /// A symlinked file is served, which is how a ConfigMap mount presents
    /// its keys.
    #[test]
    fn scan_follows_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(SNIPPET_SUBDIR);
        std::fs::create_dir_all(dir.join("..data")).unwrap();
        std::fs::write(dir.join("..data/10-hello.yaml"), "#cloud-config\n").unwrap();
        std::os::unix::fs::symlink("..data/10-hello.yaml", dir.join("10-hello.yaml")).unwrap();

        assert_eq!(
            scan_snippet_dir(root.path().to_str().unwrap()),
            SnippetScan::Found(vec!["10-hello.yaml".to_string()]),
        );
    }

    /// Snippets come from the PXE URL rather than the cached static one, the
    /// machine's own override wins over both, and a trailing slash does not
    /// become a doubled separator in the rendered URL.
    #[test]
    fn snippet_base_url_prefers_the_machines_override() {
        let mut state = test_app_state();
        state.runtime_config.static_pxe_url = "http://nginx-cache.forge".to_string();

        let mut machine = machine_with_interface(None);
        assert_eq!(
            snippet_base_url(&machine, &state),
            "http://carbide-pxe.forge"
        );

        machine.instructions.pxe_url_override = Some("http://10.99.0.1:8080/".to_string());
        assert_eq!(snippet_base_url(&machine, &state), "http://10.99.0.1:8080");
    }

    /// An unconfigured site is a served, counted outcome rather than an error.
    #[tokio::test]
    async fn user_data_without_snippets_counts_instructions_empty() {
        let metrics = MetricsCapture::start();

        let _ = user_data(machine_with_interface(None), State(test_app_state())).await;

        assert_eq!(
            metrics.counter_delta(
                BOOT_OUTCOMES_METRIC,
                &[
                    ("endpoint", "cloud_init_scout"),
                    ("reason", "instructions_empty"),
                ],
            ),
            1.0,
        );
    }

    /// A caller with no machine still gets a document, counted as served: the
    /// interface ID standing in is the designed path, not a degraded one.
    #[tokio::test]
    async fn meta_data_falls_back_to_the_interface_id() {
        let metrics = MetricsCapture::start();

        let _ = meta_data(
            machine_with_interface(Some("91609f10-c91d-470d-a260-6293ea0c1234")),
            State(test_app_state()),
        )
        .await;

        assert_eq!(
            metrics.counter_delta(
                BOOT_OUTCOMES_METRIC,
                &[("endpoint", "cloud_init_scout"), ("reason", "ok")],
            ),
            1.0,
        );
    }

    /// Both forms against the real templates: an `#include` list in filename
    /// order when snippets exist, a no-op `#cloud-config` when none do, and the
    /// operator documentation in either case.
    #[test]
    fn user_data_template_renders_both_forms() {
        let template_glob = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pxe/templates/**/*");
        let tera = tera::Tera::new(template_glob).unwrap();

        let render = |urls: Vec<String>| {
            tera.render(
                "scout-user-data",
                &tera::Context::from_serialize(ScoutUserData { snippet_urls: urls }).unwrap(),
            )
            .unwrap()
        };

        let with_snippets = render(vec![
            "http://carbide-pxe.forge/public/blobs/internal/cloud-init.d/scout/10-first.yaml"
                .to_string(),
            "http://carbide-pxe.forge/public/blobs/internal/cloud-init.d/scout/20-second.yaml"
                .to_string(),
        ]);
        assert!(with_snippets.starts_with("#include\n"));
        assert!(
            with_snippets.find("10-first.yaml").unwrap()
                < with_snippets.find("20-second.yaml").unwrap()
        );

        let without_snippets = render(Vec::new());
        assert!(without_snippets.starts_with("#cloud-config\n"));
        assert!(without_snippets.contains("\n{}\n"));
        assert!(!without_snippets.contains("http://"));

        // The three operator rules ship on every boot, in both forms.
        for rendered in [&with_snippets, &without_snippets] {
            assert!(rendered.contains("COMPOSING SNIPPETS."));
            assert!(rendered.contains("merge_how:"));
            assert!(rendered.contains("TIME BOUND."));
            assert!(rendered.contains("systemctl show -p TimeoutStartUSec cloud-final.service"));
            assert!(rendered.contains("DO NOT REBOOT FROM A SNIPPET."));
            assert!(rendered.contains("NO SECRETS."));
        }
    }

    /// The configured path end to end: real files in a real directory, through
    /// the real handler and template, to the bytes a machine receives. The only
    /// test proving the scan and the template are wired together.
    #[tokio::test]
    async fn user_data_serves_an_include_list_of_the_configured_snippets() {
        // Takes the lock that keeps other tests' exact counter deltas meaningful,
        // since driving a handler emits into the shared registry.
        let _metrics = MetricsCapture::start();

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(SNIPPET_SUBDIR);
        std::fs::create_dir_all(&dir).unwrap();
        // Written out of order on purpose: the served order must come from the
        // sort, not from readdir.
        std::fs::write(dir.join("20-second.yaml"), "#cloud-config\n").unwrap();
        std::fs::write(dir.join("10-first.yaml"), "#cloud-config\n").unwrap();

        let mut state = test_app_state_with_templates();
        state.static_dir = root.path().to_str().unwrap().to_string();

        let response = user_data(machine_with_interface(None), State(state))
            .await
            .into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(
            body.starts_with("#include\n"),
            "a configured site must get an #include document, got:\n{body}"
        );

        let first =
            format!("http://carbide-pxe.forge{STATIC_URL_PREFIX}/{SNIPPET_SUBDIR}/10-first.yaml");
        let second =
            format!("http://carbide-pxe.forge{STATIC_URL_PREFIX}/{SNIPPET_SUBDIR}/20-second.yaml");
        assert!(body.contains(&first), "missing {first} in:\n{body}");
        assert!(body.contains(&second), "missing {second} in:\n{body}");
        assert!(
            body.find(&first).unwrap() < body.find(&second).unwrap(),
            "snippets must be listed in filename order, got:\n{body}"
        );

        // Operator documentation ships with the list, not only the no-op form.
        assert!(body.contains("DO NOT REBOOT FROM A SNIPPET."));
        assert!(body.contains("NO SECRETS."));
    }

    /// A discovery host is told its own name, so console and log lines can be
    /// attributed to a machine rather than to another host called `scout`.
    #[tokio::test]
    async fn meta_data_serves_the_interface_hostname_as_local_hostname() {
        // Takes the lock that keeps other tests' exact counter deltas meaningful,
        // since driving a handler emits into the shared registry.
        let _metrics = MetricsCapture::start();

        let response = meta_data(
            machine_with_hostname("node-17"),
            State(test_app_state_with_templates()),
        )
        .await
        .into_response();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("local-hostname: \"node-17\""),
            "expected local-hostname in:\n{text}"
        );
    }

    /// An interface with no hostname omits the field rather than sending an
    /// empty one, leaving cloud-init to derive a hostname itself.
    #[tokio::test]
    async fn meta_data_omits_local_hostname_when_the_interface_has_none() {
        // Takes the lock that keeps other tests' exact counter deltas meaningful,
        // since driving a handler emits into the shared registry.
        let _metrics = MetricsCapture::start();

        let response = meta_data(
            machine_with_interface(None),
            State(test_app_state_with_templates()),
        )
        .await
        .into_response();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            !text.contains("local-hostname"),
            "expected no local-hostname in:\n{text}"
        );
    }

    #[test]
    fn meta_data_template_renders_the_instance_id() {
        let template_glob = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pxe/templates/**/*");
        let tera = tera::Tera::new(template_glob).unwrap();

        let rendered = tera
            .render(
                "scout-meta-data",
                &tera::Context::from_serialize(ScoutMetaData {
                    instance_id: "91609f10-c91d-470d-a260-6293ea0c1234".to_string(),
                    local_hostname: None,
                })
                .unwrap(),
            )
            .unwrap();

        assert_eq!(
            rendered.trim(),
            "instance-id: 91609f10-c91d-470d-a260-6293ea0c1234",
        );
    }
}
