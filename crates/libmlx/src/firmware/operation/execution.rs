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

//! Persisted execution boundaries around the existing firmware apply workflow.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use carbide_libmlx_model::firmware::operation::{FirmwareOperationPhase, FirmwareOperationState};
use nix::libc;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::journal::{
    JournalError, JournalRecord, OperationJournal, OperationUpdate, PreparedArtifact,
    PreparedInputs,
};
use crate::device::discovery::DeviceDiscoveryError;
use crate::firmware::config::{FirmwareFlasherProfile, FirmwareUpdateContext, FlashSpec};
use crate::firmware::credentials::Credentials;
use crate::firmware::error::{
    AttemptError, AttemptFailure, FirmwareApplyError, FirmwareError, FirmwarePhase, FirmwareResult,
};
use crate::firmware::flasher::{FirmwareFlasher, FirmwareProgress, FirmwareProgressEvent};
use crate::firmware::preflight::FirmwarePreflightError;
use crate::runner::FirmwareToolExecutor;

/// `run_operation` executes an already admitted request once. Execution errors
/// become persisted receipts; an error return means that receipt could not be
/// saved, so the caller must retain its mutation claim.
pub(super) async fn run_operation(
    device: String,
    operation_id: String,
    mut profile: FirmwareFlasherProfile,
    card_directory: PathBuf,
    journal: Arc<Mutex<OperationJournal>>,
    executor: FirmwareToolExecutor,
    cancellation: watch::Receiver<Option<String>>,
) -> Result<(), JournalError> {
    // The admitted request stays immutable. Only this runtime copy selects the
    // helper's private staging directory for the existing preparation path.
    profile.flash_spec.cache_dir = Some(card_directory.clone());
    let subscribing = journal.clone();
    let reload_required = tokio::task::spawn_blocking(move || {
        subscribing
            .lock()
            .expect("firmware journal mutex poisoned; saved state may be inconsistent")
            .subscribe_reload_required()
    })
    .await?;
    let progress = Arc::new(JournalProgress {
        operation_id: operation_id.clone(),
        card_directory,
        journal,
        reload_required,
        redactions: diagnostic_redactions(&profile.flash_spec),
        #[cfg(test)]
        proof_gate: None,
    });
    let result = async {
        let inspecting = progress.clone();
        let update_context = tokio::task::spawn_blocking(move || inspecting.inspect())
            .await
            .map_err(|source| progress_error(source.into()))??;
        let flasher = FirmwareFlasher::new_with_executor(
            device,
            &profile.firmware_spec,
            executor.continue_on_owner_exit(),
        )
        .await?
        .with_progress(progress.clone())
        .with_preparation_cancel(operation_id, cancellation);
        let flasher = if let Some(context) = update_context {
            flasher.with_update_validation(context.expected_device)
        } else {
            flasher
        };
        flasher.apply(&profile).await
    }
    .await;
    if let Err(error) = result {
        tokio::task::spawn_blocking(move || progress.record_failure(error)).await??;
    }
    Ok(())
}

/// `JournalProgress` binds apply callbacks to the admitted receipt, so staged
/// inputs and mutation intent stay under the same operation owner.
#[derive(Clone)]
struct JournalProgress {
    operation_id: String,
    card_directory: PathBuf,
    journal: Arc<Mutex<OperationJournal>>,
    reload_required: watch::Receiver<bool>,
    redactions: Vec<String>,
    #[cfg(test)]
    proof_gate: Option<Arc<ArtifactProofGate>>,
}

/// `ArtifactProofGate` pauses evidence work only in unit tests, so an independent
/// thread can detect a blocked service runtime without hanging the test process.
#[cfg(test)]
struct ArtifactProofGate {
    entered: std::sync::Barrier,
    release: std::sync::Barrier,
}

impl JournalProgress {
    /// `inspect` preserves the admission report before device discovery and
    /// honors cancellation before any inspection or preparation starts.
    /// Returns the saved context required for checked-update validation.
    fn inspect(&self) -> FirmwareResult<Option<FirmwareUpdateContext>> {
        let mut journal = self
            .journal
            .lock()
            .expect("firmware journal mutex poisoned; saved state may be inconsistent");
        let record = journal
            .record(&self.operation_id)
            .map_err(progress_error)?
            .ok_or_else(|| progress_error(JournalError::NotFound(self.operation_id.clone())))?;
        if record.operation.state == FirmwareOperationState::CancelRequested {
            return Err(FirmwareError::OperationCanceled);
        }
        let report = record.operation.report.clone();
        let update_context = record
            .requested
            .as_ref()
            .and_then(|requested| requested.update_context.clone());
        journal
            .update(
                &self.operation_id,
                OperationUpdate {
                    state: FirmwareOperationState::Running,
                    phase: Some(FirmwareOperationPhase::Inspect),
                    report,
                    diagnostic: None,
                    prepared_inputs: None,
                    mutation_started: false,
                },
            )
            .map_err(progress_error)?;
        Ok(update_context)
    }

    /// `record_failure` saves the settled report without erasing mutation intent.
    /// Uncertain or post-mutation failures require reconciliation, not replay.
    fn record_failure(&self, error: FirmwareApplyError) -> Result<(), JournalError> {
        let mut journal = self
            .journal
            .lock()
            .expect("firmware journal mutex poisoned; saved state may be inconsistent");
        let record = journal
            .record(&self.operation_id)?
            .ok_or_else(|| JournalError::NotFound(self.operation_id.clone()))?;
        let state = if matches!(*error.source, FirmwareError::OperationCanceled) {
            FirmwareOperationState::Canceled
        } else if failed_before_mutation(record, &error.source) {
            FirmwareOperationState::Failed
        } else {
            FirmwareOperationState::ManualIntervention
        };
        let update = OperationUpdate {
            state,
            phase: record.operation.phase,
            report: error
                .report
                .unwrap_or_else(|| record.operation.report.clone()),
            diagnostic: Some(failure_diagnostic(&error.source, &self.redactions)),
            prepared_inputs: None,
            mutation_started: record.mutation_started,
        };
        journal.update(&self.operation_id, update)?;
        Ok(())
    }
}

