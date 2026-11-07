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
use std::io::Read;
use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;
use uzers::get_current_username;

use crate::duppet::sync::maybe_update_file;
use crate::duppet::{
    FileEnsure, FileSpec, SummaryFormat, SyncOptions, SyncStatus, sync, sync_file,
};

// default_test_options are the default
// SyncOptions used for testing.
fn default_test_options() -> SyncOptions {
    SyncOptions {
        dry_run: false,
        quiet: true,
        no_color: true,
        summary_format: SummaryFormat::PlainText,
    }
}

#[test]
// test_create_file ensures that when we run sync_file
// against a new file, that we get back SyncStatus::Created,
// that the path exists, and that the content matches.
fn test_create_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("new-file.lolz");
    let content = "gibletts saaaaaaaaank";
    let file_spec = FileSpec::new().with_content(content);

    let status = sync_file(&path, &file_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Created);
    assert!(path.exists());

    let mut observed = String::new();
    File::open(&path)
        .unwrap()
        .read_to_string(&mut observed)
        .unwrap();
    assert_eq!(observed, content);
}

#[test]
// test_no_change syncs the new file, ignores the result, and then
// syncs the file again to verify that nothing changed the second time.
fn test_no_change() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("no-changes-heeeeah");
    let content = "frostycup=1";
    let file_spec = FileSpec::new().with_content(content);

    // First sync to get it written.
    sync_file(&path, &file_spec, &default_test_options()).unwrap();

    // Second sync that should be unchanged.
    let status = sync_file(&path, &file_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Unchanged);
}

#[test]
// test_update_file_content syncs the initial data, then different
// data, and makes sure it comes back as being updated, and that the
// actual context itself was indeed updated.
fn test_update_file_content() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("updated-file-party");
    let original_spec = FileSpec::new().with_content("we started like this this this this this");
    let updated_spec = FileSpec::new().with_content("and we ended like that that that that that");

    sync_file(&path, &original_spec, &default_test_options()).unwrap();

    let status = sync_file(&path, &updated_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Updated);

    let mut observed_content = String::new();
    File::open(&path)
        .unwrap()
        .read_to_string(&mut observed_content)
        .unwrap();
    assert_eq!(observed_content, updated_spec.content);
}

#[test]
// test_fix_permissions_only makes sure just permissions get
// fixed when needed, and that it gets reported as updated.
fn test_fix_permissions_only() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("fixin-perms.dat");
    let content = "snooooze";
    let file_spec = FileSpec::new().with_content(content).with_perms(0o644);

    // Make a file and set its permissions to 0600.
    sync_file(&path, &file_spec, &default_test_options()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

    // And now sync again and make sure the permissions got
    // put "back" to 0644.
    let status = maybe_update_file(&path, &file_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Updated);

    let mode = File::open(&path)
        .unwrap()
        .metadata()
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o644);
}

#[test]
// test_dry_run_is_so_dry makes sure the dry run is actually
// indeed a dry run for creating a new file.
fn test_dry_run_is_so_dry() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dry-created.file");
    let content = "thats some pretty dry content for open mic night";
    let file_spec = FileSpec::new().with_content(content);
    let opts = SyncOptions {
        dry_run: true,
        ..default_test_options()
    };

    // It will report as created.
    let status = sync_file(&path, &file_spec, &opts).unwrap();
    assert_eq!(status, SyncStatus::Created);

    // But not actually exist.
    assert!(!path.exists());
}

#[test]
// test_sync tests a sync and makes sure it works.
fn test_sync() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("some-file");
    let content = "this is my file";
    let mut files = HashMap::new();
    files.insert(path.clone(), FileSpec::new().with_content(content));
    let result = sync(files, default_test_options());
    assert!(result.is_ok());

    let summary = result.unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary.get(&path), Some(&SyncStatus::Created));

    let mut observed = String::new();
    File::open(&path)
        .unwrap()
        .read_to_string(&mut observed)
        .unwrap();
    assert_eq!(observed, content);
}

#[test]
// test_ownership_noop_with_current_user just tests to
// make sure the owner can be set as the current owner
// running the test (or that its a noop) -- it's not much
// of a test, but it's something. I don't know how
// to portably test this right now otherwise.
fn test_ownership_noop_with_current_user() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("ownership-test-file");

    let current_user = get_current_username()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let file_spec = FileSpec::new()
        .with_content("i like to eat aye-ples and ba-naye-nayes")
        .with_ownership(Some(current_user), None);

    // First sync to create the file (and apply ownership).
    let status = sync_file(&path, &file_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Created);

    // Second sync should be a noop.
    let status = sync_file(&path, &file_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Unchanged);
}

#[test]
// test_delete_file ensures that when we run sync_file
// with FileEnsure::Absent on an existing file, that the
// file gets deleted and we get back SyncStatus::Updated.
fn test_delete_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("file-to-delete.txt");
    let content = "goodbye cruel world";

    // First create the file.
    let create_spec = FileSpec::new().with_content(content);
    sync_file(&path, &create_spec, &default_test_options()).unwrap();
    assert!(path.exists());

    // Now sync with FileEnsure::Absent to delete it.
    let delete_spec = FileSpec::new().with_ensure(FileEnsure::Absent);
    let status = sync_file(&path, &delete_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Updated);
    assert!(!path.exists());
}

#[test]
// test_delete_nonexistent_file ensures that when we run
// sync_file with FileEnsure::Absent on a file that doesn't
// exist, we get back SyncStatus::Unchanged and no error.
fn test_delete_nonexistent_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("never-existed.txt");

    // Try to delete a file that doesn't exist.
    let delete_spec = FileSpec::new().with_ensure(FileEnsure::Absent);
    let status = sync_file(&path, &delete_spec, &default_test_options()).unwrap();
    assert_eq!(status, SyncStatus::Unchanged);
    assert!(!path.exists());
}

#[test]
// test_dry_run_delete ensures that dry run mode doesn't
// actually delete files when FileEnsure::Absent is set.
fn test_dry_run_delete() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dry-delete.txt");
    let content = "still here in dry run";

    // First create the file.
    let create_spec = FileSpec::new().with_content(content);
    sync_file(&path, &create_spec, &default_test_options()).unwrap();
    assert!(path.exists());

    // Now try to delete with dry run enabled.
    let opts = SyncOptions {
        dry_run: true,
        ..default_test_options()
    };
    let delete_spec = FileSpec::new().with_ensure(FileEnsure::Absent);
    let status = sync_file(&path, &delete_spec, &opts).unwrap();

    // It will report as updated.
    assert_eq!(status, SyncStatus::Updated);

    // But the file should still exist.
    assert!(path.exists());
}
