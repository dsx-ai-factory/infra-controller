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
use std::process::{Command, Stdio};

use crate::lockdown::error::{MlxError, MlxResult};

// FlintRunner is a wrapper for executing flint commands.
pub struct FlintRunner {
    // flint_path is the path to the flint executable.
    flint_path: String,
    // dry_run determines whether to perform dry-run operations.
    dry_run: bool,
}

impl FlintRunner {
    // new creates a new FlintRunner instance.
    pub fn new() -> MlxResult<Self> {
        let flint_path = Self::find_flint()?;
        Ok(Self {
            flint_path,
            dry_run: false,
        })
    }

    // with_path creates a new FlintRunner with a custom flint path.
    pub fn with_path<P: Into<String>>(path: P) -> Self {
        Self {
            flint_path: path.into(),
            dry_run: false,
        }
    }

    // with_dry_run creates a FlintRunner with dry-run enabled.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    // find_flint attempts to find the flint executable in common locations.
    fn find_flint() -> MlxResult<String> {
        let common_paths = [
            "flint",
            "/usr/bin/flint",
            "/usr/local/bin/flint",
            "/opt/mellanox/mft/bin/flint",
        ];

        for path in &common_paths {
            if let Ok(output) = Command::new(path)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                && output.success()
            {
                return Ok(path.to_string());
            }
        }

        Err(MlxError::FlintNotFound)
    }

    // build_command builds a command string for logging/dry-run purposes.
    fn build_command(&self, args: &[&str]) -> String {
        format!("{} {}", self.flint_path, args.join(" "))
    }

