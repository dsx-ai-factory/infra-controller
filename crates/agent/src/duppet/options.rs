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

/// SummaryFormat allows the caller to configure how they
/// want a summary reported at the end of of the run.
#[derive(Debug)]
pub enum SummaryFormat {
    PlainText,
    Json,
    Yaml,
}

/// FileEnsure specifies whether a file should be present
/// or absent on the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEnsure {
    /// Present indicates the file should exist with the
    /// specified content and attributes.
    Present,
    /// Absent indicates the file should not exist and
    /// should be deleted if present.
    Absent,
}

/// SyncOptions allows the caller to control various
/// aspects of the duppet sync.
#[derive(Debug)]
pub struct SyncOptions {
    /// dry_run allows the caller to perform a dry run
    /// on the sync -- no files will be created or updated,
    /// and it will simply log and report what would have
    /// been done.
    pub dry_run: bool,
    /// quiet will make it so duppet doesn't log individual
    /// file updates, and will leave it until the end when
    /// a summary is printed.
    pub quiet: bool,
    /// no_color will exclude the beautiful colors that
    /// are included in messages, if that's what you really
    /// want.
    pub no_color: bool,
    /// summary_format is the format of the report summary
    /// at the end of the run (plaintext, json, yaml).
    pub summary_format: SummaryFormat,
}

/// FileSpec defines a file specification for the
/// desired state of the file being created, including
/// the content, the permissions, the owner, and the
/// group.
#[derive(Debug, Clone)]
pub struct FileSpec {
    /// content is the actual file content to set.
    pub content: String,
    /// permissions are the optional permissions to
    /// set on the file. If None, no permission management
    /// will happen, and system defaults will be used, and
    /// no attempts to keep permissions in sync will occur.
    pub permissions: Option<u32>,
    /// owner is an optional owner to set for the file. If
    /// None, then no owner management will happen, and
    /// the system default will be used, and no attempts
    /// to keep the owner in sync will occur.
    pub owner: Option<String>,
    /// group is an optional group to set for the file. If None,
    /// then no group management will happen, and the system
    /// default will be used, and no attempts to keep the group
    /// in sync will occur.
    pub group: Option<String>,
    /// ensure specifies whether the file should be present
    /// or absent on the filesystem.
    pub ensure: FileEnsure,
    /// exec_on_change triggers the execution of a file after
    /// it has been created.
    pub exec_on_change: bool,
}

impl FileSpec {
    /// new creates a new FileSpec with default values: empty content,
    /// permissions set to 0o644, no owner/group management, and
    /// ensure set to Present.
    pub fn new() -> Self {
        FileSpec {
            content: String::new(),
            permissions: Some(0o644),
            owner: None,
            group: None,
            ensure: FileEnsure::Present,
            exec_on_change: false,
        }
    }

    /// with_exec_on_change is a builder method that sets the exec_on_change
    /// flag on the FileSpec, which will trigger the execution of the file
    /// on create or update.
    pub fn with_exec_on_change(mut self) -> Self {
        self.exec_on_change = true;
        self
    }

    /// with_content is a builder method that sets the content
    /// field on an existing FileSpec.
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// with_perms is a builder method that sets the permissions
    /// field on an existing FileSpec.
    pub fn with_perms(mut self, permissions: u32) -> Self {
        self.permissions = Some(permissions);
        self
    }

    /// with_ownership is a builder method that sets the owner
    /// and group fields on an existing FileSpec.
    pub fn with_ownership(mut self, owner: Option<String>, group: Option<String>) -> Self {
        self.owner = owner;
        self.group = group;
        self
    }

    /// with_ensure is a builder method that sets the ensure
    /// field on an existing FileSpec.
    pub fn with_ensure(mut self, ensure: FileEnsure) -> Self {
        self.ensure = ensure;
        self
    }
}

impl Default for FileSpec {
    fn default() -> Self {
        Self::new()
    }
}
