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

//! The NoCloud datasource served to an assigned instance.
//!
//! A tenant's machine reaches this from `ds=nocloud;s=` on its kernel
//! command line and fetches the full NoCloud set, so this prefix carries all
//! four documents.

use std::borrow::Cow;
use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use carbide_instrument::emit;

use super::log_and_generate_generic_error;
use crate::common::{AppState, Machine};
use crate::metrics::{BootEndpoint, CloudInitConsumer, OutcomeReason, PxeBootOutcome};

/// Every outcome on these routes carries the same endpoint label.
const ENDPOINT: BootEndpoint = BootEndpoint::CloudInit(CloudInitConsumer::Tenant);
const CLOUD_CONFIG_HEADER: &str = "#cloud-config";
const CLOUD_CONFIG_ARCHIVE_HEADER: &str = "#cloud-config-archive";
const CLOUD_CONFIG_CONTENT_TYPE: &str = "text/cloud-config";
const ARCHIVE_CONTENT_KEY: &str = "content";
const ARCHIVE_TYPE_KEY: &str = "type";
const NETWORK_CONFIG_KEY: &str = "network";

/// Serves the tenant's cloud-init document, removing a supported network
/// block delivered separately through NoCloud's network-config document.
///
/// An instance with none configured gets an empty document rather than an
/// error: having no user-data is an ordinary way to run an instance, and
/// cloud-init still needs something to fetch.
async fn user_data(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let mut template_data: HashMap<String, String> = HashMap::new();
    template_data.insert(
        "user_data".to_string(),
        machine
            .instructions
            .custom_cloud_init
            .map(|data| user_data_without_network(&data).unwrap_or(data))
            .unwrap_or_else(|| "{}".to_string()),
    );
    emit(PxeBootOutcome {
        endpoint: ENDPOINT,
        reason: OutcomeReason::Ok,
    });

    axum_template::Render(
        "user-data-assigned".to_string(),
        state.engine.clone(),
        template_data,
    )
}

/// Removes network settings delivered separately for plain cloud-config or a
/// supported archive. Other formats and unsupported layouts pass through.
fn user_data_without_network(custom_cloud_init: &str) -> Option<String> {
    let header = custom_cloud_init.split_whitespace().next()?;
    if header.eq_ignore_ascii_case(CLOUD_CONFIG_ARCHIVE_HEADER) {
        return split_archive_network(custom_cloud_init).map(|split| split.user_data);
    }
    // Require a complete header token so other suffixed formats pass through unchanged.
    if !header.eq_ignore_ascii_case(CLOUD_CONFIG_HEADER) {
        return None;
    }
    split_cloud_config_network(custom_cloud_init).map(|split| split.user_data)
}

/// Separates an unindented `network:` entry while preserving both YAML sources,
/// including quoted strings interpreted with cloud-init's YAML 1.1 rules.
/// Archive parts may declare their format through a MIME type instead of a header.
/// Callers must check that the network source stands alone: its aliases may
/// reference anchors in the retained user-data.
fn split_cloud_config_network(custom_cloud_init: &str) -> Option<NetworkConfigSplit> {
    let mut expected: serde_yaml::Value = serde_yaml::from_str(custom_cloud_init).ok()?;
    expected.as_mapping_mut()?.remove(NETWORK_CONFIG_KEY)?;

    let mut start = None;
    let mut end = custom_cloud_init.len();
    let mut offset = 0;
    for line in custom_cloud_init.split_inclusive('\n') {
        if start.is_some() && !line.starts_with(char::is_whitespace) && !line.starts_with('#') {
            end = offset;
            break;
        }
        if line
            .strip_prefix(NETWORK_CONFIG_KEY)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            start = Some(offset);
        }
        offset += line.len();
    }
    let start = start?;
    let network_config = custom_cloud_init[start + NETWORK_CONFIG_KEY.len() + 1..end].to_string();
    let mut user_data = custom_cloud_init.to_string();
    user_data.replace_range(start..end, "");
    // A network-only document must still contain an empty cloud-config mapping.
    if expected.as_mapping()?.is_empty() {
        user_data = format!("{CLOUD_CONFIG_HEADER}\n{{}}\n");
    }
    let actual: serde_yaml::Value = serde_yaml::from_str(&user_data).ok()?;
    (actual == expected).then_some(NetworkConfigSplit {
        user_data,
        network_config,
    })
}