    // query_device queries device information and hardware access status.
    pub fn query_device(&self, device_id: &str) -> MlxResult<String> {
        let args = ["-d", device_id, "q"];

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&args)));
        }

        let output = Command::new(&self.flint_path)
            .args(args)
            .output()
            .map_err(|e| MlxError::CommandFailed(format!("Failed to execute query: {e}")))?;

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            // Check specific error conditions first.
            if stderr.contains("HW access is disabled") || stdout.contains("HW access is disabled")
            {
                return Ok("locked".to_string());
            } else if stderr.contains("Cannot open") || stdout.contains("Cannot open") {
                return Err(MlxError::DeviceNotFound(device_id.to_string()));
            } else if stderr.contains("Permission denied") || stdout.contains("Permission denied") {
                return Err(MlxError::PermissionDenied);
            }

            let error_msg = format!("stdout: {}\nstderr: {}", stdout.trim(), stderr.trim());
            return Err(MlxError::CommandFailed(error_msg));
        }

        Ok("unlocked".to_string())
    }

    /// Enable hardware access with a key of 1–16 bare ASCII hexadecimal digits.
    ///
    /// Returns [`MlxError::InvalidKey`] before execution for malformed keys, or
    /// [`MlxError::DryRun`] with the key redacted when dry-run is enabled.
    /// [`MlxError::AlreadyUnlocked`] takes precedence over exit status when flint
    /// reports access already enabled. Other launch or exit failures return
    /// [`MlxError::CommandFailed`] without tool output, which may contain keys.
    pub fn enable_hw_access(&self, device_id: &str, key: &str) -> MlxResult<()> {
        if !Self::is_valid_key(key) {
            return Err(MlxError::InvalidKey);
        }

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&[
                "-d",
                device_id,
                "hw_access",
                "enable",
                "[REDACTED]",
            ])));
        }

        let args = ["-d", device_id, "hw_access", "enable", key];
        let output = Command::new(&self.flint_path)
            .args(args)
            .output()
            .map_err(|e| MlxError::CommandFailed(format!("Failed to execute enable: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Check for "already enabled" even on success (exit code 0)
        if stderr.contains("already enabled") || stdout.contains("already enabled") {
            return Err(MlxError::AlreadyUnlocked);
        }

        if !output.status.success() {
            return Err(MlxError::CommandFailed(format!(
                "hw_access enable failed: {}",
                output.status
            )));
        }

        Ok(())
    }

    /// Disable hardware access with a key of 1–16 bare ASCII hexadecimal digits.
    ///
    /// Returns [`MlxError::InvalidKey`] before execution for malformed keys, or
    /// [`MlxError::DryRun`] with the key redacted when dry-run is enabled.
    /// [`MlxError::AlreadyLocked`] takes precedence over exit status when flint
    /// reports access already disabled. Other launch or exit failures return
    /// [`MlxError::CommandFailed`] without tool output, which may contain keys.
    pub fn disable_hw_access(&self, device_id: &str, key: &str) -> MlxResult<()> {
        if !Self::is_valid_key(key) {
            return Err(MlxError::InvalidKey);
        }

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&[
                "-d",
                device_id,
                "hw_access",
                "disable",
                "[REDACTED]",
            ])));
        }

        let args = ["-d", device_id, "hw_access", "disable", key];
        let output = Command::new(&self.flint_path)
            .args(args)
            .output()
            .map_err(|e| MlxError::CommandFailed(format!("Failed to execute disable: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Check for "already disabled" even on success (exit code 0)
        if stderr.contains("already disabled") || stdout.contains("already disabled") {
            return Err(MlxError::AlreadyLocked);
        }

        if !output.status.success() {
            return Err(MlxError::CommandFailed(format!(
                "hw_access disable failed: {}",
                output.status
            )));
        }

        Ok(())
    }

    /// Set a hardware access key of 1–16 bare ASCII hexadecimal digits.
    ///
    /// Returns [`MlxError::InvalidKey`] before execution for malformed keys, or
    /// [`MlxError::DryRun`] with the key redacted when dry-run is enabled.
    /// Launch or exit failures return [`MlxError::CommandFailed`] without tool
    /// output, which may contain keys. This operation does not classify Already
    /// responses; any successful exit returns `Ok(())`.
    pub fn set_key(&self, device_id: &str, key: &str) -> MlxResult<()> {
        if !Self::is_valid_key(key) {
            return Err(MlxError::InvalidKey);
        }

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&[
                "-d",
                device_id,
                "set_key",
                "[REDACTED]",
            ])));
        }

        let args = ["-d", device_id, "set_key", key];
        let output = Command::new(&self.flint_path)
            .args(args)
            .output()
            .map_err(|e| MlxError::CommandFailed(format!("Failed to execute set_key: {e}")))?;

        if !output.status.success() {
            return Err(MlxError::CommandFailed(format!(
                "set_key failed: {}",
                output.status
            )));
        }

        Ok(())
    }

    // burn burns a firmware image onto the device. This runs:
    // flint -d <device> -y -i <image_path> burn
    //
    // TODO(chet): I realize this is a weird place to put `burn`, but this
    // was where all of the existing `flint` calls were, so I wanted to
    // keep them together for now. Ultimately I want to refactor/collapse
    // all of the mlxconfig-* stuff into a single crate with everything,
    // at which point I think things can be generalized, or at least maybe
    // restructured per command? Tbd.
    pub fn burn(&self, device_id: &str, image_path: &Path) -> MlxResult<String> {
        if !image_path.exists() {
            return Err(MlxError::CommandFailed(format!(
                "Firmware image does not exist: {}",
                image_path.display()
            )));
        }

        let image_str = image_path.to_string_lossy();
        let args = ["-d", device_id, "-y", "-i", &image_str, "burn"];

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&args)));
        }

        let output = Command::new(&self.flint_path)
            .args(args)
            .output()
            .map_err(|e| MlxError::CommandFailed(format!("Failed to execute burn: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            if stderr.contains("Permission denied") || stdout.contains("Permission denied") {
                return Err(MlxError::PermissionDenied);
            }
            if stderr.contains("Cannot open") || stdout.contains("Cannot open") {
                return Err(MlxError::DeviceNotFound(device_id.to_string()));
            }
            let error_msg = format!("stdout: {}\nstderr: {}", stdout.trim(), stderr.trim());
            return Err(MlxError::CommandFailed(error_msg));
        }

        Ok(stdout)
    }

    // verify_image verifies the firmware on the device against a given
    // image file. This runs: flint -d <device> -i <image_path> verify
    //
    // TODO(chet): See comments above in `fn burn` re: why this is in
    // the lockdown crate. Seems kind of weird, but I'm also trying to
    // keep command usage together, and right now all of the `flint`
    // stuff is in here.
    pub fn verify_image(&self, device_id: &str, image_path: &Path) -> MlxResult<String> {
        if !image_path.exists() {
            return Err(MlxError::CommandFailed(format!(
                "Firmware image does not exist: {}",
                image_path.display()
            )));
        }

        let image_str = image_path.to_string_lossy();
        let args = ["-d", device_id, "-i", &image_str, "verify"];

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&args)));
        }

        let output = Command::new(&self.flint_path)
            .args(args)
            .output()
            .map_err(|e| {
                MlxError::CommandFailed(format!("Failed to execute verify with image: {e}"))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            if stderr.contains("Permission denied") || stdout.contains("Permission denied") {
                return Err(MlxError::PermissionDenied);
            }
            if stderr.contains("Cannot open") || stdout.contains("Cannot open") {
                return Err(MlxError::DeviceNotFound(device_id.to_string()));
            }
            let error_msg = format!("stdout: {}\nstderr: {}", stdout.trim(), stderr.trim());
            return Err(MlxError::CommandFailed(error_msg));
        }

        Ok(stdout)
    }

    // Keep the library's bare-hex rule, allowing unpadded values up to 64 bits.
    fn is_valid_key(key: &str) -> bool {
        (1..=16).contains(&key.len()) && key.bytes().all(|c| c.is_ascii_hexdigit())
    }

    // validate_device_id validates device ID format.
    pub fn validate_device_id(device_id: &str) -> MlxResult<()> {
        // Accept various formats: PCI addresses (XX:XX.X), device paths, or names
        if device_id.is_empty() {
            return Err(MlxError::InvalidDeviceId(
                "Device ID cannot be empty".to_string(),
            ));
        }

        // Basic validation.
        // TODO(chet): Wire this in with the device module ID parsing; this
        // is basically just a placeholder for me to improve on later.
        if device_id.contains(' ') {
            return Err(MlxError::InvalidDeviceId(
                "Device ID cannot contain spaces".to_string(),
            ));
        }

        Ok(())
    }
}

