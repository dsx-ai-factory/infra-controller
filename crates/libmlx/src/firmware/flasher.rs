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

//! Preparation, firmware mutation, activation, and observation for one device.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use tokio::sync::watch;

use crate::device::discovery::{QUERY_TIMEOUT, discover_device_bounded_with_executor};
use crate::firmware::config::{FirmwareFlasherProfile, FirmwareSpec, FlashSpec};
use crate::firmware::error::{
    AttemptError, FirmwareApplyError, FirmwareError, FirmwarePhase, FirmwareResult,
};
use crate::firmware::preflight::{
    FirmwarePreflightError, check_hardware_identity, check_readable_file, is_usable_literal,
};
use crate::firmware::reset::{DEFAULT_RESET_LEVEL, MlxFwResetRunner};
use crate::firmware::source::FirmwareSource;
use crate::lockdown::runner::FlintRunner;
use crate::runner::FirmwareToolExecutor;
use crate::runner::applier::MlxConfigApplier;
use crate::runner::command_builder::CommandSpec;

// Administrative limits for individual commands, not measured hardware limits.
// Expiration does not prove that a device stopped changing. Cleanup has its own
// five-second bound; callers must reconcile a failed mutation before retrying.
const CONFIGURATION_TIMEOUT: Duration = Duration::from_secs(30);
const FIRMWARE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// `FirmwareProgress` records recovery points before execution can advance.
/// Returning an error stops the workflow at that boundary, including when a
/// command succeeded but its completion could not be recorded.
#[async_trait::async_trait]
pub(crate) trait FirmwareProgress: Send + Sync {
    /// `record` must settle this boundary before apply advances; returning an
    /// error withholds later work even when the preceding command succeeded.
    async fn record(&self, event: FirmwareProgressEvent) -> FirmwareResult<()>;
}

/// `FirmwareProgressEvent` exposes staging and execution boundaries to the
/// durable operation owner. Tool discovery does not replace a mutation's state.
pub(crate) enum FirmwareProgressEvent {
    Prepare,
    Prepared {
        directory: PathBuf,
        image: PathBuf,
        configuration: Option<PathBuf>,
    },
    Starting(FirmwarePhase),
    Completed(FirmwarePhase),
    ObservingVersion,
    ObservedVersion(String),
    Complete(FirmwareFlashReport),
}

/// `PreparedFirmware` owns separate, readable, nonempty image/configuration files.
///
/// Legacy `FlashSpec` has no digests: preparation does not certify file contents.
/// After a real burn, call `finish` only when every requested phase succeeded.
/// Otherwise dropping this owner retains its inputs, including after a failed
/// or cancelled standalone reset. Preparations dropped before mutation are removed.
/// Cancelled I/O, failed mutations, and unresolved cleanup retain the directory
/// and log its path for diagnosis and reconciliation.
pub struct PreparedFirmware {
    directory: Option<tempfile::TempDir>,
    image: PathBuf,
    device_config: Option<PathBuf>,
    retain: bool,
}

impl PreparedFirmware {
    /// `finish` releases staged inputs after all requested phases succeed or
    /// reconciliation is complete. Do not call it while a command or its cleanup
    /// might still use the files. Dropping a burned owner without finishing keeps
    /// the inputs and logs their directory instead.
    pub fn finish(mut self) {
        self.retain = false;
    }
}

impl Drop for PreparedFirmware {
    fn drop(&mut self) {
        let Some(directory) = self.directory.take() else {
            return;
        };
        if self.retain {
            let path = directory.keep();
            tracing::warn!(path = %path.display(), "retaining firmware inputs after incomplete execution");
        } else {
            let path = directory.path().to_path_buf();
            if let Err(error) = directory.close() {
                tracing::warn!(path = %path.display(), %error, "failed to remove prepared firmware inputs");
            }
        }
    }
}

/// `FirmwareFlasher` validates one device and executes each requested phase once.
///
/// Tool execution requires Linux and `mlx-firmware-supervisor`. A command error
/// stops the workflow; successful process cleanup does not prove that a
/// device-internal operation stopped. This type does not own durable recovery.
pub struct FirmwareFlasher {
    device_id: String,
    firmware_spec: FirmwareSpec,
    dry_run: bool,
    executor: FirmwareToolExecutor,
    progress: Option<Arc<dyn FirmwareProgress>>,
    preparation_cancel: Option<(String, watch::Receiver<Option<String>>)>,
}

impl FirmwareFlasher {
    /// `new` queries the selected tool locator and checks its part number and
    /// PSID. PCI addresses and MST paths retain the tool's lookup semantics;
    /// this does not establish physical identity or flash/reset eligibility.
    pub async fn new(device_id: impl Into<String>, spec: &FirmwareSpec) -> FirmwareResult<Self> {
        Self::new_with_executor(device_id, spec, FirmwareToolExecutor::default()).await
    }

    /// `new_with_executor` uses an explicitly configured supervisor for the
    /// initial device query and every later phase. This allows embedding
    /// applications to install the helper outside their executable directory.
    pub async fn new_with_executor(
        device_id: impl Into<String>,
        spec: &FirmwareSpec,
        executor: FirmwareToolExecutor,
    ) -> FirmwareResult<Self> {
        let device_id = device_id.into();
        let device = discover_device_bounded_with_executor(&device_id, &executor).await?;
        check_hardware_identity(&device, spec)?;
        Ok(Self {
            device_id,
            firmware_spec: spec.clone(),
            dry_run: false,
            executor,
            progress: None,
            preparation_cancel: None,
        })
    }