struct NetworkConfigSplit {
    user_data: String,
    network_config: String,
}

/// Split static cloud-config parts atomically for both NoCloud responses. With
/// cloud-init's default merge rules, the last part defining `network` wins.
/// Custom mergers, launch-index filters, dynamic parts, and unsafe source edits
/// leave the entire archive on its existing passthrough/default-network path.
fn split_archive_network(custom_cloud_init: &str) -> Option<NetworkConfigSplit> {
    let mut archive: serde_yaml::Value = serde_yaml::from_str(custom_cloud_init).ok()?;
    let mut network_config = None;
    for part in archive.as_sequence_mut()? {
        let content_type = match part.get(ARCHIVE_TYPE_KEY) {
            None | Some(serde_yaml::Value::Null) => None,
            Some(value) => Some(value.as_str()?.to_ascii_lowercase()),
        };
        if let Some(mapping) = part.as_mapping()
            && mapping.keys().any(|key| {
                key.as_str().is_some_and(|key| {
                    ["merge-type", "x-merge-type", "launch-index", "<<"]
                        .iter()
                        .any(|unsupported| key.eq_ignore_ascii_case(unsupported))
                })
            })
        {
            return None;
        }
        let content = if part.is_string() {
            part
        } else {
            part.get_mut(ARCHIVE_CONTENT_KEY)?
        };
        let text = content.as_str()?;
        let header = text.split_whitespace().next().unwrap_or_default();
        match content_type.as_deref().filter(|value| !value.is_empty()) {
            Some(CLOUD_CONFIG_CONTENT_TYPE) => {}
            Some(value)
                if value.starts_with("text/x-shellscript") || value == "text/cloud-boothook" =>
            {
                continue;
            }
            Some(_) => return None,
            None if header.starts_with("#!") => continue,
            None if header.starts_with('#')
                && header != "#"
                && !header.eq_ignore_ascii_case(CLOUD_CONFIG_HEADER) =>
            {
                return None;
            }
            None => {}
        }
        if header == "##" {
            return None;
        }
        let config: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
        let mapping = config.as_mapping()?;
        if ["merge_how", "merge_type", "launch-index", "<<"]
            .iter()
            .any(|key| mapping.contains_key(*key))
        {
            return None;
        }
        if let Some(network) = mapping.get(NETWORK_CONFIG_KEY) {
            let split = split_cloud_config_network(text)?;
            let actual_network: serde_yaml::Value =
                serde_yaml::from_str(&split.network_config).ok()?;
            if &actual_network != network {
                return None;
            }
            network_config = Some(split.network_config);
            *content = split.user_data.into();
        }
    }
    let network_config = network_config?;
    // JSON is YAML-compatible and quotes every string in the envelope. YAML 1.1
    // must not reinterpret a filename such as 'on'; embedded YAML stays verbatim.
    let envelope = serde_json::to_string_pretty(&archive).ok()?;
    if serde_yaml::from_str::<serde_yaml::Value>(&envelope).ok()? != archive {
        return None;
    }
    Some(NetworkConfigSplit {
        user_data: format!("{CLOUD_CONFIG_ARCHIVE_HEADER}\n{envelope}\n"),
        network_config,
    })
}

