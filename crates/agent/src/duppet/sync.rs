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

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use colored::Colorize;
use nix::unistd::{Gid, Uid, chown};
use sha2::{Digest, Sha256};
use uzers::{get_group_by_name, get_user_by_name};

use super::log::{build_diff, maybe_colorize};
use super::{FileEnsure, FileSpec, SummaryFormat, SyncOptions, SyncStatus};

/// sync is the main entrypoint into duppet doing a sync. It
/// takes the hashmap of file output path and content, as well
/// as various sync options to use for the run.
pub fn sync(
    files: HashMap<PathBuf, FileSpec>,
    options: SyncOptions,
) -> io::Result<HashMap<PathBuf, SyncStatus>> {
    let summary = sync_files(files, &options)?;

    // Note that currently, a summary is ALWAYS printed, even if
    // --quiet is set (since --quiet is meant for silencing the
    // per-file logging). We might want to make this included in
    // --quiet.
    match options.summary_format {
        // Don't use tracing::info for JSON or YAML, otherwise it'd
        // prefix it with whatever tracing formatting is configured,
        // which is probably undesired in this case.
        SummaryFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&summary).unwrap());
        }
        SummaryFormat::Yaml => {
            println!("{}", serde_yaml::to_string(&summary).unwrap());
        }
        // Don't leverage the logln! macro for any of this, since
        // --quiet would mean this doesn't get printed at all.
        SummaryFormat::PlainText => {
            tracing::info!("Duppet Sync Summary:");
            for (path, status) in &summary {
                let label = match status {
                    SyncStatus::Created => {
                        maybe_colorize("Created", |s| s.green().bold(), &options)
                    }
                    SyncStatus::Updated => {
                        maybe_colorize("Updated", |s| s.yellow().bold(), &options)
                    }
                    SyncStatus::Unchanged => maybe_colorize("Unchanged", |s| s.blue(), &options),
                };
                tracing::info!("  {:<10} {}", label, path.display());
            }
        }
    }

    Ok(summary)
}

/// sync_files takes all of the files to sync, along with
/// the provided options, and attempts to sync each file,
/// ensuring the file has the expected content. Once it's
/// done, it returns the results for each path!
fn sync_files(
    files: HashMap<PathBuf, FileSpec>,
    options: &SyncOptions,
) -> io::Result<HashMap<PathBuf, SyncStatus>> {
    let mut results = HashMap::new();

    for (dest_path, source_content) in files {
        let status = sync_file(&dest_path, &source_content, options)?;
        results.insert(dest_path, status);
    }

    Ok(results)
}

/// sync_file syncs a single file, ensuring it has
/// the expected content. It will either create or update
/// the file as needed, or delete it if FileEnsure::Absent
/// is specified.
///
/// NOTE: If --dry-run is set, it won't actually apply
/// a change (if a change is needed).
pub fn sync_file(
    dest_path: &Path,
    file_spec: &FileSpec,
    options: &SyncOptions,
) -> io::Result<SyncStatus> {
    // If the file doesn't exist, lets see if it needs to,
    // or if our work here is done!
    if !dest_path.exists() {
        return match file_spec.ensure {
            // The file doesn't exist, and we don't want it to exist,
            // so our work here is done! Very much excite.
            FileEnsure::Absent => {
                logln!(
                    options,
                    "{}: {}",
                    maybe_colorize("File already absent", |s| s.blue().bold(), options),
                    dest_path.display()
                );
                Ok(SyncStatus::Unchanged)
            }
            // The file doesn't exist, but it needs to be, so create it,
            // making sure to create any parent directories along the way.
            FileEnsure::Present => {
                create_file(dest_path, file_spec, options)?;
                if file_spec.exec_on_change {
                    logln!(
                        options,
                        "{}: {}",
                        maybe_colorize("Executing file as requested", |s| s.blue().bold(), options),
                        dest_path.display()
                    );
                    std::process::Command::new(dest_path).output()?;
                }
                Ok(SyncStatus::Created)
            }
        };
    }

    // And if it does exist, maybe update it or delete it,
    // depending on the ensure setting.
    maybe_update_file(dest_path, file_spec, options)
}