    /// `with_progress` lets the operation owner persist boundaries around the
    /// existing apply pipeline without implementing a second execution path.
    pub(crate) fn with_progress(mut self, progress: Arc<dyn FirmwareProgress>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// `with_preparation_cancel` permits dropping only pre-mutation staging.
    /// The operation owner sends this signal after persisting cancellation.
    pub(crate) fn with_preparation_cancel(
        mut self,
        operation_id: String,
        cancellation: watch::Receiver<Option<String>>,
    ) -> Self {
        self.preparation_cancel = Some((operation_id, cancellation));
        self
    }

    /// `with_dry_run` logs requested commands without running them. Preparation
    /// still resolves and checks files; no live version observation is reported.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// `prepare` resolves both inputs into distinct owned directories before
    /// any configuration or firmware mutation. Legacy sources provide no digest
    /// contract; preparation checks file readability, not authenticity.
    pub async fn prepare(&self, spec: &FlashSpec) -> FirmwareResult<PreparedFirmware> {
        self.record_progress(FirmwareProgressEvent::Prepare).await?;
        let firmware = spec.build_firmware_source()?;
        let configuration = spec.build_device_conf_source()?;
        let cache_dir = spec.cache_dir_or_default();
        tokio::fs::create_dir_all(&cache_dir).await?;
        let directory = tempfile::Builder::new()
            .prefix("firmware-")
            .tempdir_in(cache_dir)?;
        let directory_path = directory.path().to_path_buf();
        let mut prepared = PreparedFirmware {
            image: directory.path().join("image"),
            device_config: configuration
                .as_ref()
                .map(|_| directory.path().join("configuration")),
            directory: Some(directory),
            // Tokio filesystem work can outlive a dropped future. Keep its
            // directory on cancellation so removal cannot race that same work.
            retain: true,
        };
        let staging = async {
            prepared.image = stage_source(&firmware, &prepared.image).await?;
            if let (Some(source), Some(directory)) = (configuration, &prepared.device_config) {
                prepared.device_config = Some(stage_source(&source, directory).await?);
            }
            check_readable_file(&prepared.image).await?;
            if let Some(configuration) = &prepared.device_config {
                check_readable_file(configuration).await?;
            }
            FirmwareResult::Ok(())
        };
        let result = if let Some((operation_id, cancellation)) = &self.preparation_cancel {
            let mut cancellation = cancellation.clone();
            tokio::select! {
                biased;
                _ = async {
                    if cancellation
                        .wait_for(|requested| requested.as_deref() == Some(operation_id.as_str()))
                        .await
                        .is_err()
                    {
                        // Losing the sender is not a durable stop request.
                        std::future::pending::<()>().await;
                    }
                } => return Err(FirmwareError::OperationCanceled),
                result = staging => result,
            }
        } else {
            staging.await
        };
        if let Err(error) = result {
            prepared.retain = false;
            return Err(error);
        }
        self.record_progress(FirmwareProgressEvent::Prepared {
            directory: directory_path,
            image: prepared.image.clone(),
            configuration: prepared.device_config.clone(),
        })
        .await?;
        prepared.retain = false;
        Ok(prepared)
    }

    /// `burn` applies the optional prepared configuration, then burns the image.
    /// Neither mutation is retried. Any failure prevents the next command.
    /// A successful real burn retains its inputs until `PreparedFirmware::finish`,
    /// so callers can safely keep the owner through later activation/observation.
    pub async fn burn(&self, prepared: &mut PreparedFirmware) -> FirmwareResult<()> {
        let flint = self.flint().await?;
        let retained_before_burn = prepared.retain;
        let mut configuration_applied = false;
        // Keep the inputs if this future is dropped while a mutation runs.
        // A returned NotStarted can release them only if no earlier mutation
        // or uncertain attempt has already made them useful for reconciliation.
        prepared.retain = true;
        let result = async {
            if let Some(configuration) = &prepared.device_config {
                let command = MlxConfigApplier::new(&self.device_id).apply_command(configuration);
                self.execute(
                    &command,
                    FirmwarePhase::Configuration,
                    CONFIGURATION_TIMEOUT,
                )
                .await?;
                configuration_applied = !self.dry_run;
            }
            self.execute(
                &flint.burn_command(&self.device_id, &prepared.image),
                FirmwarePhase::Burn,
                FIRMWARE_COMMAND_TIMEOUT,
            )
            .await?;
            FirmwareResult::Ok(())
        }
        .await;
        prepared.retain = retained_before_burn
            || match &result {
                Ok(()) => !self.dry_run,
                Err(FirmwareError::PhaseFailed { source, .. }) if !source.started() => {
                    configuration_applied
                }
                Err(_) => true,
            };
        result
    }

    /// `flash` prepares and burns once, returning ownership of the staged files.
    /// Call `PreparedFirmware::finish` after every requested phase succeeds. A
    /// standalone flash can finish immediately; a later reset or observation must
    /// finish first, so failures or cancellation retain the exact burned inputs.
    pub async fn flash(&self, spec: &FlashSpec) -> FirmwareResult<PreparedFirmware> {
        let mut prepared = self.prepare(spec).await?;
        self.burn(&mut prepared).await?;
        Ok(prepared)
    }

    /// `verify_image` compares device firmware with the requested image.
    /// An existing legacy cache entry is used only when `verify_from_cache` is
    /// enabled; otherwise the source is resolved into an owned preparation.
    pub async fn verify_image(&self, spec: &FlashSpec) -> FirmwareResult<String> {
        let mut prepared = self.prepare_verification(spec).await?;
        self.verify_prepared_image(&mut prepared).await
    }

    async fn prepare_verification(&self, spec: &FlashSpec) -> FirmwareResult<PreparedFirmware> {
        let mut image_spec = spec.clone();
        image_spec.device_conf_url = None;
        image_spec.device_conf_credentials = None;
        if spec.verify_from_cache {
            let filename = spec
                .firmware_url
                .rsplit('/')
                .next()
                .unwrap_or("firmware.bin");
            let cached = spec.cache_dir_or_default().join(filename);
            if cached.exists() {
                image_spec.firmware_url = cached.to_string_lossy().into_owned();
                image_spec.firmware_credentials = None;
            }
        }
        self.prepare(&image_spec).await
    }

    async fn verify_prepared_image(
        &self,
        prepared: &mut PreparedFirmware,
    ) -> FirmwareResult<String> {
        let flint = self.flint().await?;
        let retained_before_verification = prepared.retain;
        prepared.retain = true;
        let result = self
            .execute(
                &flint.verify_image_command(&self.device_id, &prepared.image),
                FirmwarePhase::ImageVerification,
                FIRMWARE_COMMAND_TIMEOUT,
            )
            .await;
        // Verification does not mutate the device. A settled read failure does
        // not need its inputs, but unresolved process cleanup still does. Never
        // discard files retained by an earlier uncertain mutation.
        prepared.retain = retained_before_verification
            || matches!(&result, Err(FirmwareError::PhaseFailed {
                source: AttemptError::Started { cleanup_errors, .. }, ..
            }) if !cleanup_errors.is_empty());
        result
    }

    /// `verify_version` queries the selected device and requires the literal
    /// target version. Dry-run returns `None`, with no observed version.
    pub async fn verify_version(&self) -> FirmwareResult<Option<String>> {
        if self.dry_run {
            return Ok(None);
        }
        let observation = self.observe_version().await?;
        self.check_version(&observation)?;
        Ok(Some(observation))
    }

    /// `observe_version` records the query boundary and the returned version
    /// before comparison, preserving the evidence when verification fails.
    async fn observe_version(&self) -> FirmwareResult<String> {
        self.record_progress(FirmwareProgressEvent::ObservingVersion)
            .await?;
        // Keep the version actually returned by the final query. Admission
        // checked PN/PSID before mutation; this observation is not another
        // hardware-eligibility check or proof of physical device identity.
        let observation = discover_device_bounded_with_executor(&self.device_id, &self.executor)
            .await?
            .fw_version_current
            .filter(|version| is_usable_literal(version))
            .ok_or(FirmwarePreflightError::UnknownFirmwareVersion)?;
        self.record_progress(FirmwareProgressEvent::ObservedVersion(observation.clone()))
            .await?;
        Ok(observation)
    }

    fn check_version(&self, observation: &str) -> FirmwareResult<()> {
        if observation != self.firmware_spec.version {
            return Err(FirmwareError::VerificationFailed(format!(
                "firmware version mismatch on '{}': expected '{}', found '{}'",
                self.device_id, self.firmware_spec.version, observation,
            )));
        }
        Ok(())
    }

    /// `reset` requests activation at the existing default reset level.
    pub async fn reset(&self) -> FirmwareResult<String> {
        self.reset_with_level(DEFAULT_RESET_LEVEL).await
    }

    /// `reset_with_level` invokes `mlxfwreset` once at the supplied level.
    /// The level is passed through; no new hardware support is implied.
    pub async fn reset_with_level(&self, level: u8) -> FirmwareResult<String> {
        let program = self.find_tool(&MlxFwResetRunner::CANDIDATE_PATHS).await?;
        let command = MlxFwResetRunner::with_path(program).reset_command(&self.device_id, level);
        self.execute(
            &command,
            FirmwarePhase::Activation,
            FIRMWARE_COMMAND_TIMEOUT,
        )
        .await
    }

    /// `apply` prepares, burns, activates, and verifies the requested phases.
    ///
    /// The first error stops subsequent phases and returns its cause. Progress
    /// is available after `burn` succeeds, including its completion checkpoint;
    /// a failed checkpoint can leave a completed command unreported. Dry-run
    /// reports no completed mutations or observations. Progress alone does not
    /// establish whether retrying is safe.
    pub async fn apply(
        &self,
        profile: &FirmwareFlasherProfile,
    ) -> Result<FirmwareFlashReport, FirmwareApplyError> {
        let options = &profile.flash_options;
        let mut prepared = self.flash(&profile.flash_spec).await?;
        let mut report = FirmwareFlashReport {
            flashed: !self.dry_run,
            reset: options.reset.then_some(false),
            verified_image: options.verify_image.then_some(false),
            verified_version: options.verify_version.then_some(false),
            observed_version: None,
            expected_version: Some(self.firmware_spec.version.clone()),
        };
        let result = async {
            if options.reset {
                self.reset_with_level(options.reset_level).await?;
                report.reset = Some(!self.dry_run);
            }
            if options.verify_image {
                self.verify_prepared_image(&mut prepared).await?;
                report.verified_image = Some(!self.dry_run);
            }
            if options.verify_version && !self.dry_run {
                let observation = self.observe_version().await?;
                let checked = self.check_version(&observation);
                report.observed_version = Some(observation);
                checked?;
                report.verified_version = Some(true);
            }
            // Keep the staged inputs until the owner has recorded success.
            // Losing that write must leave the same inputs for reconciliation.
            self.record_progress(FirmwareProgressEvent::Complete(report.clone()))
                .await?;
            FirmwareResult::Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                prepared.finish();
                Ok(report)
            }
            Err(source) => Err(FirmwareApplyError {
                report: Some(report),
                source: Box::new(source),
            }),
        }
    }

    async fn flint(&self) -> FirmwareResult<FlintRunner> {
        let program = self.find_tool(&FlintRunner::CANDIDATE_PATHS).await?;
        Ok(FlintRunner::with_path(program))
    }