impl Default for FlintRunner {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self::with_path("flint"))
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    // Compare error variants without requiring MlxError to implement PartialEq.
    fn err_kind(e: &MlxError) -> &'static str {
        match e {
            MlxError::CommandFailed(_) => "CommandFailed",
            MlxError::DeviceNotFound(_) => "DeviceNotFound",
            MlxError::InvalidDeviceId(_) => "InvalidDeviceId",
            MlxError::AlreadyLocked => "AlreadyLocked",
            MlxError::AlreadyUnlocked => "AlreadyUnlocked",
            MlxError::InvalidKey => "InvalidKey",
            MlxError::PermissionDenied => "PermissionDenied",
            MlxError::FlintNotFound => "FlintNotFound",
            MlxError::ParseError(_) => "ParseError",
            MlxError::DryRun(_) => "DryRun",
            MlxError::IoError(_) => "IoError",
            MlxError::SerializationError(_) => "SerializationError",
        }
    }

    /// Accept full derived keys and shorter tool-supported inputs while keeping
    /// validation limited to the bare hexadecimal representation of a 64-bit key.
    #[test]
    fn is_valid_key_accepts_up_to_sixteen_hex_digits() {
        value_scenarios!(
            run = FlintRunner::is_valid_key;
            "a single hex digit" {
                // The minimum accepted width must not require zero padding.
                "a" => true,
            }

            "eight lowercase hex digits" {
                "0a1b2c3d" => true,
            }

            "eight uppercase hex digits" {
                "ABCDEF01" => true,
            }

            "eight digits, all numeric" {
                "12345678" => true,
            }

            "shorter unpadded keys" {
                // Callers may pass a key below eight digits without padding it.
                "1234567" => true,
            }

            "keys longer than eight digits" {
                // Intermediate widths must work alongside eight and sixteen.
                "123456789" => true,
            }

            "a full 64-bit key" {
                // Derived keys encode all eight bytes as sixteen hex digits.
                "0123456789abcdef" => true,
            }

            "seventeen digits exceeds the key width" {
                // A leading zero must not bypass the representation's width limit.
                "0123456789abcdef0" => false,
            }

            "empty string" {
                "" => false,
            }

            "a non-hex character is rejected" {
                "1234567g" => false,
            }

            "a space is not a hex digit" {
                "1234 678" => false,
            }

            "0x-prefixed is not bare hex" {
                "0x123456" => false,
            }
        );
    }

    // validate_device_id rejects empty and space-containing IDs, accepts the rest.
    // Errors aren't PartialEq, so the rejection rows use Fails + map_err(drop).
    #[test]
    fn validate_device_id_rejects_empty_and_spaces() {
        scenarios!(
            run = |id| FlintRunner::validate_device_id(id).map_err(drop);
            "a PCI-style address is accepted" {
                "0000:01:00.0" => Yields(()),
            }

            "a simple device name is accepted" {
                "mlx5_0" => Yields(()),
            }

            "a device path is accepted" {
                "/dev/mst/mt4119_pciconf0" => Yields(()),
            }

            "empty is rejected" {
                "" => Fails,
            }

            "a leading space is rejected" {
                " 01:00.0" => Fails,
            }

            "an interior space is rejected" {
                "01:00 .0" => Fails,
            }
        );
    }

    // validate_device_id's two rejection paths carry distinct InvalidDeviceId
    // messages; pin which message each path produces.
    #[test]
    fn validate_device_id_error_messages() {
        let empty = FlintRunner::validate_device_id("").unwrap_err();
        assert_eq!(err_kind(&empty), "InvalidDeviceId");
        assert_eq!(
            empty.to_string(),
            "invalid device ID format: Device ID cannot be empty"
        );

        let spaced = FlintRunner::validate_device_id("a b").unwrap_err();
        assert_eq!(err_kind(&spaced), "InvalidDeviceId");
        assert_eq!(
            spaced.to_string(),
            "invalid device ID format: Device ID cannot contain spaces"
        );
    }

    // build_command joins the path and args with single spaces. Covers a custom
    // path, an empty arg slice (trailing space after the path), and a single arg.
    #[test]
    fn build_command_formats_path_and_args() {
        let runner = FlintRunner::with_path("/opt/mellanox/mft/bin/flint");
        value_scenarios!(
            run = |args| runner.build_command(args);
            "typical query args" {
                &["-d", "dev0", "q"][..] => "/opt/mellanox/mft/bin/flint -d dev0 q".to_string(),
            }

            "empty args leaves a trailing space" {
                &[][..] => "/opt/mellanox/mft/bin/flint ".to_string(),
            }

            "a single arg" {
                &["burn"][..] => "/opt/mellanox/mft/bin/flint burn".to_string(),
            }
        );
    }

    // with_path / with_dry_run are builders; read the private fields back to
    // confirm the path is stored verbatim and dry_run defaults off then toggles.
    #[test]
    fn builders_store_path_and_dry_run() {
        let plain = FlintRunner::with_path("flint");
        assert_eq!(plain.flint_path, "flint");
        assert!(!plain.dry_run, "dry_run defaults to false");

        let dry = FlintRunner::with_path("/usr/bin/flint").with_dry_run(true);
        assert_eq!(dry.flint_path, "/usr/bin/flint");
        assert!(dry.dry_run, "with_dry_run(true) enables dry-run");

        // with_dry_run(false) is the identity for the flag.
        let off = FlintRunner::with_path("flint").with_dry_run(false);
        assert!(!off.dry_run);
    }

    /// Keep dry-run diagnostics useful without exposing keys through either
    /// error formatter or requiring a flint executable.
    #[test]
    fn dry_run_methods_short_circuit() {
        let runner = FlintRunner::with_path("flint").with_dry_run(true);

        // query_device has no key validation: always DryRun in dry-run mode.
        let q = runner.query_device("dev0").unwrap_err();
        assert_eq!(err_kind(&q), "DryRun");
        assert_eq!(
            q.to_string(),
            "dry run - would have executed: flint -d dev0 q"
        );

        // enable_hw_access: valid key -> DryRun with the enable command string.
        let en = runner.enable_hw_access("dev0", "0a1b2c3d").unwrap_err();
        assert_eq!(err_kind(&en), "DryRun");
        assert_eq!(
            en.to_string(),
            "dry run - would have executed: flint -d dev0 hw_access enable [REDACTED]"
        );

        // disable_hw_access: valid key -> DryRun with the disable command string.
        let dis = runner.disable_hw_access("dev0", "0a1b2c3d").unwrap_err();
        assert_eq!(err_kind(&dis), "DryRun");
        assert_eq!(
            dis.to_string(),
            "dry run - would have executed: flint -d dev0 hw_access disable [REDACTED]"
        );

        // set_key: valid key -> DryRun with the set_key command string.
        let sk = runner.set_key("dev0", "0a1b2c3d").unwrap_err();
        assert_eq!(err_kind(&sk), "DryRun");
        assert_eq!(
            sk.to_string(),
            "dry run - would have executed: flint -d dev0 set_key [REDACTED]"
        );

        for error in [en, dis, sk] {
            assert!(!format!("{error:?}").contains("0a1b2c3d"));
        }
    }

    /// Reject malformed keys before dry-run command construction so validation
    /// errors cannot disclose the rejected input.
    #[test]
    fn invalid_key_takes_precedence_over_dry_run() {
        let runner = FlintRunner::with_path("flint").with_dry_run(true);

        // Each key-taking method must reject malformed keys without exposing them.
        value_scenarios!(
            run = |which| {
                let bad = "xyz";
                let e = match which {
                    "enable" => runner.enable_hw_access("dev0", bad).unwrap_err(),
                    "disable" => runner.disable_hw_access("dev0", bad).unwrap_err(),
                    "set_key" => runner.set_key("dev0", bad).unwrap_err(),
                    _ => unreachable!(),
                };
                assert!(!e.to_string().contains(bad));
                assert!(!format!("{e:?}").contains(bad));
                err_kind(&e)
            };
            "enable_hw_access rejects a bad key" {
                "enable" => "InvalidKey",
            }

            "disable_hw_access rejects a bad key" {
                "disable" => "InvalidKey",
            }

            "set_key rejects a bad key" {
                "set_key" => "InvalidKey",
            }
        );
    }

    /// Forward full keys through the manager without changing argument boundaries,
    /// while preserving success, Already results, and secret-free failure details.
    #[cfg(unix)]
    #[test]
    fn key_commands_preserve_arguments_and_results() {
        use std::os::unix::fs::PermissionsExt;

        use crate::lockdown::lockdown::LockdownManager;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flint");
        let key = "0a1B2c3D4e5F6789";

        value_scenarios!(
            run = |(operation, status, stdout, stderr): (&str, i32, &str, &str)| {
                let expected_args: &[&str] = match operation {
                    "unlock" => &["-d", "dev0", "hw_access", "enable", key],
                    "lock" => &["-d", "dev0", "hw_access", "disable", key],
                    "set_key" => &["-d", "dev0", "set_key", key],
                    _ => unreachable!(),
                };
                let argument_checks = expected_args
                    .iter()
                    .enumerate()
                    .map(|(index, arg)| format!("[ \"${}\" = \"{arg}\" ] || exit 99", index + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                // A wrong operation, argument boundary, or key spelling exits 99
                // before the requested result; echoed output uses both key cases.
                std::fs::write(
                    &path,
                    format!(
                        "#!/bin/sh\n[ \"$#\" -eq {} ] || exit 99\n{argument_checks}\n\
                         printf '%s\\n' \"$*\" '{stdout}'\n\
                         printf '%s\\n' '{}' '{stderr}' >&2\nexit {status}\n",
                        expected_args.len(),
                        key.to_uppercase(),
                    ),
                )
                .unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
                let manager =
                    LockdownManager::with_runner(FlintRunner::with_path(path.to_str().unwrap()));
                let result = match operation {
                    "unlock" => manager.unlock_device("dev0", key).map(|_| ()),
                    "lock" => manager.lock_device("dev0", key).map(|_| ()),
                    "set_key" => manager.set_device_key("dev0", key),
                    _ => unreachable!(),
                };
                match result {
                    Ok(()) => ("Ok", String::new()),
                    Err(error) => {
                        let display = error.to_string();
                        let debug = format!("{error:?}");
                        for rendered in [&display, &debug] {
                            assert!(!rendered.contains(key));
                            assert!(!rendered.contains(&key.to_uppercase()));
                        }
                        (err_kind(&error), display)
                    }
                }
            };
            "enable failure" {
                // Unlock failures can echo the key in either output stream.
                ("unlock", 42, "", "") =>
                    ("CommandFailed", "command execution failed: hw_access enable failed: exit status: 42".to_string()),
            }

            "disable failure" {
                // Lock failures must preserve status without echoed key text.
                ("lock", 42, "", "") =>
                    ("CommandFailed", "command execution failed: hw_access disable failed: exit status: 42".to_string()),
            }

            "set_key failure" {
                // Direct key updates have their own command-failure branch.
                ("set_key", 42, "", "") =>
                    ("CommandFailed", "command execution failed: set_key failed: exit status: 42".to_string()),
            }

            "a successful command is not an error" {
                // Output suppression must not turn a successful key update into failure.
                ("set_key", 0, "", "") => ("Ok", String::new()),
            }

            "already disabled on a successful exit" {
                // Flint's stdout can report an existing lock despite exit zero.
                ("lock", 0, "already disabled", "") =>
                    ("AlreadyLocked", "hardware access is already disabled".to_string()),
            }

            "already enabled takes precedence over a failed exit" {
                // The semantic stderr result must win over the nonzero exit status.
                ("unlock", 42, "", "already enabled") =>
                    ("AlreadyUnlocked", "hardware access is already enabled".to_string()),
            }
        );
    }

    /// Reject malformed keys before starting a non-dry-run executable; a valid
    /// key proves the same executable would otherwise be invoked.
    #[cfg(unix)]
    #[test]
    fn invalid_keys_do_not_start_the_executable() {
        use std::os::unix::fs::PermissionsExt;

        use crate::lockdown::lockdown::LockdownManager;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("flint");
        let marker = directory.path().join("flint.invoked");
        std::fs::write(&path, "#!/bin/sh\nprintf invoked > \"$0.invoked\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manager = LockdownManager::with_runner(FlintRunner::with_path(path.to_str().unwrap()));

        for operation in [
            // Locking must reject malformed input before spawn.
            "lock", // Unlock has an independent gate before its executable call.
            "unlock",
            // Setting a key must not invoke the tool with rejected input either.
            "set_key",
        ] {
            let result = match operation {
                "lock" => manager.lock_device("dev0", "xyz").map(|_| ()),
                "unlock" => manager.unlock_device("dev0", "xyz").map(|_| ()),
                "set_key" => manager.set_device_key("dev0", "xyz"),
                _ => unreachable!(),
            };
            assert!(
                matches!(result, Err(MlxError::InvalidKey)),
                "{operation}: {result:?}"
            );
            assert!(
                !marker.exists(),
                "{operation} invoked flint for an invalid key"
            );
        }

        // The absence assertions need a runnable fixture, not a missing executable.
        manager.set_device_key("dev0", "0a1B2c3D4e5F6789").unwrap();
        assert!(marker.exists());
    }

    // burn / verify_image check that the image path exists BEFORE any dry-run or
    // spawn, returning CommandFailed for a missing image. A nonexistent path is
    // deterministic regardless of dry-run, so both methods can be exercised here.
    #[test]
    fn burn_and_verify_reject_missing_image() {
        let runner = FlintRunner::with_path("flint").with_dry_run(true);
        let missing = Path::new("/nonexistent/definitely/not/here.bin");

        let b = runner.burn("dev0", missing).unwrap_err();
        assert_eq!(err_kind(&b), "CommandFailed");
        assert!(
            b.to_string().contains("Firmware image does not exist"),
            "burn names the missing image: {b}"
        );

        let v = runner.verify_image("dev0", missing).unwrap_err();
        assert_eq!(err_kind(&v), "CommandFailed");
        assert!(
            v.to_string().contains("Firmware image does not exist"),
            "verify_image names the missing image: {v}"
        );
    }

    // When the image exists and dry-run is on, burn/verify short-circuit to DryRun
    // with the full command string (including the resolved image path). Creates a
    // real temp file so the exists() gate passes deterministically.
    #[test]
    fn burn_and_verify_dry_run_with_existing_image() {
        let runner = FlintRunner::with_path("flint").with_dry_run(true);

        let existing = std::env::temp_dir().join(format!(
            "libmlx_runner_cov_{}_{}.bin",
            std::process::id(),
            line!()
        ));
        std::fs::write(&existing, b"fw").expect("write temp fixture");
        assert!(
            existing.exists(),
            "test fixture must exist: {}",
            existing.display()
        );

        let img = existing.to_string_lossy().to_string();

        let b = runner.burn("dev0", &existing).unwrap_err();
        assert_eq!(err_kind(&b), "DryRun");
        assert_eq!(
            b.to_string(),
            format!("dry run - would have executed: flint -d dev0 -y -i {img} burn")
        );

        let v = runner.verify_image("dev0", &existing).unwrap_err();
        assert_eq!(err_kind(&v), "DryRun");
        assert_eq!(
            v.to_string(),
            format!("dry run - would have executed: flint -d dev0 -i {img} verify")
        );

        let _ = std::fs::remove_file(&existing);
    }
}