/// maybe_update_file is called when it is determined
/// the file already exists, and now we check to see if
/// it needs to be updated, deleted, or if it's good to go.
pub(super) fn maybe_update_file(
    dest_path: &Path,
    file_spec: &FileSpec,
    options: &SyncOptions,
) -> io::Result<SyncStatus> {
    // If the file exists but should be absent, delete it.
    if file_spec.ensure == FileEnsure::Absent {
        delete_file(dest_path, options)?;
        return Ok(SyncStatus::Updated);
    }

    let mut updated = false;
    let mut existing = String::new();
    File::open(dest_path)?.read_to_string(&mut existing)?;

    let src_hash = hex::encode(Sha256::digest(file_spec.content.as_bytes()));
    let dst_hash = hex::encode(Sha256::digest(existing.as_bytes()));

    // If the observed data isn't the expected
    // data, lets update it!
    if src_hash != dst_hash {
        update_file(
            dest_path,
            &file_spec.content,
            &existing,
            &src_hash,
            &dst_hash,
            options,
        )?;
        updated = true;
    }

    // And now check on file permissions and ownership.
    updated |= maybe_update_file_permissions(dest_path, file_spec.permissions, options)?;
    updated |= maybe_update_file_ownership(dest_path, &file_spec.owner, &file_spec.group, options)?;

    if updated {
        if file_spec.exec_on_change {
            logln!(
                options,
                "{}: {}",
                maybe_colorize(
                    "Executing file on change requested",
                    |s| s.blue().bold(),
                    options
                ),
                dest_path.display()
            );

            std::process::Command::new(dest_path).output()?;
        }

        return Ok(SyncStatus::Updated);
    }

    logln!(
        options,
        "{}: {} (sha256: {})",
        maybe_colorize("Destination file unchanged", |s| s.blue().bold(), options),
        dest_path.display(),
        dst_hash
    );

    Ok(SyncStatus::Unchanged)
}

/// create_file is called when it is determined the file
/// doesn't exist yet, so we create it with the expected
/// content.
fn create_file(dest_path: &Path, file_spec: &FileSpec, options: &SyncOptions) -> io::Result<()> {
    let hash = hex::encode(Sha256::digest(file_spec.content.as_bytes()));

    logln!(
        options,
        "{}: {} (sha256: {})",
        maybe_colorize("Creating new file", |s| s.green().bold(), options),
        dest_path.display(),
        hash
    );

    // Make sure the parent directory has been
    // created before trying to put the file in it.
    if !options.dry_run
        && let Some(parent) = dest_path.parent()
    {
        fs::create_dir_all(parent)?;
    }

    write_file_content(dest_path, &file_spec.content, options)?;

    // There's no point in calling these if it's a dry run
    // for creating a new file, because the file won't exist
    // to actually check permissions and ownership on.
    if !options.dry_run {
        maybe_update_file_permissions(dest_path, file_spec.permissions, options)?;
        maybe_update_file_ownership(dest_path, &file_spec.owner, &file_spec.group, options)?;
    }

    Ok(())
}

/// update_file updates an existing file with whatever
/// source content we provide it.
fn update_file(
    dest_path: &Path,
    source_content: &str,
    existing_content: &str,
    src_hash: &str,
    dst_hash: &str,
    options: &SyncOptions,
) -> io::Result<()> {
    let diff = build_diff(source_content, existing_content);
    logln!(
        options,
        "{}: {} (expected sha256: {}, observed sha256: {}), diff:\n{}",
        maybe_colorize("Updating existing file", |s| s.yellow().bold(), options),
        dest_path.display(),
        src_hash,
        dst_hash,
        diff,
    );

    write_file_content(dest_path, source_content, options)?;
    Ok(())
}