impl JournalProgress {
    /// `record_blocking` keeps hashing and serialized persistence off the
    /// service runtime while retaining the intent-before-effect ordering.
    fn record_blocking(&self, event: FirmwareProgressEvent) -> FirmwareResult<()> {
        // Hash outside the journal lock so clients can still request a stop
        // while preparation finishes. No mutator can run before this returns.
        let prepared_inputs = match &event {
            FirmwareProgressEvent::Prepared {
                directory,
                image,
                configuration,
            } => {
                #[cfg(test)]
                if let Some(gate) = &self.proof_gate {
                    gate.entered.wait();
                    gate.release.wait();
                }
                let inputs = PreparedInputs {
                    image: prepared_artifact(&self.card_directory, image)
                        .map_err(FirmwareError::ProgressPersistence)?,
                    configuration: configuration
                        .as_deref()
                        .map(|path| prepared_artifact(&self.card_directory, path))
                        .transpose()
                        .map_err(FirmwareError::ProgressPersistence)?,
                };
                sync_directory(directory).map_err(FirmwareError::ProgressPersistence)?;
                Some(inputs)
            }
            _ => None,
        };
        let mut journal = self
            .journal
            .lock()
            .expect("firmware journal mutex poisoned; saved state may be inconsistent");
        let record = journal
            .record(&self.operation_id)
            .map_err(progress_error)?
            .ok_or_else(|| progress_error(JournalError::NotFound(self.operation_id.clone())))?;
        let cancel_requested = record.operation.state == FirmwareOperationState::CancelRequested;
        let digest_error = prepared_inputs.as_ref().and_then(|inputs| {
            let expected = &record
                .requested
                .as_ref()?
                .update_context
                .as_ref()?
                .expected_digests;
            if inputs.image.sha256 != expected.image_sha256 {
                Some(FirmwareError::ImageDigestMismatch)
            } else if inputs
                .configuration
                .as_ref()
                .map(|artifact| artifact.sha256)
                != expected.configuration_sha256
            {
                Some(FirmwareError::ConfigurationDigestMismatch)
            } else {
                None
            }
        });
        if cancel_requested
            && matches!(
                event,
                FirmwareProgressEvent::Prepare
                    | FirmwareProgressEvent::Starting(_)
                    | FirmwareProgressEvent::ObservingVersion
            )
        {
            return Err(FirmwareError::OperationCanceled);
        }
        let mut update = OperationUpdate {
            state: if cancel_requested {
                FirmwareOperationState::CancelRequested
            } else {
                FirmwareOperationState::Running
            },
            phase: record.operation.phase,
            report: record.operation.report.clone(),
            diagnostic: None,
            prepared_inputs,
            mutation_started: false,
        };
        match event {
            FirmwareProgressEvent::Prepare | FirmwareProgressEvent::Prepared { .. } => {
                update.phase = Some(FirmwareOperationPhase::Prepare);
            }
            FirmwareProgressEvent::Starting(phase) => {
                update.phase = Some(operation_phase(phase)?);
                update.mutation_started = matches!(
                    phase,
                    FirmwarePhase::Configuration | FirmwarePhase::Burn | FirmwarePhase::Activation
                );
            }
            FirmwareProgressEvent::Completed(phase) => {
                update.phase = Some(operation_phase(phase)?);
                match phase {
                    FirmwarePhase::Burn => update.report.flashed = true,
                    FirmwarePhase::Activation => update.report.reset = Some(true),
                    FirmwarePhase::ImageVerification => update.report.verified_image = Some(true),
                    FirmwarePhase::Configuration | FirmwarePhase::ToolDiscovery => {}
                }
            }
            FirmwareProgressEvent::ObservingVersion => {
                update.phase = Some(FirmwareOperationPhase::VerifyVersion);
            }
            FirmwareProgressEvent::ObservedVersion(version) => {
                update.phase = Some(FirmwareOperationPhase::VerifyVersion);
                update.report.observed_version = Some(version);
            }
            FirmwareProgressEvent::Complete(report) => {
                // A stop arriving during the last phase does not undo a fully
                // completed operation. There is no subsequent phase to cancel.
                update.state = FirmwareOperationState::Complete;
                update.report = report;
            }
        }
        journal
            .update(&self.operation_id, update)
            .map_err(progress_error)?;
        // Save the actual artifact hashes even on mismatch, but withhold the
        // callback acknowledgement so configuration and burn cannot start.
        match digest_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl FirmwareProgress for JournalProgress {
    fn check(&self) -> FirmwareResult<()> {
        if *self.reload_required.borrow() {
            return Err(progress_error(JournalError::ReloadRequired));
        }
        Ok(())
    }

    async fn wait_for_failure(&self) -> FirmwareError {
        let mut reload_required = self.reload_required.clone();
        if reload_required
            .wait_for(|required| *required)
            .await
            .is_err()
        {
            // The progress owner keeps the sender alive. Losing a notification
            // channel alone is not evidence of a failed journal write.
            std::future::pending::<()>().await;
        }
        progress_error(JournalError::ReloadRequired)
    }

    /// `record` awaits the owned blocking task before apply can advance. RPCs
    /// and observer monitoring remain pollable while artifact evidence is read.
    async fn record(&self, event: FirmwareProgressEvent) -> FirmwareResult<()> {
        let progress = self.clone();
        tokio::task::spawn_blocking(move || progress.record_blocking(event))
            .await
            .map_err(|source| progress_error(source.into()))?
    }
}

/// `operation_phase` maps command phases to receipt phases while rejecting tool
/// discovery, which must not replace the hardware step being diagnosed.
fn operation_phase(phase: FirmwarePhase) -> FirmwareResult<FirmwareOperationPhase> {
    Ok(match phase {
        FirmwarePhase::Configuration => FirmwareOperationPhase::Configure,
        FirmwarePhase::Burn => FirmwareOperationPhase::Burn,
        FirmwarePhase::Activation => FirmwareOperationPhase::Activate,
        FirmwarePhase::ImageVerification => FirmwareOperationPhase::VerifyImage,
        FirmwarePhase::ToolDiscovery => {
            return Err(FirmwareError::ConfigError(
                "tool discovery cannot replace the recorded operation phase".to_string(),
            ));
        }
    })
}

/// `prepared_artifact` syncs a staged regular file and fingerprints its bytes,
/// so mutation intent can refer to a durable input inside the owned directory.
fn prepared_artifact(card_directory: &Path, path: &Path) -> io::Result<PreparedArtifact> {
    let relative = path
        .strip_prefix(card_directory)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "prepared firmware path must remain within the owned directory",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "prepared firmware input must be a regular file",
        ));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(PreparedArtifact {
        path: relative.to_path_buf(),
        sha256: digest.finalize().into(),
    })
}

