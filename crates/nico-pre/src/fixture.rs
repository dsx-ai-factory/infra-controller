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

//! Test fixture: a private copy of `examples/` (the site definition and its collaterals)
//! that a test edits one line at a time before running a check against it.

use std::path::{Path, PathBuf};

use crate::harness::{Ctx, Outcome};
use crate::register::Register;

const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");

pub(crate) struct Example {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Example {
    /// A fresh copy of `examples/site-def.yaml` and every collateral under `examples/`.
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = dir.path().to_path_buf();
        copy_tree(Path::new(EXAMPLES), &root);
        Example { _dir: dir, root }
    }

    /// Replaces the first `from` in the copied file at `rel` (relative to the site
    /// definition) with `to`. Panics when `from` is not there, so a renamed fixture line
    /// fails the test instead of silently testing nothing.
    pub(crate) fn splice(&self, rel: &str, from: &str, to: &str) -> &Self {
        let path = self.root.join(rel);
        let text = std::fs::read_to_string(&path).expect("fixture file");
        assert!(text.contains(from), "splice target `{from}` not in {rel}");
        std::fs::write(&path, text.replacen(from, to, 1)).expect("writing the fixture");
        self
    }

    /// Runs `check` on the copied site definition.
    pub(crate) fn run(&self, check: fn(&Ctx) -> eyre::Result<Outcome>) -> Outcome {
        self.try_run(check).expect("the check returns an outcome")
    }

    /// The check's result, error included, for tests of what a loader refuses.
    pub(crate) fn try_run(
        &self,
        check: fn(&Ctx) -> eyre::Result<Outcome>,
    ) -> eyre::Result<Outcome> {
        self.with_ctx(check)
    }

    /// A path inside the copy, for tests that add or move fixture files.
    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// The derived decisions of the copied site definition.
    pub(crate) fn derive(&self) -> Vec<(String, crate::derived::Derived)> {
        self.with_ctx(|ctx| crate::derived::derive_all(ctx).expect("the collaterals load"))
    }

    fn with_ctx<T>(&self, f: impl FnOnce(&Ctx) -> T) -> T {
        let path = self.root.join("site-def.yaml");
        let sd = crate::sitedef::load(&path).expect("the copied site definition parses");
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: &path,
            decisions: &dec,
            evidence: None,
        };
        f(&ctx)
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("creating the fixture directory");
    for entry in std::fs::read_dir(from).expect("reading examples/") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copying a fixture file");
        }
    }
}

/// The observed text of any outcome, for assertions.
pub(crate) fn observed(o: &Outcome) -> &str {
    match o {
        Outcome::Pass(s) | Outcome::Skip(s) => s.as_str(),
        Outcome::Warn { observed, .. } | Outcome::Fail { observed, .. } => observed.as_str(),
    }
}
