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

use std::path::Path;
use std::process::Command;

/// Set build script environment variables. Call this from a build script.
///
/// Everything here reads the environment with `std::env::var` at build-script
/// runtime, never with `option_env!`. `option_env!` bakes the value into this
/// crate's own compiled rlib, so machine- and commit-specific strings
/// (`USER`, `HOSTNAME`, `VERSION`, `CI_COMMIT_SHORT_SHA`) become part of
/// carbide-version's compilation fingerprint: every build script that links
/// this crate recompiles whenever one of them changes (in CI that is every
/// runner and every commit), and sccache cannot reuse any of it. The same
/// environment is present when the build script runs, so the values are
/// identical; the only difference is when they are read.
pub fn build() {
    // In a git worktree in a container (local dev) none of the git commands will work because
    // the real git directory isn't mounted.
    let in_git_repo = Command::new("git")
        .args(["rev-parse"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if in_git_repo {
        git_allow();
    }
    // Source snapshots (`git archive`, vendored trees) and the unborn
    // repository the release Dockerfiles get from `cargo new` have no
    // history; there the version fields come from the environment only.
    let repo = Path::new(".");
    let can_git = git_has_head(repo);

    let user = std::env::var("USER").unwrap_or_default();
    // TODO: Remove after migration to new CARBIDE_ naming
    println!("cargo:rustc-env=FORGE_BUILD_USER={user}");
    println!("cargo:rustc-env=CARBIDE_BUILD_USER={user}");

    let hostname = std::env::var("HOSTNAME").unwrap_or_default();
    // TODO: Remove after migration to new CARBIDE_ naming
    println!("cargo:rustc-env=FORGE_BUILD_HOSTNAME={hostname}");
    println!("cargo:rustc-env=CARBIDE_BUILD_HOSTNAME={hostname}");

    // The committer date of HEAD, not wall-clock `date`. A wall-clock stamp
    // changes on every invocation and lands in the env-dep list of every
    // crate that embeds `v!(build_date)`, guaranteeing an sccache miss for
    // those crates and all their dependents, even for two jobs compiling the
    // same commit in the same CI run. The committer date only moves when the
    // commit does, which is the granularity of everything else stamped here.
    let build_date = build_date(
        repo,
        std::env::var("CI_COMMIT_TIMESTAMP").ok(),
        std::env::var("SOURCE_DATE_EPOCH").ok(),
    );
    // TODO: Remove after migration to new CARBIDE_ naming
    println!("cargo:rustc-env=FORGE_BUILD_DATE={build_date}");
    println!("cargo:rustc-env=CARBIDE_BUILD_DATE={build_date}");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = run(&rustc, &["--version"]);
    // TODO: Remove after migration to new CARBIDE_ naming
    println!("cargo:rustc-env=FORGE_BUILD_RUSTC_VERSION={rustc_version}");
    println!("cargo:rustc-env=CARBIDE_BUILD_RUSTC_VERSION={rustc_version}");

    println!("cargo:rerun-if-env-changed=CARBIDE_BUILD_HELM_VERSION");
    // Emitting any rerun-if directive replaces cargo's default rules, and
    // every value stamped here is read at script runtime, so each env var
    // must be declared here or a change on an unchanged commit does not
    // re-stamp.
    // `RUSTC` is deliberately absent: cargo sets it for build scripts itself,
    // and `rerun-if-env-changed` cannot track cargo-provided variables.
    for var in [
        "VERSION",
        "CI_COMMIT_SHORT_SHA",
        "CI_COMMIT_TIMESTAMP",
        "SOURCE_DATE_EPOCH",
        "USER",
        "HOSTNAME",
        "REPO_ROOT",
        "CONTAINER_REPO_ROOT",
        "FORGE_VERSION_AVOID_REBUILD",
        "CARBIDE_VERSION_AVOID_REBUILD",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    // For these two in CI we use the env var, locally we query git

    let sha = env_or_git(
        std::env::var("CI_COMMIT_SHORT_SHA").ok(),
        repo,
        &["rev-parse", "--short=8", "HEAD"],
    );
    // TODO: Remove after migration to new CARBIDE_ naming
    println!("cargo:rustc-env=FORGE_BUILD_GIT_HASH={sha}");
    println!("cargo:rustc-env=CARBIDE_BUILD_GIT_HASH={sha}");

    let build_version = env_or_git(
        std::env::var("VERSION").ok(),
        repo,
        &["describe", "--tags", "--first-parent", "--always", "--long"],
    );
    if sha.is_empty() || build_version.is_empty() {
        println!("cargo:warning=No git and no VERSION/CI_COMMIT_SHORT_SHA, version will be blank");
    }
    // TODO: Remove after migration to new CARBIDE_ naming
    println!("cargo:rustc-env=FORGE_BUILD_GIT_TAG={build_version}");
    println!("cargo:rustc-env=CARBIDE_BUILD_GIT_TAG={build_version}");

    // If CI pre-computed CARBIDE_BUILD_HELM_VERSION (passed as a Docker build-arg), use it
    // directly. This avoids re-deriving helm_version in Rust and keeps CI as the single
    // source of truth — important for RC tags like v2.0.0-rc.14 where the naive rfind('-')
    // rewrite would produce the incorrect 2.0.0.rc.14.
    if let Some(v) = std::env::var("CARBIDE_BUILD_HELM_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
    {
        println!("cargo:rustc-env=CARBIDE_BUILD_HELM_VERSION={v}");
    } else {
        // Local fallback: strip leading 'v', then rewrite only the git-describe
        // suffix (-<count>-g<hash>) by replacing its internal '-' with '.'.
        // e.g. "v1.2.3-42-gabcdef1"  → "1.2.3-42.gabcdef1"
        //      "v2.0.0-rc.14"        → "2.0.0-rc.14"  (clean tag, no rewrite)
        //      "v2.0.0-rc.14-0-gabcdef1" → "2.0.0-rc.14-0.gabcdef1"
        let helm_version = {
            let s = build_version.trim_start_matches('v');
            // git describe --long always ends in -<digits>-g<hex>. Split from the
            // right into at most 3 parts to detect that pattern without touching
            // semver pre-release separators (e.g. the '-' in "rc.14").
            let parts: Vec<&str> = s.rsplitn(3, '-').collect();
            let is_git_describe = parts.len() == 3
                && parts[0].starts_with('g')
                && parts[0][1..].chars().all(|c| c.is_ascii_hexdigit())
                && parts[1].chars().all(|c| c.is_ascii_digit());
            if is_git_describe {
                format!("{}-{}.{}", parts[2], parts[1], parts[0])
            } else {
                s.to_string()
            }
        };
        println!("cargo:rustc-env=CARBIDE_BUILD_HELM_VERSION={helm_version}");
    }

    // Only re-calculate all of this when there's a new commit... but use an env var to allow
    // avoiding rebuilds when the commit hash changes. (This is good for local development iteration
    // loops when we want to avoid recompiling and when we don't really care if the generated
    // version is stale.)
    // TODO: Remove after migration to new CARBIDE_ naming
    if can_git
        && (std::env::var("FORGE_VERSION_AVOID_REBUILD").is_err()
            || std::env::var("CARBIDE_VERSION_AVOID_REBUILD").is_err())
    {
        let git_query_head =
            run("git", &["rev-parse", "--path-format=absolute", "--git-dir"]) + "/HEAD";
        let git_head = if Path::new(&git_query_head).exists() {
            // dev
            git_query_head
        } else {
            // CI
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../.git/HEAD").to_string()
        };

        // Check that this file is still relative to the repository root where we expect.
        // If it isn't, then rerun-if-changed is wrong - and we will rebuilt the version
        // crate and all dependents on each `cargo build`
        assert!(
            std::path::Path::new(&git_head).exists(),
            "Git HEAD not found at {git_head}. Adjust location to avoid double compilation"
        );

        println!("cargo:rerun-if-changed={git_head}");
    }
}

/// In CI the committer date comes from the environment, like the SHA and
/// version: the release Dockerfiles build inside the empty repository that
/// `cargo new` creates (`.git/` is excluded from the build context), so git
/// works there but has no commits to read. Locally it comes from git when
/// HEAD resolves. Otherwise (source snapshots, local containers without git,
/// or an unborn HEAD) use `SOURCE_DATE_EPOCH` when the builder supplies it,
/// and only then fall back to wall clock, where caching is not at stake.
fn build_date(
    repo: &Path,
    ci_commit_timestamp: Option<String>,
    source_date_epoch: Option<String>,
) -> String {
    let date = env_or_git(ci_commit_timestamp, repo, &["log", "-1", "--format=%cI"]);
    if !date.is_empty() {
        return date;
    }
    if let Some(epoch) = source_date_epoch.and_then(|v| v.parse().ok()) {
        return utc_timestamp(epoch);
    }
    run("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]) // like 'date --iso-8601=seconds --utc' but portable across GNU/BSD
}

/// `value` (a builder-supplied env var) when set and non-empty, otherwise the
/// output of `git <args>` when HEAD resolves in `repo`, otherwise empty. An
/// unpassed Dockerfile `ARG` sets its `ENV` to the empty string.
fn env_or_git(value: Option<String>, repo: &Path, args: &[&str]) -> String {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        return value;
    }
    if !git_has_head(repo) {
        return String::new();
    }
    let repo = repo.to_string_lossy();
    run("git", &[&["-C", repo.as_ref()], args].concat())
}

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`, using Howard
/// Hinnant's `civil_from_days` so this crate stays dependency-free.
fn utc_timestamp(epoch: u64) -> String {
    let (days, secs) = (epoch / 86_400, epoch % 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

/// Whether `repo` is inside a git repository whose HEAD names a commit. A bare
/// `git rev-parse` also succeeds in a freshly initialised repository, where
/// every command that reads history fails.
fn git_has_head(repo: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .current_dir(repo)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

// If the current user is not the owner of the repo root (containing .git), then
// git will exit with status 128 "fatal: detected dubious ownership".
// This happens in containers.
// Exit code 128 means many things, this just handles one of them.
//
// "git config --add" is not idempotent, so only do this if we have to.
fn git_allow() {
    match Command::new("git").arg("status").status() {
        Err(err) => {
            println!("cargo:warning=build.rs error running 'git status': {err}.")
        }
        Ok(status) => match status.code() {
            Some(128) => git_mark_safe_directory(),
            Some(_) => {}
            None => {}
        },
    }
}

fn git_mark_safe_directory() {
    // Only ever trust one concrete repository root. A wildcard
    // (`safe.directory=*`) disables git's ownership check for every
    // repository of this user, and outside CI that lands in a developer's
    // real global config. With no root configured, leave git alone: the
    // repo-dependent git calls fail softly through `run()`, and the affected
    // version fields come out blank, the same as having no git at all.
    let repo_root = std::env::var("REPO_ROOT")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("CONTAINER_REPO_ROOT")
                .ok()
                .filter(|v| !v.is_empty())
        });
    let Some(repo_root) = repo_root else {
        println!(
            "cargo:warning=git reports dubious ownership and neither REPO_ROOT nor CONTAINER_REPO_ROOT is set; leaving safe.directory alone, version info may be blank"
        );
        return;
    };
    run(
        "git",
        &["config", "--global", "--add", "safe.directory", &repo_root],
    );
}

/// Run a command from a build script returning its stdout, logging errors with cargo:warning
fn run(cmd: &str, args: &[&str]) -> String {
    let output = match Command::new(cmd).args(args).output() {
        Ok(output) => {
            if !output.status.success() {
                println!(
                    "cargo:warning=build.rs failed running '{cmd} {}': '{output:?}'",
                    args.join(" ")
                );
                return String::new();
            }
            output
        }
        Err(err) => {
            println!(
                "cargo:warning=build.rs error running '{cmd} {}': {err}.",
                args.join(" ")
            );
            return String::new();
        }
    };
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Individual parts of the version. Usage:: forge_version::v!(build_version)
/// If that part is not present expands to an empty &str
// TODO: Change to CARBIDE_
#[macro_export]
macro_rules! v {
    (build_version) => {
        option_env!("FORGE_BUILD_GIT_TAG").unwrap_or_default()
    };
    (build_date) => {
        option_env!("FORGE_BUILD_DATE").unwrap_or_default()
    };
    (git_sha) => {
        option_env!("FORGE_BUILD_GIT_HASH").unwrap_or_default()
    };
    (rust_version) => {
        option_env!("FORGE_BUILD_RUSTC_VERSION").unwrap_or_default()
    };
    (build_user) => {
        option_env!("FORGE_BUILD_USER").unwrap_or_default()
    };
    (build_hostname) => {
        option_env!("FORGE_BUILD_HOSTNAME").unwrap_or_default()
    };
    (helm_version) => {
        option_env!("CARBIDE_BUILD_HELM_VERSION").unwrap_or_default()
    };
}

/// Same a v! but expands to a literal. That allows using it in `concat!` macro.
/// Panics if the part is not present. Prefer `v!` above.
// TODO: Change to CARBIDE_
#[macro_export]
macro_rules! literal {
    (build_version) => {
        env!("FORGE_BUILD_GIT_TAG")
    };
    (build_date) => {
        env!("FORGE_BUILD_DATE")
    };
    (git_sha) => {
        env!("FORGE_BUILD_GIT_HASH")
    };
    (rust_version) => {
        env!("FORGE_BUILD_RUSTC_VERSION")
    };
    (build_user) => {
        env!("FORGE_BUILD_USER")
    };
    (build_hostname) => {
        env!("FORGE_BUILD_HOSTNAME")
    };
    (helm_version) => {
        env!("CARBIDE_BUILD_HELM_VERSION")
    };
}

/// Version as a string. `version::build()` must have been called previously in build script.
// TODO: Change to CARBIDE_
#[macro_export]
macro_rules! version {
     () => {
         format!(
             "build_version={}, build_date={}, git_sha={}, rust_version={}, build_user={}, build_hostname={}",
             option_env!("FORGE_BUILD_GIT_TAG").unwrap_or_default(),
             option_env!("FORGE_BUILD_DATE").unwrap_or_default(),
             option_env!("FORGE_BUILD_GIT_HASH").unwrap_or_default(),
             option_env!("FORGE_BUILD_RUSTC_VERSION").unwrap_or_default(),
             option_env!("FORGE_BUILD_USER").unwrap_or_default(),
             option_env!("FORGE_BUILD_HOSTNAME").unwrap_or_default(),
         );
     };
 }

#[cfg(test)]
mod tests {
    use super::*;

    const COMMITTER_DATE: &str = "2026-01-02T03:04:05+00:00";
    const CI_TIMESTAMP: &str = "2026-05-06T07:08:09+00:00";
    const SOURCE_DATE_EPOCH: &str = "1792084745";
    const SOURCE_DATE: &str = "2026-10-15T17:19:05Z";

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(repo)
            .env("GIT_COMMITTER_DATE", COMMITTER_DATE)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn is_utc_timestamp(date: &str) -> bool {
        // YYYY-MM-DDTHH:MM:SSZ
        date.len() == 20 && date.as_bytes()[10] == b'T' && date.ends_with('Z')
    }

    // The release Dockerfiles build inside the repository `cargo new`
    // initialises: git works there, but HEAD is unborn. An `ARG` that is not
    // passed still sets its `ENV` to the empty string.
    #[test]
    fn build_date_is_never_empty() {
        let repo = tempfile::tempdir().expect("create temporary repository");
        let ci = || Some(CI_TIMESTAMP.to_string());
        let epoch = || Some(SOURCE_DATE_EPOCH.to_string());
        git(repo.path(), &["init", "--quiet"]);

        assert_eq!(
            build_date(repo.path(), ci(), epoch()),
            CI_TIMESTAMP,
            "unborn HEAD, CI timestamp"
        );
        assert_eq!(
            build_date(repo.path(), Some(String::new()), epoch()),
            SOURCE_DATE,
            "unborn HEAD, empty CI timestamp, SOURCE_DATE_EPOCH"
        );
        for (scenario, ci_timestamp) in [
            ("unborn HEAD, no CI timestamp", None),
            ("unborn HEAD, empty CI timestamp", Some(String::new())),
        ] {
            let date = build_date(repo.path(), ci_timestamp, None);
            assert!(is_utc_timestamp(&date), "{scenario}: {date:?}");
        }

        git(
            repo.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "initial"],
        );
        assert_eq!(
            build_date(repo.path(), ci(), epoch()),
            CI_TIMESTAMP,
            "HEAD, CI timestamp"
        );
        // git >= 2.50 prints a UTC committer date as `Z`, older as `+00:00`.
        assert_eq!(
            build_date(repo.path(), None, epoch()).replace('Z', "+00:00"),
            COMMITTER_DATE,
            "HEAD, no CI timestamp, SOURCE_DATE_EPOCH"
        );
    }

    // A source snapshot (`git archive`, vendored tree) has no repository at
    // all; the builder-supplied values must still be stamped.
    #[test]
    fn source_snapshot_stamps_builder_values() {
        let snapshot = tempfile::tempdir().expect("create temporary snapshot");
        let sha_args = &["rev-parse", "--short=8", "HEAD"];

        assert_eq!(
            env_or_git(Some("v2.2.2".to_string()), snapshot.path(), &["describe"]),
            "v2.2.2"
        );
        assert_eq!(
            env_or_git(Some("80d131a5".to_string()), snapshot.path(), sha_args),
            "80d131a5"
        );
        assert_eq!(
            env_or_git(Some(String::new()), snapshot.path(), sha_args),
            ""
        );
        assert_eq!(
            build_date(snapshot.path(), None, Some(SOURCE_DATE_EPOCH.to_string())),
            SOURCE_DATE
        );
    }

    #[test]
    fn utc_timestamp_formats_epoch_seconds() {
        for (epoch, expected) in [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_792_084_745, SOURCE_DATE),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
        ] {
            assert_eq!(utc_timestamp(epoch), expected, "epoch {epoch}");
        }
    }
}