/// `sync_directory` persists staged directory entries before a receipt refers
/// to them; a symlink cannot stand in for the directory being synced.
fn sync_directory(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?
        .sync_all()
}

/// `progress_error` carries the journal cause through apply's error contract,
/// preserving the distinction between persistence and hardware failures.
fn progress_error(source: JournalError) -> FirmwareError {
    FirmwareError::ProgressPersistence(io::Error::other(source))
}

/// `failed_before_mutation` distinguishes settled failures from uncertain
/// commands or mutations. A proven first-command launch failure remains
/// `Failed` after intent was saved, without clearing that sticky intent.
fn failed_before_mutation(record: &JournalRecord, error: &FirmwareError) -> bool {
    let attempt = match error {
        FirmwareError::PhaseFailed { source, .. }
        | FirmwareError::Discovery(DeviceDiscoveryError::QueryAttempt(source))
        | FirmwareError::Inspection(FirmwarePreflightError::Discovery(
            DeviceDiscoveryError::QueryAttempt(source),
        )) => Some(source),
        _ => None,
    };
    // A read can still own the card before mutation intent. Keep exclusion
    // until the helper proves the tool and its descendants have stopped.
    if matches!(
        attempt,
        Some(AttemptError::Started { status, cleanup_errors, .. })
            if status.is_none() || !cleanup_errors.is_empty()
    ) {
        return false;
    }
    if !record.mutation_started {
        return true;
    }
    let FirmwareError::PhaseFailed { phase, source } = error else {
        return false;
    };
    if source.started() {
        return false;
    }
    match phase {
        FirmwarePhase::Configuration => true,
        FirmwarePhase::Burn => record
            .prepared_inputs
            .as_ref()
            .is_some_and(|inputs| inputs.configuration.is_none()),
        _ => false,
    }
}

