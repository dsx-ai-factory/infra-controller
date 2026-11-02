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

//! Environment isolation for `figment::Jail` config tests.

/// Removes the inherited environment variables whose names start with `prefix`, for
/// the lifetime of `jail`.
///
/// `Jail::clear_env` removes every variable in the process. Tests running in parallel
/// on other threads read their own variables (`DATABASE_URL`, `PATH`, ...) outside the
/// jail's lock and see them vanish. This removes only the configuration prefix a test
/// cares about, and records each original value in the jail so it is restored when the
/// jail drops, even while unwinding.
pub fn remove_prefixed_env(jail: &mut figment::Jail, prefix: &str) {
    let keys: Vec<String> = std::env::vars_os()
        .filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| key.starts_with(prefix))
        .collect();
    for key in keys {
        // Journal the original value so the jail restores it on drop.
        jail.set_env(&key, "");
        // SAFETY: Initial lint enablement: the caller holds a `figment::Jail`, which
        // serializes jail users but does not prove Unix process-wide exclusion from
        // environment readers. This needs owner review.
        unsafe { std::env::remove_var(&key) }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const SENTINEL: &str = "JAIL_ENV_TEST_CHILD";
    const PREFIXED: &str = "JAIL_ENV_TEST_PREFIX_inherited";
    const OTHER: &str = "JAIL_ENV_TEST_OTHER_untouched";

    /// Runs the named test again in a child process with `envs` set, so the parent's
    /// environment is never touched. Panics with the child's output on failure.
    fn run_in_child_process(test_name: &str, sentinel: &str, envs: &[(&str, &str)]) {
        let output = Command::new(std::env::current_exe().expect("test binary path"))
            .args([test_name, "--exact", "--test-threads=1"])
            .env(sentinel, "1")
            .envs(envs.iter().copied())
            .output()
            .expect("spawn child test process");
        assert!(
            output.status.success(),
            "child process failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// The parent re-runs this test in a child with the variables set, so the shared
    /// process environment of parallel tests is never modified. The child fails if the
    /// helper clears unrelated variables or does not restore the removed one.
    #[test]
    #[allow(clippy::result_large_err)] // Figment controls the error representation.
    fn removes_only_the_prefix_and_restores_it_after_the_jail() {
        const TEST_NAME: &str =
            "jail_env::tests::removes_only_the_prefix_and_restores_it_after_the_jail";
        if std::env::var_os(SENTINEL).is_none() {
            run_in_child_process(
                TEST_NAME,
                SENTINEL,
                &[(PREFIXED, "inherited"), (OTHER, "untouched")],
            );
            return;
        }

        figment::Jail::expect_with(|jail| {
            remove_prefixed_env(jail, "JAIL_ENV_TEST_PREFIX_");
            assert!(std::env::var_os(PREFIXED).is_none(), "prefixed key removed");
            assert_eq!(std::env::var(OTHER).as_deref(), Ok("untouched"));

            // A value set by the test itself is still dropped with the jail.
            jail.set_env(PREFIXED, "set-in-jail");
            assert_eq!(std::env::var(PREFIXED).as_deref(), Ok("set-in-jail"));
            Ok(())
        });

        assert_eq!(std::env::var(PREFIXED).as_deref(), Ok("inherited"));
        assert_eq!(std::env::var(OTHER).as_deref(), Ok("untouched"));
    }
}
