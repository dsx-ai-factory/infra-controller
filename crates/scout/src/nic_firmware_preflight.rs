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

//! Local NIC preflight stages checked files before inspecting live hardware.
//! The files remain available through inspection, but no mutation or firmware
//! execution request is made and the result is not approval to flash.

use std::time::Duration;

use clap::{Args, Subcommand};
use eyre::WrapErr;
use libmlx::firmware::config::FirmwareSpec;
use libmlx::firmware::preflight::{
    FirmwareComparison, FirmwarePreflightResult, LocalFirmwareFiles, inspect_prepared_firmware,
    validate_preflight_request,
};
use reqwest::Url;
use tokio::signal::unix::{SignalKind, signal};

use crate::firmware_staging::FirmwareStaging;
use crate::metrics::FirmwareDownloadKind;

// Each artifact gets a cumulative 600-second download/retry deadline;
// hashing follows it and is not part of that deadline.
const ARTIFACT_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Subcommand, Clone)]
pub(crate) enum FirmwareAction {
    #[command(
        about = "Verify firmware files and compare a live NIC with an exact target",
        long_about = "Verify the image and optional device-config SHA-256 digests, then inspect \
            the selected live NIC's part number, PSID, and firmware version. Requires --mode \
            standalone. The query must report the same PCI address; MST aliases are not \
            resolved.\n\n\
            An exact version match and a change needed both exit successfully; unavailable or \
            incompatible device facts and download/checksum failures return an error. A newer \
            installed version still means a change is needed. This does not interpret image or \
            device-config contents, approve flashing, or change hardware.\n\n\
            Each present input is staged and checked before the device query. Temporary files \
            are removed on completion or catchable SIGINT/SIGTERM cancellation, not guaranteed \
            after SIGKILL or power loss. Each artifact has a 600-second download/retry deadline \
            (hashing follows it). The subsequent read-only device query requires Linux and \
            mlx-firmware-supervisor beside forge-scout. Its 30-second deadline includes the \
            tool, all descendants, and output collection. Each output stream is capped at \
            1 MiB. Query failure or timeout allows up to five seconds for cleanup and five \
            more seconds to receive the result. Cancellation requests cleanup independently \
            without waiting for confirmation. Terminating the helper, including the whole \
            service control group, prevents its cleanup."
    )]
    Preflight(PreflightArgs),
}

impl FirmwareAction {
    pub(crate) async fn run(&self) -> eyre::Result<()> {
        // Register before starting downloads or a query. Selecting a signal
        // drops the operation so staging and subprocess guards can clean up.
        let mut interrupt = signal(SignalKind::interrupt())
            .wrap_err("failed to register firmware preflight SIGINT handler")?;
        let mut terminate = signal(SignalKind::terminate())
            .wrap_err("failed to register firmware preflight SIGTERM handler")?;
        tokio::select! {
            biased;
            received = interrupt.recv() => match received {
                Some(()) => eyre::bail!("firmware preflight cancelled by SIGINT"),
                None => eyre::bail!("firmware preflight SIGINT handler closed"),
            },
            received = terminate.recv() => match received {
                Some(()) => eyre::bail!("firmware preflight cancelled by SIGTERM"),
                None => eyre::bail!("firmware preflight SIGTERM handler closed"),
            },
            result = async {
                match self {
                    Self::Preflight(args) => args.run().await,
                }
            } => result,
        }
    }
}

#[derive(Args, Clone)]
pub(crate) struct PreflightArgs {
    #[arg(
        long,
        value_name = "PCI_ADDRESS",
        help = "Local PCI address, [dddd:]bb:dd.f; MST paths are not accepted"
    )]
    device: String,
    #[arg(
        long,
        help = "Expected literal part number reported by the selected NIC"
    )]
    part_number: String,
    #[arg(long, help = "Expected literal PSID reported by the selected NIC")]
    psid: String,
    #[arg(long, help = "Exact target firmware version; not a minimum version")]
    version: String,
    #[arg(
        long,
        help = "Image HTTP(S) URL without authentication, query, or fragment; redirects are rejected"
    )]
    image_url: String,
    #[arg(long, value_parser = parse_sha256, value_name = "64_HEX_DIGITS", help = "Required image SHA-256 digest, in upper- or lowercase hexadecimal")]
    image_sha256: String,
    #[arg(
        long,
        requires = "device_config_sha256",
        help = "Optional device-config HTTP(S) URL with the same restrictions as --image-url; requires --device-config-sha256"
    )]
    device_config_url: Option<String>,
    #[arg(long, requires = "device_config_url", value_parser = parse_sha256, value_name = "64_HEX_DIGITS", help = "Device-config SHA-256 digest, required with --device-config-url; omission of both skips device-config staging")]
    device_config_sha256: Option<String>,
}