/// `diagnostic_redactions` keeps request secrets in memory only, so useful tool
/// output can be retained without copying source locations or credentials.
fn diagnostic_redactions(spec: &FlashSpec) -> Vec<String> {
    let mut values = vec![spec.firmware_url.clone()];
    values.extend(spec.device_conf_url.iter().cloned());
    for source in std::iter::once(&spec.firmware_url).chain(spec.device_conf_url.iter()) {
        if let Ok(crate::firmware::source::FirmwareSource::Ssh(source)) =
            crate::firmware::source::FirmwareSource::from_url(source)
        {
            values.push(source.username);
        } else if let Ok(source) = url::Url::parse(source) {
            values.push(source.username().to_string());
            values.extend(source.password().map(str::to_string));
        }
    }
    for credential in spec
        .firmware_credentials
        .iter()
        .chain(spec.device_conf_credentials.iter())
    {
        match credential {
            Credentials::BearerToken { token } => values.push(token.clone()),
            Credentials::BasicAuth { username, password } => {
                values.extend([username.clone(), password.clone()]);
            }
            Credentials::Header { name, value } => values.extend([name.clone(), value.clone()]),
            Credentials::SshKey { path, passphrase } => {
                values.push(path.clone());
                values.extend(passphrase.iter().cloned());
            }
            Credentials::SshAgent {} => {}
        }
    }
    values
        .into_iter()
        .flat_map(|value| value.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter(|value| !value.is_empty())
        .collect()
}

/// `safe_diagnostic_text` omits an entire sensitive line instead of leaving
/// fragments of overlapping secrets. Bound only after redaction, on UTF-8 edges.
fn safe_diagnostic_text(text: &str, redactions: &[String]) -> String {
    let mut safe = text
        .lines()
        .map(|line| {
            if line.contains("://") || redactions.iter().any(|value| line.contains(value)) {
                "[REDACTED]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut end = safe.len().min(768);
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    safe.truncate(end);
    safe
}

/// `io_diagnostic` preserves the actionable OS classification without custom
/// error text, which can contain an input path or credentials.
fn io_diagnostic(error: &io::Error) -> String {
    if let Some(JournalError::Io { source, .. }) = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<JournalError>())
    {
        return io_diagnostic(source);
    }
    format!("kind={:?}, errno={:?}", error.kind(), error.raw_os_error())
}

/// `attempt_diagnostic` retains process and cleanup evidence separately from
/// redacted output, so hiding a secret-bearing line does not hide the outcome.
fn attempt_diagnostic(
    error: &crate::firmware::error::AttemptError,
    redactions: &[String],
) -> String {
    use crate::firmware::error::AttemptError;
    match error {
        AttemptError::SupervisorNotStarted { source, .. } => {
            format!(
                "firmware supervisor did not start: {}",
                io_diagnostic(source)
            )
        }
        AttemptError::NotStarted { source, .. } => {
            format!("firmware tool did not start: {}", io_diagnostic(source))
        }
        AttemptError::Started {
            status,
            stdout,
            stderr,
            reason,
            cleanup_errors,
            ..
        } => {
            let reason = match reason {
                AttemptFailure::Io(error) => format!("tool I/O failed: {}", io_diagnostic(error)),
                reason => reason.to_string(),
            };
            let cleanup = cleanup_errors
                .iter()
                .map(io_diagnostic)
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "{reason}; exit_code={:?}, signal={:?}; cleanup=[{cleanup}]; stderr: {}; stdout: {}",
                status.as_ref().and_then(|status| status.code()),
                status.as_ref().and_then(|status| status.signal()),
                safe_diagnostic_text(&String::from_utf8_lossy(stderr), redactions),
                safe_diagnostic_text(&String::from_utf8_lossy(stdout), redactions),
            )
        }
    }
}

/// `failure_diagnostic` retains bounded causes without storing source URLs or
/// credentials. Process status and OS evidence remain visible after redaction.
fn failure_diagnostic(error: &FirmwareError, redactions: &[String]) -> String {
    match error {
        FirmwareError::OperationCanceled => {
            "canceled at a phase boundary; completed device changes were not undone".into()
        }
        FirmwareError::ProgressPersistence(source) => {
            format!(
                "firmware progress could not be persisted: {}",
                io_diagnostic(source)
            )
        }
        FirmwareError::PhaseFailed { phase, source } => {
            format!("{phase:?}: {}", attempt_diagnostic(source, redactions))
        }
        FirmwareError::Discovery(DeviceDiscoveryError::QueryAttempt(source))
        | FirmwareError::Inspection(FirmwarePreflightError::Discovery(
            DeviceDiscoveryError::QueryAttempt(source),
        )) => {
            format!("device query: {}", attempt_diagnostic(source, redactions))
        }
        FirmwareError::Io(source) => format!("firmware input I/O: {}", io_diagnostic(source)),
        FirmwareError::Inspection(FirmwarePreflightError::InputIo { source, .. }) => {
            format!("firmware input inspection: {}", io_diagnostic(source))
        }
        FirmwareError::HttpError(message) => {
            let status = message
                .strip_prefix("HTTP ")
                .and_then(|text| text.split_whitespace().next())
                .and_then(|text| text.parse::<u16>().ok());
            format!(
                "firmware HTTP request failed; status={status:?}; {}",
                safe_diagnostic_text(message, redactions)
            )
        }
        FirmwareError::VerificationFailed(message) => {
            format!(
                "firmware version mismatch: {}",
                safe_diagnostic_text(message, redactions)
            )
        }
        FirmwareError::PostResetVerificationTimeout { timeout, source } => {
            let summary =
                format!("post-reset firmware verification exceeded its {timeout:?} budget");
            match source {
                Some(source) => format!(
                    "{summary}; last verification failure: {}",
                    failure_diagnostic(source, redactions)
                ),
                None => summary,
            }
        }
        FirmwareError::SourceResolution { reason, .. } => {
            format!(
                "firmware source resolution failed: {}",
                safe_diagnostic_text(reason, redactions)
            )
        }
        error => safe_diagnostic_text(&error.to_string(), redactions),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use carbide_libmlx_model::firmware::FirmwareSpec;
    use carbide_libmlx_model::firmware::operation::{FirmwareOperation, ObserverExitBehavior};
    use carbide_libmlx_model::firmware::result::FirmwareFlashReport;

    use super::*;
    use crate::firmware::config::FlashOptions;
    use crate::firmware::operation::journal::RequestedFirmware;

    /// Source credentials may appear without their URL in a transfer failure;
    /// redaction must cover those values while preserving independent causes.
    #[test]
    fn diagnostics_redact_source_credentials_and_preserve_structured_causes() {
        let spec = FlashSpec {
            firmware_url: "https://source-user:source-password@example.invalid/image".into(),
            device_conf_url: Some("ssh://config-user@host:configuration".into()),
            firmware_credentials: Some(Credentials::BearerToken {
                token: "first-line\nsecond-line".into(),
            }),
            device_conf_credentials: None,
            verify_from_cache: false,
            cache_dir: None,
        };
        let redactions = diagnostic_redactions(&spec);
        let safe = safe_diagnostic_text(
            "source-user\nsource-password\nconfig-user\nfirst-line\nsecond-line\nuseful cause",
            &redactions,
        );
        assert_eq!(
            safe,
            "[REDACTED]\n[REDACTED]\n[REDACTED]\n[REDACTED]\n[REDACTED]\nuseful cause"
        );
        let http = failure_diagnostic(
            &FirmwareError::HttpError(format!(
                "HTTP 503 Service Unavailable from {}",
                spec.firmware_url
            )),
            &redactions,
        );
        assert!(http.contains("status=Some(503)"));
        assert!(!http.contains("source-password"));
        assert!(!http.contains("example.invalid"));
        let persistence = progress_error(JournalError::Io {
            operation: "sync directory",
            path: PathBuf::from("private-state"),
            source: io::Error::from_raw_os_error(libc::ENOSPC),
        });
        assert!(failure_diagnostic(&persistence, &redactions).contains("errno=Some(28)"));

        // The deadline explains why work stopped; the last failed read explains what
        // the operator should investigate. It needs the same source redaction.
        let expired = FirmwareError::PostResetVerificationTimeout {
            timeout: std::time::Duration::from_secs(30),
            source: Some(Box::new(FirmwareError::VerificationFailed(
                "source-password\nobserved old firmware".into(),
            ))),
        };
        let diagnostic = failure_diagnostic(&expired, &redactions);
        assert!(diagnostic.contains("30s budget"));
        assert!(diagnostic.contains("observed old firmware"));
        assert!(!diagnostic.contains("source-password"));
        // Expiry before any failed read has no cause to append or reconstruct.
        assert_eq!(
            failure_diagnostic(
                &FirmwareError::PostResetVerificationTimeout {
                    timeout: std::time::Duration::from_secs(30),
                    source: None,
                },
                &redactions,
            ),
            "post-reset firmware verification exceeded its 30s budget",
        );
    }

    /// `run_operation` must derive redactions from the submitted profile before
    /// saving a tool failure, not rely on callback fixtures supplying them.
    #[tokio::test]
    async fn run_operation_redacts_profile_credentials_in_the_persisted_receipt() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        use crate::runner::{HELLO, Request, Response};

        const TOKEN: &str = "AsterToken4821";
        const USERNAME: &str = "BirchUser752";
        const PASSWORD: &str = "CedarPassword937";
        let temporary = tempfile::tempdir().expect("create operation fixture");
        let directory = temporary.path();
        let card_directory = directory.join("card");
        let journal = admitted_progress(&card_directory, FlashOptions::default(), None).journal;
        let profile = FirmwareFlasherProfile {
            firmware_spec: FirmwareSpec {
                part_number: "part-number".into(),
                psid: "psid".into(),
                version: "target-version".into(),
            },
            flash_spec: FlashSpec {
                firmware_url: "https://firmware.example.invalid/image.bin".into(),
                firmware_credentials: Some(Credentials::BearerToken {
                    token: TOKEN.into(),
                }),
                device_conf_url: Some("https://configuration.example.invalid/device.bin".into()),
                device_conf_credentials: Some(Credentials::BasicAuth {
                    username: USERNAME.into(),
                    password: PASSWORD.into(),
                }),
                verify_from_cache: false,
                cache_dir: None,
            },
            flash_options: FlashOptions::default(),
        };

        // The query fails before downloads or mutations. Bare credential values
        // cannot be hidden by the generic URL filter or the older "secret" fixture.
        let response = serde_json::to_string(&Response::Finished {
            status: 41 << 8,
            stdout: Vec::new(),
            stderr: format!("device semaphore busy\n{TOKEN}\n{USERNAME}\n{PASSWORD}\n")
                .into_bytes(),
        })
        .unwrap();
        let frame_prefix: String = u32::try_from(response.len())
            .unwrap()
            .to_be_bytes()
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect();
        let contents = format!(
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
        let helper = directory.join("query-failure-helper");
        // Keep writable executable descriptors out of concurrent test forks.
        let status = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "set -eu; umask 077; printf '%s' \"$2\" > \"$1\"",
                    "fixture-writer",
                ])
                .arg(&helper)
                .arg(contents)
                .kill_on_drop(true)
                .status(),
        )
        .await
        .expect("fixture writer completes within its watchdog")
        .expect("write fake supervisor");
        assert!(status.success());
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let (_cancellation, cancellation) = watch::channel(None);
        tokio::time::timeout(
            Duration::from_secs(10),
            run_operation(
                "0000:01:00.0".into(),
                "operation".into(),
                profile,
                card_directory.clone(),
                journal.clone(),
                FirmwareToolExecutor::with_supervisor_path(helper),
                cancellation,
            ),
        )
        .await
        .expect("failed operation completes within its independent watchdog")
        .expect("persist failed operation");
        drop(journal);

        // Reopening proves the failure was saved, rather than checking only the
        // in-memory redaction helper or the value returned by execution.
        let journal = OperationJournal::open(&card_directory, "0000:01:00.0", "test-boot")
            .expect("reload failed operation");
        let record = journal
            .record("operation")
            .unwrap()
            .expect("find persisted receipt");
        assert_eq!(record.operation.state, FirmwareOperationState::Failed);
        assert_eq!(
            record.operation.phase,
            Some(FirmwareOperationPhase::Inspect)
        );
        assert!(!record.mutation_started);
        let diagnostic = record.operation.diagnostic.as_deref().unwrap();
        assert!(diagnostic.contains("exit status: 41"), "{diagnostic}");
        assert!(diagnostic.contains("device semaphore busy"), "{diagnostic}");
        assert_eq!(diagnostic.matches("[REDACTED]").count(), 3);
        let persisted = fs::read_to_string(card_directory.join("operations.json")).unwrap();
        for credential in [TOKEN, USERNAME, PASSWORD] {
            assert!(
                !persisted.contains(credential),
                "profile credential reached the receipt"
            );
        }

        // The helper only records requests; none of the requested hardware tools
        // is executed, and a second phase would be visible as another request.
        let requests = fs::read_to_string(directory.join("query-failure-helper.requests")).unwrap();
        assert_eq!(requests.lines().count(), 1);
        let request: Request = serde_json::from_str(requests.trim()).unwrap();
        assert_eq!(request.program, "mlxfwmanager");
        assert_eq!(
            request.args,
            ["--dev", "0000:01:00.0", "--query-format", "xml"]
        );
    }

    /// `admitted_progress` gives callback tests a real persisted admission, so
    /// reopening the journal can verify their recovery-visible effects.
    fn admitted_progress(
        directory: &Path,
        options: FlashOptions,
        update_context: Option<crate::firmware::config::FirmwareUpdateContext>,
    ) -> JournalProgress {
        let mut journal = OperationJournal::open(directory, "0000:01:00.0", "test-boot")
            .expect("open fixture journal");
        journal
            .admit(JournalRecord {
                operation: FirmwareOperation {
                    operation_id: "operation".to_string(),
                    state: FirmwareOperationState::Accepted,
                    phase: None,
                    report: FirmwareFlashReport {
                        expected_version: Some("target-version".to_string()),
                        reset: options.reset.then_some(false),
                        ..Default::default()
                    },
                    diagnostic: None,
                },
                request_sha256: [1; 32],
                boot_id: "test-boot".to_string(),
                requested: Some(RequestedFirmware {
                    target: FirmwareSpec {
                        part_number: "part-number".to_string(),
                        psid: "psid".to_string(),
                        version: "target-version".to_string(),
                    },
                    options,
                    update_context,
                }),
                observer_exit_behavior: ObserverExitBehavior::Continue,
                prepared_inputs: None,
                mutation_started: false,
            })
            .expect("admit fixture operation");
        JournalProgress {
            operation_id: "operation".to_string(),
            card_directory: directory.to_path_buf(),
            reload_required: journal.subscribe_reload_required(),
            journal: Arc::new(Mutex::new(journal)),
            redactions: vec!["secret".to_string()],
            proof_gate: None,
        }
    }

    /// Cancellation and observer futures must advance while artifact hashing
    /// is blocked, without allowing apply to overtake the evidence write.
    #[tokio::test(flavor = "current_thread")]
    async fn artifact_proof_yields_to_cancellation_before_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("card");
        let mut progress = admitted_progress(&directory, FlashOptions::default(), None);
        let staged = directory.join("staged");
        fs::create_dir(&staged).unwrap();
        let image = staged.join("image.bin");
        fs::write(&image, b"image bytes").unwrap();
        let gate = Arc::new(ArtifactProofGate {
            entered: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        });
        progress.proof_gate = Some(gate.clone());
        let (entered, proof_entered) = tokio::sync::oneshot::channel();
        let (advanced, runtime_advanced) = std::sync::mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            gate.entered.wait();
            entered.send(()).unwrap();
            let advanced = runtime_advanced
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok();
            gate.release.wait();
            advanced
        });
        let recording = progress.clone();
        let record = tokio::spawn(async move {
            recording
                .record(FirmwareProgressEvent::Prepared {
                    directory: staged,
                    image,
                    configuration: None,
                })
                .await
        });
        proof_entered.await.unwrap();
        let journal = progress.journal.clone();
        tokio::task::spawn_blocking(move || {
            let mut journal = journal.lock().unwrap();
            let receipt = journal.snapshot("operation").unwrap().unwrap();
            journal
                .update(
                    "operation",
                    OperationUpdate {
                        state: FirmwareOperationState::CancelRequested,
                        phase: receipt.phase,
                        report: receipt.report,
                        diagnostic: None,
                        prepared_inputs: None,
                        mutation_started: false,
                    },
                )
                .unwrap();
        })
        .await
        .unwrap();
        let _ = advanced.send(());
        record.await.unwrap().unwrap();
        assert!(
            watchdog.join().unwrap(),
            "artifact proof blocked cancellation on the runtime"
        );
        assert!(matches!(
            progress
                .record(FirmwareProgressEvent::Starting(FirmwarePhase::Burn))
                .await,
            Err(FirmwareError::OperationCanceled)
        ));
        let journal = progress.journal.lock().unwrap();
        let receipt = journal.record("operation").unwrap().unwrap();
        assert_eq!(
            receipt.operation.state,
            FirmwareOperationState::CancelRequested
        );
        assert!(!receipt.mutation_started);
        let digest: [u8; 32] = Sha256::digest(b"image bytes").into();
        assert_eq!(
            receipt.prepared_inputs.as_ref().unwrap().image.sha256,
            digest
        );
    }

    /// Both artifact digests must match before preparation acknowledges success.
    /// A mismatch keeps the actual hashes in the durable failure receipt, with
    /// no mutation intent that could be mistaken for a completed configuration.
    #[tokio::test]
    async fn checked_preparation_rejects_either_digest_before_mutation() {
        use crate::firmware::config::{FirmwareArtifactDigests, FirmwareUpdateContext};

        let image_sha256: [u8; 32] = Sha256::digest(b"image").into();
        let configuration_sha256: [u8; 32] = Sha256::digest(b"configuration").into();
        for (scenario, expected_image, expected_configuration, diagnostic) in [
            (
                "wrong image",
                [0; 32],
                configuration_sha256,
                "firmware image SHA-256",
            ),
            (
                "wrong configuration after valid image",
                image_sha256,
                [0; 32],
                "firmware configuration SHA-256",
            ),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let directory = temporary.path().join("card");
            let context = FirmwareUpdateContext {
                phase_id: "dispatch-1".into(),
                expected_digests: FirmwareArtifactDigests {
                    image_sha256: expected_image,
                    configuration_sha256: Some(expected_configuration),
                },
                expected_device: None,
            };
            let progress =
                admitted_progress(&directory, FlashOptions::default(), Some(context.clone()));
            let staged = directory.join("staged");
            fs::create_dir(&staged).unwrap();
            let image = staged.join("image.bin");
            let configuration = staged.join("config.bin");
            fs::write(&image, b"image").unwrap();
            fs::write(&configuration, b"configuration").unwrap();
            let error = progress
                .record(FirmwareProgressEvent::Prepared {
                    directory: staged,
                    image,
                    configuration: Some(configuration),
                })
                .await
                .expect_err(scenario);
            assert!(error.to_string().contains(diagnostic), "{scenario}");
            progress.record_failure(error.into()).unwrap();
            drop(progress);

            // Reload the committed failure, not the progress object's cache.
            let journal = OperationJournal::open(&directory, "0000:01:00.0", "test-boot").unwrap();
            let record = journal.record("operation").unwrap().unwrap();
            assert_eq!(
                record.operation.state,
                FirmwareOperationState::Failed,
                "{scenario}"
            );
            assert_eq!(
                record.operation.phase,
                Some(FirmwareOperationPhase::Prepare),
                "{scenario}"
            );
            assert!(!record.mutation_started, "{scenario}");
            assert!(record.is_safe_to_release(), "{scenario}");
            assert_eq!(
                record.requested.as_ref().unwrap().update_context.as_ref(),
                Some(&context)
            );
            let prepared = record.prepared_inputs.as_ref().unwrap();
            assert_eq!(prepared.image.sha256, image_sha256, "{scenario}");
            assert_eq!(
                prepared.configuration.as_ref().unwrap().sha256,
                configuration_sha256,
                "{scenario}"
            );
        }
    }

    /// Cancellation preserves completed phase results. It stops a later phase,
    /// but cannot undo an operation whose final phase has already succeeded.
    #[tokio::test]
    async fn cancellation_preserves_settled_progress_and_prepared_inputs() {
        for expected_state in [
            FirmwareOperationState::Canceled,
            FirmwareOperationState::Complete,
        ] {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let progress = admitted_progress(
                &directory,
                FlashOptions {
                    reset: expected_state == FirmwareOperationState::Canceled,
                    ..FlashOptions::default()
                },
                None,
            );
            progress
                .record(FirmwareProgressEvent::Prepare)
                .await
                .expect("record preparation");
            let staged = directory.join("staged");
            fs::create_dir(&staged).expect("create prepared directory");
            let image = staged.join("image.bin");
            let configuration = staged.join("configuration.bin");
            fs::write(&image, b"firmware image").expect("write prepared image");
            fs::write(&configuration, b"configuration").expect("write prepared configuration");
            progress
                .record(FirmwareProgressEvent::Prepared {
                    directory: staged.clone(),
                    image: image.clone(),
                    configuration: Some(configuration.clone()),
                })
                .await
                .expect("record checked inputs");
            progress
                .record(FirmwareProgressEvent::Starting(FirmwarePhase::Burn))
                .await
                .expect("record burn intent");
            {
                let mut journal = progress.journal.lock().unwrap();
                let operation = journal.snapshot("operation").unwrap().unwrap();
                journal
                    .update(
                        "operation",
                        OperationUpdate {
                            state: FirmwareOperationState::CancelRequested,
                            phase: operation.phase,
                            report: operation.report,
                            diagnostic: None,
                            prepared_inputs: None,
                            mutation_started: false,
                        },
                    )
                    .expect("request cancellation during burn");
            }
            progress
                .record(FirmwareProgressEvent::Completed(FirmwarePhase::Burn))
                .await
                .expect("persist completed burn despite cancellation");
            if expected_state == FirmwareOperationState::Complete {
                progress
                    .record(FirmwareProgressEvent::Complete(FirmwareFlashReport {
                        flashed: true,
                        expected_version: Some("target-version".into()),
                        ..Default::default()
                    }))
                    .await
                    .expect("record success after cancellation during the final phase");
            } else {
                let stop = progress
                    .record(FirmwareProgressEvent::Starting(FirmwarePhase::Activation))
                    .await
                    .expect_err("stop before activation");
                assert!(matches!(stop, FirmwareError::OperationCanceled));
                progress
                    .record_failure(stop.into())
                    .expect("record boundary cancellation");
            }
            drop(progress);

            let journal = OperationJournal::open(&directory, "0000:01:00.0", "test-boot")
                .expect("reload canceled operation");
            let record = journal.record("operation").unwrap().unwrap();
            assert_eq!(record.operation.state, expected_state);
            assert_eq!(record.operation.phase, Some(FirmwareOperationPhase::Burn));
            assert!(record.operation.report.flashed);
            assert!(record.mutation_started);
            let inputs = record.prepared_inputs.as_ref().unwrap();
            assert_eq!(inputs.image.path, Path::new("staged/image.bin"));
            let image_digest: [u8; 32] = Sha256::digest(b"firmware image").into();
            assert_eq!(inputs.image.sha256, image_digest);
            let configuration = inputs.configuration.as_ref().unwrap();
            assert_eq!(configuration.path, Path::new("staged/configuration.bin"));
            let configuration_digest: [u8; 32] = Sha256::digest(b"configuration").into();
            assert_eq!(configuration.sha256, configuration_digest);
            assert_eq!(
                record.is_safe_to_release(),
                expected_state == FirmwareOperationState::Complete
            );
        }
    }

    /// Artifact evidence covers every byte and rejects paths that cannot name
    /// a regular input inside the operation's staging directory.
    #[test]
    fn prepared_artifact_checks_complete_bytes_and_path_boundaries() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let card = root.path().join("card");
        fs::create_dir(&card).unwrap();
        let image = card.join("image.bin");
        let bytes: Vec<u8> = (0..(2 * 64 * 1024 + 37))
            .map(|offset| (offset % 251) as u8)
            .collect();
        fs::write(&image, &bytes).unwrap();
        let outside = root.path().join("outside.bin");
        fs::write(&outside, b"outside").unwrap();
        let linked = card.join("linked.bin");
        symlink(&outside, &linked).unwrap();

        enum Expected {
            Digest,
            InvalidInput,
            Symlink,
        }
        for (scenario, path, expected) in [
            ("multiple chunks and tail", image, Expected::Digest),
            ("outside card directory", outside, Expected::InvalidInput),
            ("symbolic link", linked, Expected::Symlink),
        ] {
            let result = prepared_artifact(&card, &path);
            match expected {
                Expected::Digest => {
                    let artifact = result.expect(scenario);
                    let digest: [u8; 32] = Sha256::digest(&bytes).into();
                    assert_eq!(artifact.sha256, digest, "{scenario}");
                    assert_eq!(artifact.path, Path::new("image.bin"), "{scenario}");
                }
                Expected::InvalidInput => assert_eq!(
                    result.unwrap_err().kind(),
                    io::ErrorKind::InvalidInput,
                    "{scenario}"
                ),
                Expected::Symlink => assert_eq!(
                    result.unwrap_err().raw_os_error(),
                    Some(libc::ELOOP),
                    "{scenario}"
                ),
            }
        }
    }

    /// An unlaunched first burn is a known failure; the same launch error after
    /// configuration must preserve the earlier mutation for reconciliation.
    #[tokio::test]
    async fn burn_launch_failure_preserves_prior_configuration_effects() {
        use carbide_test_support::Outcome::Yields;
        use carbide_test_support::{Case, check_cases_async};

        check_cases_async(
            [
                Case {
                    // With no configuration effect, the failed launch proves no mutation.
                    scenario: "burn is the first mutator and never starts",
                    input: false,
                    expect: Yields(FirmwareOperationState::Failed),
                },
                Case {
                    // An unlaunched burn cannot erase a completed configuration change.
                    scenario: "configuration completed before burn failed to start",
                    input: true,
                    expect: Yields(FirmwareOperationState::ManualIntervention),
                },
            ],
            |configuration_applied| async move {
                let temporary = tempfile::tempdir().expect("create fixture directory");
                let directory = temporary.path().join("card");
                let progress = admitted_progress(&directory, FlashOptions::default(), None);
                progress
                    .record(FirmwareProgressEvent::Prepare)
                    .await
                    .expect("record preparation");
                let staged = directory.join("staged");
                fs::create_dir(&staged).expect("create prepared directory");
                let image = staged.join("image.bin");
                fs::write(&image, b"firmware image").expect("write prepared image");
                let configuration = configuration_applied.then(|| {
                    let path = staged.join("configuration.bin");
                    fs::write(&path, b"configuration").expect("write prepared configuration");
                    path
                });
                progress
                    .record(FirmwareProgressEvent::Prepared {
                        directory: staged,
                        image,
                        configuration,
                    })
                    .await
                    .expect("record checked inputs");
                if configuration_applied {
                    // This case has a completed configuration effect, not only intent.
                    for event in [
                        FirmwareProgressEvent::Starting(FirmwarePhase::Configuration),
                        FirmwareProgressEvent::Completed(FirmwarePhase::Configuration),
                    ] {
                        progress
                            .record(event)
                            .await
                            .expect("record applied configuration");
                    }
                }
                progress
                    .record(FirmwareProgressEvent::Starting(FirmwarePhase::Burn))
                    .await
                    .expect("record burn intent");
                progress
                    .record_failure(
                        FirmwareError::PhaseFailed {
                            phase: FirmwarePhase::Burn,
                            source: AttemptError::NotStarted {
                                command: "flint burn".to_string(),
                                source: io::Error::from_raw_os_error(libc::ENOENT),
                            },
                        }
                        .into(),
                    )
                    .map_err(|error| error.to_string())?;
                drop(progress);

                let journal = OperationJournal::open(&directory, "0000:01:00.0", "test-boot")
                    .map_err(|error| error.to_string())?;
                let record = journal
                    .record("operation")
                    .unwrap()
                    .expect("reload failed receipt");
                assert_eq!(record.operation.phase, Some(FirmwareOperationPhase::Burn));
                assert!(!record.operation.report.flashed);
                assert!(record.mutation_started);
                Ok::<_, String>(record.operation.state)
            },
        )
        .await;
    }

    /// Persisted failures distinguish stopped commands from unknown completion
    /// and prior mutations, without saving credentials from the error chain.
    #[tokio::test]
    async fn failure_receipts_preserve_progress_without_source_diagnostics() {
        for (scenario, phase, error, expected_state) in [
            (
                "preparation failed",
                None,
                FirmwareError::SourceResolution {
                    description: "https://user:secret@example.invalid/firmware".to_string(),
                    reason: "secret credential was rejected".to_string(),
                },
                FirmwareOperationState::Failed,
            ),
            (
                // No mutation intent does not prove the query has stopped.
                "discovery query completion is unknown",
                None,
                FirmwareError::Discovery(DeviceDiscoveryError::QueryAttempt(
                    AttemptError::Started {
                        command: "mlxfwmanager".into(),
                        status: None,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        reason: AttemptFailure::Io(io::Error::other("lost query result")),
                        cleanup_errors: Vec::new(),
                    },
                )),
                FirmwareOperationState::ManualIntervention,
            ),
            (
                // A reaped leader does not settle its descendants. Long or
                // sensitive output must not hide the structured cleanup evidence.
                "preflight cleanup is unconfirmed",
                None,
                FirmwareError::Inspection(FirmwarePreflightError::Discovery(
                    DeviceDiscoveryError::QueryAttempt(AttemptError::Started {
                        command: "mlxfwmanager secret-command".into(),
                        status: Some(std::process::ExitStatus::from_raw(41 << 8)),
                        stdout: format!("{}\nsecret credential\n", "é".repeat(2000)).into_bytes(),
                        stderr: b"device semaphore busy\nhttps://example.invalid/private\n"
                            .to_vec(),
                        reason: AttemptFailure::Nonzero,
                        cleanup_errors: vec![io::Error::from_raw_os_error(libc::EACCES)],
                    }),
                )),
                FirmwareOperationState::ManualIntervention,
            ),
            (
                // Tool lookup also runs before mutation and must retain exclusion.
                "tool discovery completion is unknown",
                None,
                FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::ToolDiscovery,
                    source: AttemptError::Started {
                        command: "flint --version".into(),
                        status: None,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        reason: AttemptFailure::Io(io::Error::other("lost probe result")),
                        cleanup_errors: Vec::new(),
                    },
                },
                FirmwareOperationState::ManualIntervention,
            ),
            (
                // A timed-out read can release the claim once cleanup is confirmed.
                "discovery deadline settled before mutation",
                None,
                FirmwareError::Discovery(DeviceDiscoveryError::QueryAttempt(
                    AttemptError::Started {
                        command: "mlxfwmanager".into(),
                        status: Some(std::process::ExitStatus::from_raw(libc::SIGKILL)),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        reason: AttemptFailure::Deadline(std::time::Duration::from_secs(30)),
                        cleanup_errors: Vec::new(),
                    },
                )),
                FirmwareOperationState::Failed,
            ),
            (
                "first configuration did not start",
                Some(FirmwarePhase::Configuration),
                FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::Configuration,
                    source: AttemptError::NotStarted {
                        command: "secret-command".to_string(),
                        source: io::Error::new(io::ErrorKind::NotFound, "secret path"),
                    },
                },
                FirmwareOperationState::Failed,
            ),
            (
                "version mismatch after burn",
                Some(FirmwarePhase::Burn),
                FirmwareError::VerificationFailed("secret tool output".to_string()),
                FirmwareOperationState::ManualIntervention,
            ),
            (
                "actionable reset failure",
                Some(FirmwarePhase::Activation),
                FirmwareError::PhaseFailed {
                    phase: FirmwarePhase::Activation,
                    source: AttemptError::Started {
                        command: "mlxfwreset secret-command".to_string(),
                        status: Some(std::process::ExitStatus::from_raw(41 << 8)),
                        stdout: b"secret credential\n".to_vec(),
                        stderr: format!(
                            "device semaphore busy\nhttps://example.invalid/private\n{}",
                            "é".repeat(2000)
                        )
                        .into_bytes(),
                        reason: AttemptFailure::Nonzero,
                        cleanup_errors: vec![io::Error::from_raw_os_error(libc::EACCES)],
                    },
                },
                FirmwareOperationState::ManualIntervention,
            ),
        ] {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let progress = admitted_progress(&directory, FlashOptions::default(), None);
            progress
                .record(FirmwareProgressEvent::Prepare)
                .await
                .expect("record preparation");
            if let Some(phase) = phase {
                progress
                    .record(FirmwareProgressEvent::Starting(phase))
                    .await
                    .expect("record phase intent");
            }
            let report = if phase == Some(FirmwarePhase::Burn) {
                progress
                    .record(FirmwareProgressEvent::Completed(FirmwarePhase::Burn))
                    .await
                    .expect("record completed burn");
                Some(FirmwareFlashReport {
                    flashed: true,
                    verified_version: Some(false),
                    observed_version: Some("observed-version".to_string()),
                    expected_version: Some("target-version".to_string()),
                    ..Default::default()
                })
            } else {
                None
            };
            progress
                .record_failure(FirmwareApplyError {
                    report,
                    source: Box::new(error),
                })
                .expect("record failure");
            drop(progress);
            let journal = OperationJournal::open(&directory, "0000:01:00.0", "test-boot")
                .expect("reload failed receipt");
            let record = journal.record("operation").unwrap().unwrap();
            assert_eq!(record.operation.state, expected_state, "{scenario}");
            assert_eq!(record.mutation_started, phase.is_some(), "{scenario}");
            assert_eq!(
                record.is_safe_to_release(),
                expected_state == FirmwareOperationState::Failed && phase.is_none(),
                "{scenario}"
            );
            assert_eq!(
                record.operation.report.flashed,
                phase == Some(FirmwarePhase::Burn),
                "{scenario}"
            );
            if phase == Some(FirmwarePhase::Burn) {
                assert_eq!(
                    record.operation.report.observed_version.as_deref(),
                    Some("observed-version")
                );
            }
            let persisted = fs::read_to_string(directory.join("operations.json")).unwrap();
            assert!(!persisted.contains("secret"), "{scenario}");
            assert!(!persisted.contains("example.invalid"), "{scenario}");
            assert!(record.operation.diagnostic.as_ref().unwrap().len() <= 2048);
            if matches!(
                scenario,
                "actionable reset failure" | "preflight cleanup is unconfirmed"
            ) {
                assert!(persisted.contains("exit_code=Some(41)"));
                assert!(persisted.contains("errno=Some(13)"));
                assert!(persisted.contains("device semaphore busy"));
                assert!(persisted.contains("[REDACTED]"));
            }
        }
    }
}