async fn meta_data(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let (template_key, template_data) = match machine.instructions.metadata {
        None => log_and_generate_generic_error(
            format!("No metadata was found for machine {machine:?}"),
            OutcomeReason::MetadataNotFound,
            ENDPOINT,
        ),
        Some(metadata) => {
            let mut template_data = HashMap::from([
                ("instance_id".to_string(), metadata.instance_id),
                ("cloud_name".to_string(), metadata.cloud_name),
                ("platform".to_string(), metadata.platform),
            ]);
            if let Some(local_hostname) = metadata.local_hostname {
                template_data.insert("local_hostname".to_string(), local_hostname);
            }

            emit(PxeBootOutcome {
                endpoint: ENDPOINT,
                reason: OutcomeReason::Ok,
            });
            ("meta-data".to_string(), template_data)
        }
    };

    axum_template::Render(template_key, state.engine.clone(), template_data)
}

/// Extracts `network:` from a plain document or supported archive's cloud-config
/// parts and returns it as its own standalone YAML
/// document, suitable for seeding NoCloud's separate `network-config`
/// file. The user-data handler removes supported network blocks because
/// cloud-init rejects that key during user-data schema validation.
fn extract_network_config(custom_cloud_init: &str) -> Option<String> {
    if custom_cloud_init
        .split_whitespace()
        .next()
        .is_some_and(|header| header.eq_ignore_ascii_case(CLOUD_CONFIG_ARCHIVE_HEADER))
    {
        return split_archive_network(custom_cloud_init).map(|split| split.network_config);
    }
    let value: serde_yaml::Value = serde_yaml::from_str(custom_cloud_init).ok()?;
    let network = value.get(NETWORK_CONFIG_KEY)?;
    if let Some(split) = split_cloud_config_network(custom_cloud_init)
        && serde_yaml::from_str::<serde_yaml::Value>(&split.network_config)
            .ok()
            .as_ref()
            == Some(network)
    {
        return Some(split.network_config);
    }
    serde_yaml::to_string(network).ok()
}

/// Default network-config served when a tenant hasn't provided a custom
/// `network:` key in their cloud-init userdata. cloud-init's own default
/// behavior (no network-config at all) only DHCPs the first network
/// interface it finds; this instead DHCPs every matching interface, under
/// both the predictable ("en*") and legacy ("eth*") naming conventions,
/// so multi-NIC hosts come up with working networking on every port.
const DEFAULT_NETWORK_CONFIG: &str = r#"version: 2
ethernets:
  predictable-names:
    match:
      name: "en*"
    dhcp4: true
    dhcp6: true
  legacy-names:
    match:
      name: "eth*"
    dhcp4: true
    dhcp6: true
"#;

/// Resolves the network-config YAML to use for a machine: the `network:`
/// key extracted from the tenant's custom cloud-init userdata if present,
/// otherwise DEFAULT_NETWORK_CONFIG (DHCP on every interface), rather
/// than an empty document that would fall back to cloud-init's own
/// first-interface-only default.
fn resolve_network_config(custom_cloud_init: Option<&str>) -> Cow<'static, str> {
    custom_cloud_init
        .and_then(extract_network_config)
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed(DEFAULT_NETWORK_CONFIG))
}

/// Serves NoCloud's `network-config` document for a tenant's assigned
/// machine, extracted from any `network:` key present in their custom
/// cloud-init userdata. When no such key is present, serves
/// DEFAULT_NETWORK_CONFIG instead of an empty document, so hosts get
/// DHCP on every interface by default rather than cloud-init's own
/// first-interface-only behavior.
async fn network_config(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let network_config_yaml =
        resolve_network_config(machine.instructions.custom_cloud_init.as_deref());
    let template_data = HashMap::from([("network_config", network_config_yaml)]);
    axum_template::Render("network-config", state.engine.clone(), template_data)
}

async fn vendor_data(state: State<AppState>) -> impl IntoResponse {
    emit(PxeBootOutcome {
        endpoint: ENDPOINT,
        reason: OutcomeReason::Ok,
    });
    axum_template::Render(
        "printcontext",
        state.engine.clone(),
        HashMap::<String, String>::new(),
    )
}