impl PreflightArgs {
    async fn run(&self) -> eyre::Result<()> {
        let desired = FirmwareSpec {
            part_number: self.part_number.clone(),
            psid: self.psid.clone(),
            version: self.version.clone(),
        };
        validate_preflight_request(&self.device, &desired)?;
        let report = self
            .inspect_with(async |files| {
                Ok(inspect_prepared_firmware(&self.device, &desired, files).await?)
            })
            .await?;

        print!("{}", self.format_report(&report));
        Ok(())
    }

    fn format_report(&self, report: &FirmwarePreflightResult) -> String {
        let comparison = match report.comparison {
            FirmwareComparison::ExactMatch => "exact match",
            FirmwareComparison::ChangeRequired => "change needed",
        };
        let mut output = format!(
            "Firmware comparison: {comparison}\nDevice: {}\n",
            report.device.pci_name
        );
        if let Some(version) = &report.device.fw_version_current {
            output.push_str(&format!("Installed firmware: {version}\n"));
        }
        output.push_str(&format!("Target firmware: {}\n", self.version));
        output.push_str("Image SHA-256: verified (contents not interpreted)\n");
        if self.device_config_url.is_some() {
            output.push_str("Device-config SHA-256: verified (contents not interpreted)\n");
        }
        output.push_str("Read-only inspection; no hardware changes or flash approval.\n");
        output
    }

    /// `inspect_with` keeps each present checked input alive through the
    /// inspector's await. A failed config download must not inspect a partial set.
    async fn inspect_with<F, T>(&self, inspect: F) -> eyre::Result<T>
    where
        F: for<'files> AsyncFnOnce(LocalFirmwareFiles<'files>) -> eyre::Result<T>,
    {
        let (image_url, device_config_source) = self.validated_artifact_sources()?;
        // This client has no Core certificates or authentication. Refusing
        // redirects also keeps the request on the source the operator supplied.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                attempt.error("artifact redirects are not supported")
            }))
            .build()
            .wrap_err("failed to create artifact HTTP client")?;
        let mut staging = FirmwareStaging::new().map_err(|error| eyre::eyre!(error))?;
        let image = staging
            .stage(
                &client,
                image_url.as_str(),
                &self.image_sha256,
                FirmwareDownloadKind::Artifact,
                ARTIFACT_DOWNLOAD_TIMEOUT,
            )
            .await
            .map_err(|error| eyre::eyre!(error))?;
        let device_config = match device_config_source {
            Some((url, sha256)) => Some(
                staging
                    .stage(
                        &client,
                        url.as_str(),
                        sha256,
                        FirmwareDownloadKind::Artifact,
                        ARTIFACT_DOWNLOAD_TIMEOUT,
                    )
                    .await
                    .map_err(|error| eyre::eyre!(error))?,
            ),
            None => None,
        };
        inspect(LocalFirmwareFiles {
            image: &image,
            device_config: device_config.as_deref(),
        })
        .await
    }

    fn validated_artifact_sources(&self) -> eyre::Result<(Url, Option<(Url, &str)>)> {
        // Clap includes rejected values in its diagnostics. Validate URLs here
        // instead so unsupported credentials never get echoed to stderr.
        let image_url = parse_artifact_url(&self.image_url)
            .map_err(|error| eyre::eyre!("invalid --image-url: {error}"))?;
        let device_config_source = match (&self.device_config_url, &self.device_config_sha256) {
            (Some(url), Some(sha256)) => Some((
                parse_artifact_url(url)
                    .map_err(|error| eyre::eyre!("invalid --device-config-url: {error}"))?,
                sha256.as_str(),
            )),
            (None, None) => None,
            _ => eyre::bail!("device-config URL and SHA-256 digest must be supplied together"),
        };
        Ok((image_url, device_config_source))
    }
}