    async fn find_tool<'a>(&self, candidates: &[&'a str]) -> FirmwareResult<&'a str> {
        for (index, program) in candidates.iter().enumerate() {
            if self.dry_run {
                return Ok(program);
            }
            let command = CommandSpec::new(*program).arg("--version");
            match self
                .execute(&command, FirmwarePhase::ToolDiscovery, QUERY_TIMEOUT)
                .await
            {
                Ok(_) => return Ok(program),
                // A started probe can leave cleanup unresolved. Do not hide
                // that failure by finding another executable and mutating.
                Err(FirmwareError::PhaseFailed {
                    source: AttemptError::NotStarted { source, .. },
                    ..
                }) if source.kind() == std::io::ErrorKind::NotFound
                    && index + 1 < candidates.len() =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Err(FirmwareError::ConfigError(
            "no firmware tool candidates".into(),
        ))
    }

    /// `execute` records real command intent and completion around one bounded
    /// attempt. Discovery probes leave the recorded operation phase unchanged.
    async fn execute(
        &self,
        command: &CommandSpec,
        phase: FirmwarePhase,
        timeout: Duration,
    ) -> FirmwareResult<String> {
        if self.dry_run {
            tracing::info!(%command, "dry run firmware command");
            return Ok(format!("[DRY RUN] {command}"));
        }
        if phase != FirmwarePhase::ToolDiscovery {
            self.record_progress(FirmwareProgressEvent::Starting(phase))
                .await?;
        }
        tracing::info!(device = %self.device_id, ?phase, "starting firmware command");
        let output = self
            .executor
            .execute(command, timeout)
            .await
            .map_err(|source| FirmwareError::PhaseFailed { phase, source })?;
        if !output.status.success() {
            return Err(FirmwareError::PhaseFailed {
                phase,
                source: AttemptError::nonzero(command, output),
            });
        }
        if phase != FirmwarePhase::ToolDiscovery {
            self.record_progress(FirmwareProgressEvent::Completed(phase))
                .await?;
        }
        tracing::info!(device = %self.device_id, ?phase, "firmware command completed");
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `record_progress` gates advancement on the owner's receipt write.
    /// Dry-run never publishes evidence of a real operation.
    async fn record_progress(&self, event: FirmwareProgressEvent) -> FirmwareResult<()> {
        if !self.dry_run
            && let Some(progress) = &self.progress
        {
            progress.record(event).await?;
        }
        Ok(())
    }
}

// Remote sources resolve into this owned directory, while local sources return
// their original path. Copy those local bytes too so later phases use the staged
// input rather than a source the caller could replace between commands.
async fn stage_source(source: &FirmwareSource, directory: &Path) -> FirmwareResult<PathBuf> {
    tokio::fs::create_dir(directory).await?;
    let resolved = source.resolve(directory).await?;
    check_readable_file(&resolved).await?;
    if resolved.parent() == Some(directory) {
        return Ok(resolved);
    }
    let filename = resolved
        .file_name()
        .ok_or_else(|| FirmwareError::ConfigError("firmware source has no filename".into()))?;
    let staged = directory.join(filename);
    tokio::fs::copy(&resolved, &staged).await?;
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc, oneshot};
    use tokio::task::JoinSet;

    use super::*;
    use crate::firmware::config::FlashOptions;

    const DEVICE_ID: &str = "test-device";
    const TARGET_VERSION: &str = "32.43.1014";
    const HTTP_TEST_TIMEOUT: Duration = Duration::from_secs(10);

    /// Keep executable writes outside this multithreaded test process so a
    /// concurrent fork cannot inherit a writable descriptor and cause ETXTBSY.
    #[cfg(target_os = "linux")]
    async fn write_fixture_executables(directory: &Path, programs: &[(&str, &str)]) {
        use std::os::unix::fs::PermissionsExt;

        let mut writer = tokio::process::Command::new("/bin/sh");
        writer
            .args([
                "-c",
                "set -eu; umask 077; while [ \"$#\" -gt 0 ]; do printf '%s' \"$2\" > \"$1\"; shift 2; done",
                "fixture-writer",
            ])
            .kill_on_drop(true);
        for (name, contents) in programs {
            writer.arg(directory.join(name)).arg(contents);
        }
        let status = tokio::time::timeout(Duration::from_secs(10), writer.status())
            .await
            .expect("fixture writer finishes within its watchdog")
            .expect("run isolated fixture writer");
        assert!(status.success(), "fixture writer failed: {status}");
        for (name, _) in programs {
            fs::set_permissions(directory.join(name), fs::Permissions::from_mode(0o700))
                .expect("make fixture executable");
        }
    }

    /// Serve the image immediately, then hold the configuration response until
    /// the test releases it. Cancellation can then leave a staged image without
    /// a configuration write still in flight.
    struct HttpInputs {
        base_url: String,
        configuration_requests: mpsc::Receiver<oneshot::Sender<()>>,
        server: JoinSet<()>,
    }

    impl HttpInputs {
        async fn start(image: Vec<u8>, configuration: Vec<u8>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let (requested, configuration_requests) = mpsc::channel(1);
            // `JoinSet` also aborts the server if an assertion unwinds the test.
            let mut server = JoinSet::new();
            server.spawn(async move {
                loop {
                    tokio::time::timeout(HTTP_TEST_TIMEOUT, async {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut stream = BufReader::new(stream);
                        let mut request = String::new();
                        stream.read_line(&mut request).await.unwrap();
                        let mut header = String::new();
                        loop {
                            header.clear();
                            assert_ne!(stream.read_line(&mut header).await.unwrap(), 0);
                            if header == "\r\n" {
                                break;
                            }
                        }
                        let body = match request.split_whitespace().nth(1) {
                            Some("/image/shared.bin") => &image,
                            Some("/configuration/shared.bin") => {
                                let (release, released) = oneshot::channel();
                                requested.send(release).await.unwrap();
                                released.await.expect("configuration response released");
                                &configuration
                            }
                            _ => panic!("unexpected HTTP request: {request:?}"),
                        };
                        let headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        stream
                            .get_mut()
                            .write_all(headers.as_bytes())
                            .await
                            .unwrap();
                        stream.get_mut().write_all(body).await.unwrap();
                        stream.get_mut().shutdown().await.unwrap();
                    })
                    .await
                    .expect("HTTP request and response complete within the fixture deadline");
                }
            });
            Self {
                base_url,
                configuration_requests,
                server,
            }
        }

        fn spec(&self, cache: &Path) -> FlashSpec {
            FlashSpec {
                firmware_url: format!("{}/image/shared.bin", self.base_url),
                firmware_credentials: None,
                device_conf_url: Some(format!("{}/configuration/shared.bin", self.base_url)),
                device_conf_credentials: None,
                verify_from_cache: false,
                cache_dir: Some(cache.to_path_buf()),
            }
        }

        async fn configuration_requested(&mut self) -> oneshot::Sender<()> {
            tokio::time::timeout(HTTP_TEST_TIMEOUT, self.configuration_requests.recv())
                .await
                .expect("prepare requests its configuration within the fixture deadline")
                .expect("HTTP server remains alive until the configuration request")
        }

        async fn stop(mut self) {
            self.server.abort_all();
            let stopped = tokio::time::timeout(HTTP_TEST_TIMEOUT, self.server.join_next())
                .await
                .expect("HTTP server stops within the fixture deadline")
                .expect("HTTP server task exists")
                .expect_err("HTTP server exits through cancellation");
            assert!(stopped.is_cancelled(), "HTTP server failed: {stopped}");
        }
    }

    /// HTTP inputs with the same basename must keep their full bytes in separate
    /// paths, and another live preparation must not replace or remove either one.
    #[tokio::test]
    async fn prepare_http_inputs_preserves_bytes_and_separates_live_attempts() {
        let fixture = tempfile::tempdir().unwrap();
        let cache = fixture.path().join("staging");
        let image: Vec<u8> = (0..=u8::MAX).cycle().take(64 * 1024 + 17).collect();
        let configuration = b"\xffconfiguration\0\xfe\r\n".to_vec();
        let mut server = HttpInputs::start(image.clone(), configuration.clone()).await;
        let spec = server.spec(&cache);
        let flasher = dry_run_flasher();
        let mut preparations = Vec::new();
        for _ in 0..2 {
            let mut operation = Box::pin(flasher.prepare(&spec));
            let release = tokio::select! {
                result = &mut operation => panic!("prepare finished before the configuration response: {:?}", result.err()),
                release = server.configuration_requested() => release,
            };
            release.send(()).unwrap();
            let prepared = tokio::time::timeout(HTTP_TEST_TIMEOUT, operation)
                .await
                .expect("prepare completes after both HTTP responses")
                .unwrap();
            let directory = prepared.directory.as_ref().unwrap().path();
            assert_eq!(prepared.image, directory.join("image/shared.bin"));
            assert_eq!(
                prepared.device_config.as_ref().unwrap(),
                &directory.join("configuration/shared.bin")
            );
            assert_eq!(fs::read(&prepared.image).unwrap(), image);
            assert_eq!(
                fs::read(prepared.device_config.as_ref().unwrap()).unwrap(),
                configuration
            );
            preparations.push(prepared);
        }
        let second = preparations.pop().unwrap();
        let first = preparations.pop().unwrap();
        let first_directory = first.directory.as_ref().unwrap().path().to_path_buf();
        let second_directory = second.directory.as_ref().unwrap().path().to_path_buf();
        assert_ne!(first_directory, second_directory);
        assert_eq!(first_directory.parent(), Some(cache.as_path()));
        assert_eq!(second_directory.parent(), Some(cache.as_path()));
        assert_eq!(fs::read(&first.image).unwrap(), image);
        assert_eq!(
            fs::read(first.device_config.as_ref().unwrap()).unwrap(),
            configuration
        );
        drop(first);
        assert!(!first_directory.exists());
        assert!(second_directory.is_dir());
        assert_eq!(fs::read(&second.image).unwrap(), image);
        assert_eq!(
            fs::read(second.device_config.as_ref().unwrap()).unwrap(),
            configuration
        );
        drop(second);
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
        server.stop().await;
        fixture.close().unwrap();
    }

    /// A configuration request starts only after the image is staged. Holding
    /// that response lets us cancel `prepare` with exact image bytes on disk,
    /// then prove a retry in the same cache leaves those bytes intact.
    #[tokio::test]
    async fn cancelled_http_prepare_retains_the_staged_image() {
        let fixture = tempfile::tempdir().unwrap();
        let cache = fixture.path().join("staging");
        let image = b"firmware\0\xff\xfebefore cancellation\n".to_vec();
        let mut server = HttpInputs::start(image.clone(), b"configuration".to_vec()).await;
        let spec = server.spec(&cache);
        let flasher = dry_run_flasher();
        let mut operation = Box::pin(flasher.prepare(&spec));
        let release = tokio::select! {
            result = &mut operation => panic!("prepare finished before cancellation: {:?}", result.err()),
            release = server.configuration_requested() => release,
        };
        let staged: Vec<_> = fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(staged.len(), 1);
        let directory = &staged[0];
        let staged_image = directory.join("image/shared.bin");
        assert_eq!(fs::read(&staged_image).unwrap(), image);
        assert!(directory.join("configuration").is_dir());
        assert!(!directory.join("configuration/shared.bin").exists());

        drop(operation);
        assert!(directory.is_dir());
        assert_eq!(fs::read(&staged_image).unwrap(), image);
        assert!(!directory.join("configuration/shared.bin").exists());
        // No configuration bytes were sent, so cancellation leaves no pending
        // filesystem write. Stop the held response before starting the retry.
        server.stop().await;
        drop(release);

        let retry_image = b"retry firmware\0\xff".to_vec();
        let mut server = HttpInputs::start(retry_image.clone(), b"configuration".to_vec()).await;
        let spec = server.spec(&cache);
        let mut retry = Box::pin(flasher.prepare(&spec));
        let release = tokio::select! {
            result = &mut retry => panic!("retry finished before the configuration response: {:?}", result.err()),
            release = server.configuration_requested() => release,
        };
        release.send(()).unwrap();
        let prepared = tokio::time::timeout(HTTP_TEST_TIMEOUT, retry)
            .await
            .expect("retry completes after both HTTP responses")
            .unwrap();
        let retry_directory = prepared.directory.as_ref().unwrap().path().to_path_buf();
        assert_ne!(&retry_directory, directory);
        assert_eq!(fs::read(&prepared.image).unwrap(), retry_image);
        assert_eq!(fs::read(&staged_image).unwrap(), image);
        drop(prepared);
        assert!(!retry_directory.exists());
        assert!(directory.is_dir());
        assert_eq!(fs::read(&staged_image).unwrap(), image);
        server.stop().await;
        fixture.close().unwrap();
    }

    /// Missing probes may fall back; started or permission failures stay visible
    /// instead of authorizing the caller to mutate through another executable.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tool_discovery_only_falls_back_when_a_candidate_is_missing() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let failed = directory.path().join("failed-probe");
        let fallback = directory.path().join("fallback-probe");
        let denied = directory.path().join("denied-probe");
        let missing = directory.path().join("missing-probe");
        write_fixture_executables(
            directory.path(),
            &[
                (
                    "failed-probe",
                    "#!/bin/sh\nprintf 'probe failed' >&2\nexit 23\n",
                ),
                ("denied-probe", "#!/bin/sh\nexit 0\n"),
                (
                    "fallback-probe",
                    "#!/bin/sh\nprintf used > \"${0%/*}/fallback-ran\"\n",
                ),
            ],
        )
        .await;
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o600)).unwrap();

        let mut flasher = dry_run_flasher().with_dry_run(false);
        flasher.executor = crate::runner::test_firmware_executor();
        // Absence is the one proven pre-start condition that permits fallback.
        assert_eq!(
            flasher
                .find_tool(&[missing.to_str().unwrap(), fallback.to_str().unwrap()])
                .await
                .unwrap(),
            fallback.to_str().unwrap()
        );
        let fallback_marker = directory.path().join("fallback-ran");
        assert!(fallback_marker.exists());
        fs::remove_file(&fallback_marker).unwrap();

        for (program, expected_started) in [
            // A launched probe can leave hardware/tool state uncertain.
            (&failed, true),
            // Permission errors identify a broken installation, not absence.
            (&denied, false),
        ] {
            let error = flasher
                .find_tool(&[program.to_str().unwrap(), fallback.to_str().unwrap()])
                .await
                .unwrap_err();
            match error {
                FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::ToolDiscovery,
                    source:
                        AttemptError::Started {
                            status: Some(status),
                            stderr,
                            ..
                        },
                } => {
                    assert!(expected_started);
                    assert_eq!(status.code(), Some(23));
                    assert_eq!(stderr, b"probe failed");
                }
                FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::ToolDiscovery,
                    source: AttemptError::NotStarted { source, .. },
                } => {
                    assert!(!expected_started);
                    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
                }
                error => panic!("unexpected probe error: {error:?}"),
            }
            assert!(!fallback_marker.exists());
        }

        // A missing supervisor is one broken installation, not a series of
        // missing tool candidates. The diagnostic must still name the first one.
        let missing_helper = directory.path().join("missing-supervisor");
        flasher.executor = FirmwareToolExecutor::with_supervisor_path(&missing_helper);
        let error = flasher
            .find_tool(&[missing.to_str().unwrap(), fallback.to_str().unwrap()])
            .await
            .unwrap_err();
        assert!(matches!(error, FirmwareError::PhaseFailed {
            phase: FirmwarePhase::ToolDiscovery,
            source: AttemptError::SupervisorNotStarted { command, helper, source },
        } if command == CommandSpec::new(missing.to_str().unwrap()).arg("--version").to_string()
            && helper == missing_helper
            && source.kind() == std::io::ErrorKind::NotFound));
        assert!(!fallback_marker.exists());

        // A supervisor can report a started probe whose cleanup is unresolved.
        // Even a working second candidate must not hide that first failure.
        let response = serde_json::json!({
            "Failed": {
                "status": null,
                "stdout": [],
                "stderr": [],
                "reason": "Deadline",
                "cleanup_errors": [{
                    "errno": null,
                    "kind": "Other",
                    "message": "fixture cleanup unresolved",
                }],
            },
        })
        .to_string();
        let helper = format!(
            r#"#!/usr/bin/python3
import json, pathlib, struct, sys
sys.stdout.buffer.write(b"MLX-SUPERVISOR-1\n")
sys.stdout.buffer.flush()
length = struct.unpack(">I", sys.stdin.buffer.read(4))[0]
request = json.loads(sys.stdin.buffer.read(length))
if request["program"] == {fallback:?}:
    pathlib.Path({marker:?}).write_text("used")
    payload = b'{{"Finished":{{"status":0,"stdout":[],"stderr":[]}}}}'
else:
    payload = {response:?}.encode()
sys.stdout.buffer.write(struct.pack(">I", len(payload)) + payload)
sys.stdout.buffer.flush()
"#,
            fallback = fallback.to_str().unwrap(),
            marker = fallback_marker.to_str().unwrap(),
        );
        write_fixture_executables(directory.path(), &[("uncertain-supervisor", &helper)]).await;
        flasher.executor = FirmwareToolExecutor::with_supervisor_path(
            directory.path().join("uncertain-supervisor"),
        );
        let error = tokio::time::timeout(
            HTTP_TEST_TIMEOUT,
            flasher.find_tool(&[failed.to_str().unwrap(), fallback.to_str().unwrap()]),
        )
        .await
        .expect("probe uncertainty returns within the fixture watchdog")
        .unwrap_err();
        assert!(matches!(error, FirmwareError::PhaseFailed {
            phase: FirmwarePhase::ToolDiscovery,
            source: AttemptError::Started { reason, cleanup_errors, .. },
        } if matches!(reason, crate::runner::attempt::AttemptFailure::Deadline(_))
            && cleanup_errors.iter().any(|error| error.to_string() == "fixture cleanup unresolved")));
        assert!(!fallback_marker.exists());
    }

    /// The production callers must dispatch their phase-specific deadlines,
    /// rather than sharing one timer because the executor accepts a duration.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn firmware_phases_forward_their_deadlines_to_the_supervisor() {
        use crate::runner::{HELLO, Request, Response};

        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path();
        let image = fixture.join("image.bin");
        let configuration = fixture.join("configuration.bin");
        fs::write(&image, b"firmware image").unwrap();
        fs::write(&configuration, b"device configuration").unwrap();

        // This helper records actual IPC requests but never launches the named
        // tools. A valid query response lets the real apply path reach every phase.
        let response = serde_json::to_string(&Response::Finished {
            status: 0,
            stdout: format!(
                "<Devices><Device pciName=\"{DEVICE_ID}\" type=\"BlueField3\" \
                 partNumber=\"test-part-number\" psid=\"test-psid\"><Versions>\
                 <FW current=\"{TARGET_VERSION}\"/></Versions></Device></Devices>"
            )
            .into_bytes(),
            stderr: Vec::new(),
        })
        .unwrap();
        let response_length = u32::try_from(response.len()).unwrap();
        let frame_prefix: String = response_length
            .to_be_bytes()
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect();
        let helper = format!(
            r#"#!/bin/sh
set -eu
printf '%s' '{hello}'
set -- $(/usr/bin/od -An -N4 -tu1)
length=$((($1 << 24) | ($2 << 16) | ($3 << 8) | $4))
/bin/dd bs=1 count="$length" status=none >> "$0.requests"
printf '\n' >> "$0.requests"
printf '{frame_prefix}%s' '{response}'
"#,
            hello = std::str::from_utf8(HELLO).unwrap(),
        );
        write_fixture_executables(fixture, &[("recording-supervisor", &helper)]).await;
        let executor =
            FirmwareToolExecutor::with_supervisor_path(fixture.join("recording-supervisor"));
        let profile = profile(
            &image,
            &fixture.join("staging"),
            Some(&configuration),
            FlashOptions {
                reset: true,
                reset_level: 1,
                verify_image: true,
                verify_version: true,
            },
        );
        let started = std::time::Instant::now();
        let report = tokio::time::timeout(Duration::from_secs(10), async {
            let flasher = FirmwareFlasher::new_with_executor(DEVICE_ID, &firmware_spec(), executor)
                .await
                .unwrap();
            flasher.apply(&profile).await.unwrap()
        })
        .await
        .expect("recording helper finishes independently of production deadlines");
        let elapsed = started.elapsed();
        assert!(report.flashed);
        assert_eq!(report.verified_version, Some(true));

        struct ExpectedRequest {
            phase: &'static str,
            program: &'static str,
            argument: &'static str,
            timeout_seconds: u64,
        }
        let expected = [
            // Admission must not inherit a mutation's longer deadline.
            ExpectedRequest {
                phase: "initial query",
                program: "mlxfwmanager",
                argument: "xml",
                timeout_seconds: 30,
            },
            // Probes remain short even when discovering a long-running tool.
            ExpectedRequest {
                phase: "burn tool discovery",
                program: "flint",
                argument: "--version",
                timeout_seconds: 30,
            },
            // Configuration has its own short limit despite mutating hardware.
            ExpectedRequest {
                phase: "configuration",
                program: "mlxconfig",
                argument: "apply",
                timeout_seconds: 30,
            },
            // The burn must receive the full firmware-command budget.
            ExpectedRequest {
                phase: "burn",
                program: "flint",
                argument: "burn",
                timeout_seconds: 1800,
            },
            // Finding the activation tool must not start its reset timer early.
            ExpectedRequest {
                phase: "reset tool discovery",
                program: "mlxfwreset",
                argument: "--version",
                timeout_seconds: 30,
            },
            // Activation uses the long limit at its own execution boundary.
            ExpectedRequest {
                phase: "activation",
                program: "mlxfwreset",
                argument: "reset",
                timeout_seconds: 1800,
            },
            // Verification rediscovery remains a short independent probe.
            ExpectedRequest {
                phase: "verification tool discovery",
                program: "flint",
                argument: "--version",
                timeout_seconds: 30,
            },
            // Image comparison gets the long limit even though it is read-only.
            ExpectedRequest {
                phase: "image verification",
                program: "flint",
                argument: "verify",
                timeout_seconds: 1800,
            },
            // The final observation must return to the query deadline.
            ExpectedRequest {
                phase: "final query",
                program: "mlxfwmanager",
                argument: "xml",
                timeout_seconds: 30,
            },
        ];
        let requests: Vec<Request> =
            fs::read_to_string(fixture.join("recording-supervisor.requests"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        assert_eq!(requests.len(), expected.len());
        for (request, expected) in requests.iter().zip(expected) {
            assert_eq!(request.program, expected.program, "{}", expected.phase);
            assert!(
                request.args.iter().any(|arg| arg == expected.argument),
                "{}",
                expected.phase
            );
            // Helper startup consumes part of the caller's budget. The whole
            // test's elapsed time bounds that deduction without sleeping for it.
            let budget = Duration::from_secs(expected.timeout_seconds);
            assert!(
                request.timeout <= budget && request.timeout >= budget.saturating_sub(elapsed),
                "{}: forwarded {:?}, budget {budget:?}, total elapsed {elapsed:?}",
                expected.phase,
                request.timeout,
            );
        }
    }

    /// Exercise actual return values and staged-file ownership without changing
    /// this test process's PATH while other firmware tests run concurrently.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn prepared_firmware_tracks_mutation_and_verification_results() {
        use tokio::process::Command;

        for scenario in [
            // One mismatched identity proves constructor validation precedes
            // staging; the pure validator covers the individual identity fields.
            "incompatible_psid",
            // Success must report completed work, not just a successful exit.
            "complete",
            // A failed starting record must prevent the first mutation.
            "progress_before_configuration",
            // A failed completion record must prevent the next mutation.
            "progress_after_configuration",
            // Failed terminal persistence must keep the exact burned inputs.
            "progress_terminal",
            // Persist the observed version even when it fails verification.
            "progress_version_mismatch",
            // Successful configuration still matters when the burn cannot start.
            "after_configuration",
            // A later pre-start failure cannot erase an earlier uncertain attempt.
            "prior_uncertain",
            // A lost helper reply cannot turn a completed configuration into no work.
            "lost_helper_result",
            // Standalone read failures release files once processes have settled.
            "verification_failure",
            // Read-only work still needs its inputs if cleanup is unconfirmed.
            "lost_verification_helper",
            // The public wrapper must use a cache hit even with no source file.
            "cached_verification",
            // Public phase composition retains inputs after failed activation.
            "manual_reset_failure",
            // Dropping the composed future must retain files after a real burn.
            "manual_reset_cancel",
            // Explicit terminal success releases a manually composed preparation.
            "manual_complete",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let fixture = directory.path();
            let tools = fixture.join("tools");
            fs::create_dir(&tools).unwrap();
            let tool = r#"#!/bin/sh
set -eu
if [ "${1-}" = --version ]; then exit 0; fi
case "${0##*/}" in
    mlxfwmanager)
        psid=test-psid
        version=32.43.1014
        if [ "$SCENARIO" = progress_version_mismatch ] && [ -f "$FIXTURE/burned" ]; then
            version=32.43.1015
        fi
        if [ "$SCENARIO" = incompatible_psid ]; then psid=other-psid; fi
        printf '<Devices><Device pciName="test-device" type="BlueField3" partNumber="test-part-number" psid="%s"><Versions><FW current="%s"/></Versions></Device></Devices>' "$psid" "$version" ;;
    mlxconfig)
        if [ "$SCENARIO" = prior_uncertain ]; then exit 41; fi
        printf applied > "$FIXTURE/configuration-applied"
        if [ "$SCENARIO" = lost_helper_result ]; then
            # The parent is the command supervisor. Lose its result only after
            # recording that the device operation has already run.
            kill -KILL "$PPID"
        fi
        if [ "$SCENARIO" = after_configuration ]; then
            /bin/mv "$FIXTURE/tools/flint" "$FIXTURE/removed-flint"
        fi ;;
    flint)
        for phase in "$@"; do :; done
        if [ "$phase" = burn ]; then printf burned > "$FIXTURE/burned"; fi
        if [ "$phase" = verify ]; then
            if [ "$SCENARIO" = verification_failure ]; then exit 41; fi
            if [ "$SCENARIO" = lost_verification_helper ]; then
                printf verified > "$FIXTURE/verification-ran"
                kill -KILL "$PPID"
            fi
            IFS= read -r image < "$4"
            printf '%s' "$image"
        fi ;;
    mlxfwreset)
        if [ "$SCENARIO" = manual_reset_failure ]; then exit 41; fi
        if [ "$SCENARIO" = manual_reset_cancel ]; then
            printf '%s' "$$" > "$FIXTURE/reset-started"
            exec /bin/sleep 30
        fi ;;
    *) exit 42 ;;