/// delete_file deletes an existing file when FileEnsure::Absent
/// is specified.
fn delete_file(dest_path: &Path, options: &SyncOptions) -> io::Result<()> {
    logln!(
        options,
        "{}: {}",
        maybe_colorize("Deleting file", |s| s.red().bold(), options),
        dest_path.display()
    );

    if !options.dry_run {
        fs::remove_file(dest_path)?;
    }

    Ok(())
}

/// write_file_content writes source content to a path.
fn write_file_content(
    dest_path: &Path,
    source_content: &str,
    options: &SyncOptions,
) -> io::Result<()> {
    if !options.dry_run {
        let mut file = File::create(dest_path)?;
        file.write_all(source_content.as_bytes())?;
    }
    Ok(())
}

/// maybe_update_file_permissions possibly updates the permissions
/// on a file (if there's a mode mismatch), and is used by both
/// create_file and update_file to ensure the correct permission is set.
fn maybe_update_file_permissions(
    path: &Path,
    expected_mode: Option<u32>,
    options: &SyncOptions,
) -> io::Result<bool> {
    // If not configured to manage the mode,
    // just return nothing was changed.
    let Some(expected_mode) = expected_mode else {
        return Ok(false);
    };
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let mut perms = metadata.permissions();
    let current_mode = perms.mode() & 0o777;

    if current_mode != expected_mode {
        logln!(
            options,
            "{}: {} (correcting from {:o} to {:o})",
            maybe_colorize("Fixed permissions", |s| s.cyan().bold(), options),
            path.display(),
            current_mode,
            expected_mode
        );

        if !options.dry_run {
            perms.set_mode(expected_mode);
            file.set_permissions(perms)?;
        }
        return Ok(true);
    }

    Ok(false)
}

/// maybe_update_file_ownership possibly updates the owner and/or
/// group on a file (if there's a mismatch), and is used by both
/// create_file and update_file to ensure the correct ownership.
fn maybe_update_file_ownership(
    path: &Path,
    owner: &Option<String>,
    group: &Option<String>,
    options: &SyncOptions,
) -> io::Result<bool> {
    if owner.is_none() && group.is_none() {
        return Ok(false);
    }

    let metadata = fs::metadata(path)?;
    let current_uid = metadata.uid();
    let current_gid = metadata.gid();

    let desired_uid = owner.as_ref().and_then(get_user_by_name).map(|u| u.uid());

    let desired_gid = group.as_ref().and_then(get_group_by_name).map(|g| g.gid());

    let uid_matches = match desired_uid {
        Some(uid) => uid == current_uid,
        None => true,
    };

    let gid_matches = match desired_gid {
        Some(gid) => gid == current_gid,
        None => true,
    };

    if uid_matches && gid_matches {
        return Ok(false);
    }

    let old_user = uzers::get_user_by_uid(current_uid)
        .and_then(|u| u.name().to_str().map(|s| s.to_owned()))
        .unwrap_or_else(|| current_uid.to_string());

    let old_group = uzers::get_group_by_gid(current_gid)
        .and_then(|g| g.name().to_str().map(|s| s.to_owned()))
        .unwrap_or_else(|| current_gid.to_string());

    let new_user = owner.clone().unwrap_or_else(|| old_user.clone());
    let new_group = group.clone().unwrap_or_else(|| old_group.clone());

    logln!(
        options,
        "{}: {} (changing ownership from {}:{} to {}:{})",
        maybe_colorize("Fixed ownership", |s| s.magenta().bold(), options),
        path.display(),
        old_user,
        old_group,
        new_user,
        new_group
    );

    if !options.dry_run {
        let uid = desired_uid
            .map(Uid::from_raw)
            .unwrap_or(Uid::from_raw(current_uid));
        let gid = desired_gid
            .map(Gid::from_raw)
            .unwrap_or(Gid::from_raw(current_gid));
        chown(path, Some(uid), Some(gid)).map_err(io::Error::other)?;
    }

    Ok(true)
}