fn parse_artifact_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "invalid artifact URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("artifact URL must use HTTP or HTTPS with a host".to_string());
    }
    // An empty username still has userinfo syntax (`http://@host/...`).
    let authority = value
        .split_once("://")
        .map(|(_, tail)| tail.split(['/', '?', '#']).next().unwrap_or(""));
    if !url.username().is_empty()
        || url.password().is_some()
        || authority.is_some_and(|authority| authority.contains('@'))
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("artifact URL must not include authentication, query, or fragment".to_string());
    }
    Ok(url)
}

fn parse_sha256(value: &str) -> Result<String, String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("SHA-256 digest must contain exactly 64 hexadecimal digits".to_string());
    }
    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::routing::get;
    use carbide_test_support::{Check, check_values};
    use clap::Parser;
    use sha2::{Digest, Sha256};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::cfg::{Command, MlxAction, Mode, Options};

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const UPPERCASE_DIGEST: &str =
        "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF";
    const BASE_ARGS: [&str; 13] = [
        "forge-scout",
        "--mode",
        "standalone",
        "mlx",
        "firmware",
        "preflight",
        "--device",
        "03:00.0",
        "--part-number",
        "test-part",
        "--psid",
        "test-psid",
        "--version",
    ];

    #[test]
    fn report_formats_comparison_and_checked_artifacts() {
        check_values(
            [
                Check {
                    scenario: "change needed without config",
                    input: (FirmwareComparison::ChangeRequired, "32.42.1000", None),
                    expect: concat!(
                        "Firmware comparison: change needed\n",
                        "Device: 03:00.0\n",
                        "Installed firmware: 32.42.1000\n",
                        "Target firmware: 32.43.1014\n",
                        "Image SHA-256: verified (contents not interpreted)\n",
                        "Read-only inspection; no hardware changes or flash approval.\n",
                    )
                    .to_string(),
                },
                Check {
                    scenario: "exact match with config",
                    input: (
                        FirmwareComparison::ExactMatch,
                        "32.43.1014",
                        Some("https://example.test/config.bin"),
                    ),
                    expect: concat!(
                        "Firmware comparison: exact match\n",
                        "Device: 03:00.0\n",
                        "Installed firmware: 32.43.1014\n",
                        "Target firmware: 32.43.1014\n",
                        "Image SHA-256: verified (contents not interpreted)\n",
                        "Device-config SHA-256: verified (contents not interpreted)\n",
                        "Read-only inspection; no hardware changes or flash approval.\n",
                    )
                    .to_string(),
                },
            ],
            |(comparison, installed_version, device_config_url)| {
                let args = PreflightArgs {
                    device: "03:00.0".to_string(),
                    part_number: "test-part".to_string(),
                    psid: "test-psid".to_string(),
                    version: "32.43.1014".to_string(),
                    image_url: "https://example.test/image.bin".to_string(),
                    image_sha256: DIGEST.to_string(),
                    device_config_url: device_config_url.map(str::to_string),
                    device_config_sha256: device_config_url.map(|_| DIGEST.to_string()),
                };
                let report = FirmwarePreflightResult {
                    device: serde_json::from_value(serde_json::json!({
                        "pci_name": "03:00.0",
                        "device_type": "BlueField3",
                        "fw_version_current": installed_version,
                    }))
                    .expect("device report fixture"),
                    comparison,
                };
                args.format_report(&report)
            },
        );
    }

    /// CLI parsing and URL admission require digests and an all-or-nothing
    /// device-config pair before creating a staging directory or HTTP client.
    #[test]
    fn preflight_command_admits_only_complete_checked_artifacts() {
        check_values(
            [
                // The image is mandatory; device config is not. Digest case
                // must not change the byte identity used by shared staging.
                (
                    "image only",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin",
                        "--image-sha256",
                        UPPERCASE_DIGEST,
                    ],
                    true,
                ),
                // Both optional flags select one additional checked input.
                (
                    "image and config",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin",
                        "--image-sha256",
                        DIGEST,
                        "--device-config-url",
                        "https://example.test/config.bin",
                        "--device-config-sha256",
                        DIGEST,
                    ],
                    true,
                ),
                // An unchecked image is never a valid preflight input.
                (
                    "missing image digest",
                    vec!["--image-url", "https://example.test/image.bin"],
                    false,
                ),
                // Supplying only either half must not silently skip config.
                (
                    "config URL without digest",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin",
                        "--image-sha256",
                        DIGEST,
                        "--device-config-url",
                        "https://example.test/config.bin",
                    ],
                    false,
                ),
                (
                    "config digest without URL",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin",
                        "--image-sha256",
                        DIGEST,
                        "--device-config-sha256",
                        DIGEST,
                    ],
                    false,
                ),
                // SHA-256 is a complete 32-byte digest, not an arbitrary token.
                (
                    "short digest",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin",
                        "--image-sha256",
                        "abcd",
                    ],
                    false,
                ),
                // A full-length non-hex value must fail before the first fetch.
                (
                    "non-hex digest",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin",
                        "--image-sha256",
                        "g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    ],
                    false,
                ),
                // This path deliberately has no authenticated source support.
                (
                    "URL credentials",
                    vec![
                        "--image-url",
                        "https://user:password@example.test/image.bin",
                        "--image-sha256",
                        DIGEST,
                    ],
                    false,
                ),
                (
                    "empty URL userinfo",
                    vec![
                        "--image-url",
                        "https://@example.test/image.bin",
                        "--image-sha256",
                        DIGEST,
                    ],
                    false,
                ),
                // Query/fragment values are not part of an admitted artifact URL.
                (
                    "URL query",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin?token=value",
                        "--image-sha256",
                        DIGEST,
                    ],
                    false,
                ),
                (
                    "URL fragment",
                    vec![
                        "--image-url",
                        "https://example.test/image.bin#fragment",
                        "--image-sha256",
                        DIGEST,
                    ],
                    false,
                ),
                // Local and SSH sources belong to the separate flasher path.
                (
                    "non HTTP source",
                    vec!["--image-url", "file:///image.bin", "--image-sha256", DIGEST],
                    false,
                ),
            ]
            .into_iter()
            .map(|(scenario, input, expect)| Check {
                scenario,
                input,
                expect,
            }),
            |artifact_args| {
                let Ok(options) = Options::try_parse_from(
                    BASE_ARGS.into_iter().chain(["1.2.3"]).chain(artifact_args),
                ) else {
                    return false;
                };
                let Some(Command::Mlx(mlx)) = options.subcmd else {
                    panic!("fixture must select mlx");
                };
                let MlxAction::Firmware(firmware) = mlx.action else {
                    panic!("fixture must select firmware");
                };
                let FirmwareAction::Preflight(args) = firmware.action;
                assert_eq!(args.image_sha256, args.image_sha256.to_ascii_lowercase());
                match args.validated_artifact_sources() {
                    Ok(_) => true,
                    Err(error) => {
                        assert!(!error.to_string().contains("password"));
                        assert!(!error.to_string().contains("token=value"));
                        false
                    }
                }
            },
        );
    }

    /// Exercise the real staging consumer: either bad digest prevents all live
    /// inspection, while success passes local bytes without a second fetch.
    #[tokio::test]
    async fn inspect_waits_for_checked_files() {
        struct Case {
            scenario: &'static str,
            image_matches: bool,
            config_matches: Option<bool>,
            expected_requests: usize,
            expected_inspections: usize,
        }
        let cases = [
            // The first failure stops before requesting the optional config.
            Case {
                scenario: "bad image",
                image_matches: false,
                config_matches: Some(true),
                expected_requests: 1,
                expected_inspections: 0,
            },
            // A checked image alone is not sufficient when config was requested.
            Case {
                scenario: "bad config",
                image_matches: true,
                config_matches: Some(false),
                expected_requests: 2,
                expected_inspections: 0,
            },
            // Equal basenames must still refer to each independently checked body.
            Case {
                scenario: "both checked",
                image_matches: true,
                config_matches: Some(true),
                expected_requests: 2,
                expected_inspections: 1,
            },
            // An omitted config must reach inspection with only the image.
            Case {
                scenario: "image only",
                image_matches: true,
                config_matches: None,
                expected_requests: 1,
                expected_inspections: 1,
            },
        ];
        for case in cases {
            let requests = Arc::new(AtomicUsize::new(0));
            let app = Router::new()
                .route(
                    "/image/input.bin",
                    get({
                        let requests = requests.clone();
                        move || {
                            requests.fetch_add(1, Ordering::SeqCst);
                            async { "firmware image bytes" }
                        }
                    }),
                )
                .route(
                    "/config/input.bin",
                    get({
                        let requests = requests.clone();
                        move || {
                            requests.fetch_add(1, Ordering::SeqCst);
                            async { "device config bytes" }
                        }
                    }),
                );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let stop = CancellationToken::new();
            let stopped = stop.clone();
            let server = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(stopped.cancelled_owned())
                    .await
                    .unwrap();
            });
            let args = PreflightArgs {
                device: "03:00.0".to_string(),
                part_number: "test-part".to_string(),
                psid: "test-psid".to_string(),
                version: "1.2.3".to_string(),
                image_url: format!("http://{address}/image/input.bin"),
                image_sha256: hex::encode(Sha256::digest(if case.image_matches {
                    "firmware image bytes"
                } else {
                    "different bytes"
                })),
                device_config_url: case
                    .config_matches
                    .map(|_| format!("http://{address}/config/input.bin")),
                device_config_sha256: case.config_matches.map(|matches| {
                    hex::encode(Sha256::digest(if matches {
                        "device config bytes"
                    } else {
                        "different bytes"
                    }))
                }),
            };
            let inspections = Cell::new(0);
            let result: eyre::Result<Vec<PathBuf>> = args
                .inspect_with(async |files| {
                    inspections.set(inspections.get() + 1);
                    assert_eq!(
                        tokio::fs::read(files.image).await.unwrap(),
                        b"firmware image bytes"
                    );
                    assert_eq!(files.device_config.is_some(), case.config_matches.is_some());
                    if let Some(config) = files.device_config {
                        assert_eq!(
                            tokio::fs::read(config).await.unwrap(),
                            b"device config bytes"
                        );
                        assert_ne!(files.image, config);
                    }
                    Ok(std::iter::once(files.image)
                        .chain(files.device_config)
                        .map(PathBuf::from)
                        .collect())
                })
                .await;

            stop.cancel();
            server.await.unwrap();
            assert_eq!(
                requests.load(Ordering::SeqCst),
                case.expected_requests,
                "{}",
                case.scenario
            );
            assert_eq!(
                inspections.get(),
                case.expected_inspections,
                "{}",
                case.scenario
            );
            if case.expected_inspections == 1 {
                let paths = result.expect(case.scenario);
                assert_eq!(
                    paths.len(),
                    1 + usize::from(case.config_matches.is_some()),
                    "{}",
                    case.scenario
                );
                // Temporary inputs must outlive inspection, not the command.
                assert!(paths.iter().all(|path| !path.exists()), "{}", case.scenario);
            } else {
                let error = result.expect_err(case.scenario);
                assert!(
                    error.to_string().contains("checksum mismatch"),
                    "{}: {error:#}",
                    case.scenario
                );
            }
        }
    }

    /// A redirect body can itself match the requested digest. Rejecting the
    /// redirect must prevent both inspection of that body and a redirected GET.
    #[tokio::test]
    async fn preflight_rejects_redirects_before_inspection() {
        struct Case {
            scenario: &'static str,
            location: Option<&'static str>,
            expected_error: &'static str,
        }
        let cases = [
            // A normal redirect must not send a request to its Location.
            Case {
                scenario: "redirect location",
                location: Some("/target?token=secret-marker"),
                expected_error: "redirect",
            },
            // Without Location, reqwest's redirect policy is not invoked, but
            // a matching response body still must not turn 302 into success.
            Case {
                scenario: "redirect without location",
                location: None,
                expected_error: "unexpected HTTP status 302",
            },
        ];
        for case in cases {
            let image_requests = Arc::new(AtomicUsize::new(0));
            let redirected_requests = Arc::new(AtomicUsize::new(0));
            let location = case.location;
            let app = Router::new()
                .route(
                    "/image.bin",
                    get({
                        let image_requests = image_requests.clone();
                        move || {
                            image_requests.fetch_add(1, Ordering::SeqCst);
                            async move {
                                let mut headers = http::HeaderMap::new();
                                if let Some(location) = location {
                                    headers.insert(
                                        http::header::LOCATION,
                                        http::HeaderValue::from_static(location),
                                    );
                                }
                                (http::StatusCode::FOUND, headers, "firmware image bytes")
                            }
                        }
                    }),
                )
                .route(
                    "/target",
                    get({
                        let redirected_requests = redirected_requests.clone();
                        move || {
                            redirected_requests.fetch_add(1, Ordering::SeqCst);
                            async { "firmware image bytes" }
                        }
                    }),
                );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let stop = CancellationToken::new();
            let stopped = stop.clone();
            let server = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(stopped.cancelled_owned())
                    .await
                    .unwrap();
            });
            let args = PreflightArgs {
                device: "03:00.0".to_string(),
                part_number: "test-part".to_string(),
                psid: "test-psid".to_string(),
                version: "1.2.3".to_string(),
                image_url: format!("http://{address}/image.bin"),
                image_sha256: hex::encode(Sha256::digest("firmware image bytes")),
                device_config_url: None,
                device_config_sha256: None,
            };
            let inspections = Cell::new(0);
            let result = args
                .inspect_with(async |_| {
                    inspections.set(inspections.get() + 1);
                    Ok(())
                })
                .await;
            stop.cancel();
            server.await.unwrap();

            // The retained staging retry policy retries only the original URL.
            assert_eq!(
                image_requests.load(Ordering::SeqCst),
                4,
                "{}",
                case.scenario
            );
            assert_eq!(
                redirected_requests.load(Ordering::SeqCst),
                0,
                "{}",
                case.scenario
            );
            assert_eq!(inspections.get(), 0, "{}", case.scenario);
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains(case.expected_error),
                "{}: {error:#}",
                case.scenario
            );
            assert!(
                !format!("{error:#}").contains("secret-marker"),
                "{}",
                case.scenario
            );
        }
    }

    /// Even with the default service mode, an explicit preflight command must
    /// fail locally rather than registering a machine or fetching Core work.
    #[tokio::test]
    async fn service_mode_rejects_preflight_before_registration() {
        // Omit both `--mode` and its value so this protects the real default,
        // not a manually constructed service configuration.
        let options = Options::try_parse_from(
            std::iter::once(BASE_ARGS[0])
                .chain(BASE_ARGS[3..].iter().copied())
                .chain([
                    "1.2.3",
                    "--image-url",
                    "https://example.test/image.bin",
                    "--image-sha256",
                    DIGEST,
                ]),
        )
        .unwrap_or_else(|error| panic!("fixture command must parse: {error}"));
        assert_eq!(options.mode, Mode::Service);
        let error = tokio::time::timeout(Duration::from_secs(5), crate::run_as_service(&options))
            .await
            .expect("local admission must not wait for registration")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "firmware preflight requires --mode standalone"
        );
        assert!(
            matches!(options.subcmd, Some(Command::Mlx(mlx)) if matches!(mlx.action, MlxAction::Firmware(_)))
        );
    }

    /// The standalone dispatch must reach local admission without contacting
    /// Core, and unsupported source credentials must not appear in the error.
    #[tokio::test]
    async fn standalone_preflight_rejects_source_authentication_locally() {
        let options = Options::try_parse_from(BASE_ARGS.into_iter().chain([
            "1.2.3",
            "--image-url",
            "https://user:secret-marker@example.test/image.bin",
            "--image-sha256",
            DIGEST,
        ]))
        .unwrap_or_else(|error| panic!("fixture command must parse: {error}"));
        let error = tokio::time::timeout(Duration::from_secs(5), crate::run_standalone(&options))
            .await
            .expect("local admission must not wait for registration")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid --image-url: artifact URL must not include authentication, query, or fragment"
        );
        assert!(!format!("{error:#}").contains("secret-marker"));
    }
}