esac
"#;
            write_fixture_executables(
                &tools,
                &[
                    ("mlxfwmanager", tool),
                    ("flint", tool),
                    ("mlxfwreset", tool),
                    ("mlxconfig", tool),
                ],
            )
            .await;
            // File-backed output does not wait for EOF from a stray descendant.
            // The parent watchdog does not depend on the supervisor under test.
            let stdout = fixture.join("stdout");
            let stderr = fixture.join("stderr");
            let mut child = Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .env("PATH", tools)
                .env("FIXTURE", fixture)
                .env("SCENARIO", scenario)
                .args([
                    "--exact",
                    "firmware::flasher::tests::prepared_firmware_subprocess",
                    "--ignored",
                    "--nocapture",
                ])
                .stdout(fs::File::create(&stdout).unwrap())
                .stderr(fs::File::create(&stderr).unwrap())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let status = match tokio::time::timeout(Duration::from_secs(20), child.wait()).await {
                Ok(Ok(status)) => status,
                failed_wait => {
                    // Preserve PATH and inputs even if independent helper cleanup
                    // continues after the embedding process has been stopped.
                    let retained = directory.keep();
                    let stopped = child.kill().await;
                    panic!(
                        "{scenario}: fixture did not finish: {failed_wait:?}; child cleanup: {stopped:?}; retained {}",
                        retained.display()
                    );
                }
            };
            assert!(
                status.success(),
                "{scenario}: stdout={} stderr={}",
                String::from_utf8_lossy(&fs::read(stdout).unwrap()),
                String::from_utf8_lossy(&fs::read(stderr).unwrap())
            );
            // A misspelled exact filter must not pass with zero tests executed.
            assert!(fixture.join("checked").exists(), "{scenario}");
        }
    }

    /// Runs only in the isolated fake-tool process created by the parent test.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "launched by prepared_firmware_tracks_mutation_and_verification_results"]
    async fn prepared_firmware_subprocess() {
        #[derive(Debug, PartialEq)]
        enum Checkpoint {
            Prepare,
            Prepared,
            Starting(FirmwarePhase),
            Completed(FirmwarePhase),
            ObservingVersion,
            ObservedVersion(String),
            Complete,
        }

        /// `ProgressFixture` checks the files visible at each boundary and can
        /// reject one write to prove that no subsequent command starts.
        struct ProgressFixture {
            fixture: PathBuf,
            reject: Option<Checkpoint>,
            recorded: std::sync::Mutex<Vec<Checkpoint>>,
        }

        #[async_trait::async_trait]
        impl FirmwareProgress for ProgressFixture {
            /// `record` checks boundary files before rejecting the configured
            /// checkpoint. The returned error proves that checkpoint was reached;
            /// only successful records enter `recorded`.
            async fn record(&self, event: FirmwareProgressEvent) -> FirmwareResult<()> {
                let checkpoint = match event {
                    FirmwareProgressEvent::Prepare => {
                        assert!(!self.fixture.join("staging").exists());
                        Checkpoint::Prepare
                    }
                    FirmwareProgressEvent::Prepared {
                        directory,
                        image,
                        configuration,
                    } => {
                        assert_eq!(image, directory.join("image/firmware.bin"));
                        assert_eq!(fs::read(image).unwrap(), b"source image\n");
                        assert_eq!(
                            configuration,
                            Some(directory.join("configuration/configuration.bin"))
                        );
                        assert_eq!(
                            fs::read(configuration.unwrap()).unwrap(),
                            b"configuration\n"
                        );
                        Checkpoint::Prepared
                    }
                    FirmwareProgressEvent::Starting(phase) => {
                        match phase {
                            FirmwarePhase::Configuration => {
                                assert!(!self.fixture.join("configuration-applied").exists());
                            }
                            FirmwarePhase::Burn => {
                                assert!(!self.fixture.join("burned").exists());
                            }
                            _ => {}
                        }
                        Checkpoint::Starting(phase)
                    }
                    FirmwareProgressEvent::Completed(phase) => {
                        match phase {
                            FirmwarePhase::Configuration => {
                                assert!(self.fixture.join("configuration-applied").exists());
                            }
                            FirmwarePhase::Burn => assert!(self.fixture.join("burned").exists()),
                            _ => {}
                        }
                        Checkpoint::Completed(phase)
                    }
                    FirmwareProgressEvent::ObservingVersion => Checkpoint::ObservingVersion,
                    FirmwareProgressEvent::ObservedVersion(version) => {
                        Checkpoint::ObservedVersion(version.to_string())
                    }
                    FirmwareProgressEvent::Complete(report) => {
                        assert!(report.flashed);
                        assert_eq!(
                            fs::read_dir(self.fixture.join("staging")).unwrap().count(),
                            1
                        );
                        Checkpoint::Complete
                    }
                };
                if self.reject.as_ref() == Some(&checkpoint) {
                    return Err(FirmwareError::ProgressPersistence(std::io::Error::other(
                        "fixture progress failure",
                    )));
                }
                self.recorded.lock().unwrap().push(checkpoint);
                Ok(())
            }
        }

        let fixture = PathBuf::from(std::env::var_os("FIXTURE").unwrap());
        let scenario = std::env::var("SCENARIO").unwrap();
        let image = fixture.join("firmware.bin");
        let configuration = fixture.join("configuration.bin");
        let cache = fixture.join("staging");
        fs::write(&image, b"source image\n").unwrap();
        fs::write(&configuration, b"configuration\n").unwrap();
        let flasher = FirmwareFlasher::new_with_executor(
            DEVICE_ID,
            &firmware_spec(),
            crate::runner::test_firmware_executor(),
        )
        .await;
        if scenario == "incompatible_psid" {
            assert!(matches!(flasher, Err(FirmwareError::Inspection(
                FirmwarePreflightError::IdentityMismatch {
                    field, expected, observed,
                }
            )) if field == "PSID" && expected == "test-psid"
                && observed.as_deref() == Some("other-psid")));
            assert!(!cache.exists());
            assert!(!fixture.join("burned").exists());
            fs::write(fixture.join("checked"), b"checked").unwrap();
            return;
        }
        let flasher = flasher.unwrap();
        let progress = Arc::new(ProgressFixture {
            fixture: fixture.clone(),
            reject: match scenario.as_str() {
                "progress_before_configuration" => {
                    Some(Checkpoint::Starting(FirmwarePhase::Configuration))
                }
                "progress_after_configuration" => {
                    Some(Checkpoint::Completed(FirmwarePhase::Configuration))
                }
                "progress_terminal" => Some(Checkpoint::Complete),
                _ => None,
            },
            recorded: std::sync::Mutex::new(Vec::new()),
        });
        let flasher = if scenario == "complete" || scenario.starts_with("progress_") {
            flasher.with_progress(progress.clone())
        } else {
            flasher
        };
        let mut spec = flash_spec(&image, &cache, Some(&configuration), false);
        let retain = match scenario.as_str() {
            "complete"
            | "progress_before_configuration"
            | "progress_after_configuration"
            | "progress_terminal"
            | "progress_version_mismatch" => {
                let profile = FirmwareFlasherProfile {
                    firmware_spec: firmware_spec(),
                    flash_spec: spec,
                    flash_options: FlashOptions {
                        reset: true,
                        verify_image: true,
                        verify_version: true,
                        reset_level: 7,
                    },
                };
                let result = flasher.apply(&profile).await;
                if scenario == "complete" {
                    assert_eq!(
                        Report::from(result.unwrap()),
                        Report {
                            flashed: true,
                            reset: Some(true),
                            verified_image: Some(true),
                            verified_version: Some(true),
                            observed_version: Some(TARGET_VERSION.into()),
                            expected_version: Some(TARGET_VERSION.into()),
                        }
                    );
                    assert_eq!(
                        *progress.recorded.lock().unwrap(),
                        [
                            Checkpoint::Prepare,
                            Checkpoint::Prepared,
                            Checkpoint::Starting(FirmwarePhase::Configuration),
                            Checkpoint::Completed(FirmwarePhase::Configuration),
                            Checkpoint::Starting(FirmwarePhase::Burn),
                            Checkpoint::Completed(FirmwarePhase::Burn),
                            Checkpoint::Starting(FirmwarePhase::Activation),
                            Checkpoint::Completed(FirmwarePhase::Activation),
                            Checkpoint::Starting(FirmwarePhase::ImageVerification),
                            Checkpoint::Completed(FirmwarePhase::ImageVerification),
                            Checkpoint::ObservingVersion,
                            Checkpoint::ObservedVersion(TARGET_VERSION.into()),
                            Checkpoint::Complete,
                        ]
                    );
                    // This positive control proves the marker used by rejected
                    // scenarios is actually written when burn is reached.
                    assert_eq!(fs::read(fixture.join("burned")).unwrap(), b"burned");
                    false
                } else {
                    let error = result.unwrap_err();
                    if scenario == "progress_version_mismatch" {
                        assert!(matches!(
                            *error.source,
                            FirmwareError::VerificationFailed(_)
                        ));
                        assert_eq!(
                            error.report.unwrap().observed_version.as_deref(),
                            Some("32.43.1015")
                        );
                        assert_eq!(
                            progress.recorded.lock().unwrap().last(),
                            Some(&Checkpoint::ObservedVersion("32.43.1015".into()))
                        );
                    } else {
                        assert!(matches!(
                            *error.source,
                            FirmwareError::ProgressPersistence(_)
                        ));
                        let completed = scenario == "progress_terminal";
                        assert_eq!(error.report.is_some(), completed);
                        assert_eq!(fixture.join("burned").exists(), completed);
                        assert_eq!(
                            fixture.join("configuration-applied").exists(),
                            scenario != "progress_before_configuration"
                        );
                        assert!(
                            !progress
                                .recorded
                                .lock()
                                .unwrap()
                                .contains(&Checkpoint::Complete)
                        );
                    }
                    true
                }
            }
            "verification_failure" => {
                let error = flasher.verify_image(&spec).await.unwrap_err();
                assert!(matches!(error, FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::ImageVerification,
                    source: AttemptError::Started { status: Some(status), cleanup_errors, .. },
                } if status.code() == Some(41) && cleanup_errors.is_empty()));
                false
            }
            "lost_verification_helper" => {
                let error = flasher.verify_image(&spec).await.unwrap_err();
                assert!(matches!(error, FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::ImageVerification,
                    source: AttemptError::Started { cleanup_errors, .. },
                } if !cleanup_errors.is_empty()));
                assert_eq!(
                    fs::read(fixture.join("verification-ran")).unwrap(),
                    b"verified"
                );
                // A standalone verification stages only the image; unresolved
                // cleanup must retain that exact input despite being read-only.
                let staged: Vec<_> = fs::read_dir(&cache)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                assert_eq!(staged.len(), 1);
                assert_eq!(
                    fs::read(staged[0].join("image/firmware.bin")).unwrap(),
                    b"source image\n"
                );
                assert!(!staged[0].join("configuration").exists());
                fs::write(fixture.join("checked"), b"checked").unwrap();
                return;
            }
            "manual_reset_failure" | "manual_complete" => {
                let mut prepared = flasher.prepare(&spec).await.unwrap();
                flasher.burn(&mut prepared).await.unwrap();
                assert_eq!(fs::read(fixture.join("burned")).unwrap(), b"burned");
                let reset = flasher.reset().await;
                if scenario == "manual_reset_failure" {
                    assert!(matches!(reset, Err(FirmwareError::PhaseFailed {
                        phase: FirmwarePhase::Activation,
                        source: AttemptError::Started { status: Some(status), .. },
                    }) if status.code() == Some(41)));
                    drop(prepared);
                    true
                } else {
                    reset.unwrap();
                    prepared.finish();
                    false
                }
            }
            "manual_reset_cancel" => {
                let reset_started = fixture.join("reset-started");
                // The operation owns the preparation; cancellation must drop it
                // after burn without the caller having to intercept the reset.
                let mut operation = Box::pin(async {
                    let mut prepared = flasher.prepare(&spec).await.unwrap();
                    flasher.burn(&mut prepared).await.unwrap();
                    flasher.reset().await.unwrap();
                    prepared.finish();
                });
                let reset_pid = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if let Ok(pid) = fs::read_to_string(&reset_started)
                            && !pid.is_empty()
                        {
                            break pid;
                        }
                        tokio::select! {
                            () = &mut operation => panic!("reset finished before cancellation"),
                            () = tokio::time::sleep(Duration::from_millis(10)) => {},
                        }
                    }
                })
                .await
                .expect("reset publishes its started handshake");
                assert_eq!(fs::read(fixture.join("burned")).unwrap(), b"burned");
                drop(operation);
                // Keep fake tools and inputs alive while the independent helper
                // observes EOF, stops the reset, and reaps its original PID.
                let process = Path::new("/proc").join(reset_pid);
                tokio::time::timeout(Duration::from_secs(10), async {
                    while process.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("cancelled reset was reaped");
                true
            }
            "cached_verification" => {
                fs::create_dir(&cache).unwrap();
                fs::write(cache.join("firmware.bin"), b"cached image\n").unwrap();
                spec.firmware_url = fixture
                    .join("missing/firmware.bin")
                    .to_string_lossy()
                    .into_owned();
                spec.verify_from_cache = true;
                assert_eq!(flasher.verify_image(&spec).await.unwrap(), "cached image");
                // The caller-owned legacy cache entry is not attempt staging.
                assert_eq!(
                    fs::read(cache.join("firmware.bin")).unwrap(),
                    b"cached image\n"
                );
                false
            }
            "lost_helper_result" => {
                let error = flasher
                    .apply(&FirmwareFlasherProfile {
                        firmware_spec: firmware_spec(),
                        flash_spec: spec,
                        flash_options: FlashOptions::default(),
                    })
                    .await
                    .unwrap_err();
                assert!(error.report.is_none());
                assert!(matches!(*error.source, FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::Configuration,
                    source: AttemptError::Started { cleanup_errors, .. },
                } if !cleanup_errors.is_empty()));
                // The fake completed its mutation before losing the reply. Keep
                // both inputs and do not begin burn or claim no mutation occurred.
                assert_eq!(
                    fs::read(fixture.join("configuration-applied")).unwrap(),
                    b"applied"
                );
                assert!(!fixture.join("burned").exists());
                true
            }
            "after_configuration" | "prior_uncertain" => {
                let mut prepared = flasher.prepare(&spec).await.unwrap();
                if scenario == "prior_uncertain" {
                    let error = flasher.burn(&mut prepared).await.unwrap_err();
                    assert!(matches!(error, FirmwareError::PhaseFailed {
                        phase: FirmwarePhase::Configuration,
                        source: AttemptError::Started { status: Some(status), .. },
                    } if status.code() == Some(41)));
                    assert!(
                        !fixture.join("burned").exists(),
                        "a failed configuration must stop before burn"
                    );
                    // A caller can reuse this public handle. Even a later
                    // NotStarted must keep the first attempt's exact inputs.
                    fs::remove_file(fixture.join("tools/mlxconfig")).unwrap();
                }
                let error = flasher.burn(&mut prepared).await.unwrap_err();
                let expected_phase = if scenario == "after_configuration" {
                    FirmwarePhase::Burn
                } else {
                    FirmwarePhase::Configuration
                };
                assert!(matches!(error, FirmwareError::PhaseFailed {
                    phase, source: AttemptError::NotStarted { source, .. },
                } if phase == expected_phase
                    && source.kind() == std::io::ErrorKind::NotFound));
                drop(prepared);
                true
            }
            _ => panic!("unknown subprocess scenario: {scenario}"),
        };
        let staged: Vec<_> = fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        if retain {
            assert_eq!(staged.len(), 1);
            assert_eq!(
                fs::read(staged[0].join("image/firmware.bin")).unwrap(),
                b"source image\n"
            );
            assert_eq!(
                fs::read(staged[0].join("configuration/configuration.bin")).unwrap(),
                b"configuration\n"
            );
        } else {
            assert!(staged.is_empty(), "{scenario}: {staged:?}");
        }
        fs::write(fixture.join("checked"), b"checked").unwrap();
    }

    #[derive(Debug, PartialEq)]
    struct Report {
        flashed: bool,
        reset: Option<bool>,
        verified_image: Option<bool>,
        verified_version: Option<bool>,
        observed_version: Option<String>,
        expected_version: Option<String>,
    }

    impl From<FirmwareFlashReport> for Report {
        fn from(report: FirmwareFlashReport) -> Self {
            Self {
                flashed: report.flashed,
                reset: report.reset,
                verified_image: report.verified_image,
                verified_version: report.verified_version,
                observed_version: report.observed_version,
                expected_version: report.expected_version,
            }
        }
    }

    #[derive(Debug, PartialEq)]
    enum ApplyError {
        Missing(PathBuf),
        Unexpected(String),
    }

    fn firmware_spec() -> FirmwareSpec {
        FirmwareSpec {
            part_number: "test-part-number".to_string(),
            psid: "test-psid".to_string(),
            version: TARGET_VERSION.to_string(),
        }
    }

    fn dry_run_flasher() -> FirmwareFlasher {
        FirmwareFlasher {
            device_id: DEVICE_ID.to_string(),
            firmware_spec: firmware_spec(),
            dry_run: false,
            executor: FirmwareToolExecutor::default(),
            progress: None,
            preparation_cancel: None,
        }
        .with_dry_run(true)
    }

    fn flash_spec(
        firmware: &Path,
        cache_dir: &Path,
        device_conf: Option<&Path>,
        verify_from_cache: bool,
    ) -> FlashSpec {
        FlashSpec {
            firmware_url: firmware.to_string_lossy().into_owned(),
            firmware_credentials: None,
            device_conf_url: device_conf.map(|path| path.to_string_lossy().into_owned()),
            device_conf_credentials: None,
            verify_from_cache,
            cache_dir: Some(cache_dir.to_path_buf()),
        }
    }

    fn profile(
        firmware: &Path,
        cache_dir: &Path,
        device_conf: Option<&Path>,
        flash_options: FlashOptions,
    ) -> FirmwareFlasherProfile {
        FirmwareFlasherProfile {
            firmware_spec: firmware_spec(),
            flash_spec: flash_spec(firmware, cache_dir, device_conf, false),
            flash_options,
        }
    }

    fn expected_report(reset: bool, verify_image: bool, verify_version: bool) -> Report {
        Report {
            flashed: false,
            reset: reset.then_some(false),
            verified_image: verify_image.then_some(false),
            verified_version: verify_version.then_some(false),
            observed_version: None,
            expected_version: Some(TARGET_VERSION.to_string()),
        }
    }

    fn apply_error(error: FirmwareApplyError) -> ApplyError {
        match *error.source {
            FirmwareError::FileNotFound(path) => ApplyError::Missing(path),
            error => ApplyError::Unexpected(format!("{error:?}")),
        }
    }

    /// Dry-run preserves requested phases without claiming mutation or a live
    /// version observation, while input preparation still rejects missing files.
    #[tokio::test]
    async fn dry_run_apply_reports_requested_lifecycle_steps() {
        let temp_dir = tempfile::tempdir().unwrap();
        let firmware = temp_dir.path().join("firmware.bin");
        let device_conf = temp_dir.path().join("device.conf");
        let missing_firmware = temp_dir.path().join("missing.bin");
        fs::write(&firmware, b"firmware").unwrap();
        fs::write(&device_conf, b"config").unwrap();

        let flasher = dry_run_flasher();
        check_cases_async(
            [
                Case {
                    scenario: "flash only",
                    input: profile(
                        &firmware,
                        &temp_dir.path().join("flash-only-cache"),
                        None,
                        FlashOptions::default(),
                    ),
                    expect: Yields(expected_report(false, false, false)),
                },
                Case {
                    scenario: "reset only",
                    input: profile(
                        &firmware,
                        &temp_dir.path().join("reset-cache"),
                        None,
                        FlashOptions {
                            reset: true,
                            ..Default::default()
                        },
                    ),
                    expect: Yields(expected_report(true, false, false)),
                },
                Case {
                    scenario: "image verification only",
                    input: profile(
                        &firmware,
                        &temp_dir.path().join("image-cache"),
                        None,
                        FlashOptions {
                            verify_image: true,
                            ..Default::default()
                        },
                    ),
                    expect: Yields(expected_report(false, true, false)),
                },
                Case {
                    scenario: "version verification only",
                    input: profile(
                        &firmware,
                        &temp_dir.path().join("version-cache"),
                        None,
                        FlashOptions {
                            verify_version: true,
                            ..Default::default()
                        },
                    ),
                    expect: Yields(expected_report(false, false, true)),
                },
                Case {
                    scenario: "every optional step with a device config",
                    input: profile(
                        &firmware,
                        &temp_dir.path().join("all-cache"),
                        Some(&device_conf),
                        FlashOptions {
                            verify_image: true,
                            verify_version: true,
                            reset: true,
                            reset_level: 7,
                        },
                    ),
                    expect: Yields(expected_report(true, true, true)),
                },
                Case {
                    scenario: "missing firmware fails before reporting",
                    input: profile(
                        &missing_firmware,
                        &temp_dir.path().join("missing-cache"),
                        None,
                        FlashOptions::default(),
                    ),
                    expect: FailsWith(ApplyError::Missing(missing_firmware)),
                },
            ],
            |profile| {
                let flasher = &flasher;
                async move {
                    flasher
                        .apply(&profile)
                        .await
                        .map(Report::from)
                        .map_err(apply_error)
                }
            },
        )
        .await;
    }

    /// Cache preference changes which bytes are staged, without making an absent
    /// cache entry prevent an otherwise valid source lookup.
    #[tokio::test]
    async fn dry_run_verify_image_selects_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let source_dir = temp_dir.path().join("source");
        let firmware = source_dir.join("firmware.bin");
        let missing_firmware = temp_dir.path().join("missing-source/firmware.bin");
        fs::create_dir(&source_dir).unwrap();
        fs::write(&firmware, b"source").unwrap();

        let disabled_cache = temp_dir.path().join("disabled-cache");
        let hit_cache = temp_dir.path().join("hit-cache");
        let miss_cache = temp_dir.path().join("miss-cache");
        fs::create_dir(&disabled_cache).unwrap();
        fs::create_dir(&hit_cache).unwrap();
        let disabled_cached_firmware = disabled_cache.join("firmware.bin");
        let hit_cached_firmware = hit_cache.join("firmware.bin");
        fs::write(&disabled_cached_firmware, b"cached").unwrap();
        fs::write(&hit_cached_firmware, b"cached").unwrap();

        let flasher = dry_run_flasher();
        check_cases_async(
            [
                Case {
                    scenario: "disabled cache selects the local source",
                    input: flash_spec(&firmware, &disabled_cache, None, false),
                    expect: Yields(b"source".to_vec()),
                },
                Case {
                    scenario: "enabled cache hit does not resolve the missing source",
                    input: flash_spec(&missing_firmware, &hit_cache, None, true),
                    expect: Yields(b"cached".to_vec()),
                },
                Case {
                    scenario: "enabled cache miss falls back to the local source",
                    input: flash_spec(&firmware, &miss_cache, None, true),
                    expect: Yields(b"source".to_vec()),
                },
            ],
            |spec| {
                let flasher = &flasher;
                async move {
                    let prepared = flasher
                        .prepare_verification(&spec)
                        .await
                        .map_err(|error| error.to_string())?;
                    fs::read(&prepared.image).map_err(|error| error.to_string())
                }
            },
        )
        .await;
    }

    /// A dry-run must not invent observed firmware from the requested target.
    #[tokio::test]
    async fn dry_run_verify_version_has_no_observation() {
        assert_eq!(dry_run_flasher().verify_version().await.unwrap(), None);
    }

    enum ResetRequest {
        Default,
        Level(u8),
    }

    fn reset_level(output: String) -> Result<u8, String> {
        let mut words = output.split_whitespace();
        while let Some(word) = words.next() {
            if word == "--level" {
                return words
                    .next()
                    .ok_or_else(|| format!("missing reset level in {output:?}"))?
                    .parse()
                    .map_err(|error| format!("invalid reset level in {output:?}: {error}"));
            }
        }
        Err(format!("missing --level argument in {output:?}"))
    }

    /// Reset levels remain caller-selected tool arguments, not a new library
    /// hardware-compatibility policy.
    #[tokio::test]
    async fn dry_run_reset_forwards_level() {
        let flasher = dry_run_flasher();
        check_cases_async(
            [
                Case {
                    scenario: "reset uses the default level",
                    input: ResetRequest::Default,
                    expect: Yields(DEFAULT_RESET_LEVEL),
                },
                Case {
                    scenario: "explicit level accepts zero",
                    input: ResetRequest::Level(0),
                    expect: Yields(0),
                },
                Case {
                    scenario: "explicit level accepts u8 max",
                    input: ResetRequest::Level(u8::MAX),
                    expect: Yields(u8::MAX),
                },
            ],
            |request| {
                let flasher = &flasher;
                async move {
                    let output = match request {
                        ResetRequest::Default => flasher.reset().await,
                        ResetRequest::Level(level) => flasher.reset_with_level(level).await,
                    };
                    output
                        .map_err(|error| error.to_string())
                        .and_then(reset_level)
                }
            },
        )
        .await;
    }
}
