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

//! A private receipt ledger with exclusive ownership by one helper. Releasing
//! its lock says nothing about orphaned firmware tools: unfinished receipts
//! remain blocked after the helper exits, even when the lock becomes available.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use carbide_libmlx_model::firmware::FirmwareSpec;
use carbide_libmlx_model::firmware::operation::{
    FirmwareOperation, FirmwareOperationPhase, FirmwareOperationState, ObserverExitBehavior,
};
use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use nix::libc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;

use crate::firmware::config::FlashOptions;

const SCHEMA_VERSION: u32 = 1;
// Keep at most 128 receipts per slot so whole-journal reads and writes stay
// bounded without silently evicting operation IDs used to prevent replay.
const MAX_RECEIPTS: usize = 128;
const MAX_LEDGER_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 2048;
const LEDGER_FILENAME: &str = "operations.json";
const OWNER_LOCK_FILENAME: &str = "owner.lock";

/// Only the target and behavior are retained; source URLs and credentials stay
/// out of the journal. `None` represents a request with no firmware work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestedFirmware {
    pub(super) target: FirmwareSpec,
    pub(super) options: FlashOptions,
}

impl RequestedFirmware {
    /// `initial_report` records requested work at admission, before cancellation
    /// or helper loss can prevent execution from selecting its first phase.
    pub(super) fn initial_report(&self) -> FirmwareFlashReport {
        FirmwareFlashReport {
            flashed: false,
            reset: self.options.reset.then_some(false),
            verified_image: self.options.verify_image.then_some(false),
            verified_version: self.options.verify_version.then_some(false),
            observed_version: None,
            expected_version: Some(self.target.version.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreparedArtifact {
    /// Relative to the owned card directory, with no parent components.
    pub(super) path: PathBuf,
    pub(super) sha256: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreparedInputs {
    pub(super) image: PreparedArtifact,
    pub(super) configuration: Option<PreparedArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalRecord {
    pub(super) operation: FirmwareOperation,
    pub(super) request_sha256: [u8; 32],
    pub(super) boot_id: String,
    pub(super) requested: Option<RequestedFirmware>,
    pub(super) observer_exit_behavior: ObserverExitBehavior,
    pub(super) prepared_inputs: Option<PreparedInputs>,
    /// Set before the first possible hardware effect and never cleared. A
    /// canceled operation is not fresh-start permission after this point.
    pub(super) mutation_started: bool,
}

impl JournalRecord {
    /// Completion or a settled failure/cancellation before mutation permits new
    /// work. Interrupted and uncertain work still needs explicit reconciliation.
    pub(super) fn is_safe_to_release(&self) -> bool {
        self.operation.is_complete()
            || (matches!(
                self.operation.state,
                FirmwareOperationState::Canceled | FirmwareOperationState::Failed
            ) && !self.mutation_started)
    }

    /// Reject inconsistent completion and unsafe staged paths on both updates
    /// and reload, so valid JSON alone cannot authorize new device work.
    fn validate(&self) -> Result<(), JournalError> {
        if self.operation.state == FirmwareOperationState::Complete {
            if !self.operation.is_complete() {
                return Err(JournalError::InvalidRecord(
                    "completed operation reports an unsuccessful phase",
                ));
            }
            if let Some(requested) = &self.requested {
                let report = &self.operation.report;
                if !report.flashed
                    || (requested.options.reset && report.reset != Some(true))
                    || (requested.options.verify_image && report.verified_image != Some(true))
                    || (requested.options.verify_version && report.verified_version != Some(true))
                {
                    return Err(JournalError::InvalidRecord(
                        "completed operation has not finished every requested phase",
                    ));
                }
            }
        }
        if let Some(inputs) = &self.prepared_inputs {
            for artifact in std::iter::once(&inputs.image).chain(inputs.configuration.iter()) {
                if artifact.path.as_os_str().is_empty()
                    || !artifact
                        .path
                        .components()
                        .all(|component| matches!(component, Component::Normal(_)))
                {
                    return Err(JournalError::InvalidArtifactPath(artifact.path.clone()));
                }
            }
        }
        if self
            .operation
            .diagnostic
            .as_ref()
            .is_some_and(|diagnostic| diagnostic.len() > MAX_DIAGNOSTIC_BYTES)
        {
            return Err(JournalError::InvalidRecord("diagnostic exceeds 2048 bytes"));
        }
        Ok(())
    }
}

/// A phase update cannot replace the request fingerprint, target, or boot ID.
pub(super) struct OperationUpdate {
    pub(super) state: FirmwareOperationState,
    pub(super) phase: Option<FirmwareOperationPhase>,
    pub(super) report: FirmwareFlashReport,
    pub(super) diagnostic: Option<String>,
    /// `None` retains any already recorded staged inputs.
    pub(super) prepared_inputs: Option<PreparedInputs>,
    /// This flag is combined with the saved value, so an update cannot erase
    /// the fact that a mutation may have started.
    pub(super) mutation_started: bool,
}

#[derive(Debug)]
pub(super) enum Admission {
    Accepted(FirmwareOperation),
    Existing(FirmwareOperation),
}

#[derive(Debug, Error)]
pub(super) enum JournalError {
    #[error("firmware journal task failed: {0}")]
    Blocking(#[from] tokio::task::JoinError),
    #[error("failed to {operation} firmware journal {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid firmware journal JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported firmware journal schema {0}")]
    UnsupportedSchema(u32),
    #[error("firmware journal is not private or has an unsupported file type: {0}")]
    UnsafePath(PathBuf),
    #[error("another firmware helper owns the card journal: {0}")]
    AlreadyOwned(PathBuf),
    #[error("established card journal is missing required state: {0}")]
    MissingState(PathBuf),
    #[error("firmware journal belongs to a different execution device")]
    ExecutionDeviceMismatch,
    #[error("firmware journal belongs to a different boot; reconciliation is required")]
    BootMismatch,
    #[error("firmware operation ID already identifies a different request: {0}")]
    ConflictingRequest(String),
    #[error("firmware operation requires reconciliation before admitting new work: {0}")]
    UnresolvedPredecessor(String),
    #[error("firmware journal has reached its 128-receipt limit")]
    ReceiptLimit,
    #[error("firmware journal exceeds its 1 MiB size limit")]
    SizeLimit,
    #[error("invalid firmware journal record: {0}")]
    InvalidRecord(&'static str),
    #[error("staged artifact path must be relative within the card directory: {0}")]
    InvalidArtifactPath(PathBuf),
    #[error("firmware operation is not recorded: {0}")]
    NotFound(String),
    #[error("firmware journal must be reopened after an uncertain save")]
    ReloadRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    execution_device: String,
    boot_id: String,
    records: Vec<JournalRecord>,
}

impl Ledger {
    /// Enforce the encoded storage limit before an admission claim or file
    /// replacement can turn an oversized request into retained work.
    fn encode_bounded(&self) -> Result<Vec<u8>, JournalError> {
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_LEDGER_BYTES {
            return Err(JournalError::SizeLimit);
        }
        Ok(encoded)
    }
}

/// `OperationJournal` holds one helper's exclusive ledger lock. Neither the
/// lock nor a PID establishes that previous device work has stopped. No record
/// is automatically evicted, including completion.
pub(super) struct OperationJournal {
    path: PathBuf,
    directory: File,
    current_boot_id: String,
    ledger: Ledger,
    reload_required: watch::Sender<bool>,
    // Keep ownership for the journal's lifetime. Never unlink the
    // lock file: two helpers could otherwise lock different inodes by one name.
    _owner_lock: OwnerLock,
}

/// `OwnerLock` releases journal ownership even if a forked child still holds
/// the descriptor before exec. Closing our descriptor alone would retain the
/// lock until that child closes its copy. Durable receipts still block replay.
struct OwnerLock {
    file: File,
    path: PathBuf,
}

impl Drop for OwnerLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!(path = %self.path.display(), %error, "failed to release firmware journal owner lock");
        }
    }
}

impl OperationJournal {
    /// Acquire ownership before reading receipts, or create a new private card
    /// directory and persist its empty ledger. A pre-existing directory without
    /// its lock or ledger requires repair, not fresh admission.
    ///
    /// The caller supplies a validated full PCI BDF. Another function sharing
    /// the slot directory is rejected before receipt recovery.
    ///
    /// Reopening marks unfinished same-boot work as interrupted without running
    /// it again. A different boot remains readable, but every mutation is
    /// rejected so reboot cannot silently authorize another burn.
    pub(super) fn open(
        directory: &Path,
        execution_device: &str,
        boot_id: &str,
    ) -> Result<Self, JournalError> {
        let created = match DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(source) => return Err(io_error("create directory", directory, source)),
        };
        let directory_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory)
            .map_err(|source| io_error("open directory", directory, source))?;
        let metadata = directory_file
            .metadata()
            .map_err(|source| io_error("inspect directory", directory, source))?;
        if !metadata.is_dir()
            || metadata.permissions().mode() & 0o7777 != 0o700
            || metadata.uid() != nix::unistd::geteuid().as_raw()
        {
            return Err(JournalError::UnsafePath(directory.to_path_buf()));
        }
        if created {
            // Syncing the ledger's directory later persists its contents, but
            // the new directory's own entry belongs to its parent.
            let parent = directory
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(parent)
                .and_then(|parent| parent.sync_all())
                .map_err(|source| io_error("sync new directory parent", parent, source))?;
        }

        let owner_lock = lock_owner(directory, created)?;
        let path = directory.join(LEDGER_FILENAME);
        let ledger = match read_ledger(&path)? {
            Some(ledger) => ledger,
            None if created => Ledger {
                schema_version: SCHEMA_VERSION,
                execution_device: execution_device.to_string(),
                boot_id: boot_id.to_string(),
                records: Vec::new(),
            },
            None => return Err(JournalError::MissingState(path)),
        };
        validate_ledger(&ledger, execution_device)?;
        let mut journal = Self {
            path,
            directory: directory_file,
            current_boot_id: boot_id.to_string(),
            ledger,
            reload_required: watch::channel(false).0,
            _owner_lock: owner_lock,
        };
        if created {
            journal.save(journal.ledger.clone())?;
        } else if journal.current_boot_id == journal.ledger.boot_id {
            journal.recover_interrupted()?;
        }
        Ok(journal)
    }

    /// Record helper loss without assuming the previous tool process family
    /// has exited. Even accepted work with no recorded mutation remains blocked
    /// until its caller establishes an explicit recovery decision.
    fn recover_interrupted(&mut self) -> Result<(), JournalError> {
        self.ensure_writable()?;
        let mut next = self.ledger.clone();
        let mut changed = false;
        for record in &mut next.records {
            if !matches!(
                record.operation.state,
                FirmwareOperationState::Accepted
                    | FirmwareOperationState::Running
                    | FirmwareOperationState::CancelRequested
            ) {
                continue;
            }
            changed = true;
            record.operation.state = if record.mutation_started {
                FirmwareOperationState::ManualIntervention
            } else {
                FirmwareOperationState::Interrupted
            };
            record.operation.diagnostic = Some(
                "previous firmware helper exited without completing the operation; reconcile the card before further mutations"
                    .to_string(),
            );
        }
        if changed {
            self.save(next)?;
        }
        Ok(())
    }

    /// Check every deterministic admission condition without changing the
    /// journal. `Some` retrieves an existing receipt; `None` permits admission.
    /// The caller must keep its journal mutex through this check, acquisition
    /// of the host mutation claim, and `admit` so rejected input cannot strand
    /// a claim. Actual save errors remain uncertain and must retain that claim.
    pub(super) fn check_admission(
        &self,
        record: &JournalRecord,
    ) -> Result<Option<FirmwareOperation>, JournalError> {
        self.ensure_writable()?;
        if let Some(existing) = self.record(&record.operation.operation_id)? {
            if existing.request_sha256 != record.request_sha256 {
                return Err(JournalError::ConflictingRequest(
                    record.operation.operation_id.clone(),
                ));
            }
            return Ok(Some(existing.operation.clone()));
        }
        if let Some(predecessor) = self
            .ledger
            .records
            .iter()
            .find(|row| !row.is_safe_to_release())
        {
            return Err(JournalError::UnresolvedPredecessor(
                predecessor.operation.operation_id.clone(),
            ));
        }
        if self.ledger.records.len() == MAX_RECEIPTS {
            return Err(JournalError::ReceiptLimit);
        }
        if record.boot_id != self.current_boot_id {
            return Err(JournalError::BootMismatch);
        }
        if record.operation.operation_id.is_empty()
            || record.operation.state != FirmwareOperationState::Accepted
            || record.operation.phase.is_some()
            || record.prepared_inputs.is_some()
            || record.mutation_started
        {
            return Err(JournalError::InvalidRecord(
                "admission must precede execution",
            ));
        }
        let mut normalized = record.clone();
        truncate_diagnostic(&mut normalized.operation.diagnostic);
        normalized.validate()?;
        let mut next = self.ledger.clone();
        next.records.push(normalized);
        drop(next.encode_bounded()?);
        Ok(None)
    }

    /// Save admission before returning `Accepted`. A lost reply can be retried
    /// with the same ID and fingerprint without adding a second operation.
    pub(super) fn admit(&mut self, mut record: JournalRecord) -> Result<Admission, JournalError> {
        if let Some(existing) = self.check_admission(&record)? {
            return Ok(Admission::Existing(existing));
        }
        truncate_diagnostic(&mut record.operation.diagnostic);
        let operation = record.operation.clone();
        let mut next = self.ledger.clone();
        next.records.push(record);
        self.save(next)?;
        Ok(Admission::Accepted(operation))
    }

    /// Persist a phase transition before its corresponding hardware effect.
    /// A save error may occur after rename: reopen before further mutation,
    /// rather than assuming either the old or proposed state is durable.
    pub(super) fn update(
        &mut self,
        operation_id: &str,
        mut update: OperationUpdate,
    ) -> Result<FirmwareOperation, JournalError> {
        self.ensure_writable()?;
        let mut next = self.ledger.clone();
        let record = next
            .records
            .iter_mut()
            .find(|record| record.operation.operation_id == operation_id)
            .ok_or_else(|| JournalError::NotFound(operation_id.to_string()))?;
        if matches!(
            record.operation.state,
            FirmwareOperationState::Interrupted | FirmwareOperationState::ManualIntervention
        ) && !matches!(
            update.state,
            FirmwareOperationState::Interrupted | FirmwareOperationState::ManualIntervention
        ) {
            return Err(JournalError::UnresolvedPredecessor(
                operation_id.to_string(),
            ));
        }
        truncate_diagnostic(&mut update.diagnostic);
        record.operation.state = update.state;
        record.operation.phase = update.phase;
        record.operation.report = update.report;
        record.operation.diagnostic = update.diagnostic;
        if let Some(inputs) = update.prepared_inputs {
            record.prepared_inputs = Some(inputs);
        }
        record.mutation_started |= update.mutation_started;
        record.validate()?;
        let operation = record.operation.clone();
        self.save(next)?;
        Ok(operation)
    }

    /// `record` returns saved request and progress together. An uncertain save
    /// may have replaced the file, so the cached receipt cannot answer reads
    /// until the journal is reopened. Earlier-boot receipts remain readable.
    pub(super) fn record(
        &self,
        operation_id: &str,
    ) -> Result<Option<&JournalRecord>, JournalError> {
        self.ensure_readable()?;
        Ok(self
            .ledger
            .records
            .iter()
            .find(|record| record.operation.operation_id == operation_id))
    }

    /// Copy the public receipt so a status response need not retain the
    /// journal lock or expose its internal recovery evidence.
    pub(super) fn snapshot(
        &self,
        operation_id: &str,
    ) -> Result<Option<FirmwareOperation>, JournalError> {
        Ok(self
            .record(operation_id)?
            .map(|record| record.operation.clone()))
    }

    /// A failed save fences this handle until reopen and wakes execution that
    /// is waiting without a command in flight. It is not accepted cancellation.
    pub(super) fn subscribe_reload_required(&self) -> watch::Receiver<bool> {
        self.reload_required.subscribe()
    }

    fn ensure_readable(&self) -> Result<(), JournalError> {
        if *self.reload_required.borrow() {
            return Err(JournalError::ReloadRequired);
        }
        Ok(())
    }

    /// A lost durability answer or changed boot must be reconciled before any
    /// subsequent write can authorize more execution.
    fn ensure_writable(&self) -> Result<(), JournalError> {
        self.ensure_readable()?;
        if self.current_boot_id != self.ledger.boot_id {
            return Err(JournalError::BootMismatch);
        }
        Ok(())
    }

    /// An `O_PATH` descriptor permits replacement but makes the subsequent
    /// directory sync fail, exercising uncertainty after a real rename.
    #[cfg(test)]
    pub(super) fn fail_directory_sync(&mut self) {
        self.directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(self.path.parent().unwrap())
            .unwrap();
    }

    /// Replace the in-memory ledger only after its replacement is durable;
    /// an uncertain I/O failure disables further writes on this handle.
    fn save(&mut self, next: Ledger) -> Result<(), JournalError> {
        let encoded = next.encode_bounded()?;
        // An I/O failure leaves no durable acceptance answer. Publish the
        // sticky fence only after failure, never while a successful save runs.
        if let Err(error) = persist_ledger(&self.path, &self.directory, &encoded) {
            self.reload_required.send_replace(true);
            return Err(error);
        }
        self.ledger = next;
        Ok(())
    }
}

/// Keep a fixed lock inode for every helper incarnation. This excludes ledger
/// writers without claiming ownership of tools left by a previous helper.
fn lock_owner(directory: &Path, created: bool) -> Result<OwnerLock, JournalError> {
    let path = directory.join(OWNER_LOCK_FILENAME);
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    if created {
        options.create_new(true);
    }
    let owner = options.open(&path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            JournalError::MissingState(path.clone())
        } else {
            io_error("open owner lock", &path, source)
        }
    })?;
    let metadata = owner
        .metadata()
        .map_err(|source| io_error("inspect owner lock", &path, source))?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.uid() != nix::unistd::geteuid().as_raw()
    {
        return Err(JournalError::UnsafePath(path));
    }
    match owner.try_lock() {
        Ok(()) => Ok(OwnerLock { file: owner, path }),
        Err(TryLockError::WouldBlock) => Err(JournalError::AlreadyOwned(path)),
        Err(TryLockError::Error(source)) => Err(io_error("acquire owner lock", &path, source)),
    }
}

/// Distinguish an absent file from corrupt, oversized, or redirected state;
/// only the directory creator may interpret absence as a new ledger.
fn read_ledger(path: &Path) -> Result<Option<Ledger>, JournalError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(JournalError::UnsafePath(path.to_path_buf()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("inspect", path, source)),
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| io_error("open", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect opened file", path, source))?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.uid() != nix::unistd::geteuid().as_raw()
    {
        return Err(JournalError::UnsafePath(path.to_path_buf()));
    }
    if metadata.len() > MAX_LEDGER_BYTES as u64 {
        return Err(JournalError::SizeLimit);
    }
    let mut encoded = Vec::new();
    file.take((MAX_LEDGER_BYTES + 1) as u64)
        .read_to_end(&mut encoded)
        .map_err(|source| io_error("read", path, source))?;
    if encoded.len() > MAX_LEDGER_BYTES {
        return Err(JournalError::SizeLimit);
    }
    Ok(Some(serde_json::from_slice(&encoded)?))
}

/// Verify schema and execution device before trusting any receipt, including
/// uniqueness of operation IDs and the shared boot identity.
fn validate_ledger(ledger: &Ledger, execution_device: &str) -> Result<(), JournalError> {
    if ledger.schema_version != SCHEMA_VERSION {
        return Err(JournalError::UnsupportedSchema(ledger.schema_version));
    }
    if ledger.execution_device != execution_device {
        return Err(JournalError::ExecutionDeviceMismatch);
    }
    if ledger.records.len() > MAX_RECEIPTS {
        return Err(JournalError::ReceiptLimit);
    }
    for (index, record) in ledger.records.iter().enumerate() {
        record.validate()?;
        if record.boot_id != ledger.boot_id || record.operation.operation_id.is_empty() {
            return Err(JournalError::InvalidRecord(
                "missing identity or inconsistent boot ID",
            ));
        }
        if ledger.records[..index]
            .iter()
            .any(|previous| previous.operation.operation_id == record.operation.operation_id)
        {
            return Err(JournalError::InvalidRecord("duplicate operation ID"));
        }
    }
    Ok(())
}

/// Sync a private replacement before rename, then sync the directory entry.
/// A final sync error can follow a successful rename and is therefore uncertain.
fn persist_ledger(path: &Path, directory: &File, encoded: &[u8]) -> Result<(), JournalError> {
    let parent = path
        .parent()
        .ok_or(JournalError::InvalidRecord("journal path has no directory"))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".operations-")
        .tempfile_in(parent)
        .map_err(|source| io_error("create replacement", path, source))?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error("set replacement permissions", path, source))?;
    temporary
        .write_all(encoded)
        .map_err(|source| io_error("write replacement", path, source))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| io_error("sync replacement", path, source))?;
    // Recheck before replacement so an existing nonregular entry is never
    // deliberately removed. The private directory belongs to the scope owner.
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.permissions().mode() & 0o7777 != 0o600
                || metadata.uid() != nix::unistd::geteuid().as_raw() =>
        {
            return Err(JournalError::UnsafePath(path.to_path_buf()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error("inspect replacement destination", path, source)),
    }
    temporary
        .persist(path)
        .map_err(|error| io_error("replace", path, error.error))?;
    directory
        .sync_all()
        .map_err(|source| io_error("sync directory", path, source))?;
    Ok(())
}