/// Builds the tenant's route table under `path_prefix`: the four documents
/// NoCloud fetches.
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
        .route(
            format!("{}/{}", path_prefix, "vendor-data").as_str(),
            get(vendor_data),
        )
        .route(
            format!("{}/{}", path_prefix, "network-config").as_str(),
            get(network_config),
        )
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::value_scenarios;
    use rpc::forge;

    use super::*;
    use crate::common::{test_app_state, test_app_state_with_templates};

    #[test]
    fn user_data_recognizes_cloud_config_headers() {
        value_scenarios!(run = |header: &str| {
            user_data_without_network(&format!("{header}\nnetwork: {{}}\nhostname: 'on'\n"))
        };
            "leading whitespace" {
                "\n  #cloud-config" => Some("\n  #cloud-config\nhostname: 'on'\n".to_string()),
            }
            "trailing whitespace" {
                "#cloud-config \t" => Some("#cloud-config \t\nhostname: 'on'\n".to_string()),
            }
            "case insensitive header" {
                "#ClOuD-CoNfIg" => Some("#ClOuD-CoNfIg\nhostname: 'on'\n".to_string()),
            }
            "header comment" {
                "#cloud-config # comment" => Some("#cloud-config # comment\nhostname: 'on'\n".to_string()),
            }
            "suffixed header tokens" {
                "\n #CLOUD-CONFIG-ARCHIVE" => None,
                "#cloud-config-jsonp" => None,
                "#cloud-config-future" => None,
            }
        );
    }

    #[tokio::test]
    async fn tenant_cloud_config_separates_network_from_user_data() {
        let _metrics = MetricsCapture::start();
        for custom_cloud_init in [
            "#cloud-config\nnetwork:\n  version: 2\n  ethernets:\n    eth0:\n      dhcp6: true\n      dhcp4: false\nhostname: 'on'\nusers: [default]\nwrite_files:\n  - path: /etc/example.yaml\n    content: 'off'\n  - path: /etc/nested.yaml\n    content: |\n      network: keep this nested content\n",
            "#cloud-config\nnetwork:\n  version: 2\n  ethernets: {}\n",
        ] {
            let mut expected: serde_yaml::Value = serde_yaml::from_str(custom_cloud_init).unwrap();
            let expected_network = expected
                .as_mapping_mut()
                .unwrap()
                .remove("network")
                .unwrap();
            let machine = || Machine {
                instructions: forge::CloudInitInstructions {
                    custom_cloud_init: Some(custom_cloud_init.to_string()),
                    ..Default::default()
                },
            };
            let state = test_app_state_with_templates();

            let response = user_data(machine(), State(state.clone()))
                .await
                .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let text = std::str::from_utf8(&body).unwrap();
            assert!(text.starts_with("#cloud-config\n"));
            if let Some((_, remaining)) = custom_cloud_init.split_once("hostname:") {
                assert!(text.contains(&format!("hostname:{remaining}")));
            }
            let actual: serde_yaml::Value = serde_yaml::from_str(text).unwrap();
            assert_eq!(
                actual, expected,
                "user-data must retain only the non-network settings"
            );

            let response = network_config(machine(), State(state))
                .await
                .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let actual: serde_yaml::Value = serde_yaml::from_slice(&body).unwrap();
            assert_eq!(actual, expected_network);
        }
    }

    #[tokio::test]
    async fn tenant_cloud_config_archive_separates_network_from_user_data() {
        let _metrics = MetricsCapture::start();
        let input = r#"#cloud-config-archive
- type: text/x-shellscript
  filename: 'on'
  content: |
    #!/bin/sh
    echo 'network: leave script alone'
- type: text/cloud-config
  content: |
    network: {version: 2, ethernets: {eth0: {dhcp4: true}}}
    users: [default]
- content: |
    #cloud-config
    network:
      version: 2
      ethernets:
        eth1:
          match: {macaddress: '52:54:00:12:34:56'}
          dhcp4: on
          addresses: [2001:db8::10/64]
    hostname: 'on'
    autoinstall:
      version: 1
      network: {version: 2, ethernets: {eth0: {dhcp4: true}}}
- type: text/cloud-config
  content: |
    #cloud-config
    phone_home:
      url: http://169.254.169.254/latest/meta-data/phone_home
      post: all
"#;
        let machine = || Machine {
            instructions: forge::CloudInitInstructions {
                custom_cloud_init: Some(input.to_string()),
                ..Default::default()
            },
        };
        let state = test_app_state_with_templates();
        let response = network_config(machine(), State(state.clone()))
            .await
            .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let actual_network: serde_yaml::Value = serde_yaml::from_slice(&body).unwrap();
        let expected_network: serde_yaml::Value = serde_yaml::from_str(
            "version: 2\nethernets: {eth1: {match: {macaddress: '52:54:00:12:34:56'}, dhcp4: on, addresses: ['2001:db8::10/64']}}\n",
        )
        .unwrap();
        assert_eq!(
            actual_network, expected_network,
            "later network replaces earlier network"
        );

        let response = user_data(machine(), State(state)).await.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.starts_with("#cloud-config-archive\n"));
        let json: serde_json::Value =
            serde_json::from_str(text.strip_prefix("#cloud-config-archive\n").unwrap()).unwrap();
        assert_eq!(
            json[0]["filename"].as_str(),
            Some("on"),
            "quote metadata strings for cloud-init's YAML 1.1 parser"
        );
        let actual: serde_yaml::Value = serde_yaml::from_str(text).unwrap();
        let mut expected: serde_yaml::Value = serde_yaml::from_str(input).unwrap();
        expected[1]["content"] = "users: [default]\n".into();
        expected[2]["content"] = "#cloud-config\nhostname: 'on'\nautoinstall:\n  version: 1\n  network: {version: 2, ethernets: {eth0: {dhcp4: true}}}\n".into();
        assert_eq!(
            actual, expected,
            "preserve every other part, field, and embedded YAML string"
        );
    }

    #[test]
    fn archive_network_handles_part_formats() {
        value_scenarios!(run = |part: &str| {
            let input = format!("{CLOUD_CONFIG_ARCHIVE_HEADER}\n{part}");
            extract_network_config(&input)
                .map(|network| serde_yaml::from_str::<serde_yaml::Value>(&network).unwrap()["version"].as_u64().unwrap())
        };
            "untyped part defaults to cloud-config" {
                "- content: |\n    # ordinary comment\n    network: {version: 2}\n" => Some(2),
            }
            "scalar part with inferred type" {
                "- |\n  #cloud-config\n  network: {version: 2}\n" => Some(2),
            }
            "explicit cloud-config type overrides content header" {
                "- type: text/cloud-config\n  content: |\n    #cloud-boothook\n    network: {version: 2}\n" => Some(2),
            }
            "explicit script type overrides content header" {
                "- type: text/x-shellscript\n  content: |\n    #cloud-config\n    network: {version: 2}\n" => None,
            }
            "inferred script leaves cloud-config networking available" {
                "- content: |\n    #!/bin/sh\n    echo ready\n- content: 'network: {version: 2}'\n" => Some(2),
            }
            "script frequency suffix leaves cloud-config networking available" {
                "- type: text/x-shellscript-per-boot\n  content: |\n    #!/bin/sh\n    echo ready\n- content: 'network: {version: 2}'\n" => Some(2),
            }
            "typed boothook leaves cloud-config networking available" {
                "- type: text/cloud-boothook\n  content: |\n    #cloud-boothook\n    echo ready\n- content: 'network: {version: 2}'\n" => Some(2),
            }
        );
        let output = user_data_without_network(
            "#cloud-config-archive\n- |\n  #cloud-config\n  network: {version: 2}\n",
        )
        .unwrap();
        let archive: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();
        assert_eq!(archive[0].as_str(), Some("#cloud-config\n{}\n"));
    }

    #[test]
    fn network_config_preserves_quoted_network_scalars() {
        let network = "\n  version: 2\n  ethernets:\n    eth0:\n      match: {macaddress: '52:54:00:12:34:56'}\n      dhcp4: on\n";
        let cloud_config = format!("#cloud-config\nnetwork:{network}");
        let archive = format!(
            "#cloud-config-archive\n- content: {}\n",
            serde_json::to_string(&cloud_config).unwrap(),
        );
        for input in [cloud_config, archive] {
            assert_eq!(extract_network_config(&input).as_deref(), Some(network));
        }
    }

    #[test]
    fn cloud_config_network_alias_can_reference_retained_installer_config() {
        let retained = "#cloud-config\nautoinstall:\n  version: 1\n  network: &net {version: 2, ethernets: {}}\n";
        let input = format!("{retained}network: *net\n");
        assert_eq!(user_data_without_network(&input).as_deref(), Some(retained));
        let network = extract_network_config(&input).unwrap();
        assert_eq!(
            serde_yaml::from_str::<serde_yaml::Value>(&network).unwrap(),
            serde_yaml::from_str::<serde_yaml::Value>("version: 2\nethernets: {}\n").unwrap(),
        );
    }

    #[test]
    fn archive_network_leaves_unsupported_archives_intact() {
        value_scenarios!(run = |part: &str| {
            let input = format!("{CLOUD_CONFIG_ARCHIVE_HEADER}\n- content: |\n    network: {{version: 2}}\n{part}");
            (user_data_without_network(&input), extract_network_config(&input))
        };
            "custom merge header" {
                "- type: text/cloud-config\n  Merge-Type: dict(no_replace)\n  content: 'hostname: node'\n" => (None, None),
            }
            "custom merge in a later part without network" {
                "- content: |\n    merge_how: dict(no_replace)\n    hostname: node\n" => (None, None),
            }
            "launch-index filter" {
                "- launch-index: 1\n  content: 'network: {version: 1}'\n" => (None, None),
            }
            "dynamic content" {
                "- content: |\n    ## template: jinja\n    #cloud-config\n    network: {{ v1.network }}\n" => (None, None),
            }
            "explicit type can modify the merged network" {
                "- type: text/cloud-config-jsonp\n  content: '[{\"op\":\"remove\",\"path\":\"/network\"}]'\n" => (None, None),
            }
            "malformed embedded YAML" {
                "- type: text/cloud-config\n  content: 'network: ['\n" => (None, None),
            }
            "metadata cannot survive JSON rendering" {
                "- filename: !custom preserved\n  content: 'hostname: node'\n" => (None, None),
            }
            "source cannot be split safely" {
                "- content: |\n    network: &net {version: 1}\n    x-network: *net\n" => (None, None),
            }
            "network source depends on a retained anchor" {
                "- content: |\n    autoinstall:\n      version: 1\n      network: &net {version: 2, ethernets: {}}\n    network: *net\n" => (None, None),
            }
        );
    }

    #[tokio::test]
    async fn user_data_preserves_documents_that_do_not_need_splitting() {
        let _metrics = MetricsCapture::start();
        for (name, input) in [
            ("absent", None),
            (
                "cloud-config without network",
                Some("#cloud-config\n# preserve comments\nusers: [default]\n"),
            ),
            (
                "script",
                Some("#!/bin/sh\n# network configuration is script-owned\nnetwork: value\n"),
            ),
            (
                "jinja template",
                Some(
                    "## template: jinja\n#cloud-config\nnetwork: {}\nhostname: '{{ v1.local_hostname }}'\n",
                ),
            ),
            (
                "malformed cloud-config",
                Some("#cloud-config\nnetwork: [\n"),
            ),
            ("unmarked YAML", Some("network: {}\n")),
            (
                "phone-home archive without network",
                Some(
                    "#cloud-config-archive\n# preserve envelope comments\n- type: text/cloud-config\n  content: |\n    #cloud-config\n    phone_home:\n      url: http://169.254.169.254/latest/meta-data/phone_home\n      post: all\n",
                ),
            ),
            (
                "flow mapping",
                Some("#cloud-config\n{network: {version: 2}, hostname: 'on'}\n"),
            ),
            (
                "network text inside a multiline scalar",
                Some(
                    "#cloud-config\nwrite_files: [{path: /etc/x, content: \"before\nnetwork: keep\nafter\"}]\nnetwork: {version: 2}\n",
                ),
            ),
            (
                "referenced network anchor",
                Some("#cloud-config\nnetwork: &network\n  version: 2\nx-network: *network\n"),
            ),
        ] {
            let response = user_data(
                Machine {
                    instructions: forge::CloudInitInstructions {
                        custom_cloud_init: input.map(str::to_string),
                        ..Default::default()
                    },
                },
                State(test_app_state_with_templates()),
            )
            .await
            .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::OK, "{name}");
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            // The template appends a newline to the supplied user-data.
            assert_eq!(
                std::str::from_utf8(&body).unwrap(),
                format!("{}\n", input.unwrap_or("{}")),
                "{name}"
            );
        }
    }

    /// Table-driven coverage for `extract_network_config` across its three
    /// input variants: a present `network:` key, a missing one, and
    /// malformed YAML.
    #[test]
    fn extract_network_config_handles_various_inputs() {
        struct Case {
            name: &'static str,
            input: &'static str,
            expect_some: bool,
        }

        let cases = [
            Case {
                name: "network key present",
                input: "#cloud-config\nnetwork:\n  version: 2\n  ethernets:\n    eth0:\n      addresses:\n        - 10.10.10.50/24\nwrite_files:\n  - path: /tmp/foo\n    content: bar\n",
                expect_some: true,
            },
            Case {
                name: "no network key",
                input: "#cloud-config\nwrite_files:\n  - path: /tmp/foo\n    content: bar\n",
                expect_some: false,
            },
            Case {
                name: "invalid yaml",
                input: "not: valid: yaml: at: all: :::",
                expect_some: false,
            },
        ];

        for case in cases {
            let result = extract_network_config(case.input);
            assert_eq!(
                result.is_some(),
                case.expect_some,
                "case '{}' failed",
                case.name
            );

            if case.expect_some {
                let parsed: serde_yaml::Value = serde_yaml::from_str(&result.unwrap()).unwrap();
                assert_eq!(parsed.get("version").unwrap().as_u64().unwrap(), 2);
                assert!(
                    parsed
                        .get("ethernets")
                        .and_then(|e| e.get("eth0"))
                        .is_some(),
                    "case '{}': expected eth0 config present",
                    case.name
                );
            }
        }
    }

    #[test]
    fn resolve_network_config_handles_various_inputs() {
        struct Case {
            name: &'static str,
            custom_cloud_init: Option<&'static str>,
            expect_default: bool,
        }

        let cases = [
            Case {
                name: "no network key in custom cloud-init",
                custom_cloud_init: Some("#cloud-config\nwrite_files: []\n"),
                expect_default: true,
            },
            Case {
                name: "network key present in custom cloud-init",
                custom_cloud_init: Some(
                    "#cloud-config\nnetwork:\n  version: 2\n  ethernets:\n    eth0:\n      addresses:\n        - 10.10.10.50/24\n",
                ),
                expect_default: false,
            },
            Case {
                name: "no custom cloud-init at all",
                custom_cloud_init: None,
                expect_default: true,
            },
        ];

        for case in cases {
            let result = resolve_network_config(case.custom_cloud_init);

            if case.expect_default {
                assert_eq!(
                    result, DEFAULT_NETWORK_CONFIG,
                    "case '{}' failed",
                    case.name
                );
            } else {
                let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap_or_else(|e| {
                    panic!("case '{}': result was not valid YAML: {}", case.name, e)
                });
                assert_eq!(
                    parsed.get("version").unwrap().as_u64().unwrap(),
                    2,
                    "case '{}' failed",
                    case.name
                );
                let eth0_addresses = parsed
                    .get("ethernets")
                    .and_then(|e| e.get("eth0"))
                    .and_then(|e| e.get("addresses"))
                    .and_then(|a| a.as_sequence())
                    .unwrap_or_else(|| {
                        panic!("case '{}': expected ethernets.eth0.addresses", case.name)
                    });
                assert_eq!(
                    eth0_addresses[0].as_str().unwrap(),
                    "10.10.10.50/24",
                    "case '{}' failed",
                    case.name
                );
            }
        }
    }

    /// When an instance has a name, meta-data includes local-hostname so
    /// cloud-init sets the OS hostname via the NoCloud datasource.
    #[tokio::test]
    async fn meta_data_includes_local_hostname_when_instance_has_name() {
        // Serializes against the tests that assert exact counter deltas: driving
        // a handler emits into the shared registry, and MetricsCapture::start is
        // what takes the lock that keeps those deltas meaningful.
        let _metrics = MetricsCapture::start();

        let response = meta_data(
            Machine {
                instructions: forge::CloudInitInstructions {
                    metadata: Some(forge::CloudInitMetaData {
                        instance_id: "test-instance-id".to_string(),
                        cloud_name: "nvidia".to_string(),
                        platform: "forge".to_string(),
                        local_hostname: Some("my-node".to_string()),
                    }),
                    ..Default::default()
                },
            },
            State(test_app_state_with_templates()),
        )
        .await
        .into_response();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(
            text.contains("local-hostname: \"my-node\""),
            "meta-data should contain local-hostname, got: {text}"
        );
    }

    /// When an instance has no name, meta-data must not include local-hostname
    /// so cloud-init falls back to its default hostname derivation.
    #[tokio::test]
    async fn meta_data_omits_local_hostname_when_instance_has_no_name() {
        // Serializes against the tests that assert exact counter deltas: driving
        // a handler emits into the shared registry, and MetricsCapture::start is
        // what takes the lock that keeps those deltas meaningful.
        let _metrics = MetricsCapture::start();

        let response = meta_data(
            Machine {
                instructions: forge::CloudInitInstructions {
                    metadata: Some(forge::CloudInitMetaData {
                        instance_id: "test-instance-id".to_string(),
                        cloud_name: "nvidia".to_string(),
                        platform: "forge".to_string(),
                        local_hostname: None,
                    }),
                    ..Default::default()
                },
            },
            State(test_app_state_with_templates()),
        )
        .await
        .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(
            !text.contains("local-hostname"),
            "meta-data must not contain local-hostname when name is empty, got: {text}"
        );
    }

    /// A meta-data request with no metadata lands in the generic-error
    /// funnel, which serves the error template and moves the outcome
    /// counter.
    #[tokio::test]
    async fn meta_data_without_metadata_counts_metadata_not_found() {
        let metrics = MetricsCapture::start();

        let _ = meta_data(
            Machine {
                instructions: Default::default(),
            },
            State(test_app_state()),
        )
        .await;

        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[
                    ("endpoint", "cloud_init_tenant"),
                    ("reason", "metadata_not_found")
                ],
            ),
            1.0,
        );
    }

    /// A user-data request answered from the tenant's custom cloud-init
    /// counts as a served outcome.
    #[tokio::test]
    async fn user_data_with_custom_cloud_init_counts_ok() {
        let metrics = MetricsCapture::start();

        let _ = user_data(
            Machine {
                instructions: forge::CloudInitInstructions {
                    custom_cloud_init: Some("#cloud-config".to_string()),
                    ..Default::default()
                },
            },
            State(test_app_state()),
        )
        .await;

        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[("endpoint", "cloud_init_tenant"), ("reason", "ok")],
            ),
            1.0,
        );
    }
}