/// Bound retained diagnostics by bytes without cutting a UTF-8 character.
fn truncate_diagnostic(diagnostic: &mut Option<String>) {
    if let Some(diagnostic) = diagnostic {
        let mut end = diagnostic.len().min(MAX_DIAGNOSTIC_BYTES);
        while !diagnostic.is_char_boundary(end) {
            end -= 1;
        }
        diagnostic.truncate(end);
    }
}

/// Preserve the I/O source alongside the failed journal step and path.
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> JournalError {
    JournalError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, symlink};

    use super::*;

    const CARD: &str = "0000:01:00.0";
    const BOOT: &str = "test-boot";

    /// A no-work request keeps admission tests independent of firmware sources
    /// and proves that no fabricated target is needed in the persisted record.
    fn request(operation_id: &str) -> JournalRecord {
        JournalRecord {
            operation: FirmwareOperation {
                operation_id: operation_id.to_string(),
                state: FirmwareOperationState::Accepted,
                phase: None,
                report: FirmwareFlashReport::default(),
                diagnostic: None,
            },
            request_sha256: [1; 32],
            boot_id: BOOT.to_string(),
            requested: None,
            observer_exit_behavior: ObserverExitBehavior::Continue,
            prepared_inputs: None,
            mutation_started: false,
        }
    }

    /// Start a test transition without staged inputs or hardware effects;
    /// each scenario adds only the evidence relevant to its phase.
    fn transition(state: FirmwareOperationState) -> OperationUpdate {
        OperationUpdate {
            state,
            phase: None,
            report: FirmwareFlashReport::default(),
            diagnostic: None,
            prepared_inputs: None,
            mutation_started: false,
        }
    }

    /// A failed save fences later writes whether replacement happened or not.
    /// Real filesystem errors distinguish cached state from recoverable bytes.
    #[test]
    fn failed_save_before_or_after_replace_requires_reopen() {
        for after_replace in [false, true] {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let mut journal = OperationJournal::open(&directory, CARD, BOOT).unwrap();
            journal.admit(request("operation")).unwrap();
            if after_replace {
                journal.fail_directory_sync();
            } else {
                fs::set_permissions(&journal.path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            let mut intent = transition(FirmwareOperationState::Running);
            intent.phase = Some(FirmwareOperationPhase::Burn);
            intent.mutation_started = true;
            let failure = journal.update("operation", intent).unwrap_err();
            if after_replace {
                assert!(
                    matches!(failure, JournalError::Io { operation: "sync directory", source, .. }
                    if source.raw_os_error() == Some(libc::EBADF))
                );
            } else {
                assert!(matches!(failure, JournalError::UnsafePath(_)));
                fs::set_permissions(&journal.path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(matches!(
                journal.snapshot("operation"),
                Err(JournalError::ReloadRequired)
            ));
            assert!(matches!(
                journal.record("missing"),
                Err(JournalError::ReloadRequired)
            ));
            assert!(matches!(
                journal.update("operation", transition(FirmwareOperationState::Complete)),
                Err(JournalError::ReloadRequired)
            ));
            assert!(matches!(
                journal.admit(request("next")),
                Err(JournalError::ReloadRequired)
            ));
            let saved: Ledger = serde_json::from_slice(&fs::read(&journal.path).unwrap()).unwrap();
            assert_eq!(saved.records[0].mutation_started, after_replace);
            assert_eq!(
                saved.records[0].operation.phase,
                after_replace.then_some(FirmwareOperationPhase::Burn)
            );
            drop(journal);

            let reopened = OperationJournal::open(&directory, CARD, BOOT).unwrap();
            assert_eq!(
                reopened.snapshot("operation").unwrap().unwrap().state,
                if after_replace {
                    FirmwareOperationState::ManualIntervention
                } else {
                    FirmwareOperationState::Interrupted
                }
            );
            assert!(reopened.record("next").unwrap().is_none());
        }
    }

    /// A lost acceptance reply must retrieve the same receipt, and neither a
    /// new fingerprint nor a new ID can bypass unresolved device work.
    #[test]
    fn duplicate_requests_do_not_admit_conflicting_or_unresolved_work() {
        let temporary = tempfile::tempdir().expect("create fixture directory");
        let directory = temporary.path().join("card");
        let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
        let empty = fs::read(&journal.path).expect("read empty ledger");
        assert!(
            journal
                .check_admission(&request("first"))
                .expect("check new request")
                .is_none()
        );
        assert!(journal.snapshot("first").unwrap().is_none());
        assert_eq!(fs::read(&journal.path).expect("read checked ledger"), empty);
        let Admission::Accepted(accepted) = journal.admit(request("first")).expect("admit first")
        else {
            panic!("first submission should be admitted");
        };
        assert_eq!(accepted.operation_id, "first");
        assert_eq!(accepted.state, FirmwareOperationState::Accepted);
        let before = fs::read(directory.join(LEDGER_FILENAME)).expect("read accepted receipt");

        let Admission::Existing(existing) =
            journal.admit(request("first")).expect("repeat request")
        else {
            panic!("repeat should retrieve the existing receipt");
        };
        assert_eq!(existing.operation_id, accepted.operation_id);
        assert_eq!(existing.state, accepted.state);
        assert_eq!(
            journal
                .check_admission(&request("first"))
                .expect("check duplicate")
                .expect("existing receipt")
                .operation_id,
            accepted.operation_id
        );
        let mut conflicting = request("first");
        conflicting.request_sha256 = [2; 32];
        assert!(matches!(
            journal.check_admission(&conflicting),
            Err(JournalError::ConflictingRequest(id)) if id == "first"
        ));
        assert!(matches!(
            journal.admit(conflicting),
            Err(JournalError::ConflictingRequest(id)) if id == "first"
        ));
        assert!(matches!(
            journal.check_admission(&request("second")),
            Err(JournalError::UnresolvedPredecessor(id)) if id == "first"
        ));
        assert!(matches!(
            journal.admit(request("second")),
            Err(JournalError::UnresolvedPredecessor(id)) if id == "first"
        ));
        assert_eq!(fs::read(&journal.path).expect("reread receipt"), before);

        // Cancellation before mutation leaves no effects to reconcile. Once a
        // mutation may have started, cancellation cannot erase that evidence.
        journal
            .update("first", transition(FirmwareOperationState::Canceled))
            .expect("cancel before mutation");
        journal
            .admit(request("second"))
            .expect("admit after safe cancel");
        let mut burn = transition(FirmwareOperationState::Running);
        burn.phase = Some(FirmwareOperationPhase::Burn);
        burn.mutation_started = true;
        journal.update("second", burn).expect("record burn intent");
        journal
            .update("second", transition(FirmwareOperationState::Canceled))
            .expect("record cancellation after mutation");
        assert!(
            journal
                .record("second")
                .unwrap()
                .expect("second receipt")
                .mutation_started
        );
        assert!(matches!(
            journal.admit(request("third")),
            Err(JournalError::UnresolvedPredecessor(id)) if id == "second"
        ));
    }

    /// A durable failure before mutation can release admission after restart;
    /// the same state must remain exclusive when mutation intent was recorded.
    #[test]
    fn reopened_failure_allows_new_work_only_before_mutation() {
        struct Case {
            label: &'static str,
            mutation_started: bool,
            allows_new_work: bool,
        }
        for case in [
            // A settled inspection/preparation failure has no hardware effects.
            Case {
                label: "failed before mutation",
                mutation_started: false,
                allows_new_work: true,
            },
            // A failed first command can retain intent even when launch failed;
            // do not turn that sticky evidence into permission to replay.
            Case {
                label: "failed with mutation intent",
                mutation_started: true,
                allows_new_work: false,
            },
        ] {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let mut journal = OperationJournal::open(&directory, CARD, BOOT).unwrap();
            journal.admit(request("failed")).unwrap();
            let mut failure = transition(FirmwareOperationState::Failed);
            failure.mutation_started = case.mutation_started;
            failure.diagnostic = Some(case.label.into());
            let failed = journal.update("failed", failure).unwrap();
            let saved = fs::read(&journal.path).unwrap();
            drop(journal);

            let mut reopened = OperationJournal::open(&directory, CARD, BOOT).unwrap();
            let Admission::Existing(repeated) = reopened.admit(request("failed")).unwrap() else {
                panic!("{} must retrieve its existing receipt", case.label);
            };
            assert_eq!(
                serde_json::to_value(repeated).unwrap(),
                serde_json::to_value(failed).unwrap(),
                "{} preserves its failure",
                case.label,
            );
            assert_eq!(fs::read(&reopened.path).unwrap(), saved);
            match reopened.admit(request("next")) {
                Ok(Admission::Accepted(next)) => {
                    assert!(case.allows_new_work, "{} remains exclusive", case.label);
                    assert_eq!(next.operation_id, "next");
                }
                Err(JournalError::UnresolvedPredecessor(operation_id)) => {
                    assert!(
                        !case.allows_new_work,
                        "{} may release admission",
                        case.label
                    );
                    assert_eq!(operation_id, "failed");
                    assert!(reopened.snapshot("next").unwrap().is_none());
                    assert_eq!(fs::read(&reopened.path).unwrap(), saved);
                }
                result => panic!("{} returned {result:?}", case.label),
            }
        }
    }

    /// Reload uses the committed receipt, including staged inputs and exact
    /// target options, rather than reconstructing progress from tool versions.
    #[test]
    fn atomic_save_reloads_private_receipts_and_relative_inputs() {
        let temporary = tempfile::tempdir().expect("create fixture directory");
        let directory = temporary.path().join("card");
        let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
        let mut submitted = request("operation");
        submitted.requested = Some(RequestedFirmware {
            target: FirmwareSpec {
                part_number: "PN-1".to_string(),
                psid: "PSID-1".to_string(),
                version: "32.43.1014".to_string(),
            },
            options: FlashOptions {
                reset: true,
                ..FlashOptions::default()
            },
        });
        journal.admit(submitted).expect("admit target");
        // A completed receipt reloads unchanged; interruption recovery has its
        // own test below, so it cannot hide loss of these saved fields.
        let mut prepared = transition(FirmwareOperationState::Complete);
        prepared.phase = Some(FirmwareOperationPhase::Activate);
        prepared.report.flashed = true;
        prepared.report.reset = Some(true);
        prepared.mutation_started = true;
        prepared.prepared_inputs = Some(PreparedInputs {
            image: PreparedArtifact {
                path: PathBuf::from("operation/image.bin"),
                sha256: [3; 32],
            },
            configuration: None,
        });
        // The limit is bytes, not characters; truncation must remain valid UTF-8.
        prepared.diagnostic = Some("あ".repeat(1000));
        journal
            .update("operation", prepared)
            .expect("record prepared input");
        let expected =
            serde_json::to_value(journal.record("operation").unwrap()).expect("encode expected");
        drop(journal);

        let loaded = OperationJournal::open(&directory, CARD, BOOT).expect("reload journal");
        assert_eq!(
            serde_json::to_value(loaded.record("operation").unwrap()).expect("encode loaded"),
            expected
        );
        assert_eq!(
            loaded
                .snapshot("operation")
                .unwrap()
                .expect("loaded receipt")
                .diagnostic,
            Some("あ".repeat(MAX_DIAGNOSTIC_BYTES / 3))
        );
        assert_eq!(
            fs::metadata(&directory)
                .expect("directory mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&loaded.path)
                .expect("ledger mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// Persisted data from a new schema, a corrupt write, or a redirected file
    /// cannot be interpreted as an empty ledger and permit another burn.
    #[test]
    fn invalid_persisted_ledgers_fail_closed() {
        enum InvalidLedger {
            Malformed,
            UnsupportedSchema,
            UnknownField,
            Oversized,
            DuplicateId,
            InconsistentBoot,
            Symlink,
            Directory,
        }
        let cases = [
            // A truncated file is not equivalent to an absent file.
            ("malformed JSON", InvalidLedger::Malformed),
            // A newer writer's semantics must not be guessed by this reader.
            ("new schema", InvalidLedger::UnsupportedSchema),
            // Unknown record fields may encode recovery requirements.
            ("unknown record field", InvalidLedger::UnknownField),
            ("oversized stored file", InvalidLedger::Oversized),
            ("duplicate operation ID", InvalidLedger::DuplicateId),
            ("record from another boot", InvalidLedger::InconsistentBoot),
            // Reading through a link could select another card's ledger.
            ("symbolic link", InvalidLedger::Symlink),
            // A nonregular destination must never be replaced as a file.
            ("directory", InvalidLedger::Directory),
        ];
        for (name, case) in cases {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
            journal.admit(request("operation")).expect("admit fixture");
            let path = journal.path.clone();
            match case {
                InvalidLedger::Malformed => fs::write(&path, b"{").expect("truncate JSON fixture"),
                InvalidLedger::UnsupportedSchema => {
                    let mut ledger = journal.ledger.clone();
                    ledger.schema_version += 1;
                    fs::write(
                        &path,
                        serde_json::to_vec(&ledger).expect("encode new schema"),
                    )
                    .expect("write new schema fixture");
                }
                InvalidLedger::UnknownField => {
                    let mut value = serde_json::to_value(&journal.ledger).expect("encode ledger");
                    value["records"][0]["future_recovery"] = serde_json::json!(true);
                    fs::write(
                        &path,
                        serde_json::to_vec(&value).expect("encode unknown field"),
                    )
                    .expect("write unknown field fixture");
                }
                InvalidLedger::Oversized => {
                    fs::write(&path, vec![b' '; MAX_LEDGER_BYTES + 1]).unwrap();
                }
                InvalidLedger::DuplicateId | InvalidLedger::InconsistentBoot => {
                    let mut ledger = journal.ledger.clone();
                    if matches!(case, InvalidLedger::DuplicateId) {
                        ledger.records.push(ledger.records[0].clone());
                    } else {
                        ledger.records[0].boot_id = "another-boot".into();
                    }
                    fs::write(&path, serde_json::to_vec(&ledger).unwrap()).unwrap();
                }
                InvalidLedger::Symlink => {
                    let target = temporary.path().join("other-card.json");
                    fs::rename(&path, &target).expect("move fixture receipt");
                    symlink(target, &path).expect("redirect fixture ledger");
                }
                InvalidLedger::Directory => {
                    fs::remove_file(&path).expect("remove fixture receipt");
                    fs::create_dir(&path).expect("replace fixture with directory");
                }
            }
            drop(journal);
            let error = match OperationJournal::open(&directory, CARD, BOOT) {
                Ok(_) => panic!("{name}: invalid ledger was accepted"),
                Err(error) => error,
            };
            match case {
                InvalidLedger::Malformed | InvalidLedger::UnknownField => {
                    assert!(matches!(error, JournalError::Json(_)), "{name}: {error}");
                }
                InvalidLedger::UnsupportedSchema => {
                    assert!(
                        matches!(error, JournalError::UnsupportedSchema(2)),
                        "{name}: {error}"
                    );
                }
                InvalidLedger::Oversized => {
                    assert!(matches!(error, JournalError::SizeLimit), "{name}: {error}");
                }
                InvalidLedger::DuplicateId | InvalidLedger::InconsistentBoot => {
                    assert!(
                        matches!(error, JournalError::InvalidRecord(_)),
                        "{name}: {error}"
                    );
                }
                InvalidLedger::Symlink | InvalidLedger::Directory => {
                    assert!(
                        matches!(error, JournalError::UnsafePath(_)),
                        "{name}: {error}"
                    );
                }
            }
        }
    }

    /// Capacity rejection must preserve the last durable receipt. Old receipts
    /// are never evicted to make a repeated operation look new.
    #[test]
    fn admission_bounds_preserve_existing_receipts() {
        for receipt_limit in [true, false] {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
            let mut ledger = journal.ledger.clone();
            // Seed completed history directly: repeating the same save loop
            // adds no proof of the two distinct admission limits.
            let count = if receipt_limit { MAX_RECEIPTS } else { 1 };
            ledger.records = (0..count)
                .map(|index| {
                    let mut record = request(&format!("completed-{index}"));
                    record.operation.state = FirmwareOperationState::Complete;
                    record
                })
                .collect();
            journal.save(ledger).expect("persist completed history");
            let before = fs::read(&journal.path).expect("read history");
            let mut candidate = request("next");
            if !receipt_limit {
                // An oversized target tests encoded bytes independently of
                // record count and bounded diagnostic text.
                candidate.requested = Some(RequestedFirmware {
                    target: FirmwareSpec {
                        part_number: "PN".to_string(),
                        psid: "PSID".to_string(),
                        version: "v".repeat(MAX_LEDGER_BYTES),
                    },
                    options: FlashOptions::default(),
                });
            }
            let error = journal
                .check_admission(&candidate)
                .expect_err("reject exhausted capacity before persistence");
            if receipt_limit {
                assert!(matches!(error, JournalError::ReceiptLimit));
            } else {
                assert!(matches!(error, JournalError::SizeLimit));
            }
            let error = journal
                .admit(candidate)
                .expect_err("reject exhausted capacity");
            if receipt_limit {
                assert!(matches!(error, JournalError::ReceiptLimit));
            } else {
                assert!(matches!(error, JournalError::SizeLimit));
            }
            assert!(journal.snapshot("next").unwrap().is_none());
            assert_eq!(fs::read(&journal.path).expect("reread history"), before);
            drop(journal);
            let loaded = OperationJournal::open(&directory, CARD, BOOT).expect("reload history");
            assert_eq!(loaded.ledger.records.len(), count);
            assert_eq!(
                loaded
                    .snapshot("completed-0")
                    .unwrap()
                    .expect("preserved receipt")
                    .state,
                FirmwareOperationState::Complete
            );
        }
    }

    /// Boot identity is a mutation boundary even when previous work completed;
    /// a retained ledger cannot establish the new boot's hardware state.
    #[test]
    fn different_boot_retains_receipts_but_rejects_writes() {
        let temporary = tempfile::tempdir().expect("create fixture directory");
        let directory = temporary.path().join("card");
        let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
        journal.admit(request("operation")).expect("admit fixture");
        journal
            .update("operation", transition(FirmwareOperationState::Complete))
            .expect("complete fixture");
        let before = fs::read(&journal.path).expect("read receipt");
        drop(journal);
        let mut recovered =
            OperationJournal::open(&directory, CARD, "later-boot").expect("read old boot");
        assert_eq!(
            recovered
                .snapshot("operation")
                .unwrap()
                .expect("old receipt")
                .state,
            FirmwareOperationState::Complete
        );
        assert!(matches!(
            recovered.admit(request("operation")),
            Err(JournalError::BootMismatch)
        ));
        assert!(matches!(
            recovered.update("operation", transition(FirmwareOperationState::Running)),
            Err(JournalError::BootMismatch)
        ));
        assert_eq!(
            fs::read(&recovered.path).expect("reread old receipt"),
            before
        );
    }

    /// A staged artifact cannot escape the card directory through either an
    /// absolute path or parent traversal, even when the request was admitted.
    #[test]
    fn unsafe_artifact_paths_preserve_the_accepted_receipt() {
        let temporary = tempfile::tempdir().expect("create fixture directory");
        let directory = temporary.path().join("card");
        let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
        journal.admit(request("operation")).expect("admit fixture");
        let before = fs::read(&journal.path).expect("read receipt");
        for path in [
            // Absolute names would ignore the owned directory on join.
            PathBuf::from("/another-card/image.bin"),
            // Relative parent components would escape the same boundary.
            PathBuf::from("../another-card/image.bin"),
        ] {
            let mut prepared = transition(FirmwareOperationState::Running);
            prepared.prepared_inputs = Some(PreparedInputs {
                image: PreparedArtifact {
                    path: path.clone(),
                    sha256: [3; 32],
                },
                configuration: None,
            });
            assert!(
                matches!(journal.update("operation", prepared), Err(JournalError::InvalidArtifactPath(rejected)) if rejected == path)
            );
            assert_eq!(fs::read(&journal.path).expect("reread receipt"), before);
        }
    }

    /// Only one helper may read and change a card's ledger. Releasing that
    /// ownership never proves that a child firmware process has exited.
    #[test]
    fn exclusive_owner_lock_does_not_authorize_replay_after_helper_loss() {
        let temporary = tempfile::tempdir().expect("create fixture directory");
        let directory = temporary.path().join("card");
        let mut original = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
        original.admit(request("operation")).expect("admit fixture");
        let mut burn = transition(FirmwareOperationState::Running);
        burn.phase = Some(FirmwareOperationPhase::Burn);
        burn.mutation_started = true;
        original
            .update("operation", burn)
            .expect("record burn intent");
        let ledger_path = original.path.clone();
        let before = fs::read(&ledger_path).expect("read burn intent");
        let lock_path = directory.join(OWNER_LOCK_FILENAME);
        let lock_inode = fs::metadata(&lock_path).expect("inspect owner lock").ino();

        assert!(
            matches!(OperationJournal::open(&directory, CARD, BOOT), Err(JournalError::AlreadyOwned(path)) if path == lock_path)
        );
        assert_eq!(
            fs::read(&ledger_path).expect("read contested ledger"),
            before
        );

        // A concurrent fork retains the same open-file description until exec.
        // Keep a duplicate alive to make that lock-lifetime race deterministic.
        let inherited_owner = original
            ._owner_lock
            .file
            .try_clone()
            .expect("duplicate inherited owner descriptor");
        drop(original);
        let mut recovered =
            OperationJournal::open(&directory, CARD, BOOT).expect("recover receipt");
        // Closing the old description must not unlock the new owner's lock.
        drop(inherited_owner);
        assert!(matches!(
            OperationJournal::open(&directory, CARD, BOOT),
            Err(JournalError::AlreadyOwned(_))
        ));
        let record = recovered
            .record("operation")
            .unwrap()
            .expect("interrupted receipt");
        assert_eq!(
            record.operation.state,
            FirmwareOperationState::ManualIntervention
        );
        assert_eq!(record.operation.phase, Some(FirmwareOperationPhase::Burn));
        assert!(record.mutation_started);
        assert_eq!(
            fs::metadata(&lock_path)
                .expect("preserved owner lock")
                .ino(),
            lock_inode
        );
        let Admission::Existing(existing) = recovered
            .admit(request("operation"))
            .expect("retrieve interrupted receipt")
        else {
            panic!("the interrupted operation must not be admitted again");
        };
        assert_eq!(existing.state, FirmwareOperationState::ManualIntervention);
        assert!(
            matches!(recovered.admit(request("replacement")), Err(JournalError::UnresolvedPredecessor(id)) if id == "operation")
        );
        assert!(
            matches!(recovered.update("operation", transition(FirmwareOperationState::Running)), Err(JournalError::UnresolvedPredecessor(id)) if id == "operation")
        );

        let reconciled = fs::read(&ledger_path).expect("read recovery receipt");
        drop(recovered);
        let reopened =
            OperationJournal::open(&directory, CARD, BOOT).expect("reopen blocked receipt");
        assert_eq!(
            reopened
                .snapshot("operation")
                .unwrap()
                .expect("blocked receipt")
                .state,
            FirmwareOperationState::ManualIntervention
        );
        assert_eq!(
            fs::read(&ledger_path).expect("read unchanged recovery"),
            reconciled
        );
    }

    /// An accepted request is durable even if the helper dies before selecting
    /// its first phase; a lost start signal is not permission to replay it.
    #[test]
    fn accepted_receipt_becomes_interrupted_without_starting_work() {
        let temporary = tempfile::tempdir().expect("create fixture directory");
        let directory = temporary.path().join("card");
        let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
        journal.admit(request("operation")).expect("admit fixture");
        drop(journal);

        let mut recovered =
            OperationJournal::open(&directory, CARD, BOOT).expect("recover accepted work");
        let record = recovered
            .record("operation")
            .unwrap()
            .expect("interrupted receipt");
        assert_eq!(record.operation.state, FirmwareOperationState::Interrupted);
        assert_eq!(record.operation.phase, None);
        assert!(!record.mutation_started);
        assert!(
            matches!(recovered.admit(request("replacement")), Err(JournalError::UnresolvedPredecessor(id)) if id == "operation")
        );
    }

    /// Absence is safe only while creating the card directory. Missing state
    /// after that point could be accidental deletion of an unresolved receipt.
    #[test]
    fn existing_card_directory_cannot_be_reinitialized_after_state_loss() {
        for missing in [OWNER_LOCK_FILENAME, LEDGER_FILENAME] {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let journal =
                OperationJournal::open(&directory, CARD, BOOT).expect("create card ledger");
            drop(journal);
            let missing_path = directory.join(missing);
            fs::remove_file(&missing_path).expect("simulate missing persisted state");
            assert!(
                matches!(OperationJournal::open(&directory, CARD, BOOT), Err(JournalError::MissingState(path)) if path == missing_path)
            );
            assert!(
                !missing_path.exists(),
                "missing state must not be recreated"
            );
        }
    }

    /// Neither an in-memory transition nor a reopened file may advertise
    /// completion while a required phase has no successful result.
    #[test]
    fn incomplete_completion_preserves_admission_block_and_fails_reload() {
        for (scenario, options, report) in [
            (
                "required reset did not complete",
                FlashOptions {
                    reset: true,
                    ..FlashOptions::default()
                },
                FirmwareFlashReport {
                    flashed: true,
                    ..FirmwareFlashReport::default()
                },
            ),
            (
                "requested firmware was never burned",
                FlashOptions::default(),
                FirmwareFlashReport::default(),
            ),
        ] {
            let temporary = tempfile::tempdir().expect("create fixture directory");
            let directory = temporary.path().join("card");
            let mut journal = OperationJournal::open(&directory, CARD, BOOT).expect("open journal");
            let mut submitted = request("operation");
            submitted.requested = Some(RequestedFirmware {
                target: FirmwareSpec {
                    part_number: "PN".to_string(),
                    psid: "PSID".to_string(),
                    version: "target-version".to_string(),
                },
                options,
            });
            journal.admit(submitted).expect("admit firmware request");
            let before = fs::read(&journal.path).expect("read admitted receipt");
            let mut incomplete = transition(FirmwareOperationState::Complete);
            incomplete.report = report.clone();
            assert!(
                matches!(
                    journal.update("operation", incomplete),
                    Err(JournalError::InvalidRecord(_))
                ),
                "{scenario}"
            );
            assert_eq!(
                fs::read(&journal.path).expect("read rejected transition"),
                before
            );
            assert!(matches!(
                journal.check_admission(&request("replacement")),
                Err(JournalError::UnresolvedPredecessor(id)) if id == "operation"
            ));

            // Persisted state must pass the same check; valid JSON alone cannot
            // turn a partial operation into permission to admit another one.
            let mut invalid = journal.ledger.clone();
            invalid.records[0].operation.state = FirmwareOperationState::Complete;
            invalid.records[0].operation.report = report;
            fs::write(
                &journal.path,
                serde_json::to_vec(&invalid).expect("encode invalid completion"),
            )
            .expect("write corrupt fixture receipt");
            drop(journal);
            assert!(
                matches!(
                    OperationJournal::open(&directory, CARD, BOOT),
                    Err(JournalError::InvalidRecord(_))
                ),
                "{scenario}"
            );
        }
    }
}
