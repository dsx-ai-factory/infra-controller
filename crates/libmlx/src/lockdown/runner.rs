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

    // enable_hw_access enables hardware access with the provided key.
    pub fn enable_hw_access(&self, device_id: &str, key: &str) -> MlxResult<()> {
        // Validate key format (should be 8 hex digits for 64-bit key)
        if !Self::is_valid_key(key) {
            return Err(MlxError::InvalidKey);
        }

        let args = ["-d", device_id, "hw_access", "enable", key];

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&args)));
        }

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
            let error_msg = format!("stdout: {}\nstderr: {}", stdout.trim(), stderr.trim());
            return Err(MlxError::CommandFailed(error_msg));
        }

        Ok(())
    }

    // disable_hw_access disables hardware access with the provided key.
    pub fn disable_hw_access(&self, device_id: &str, key: &str) -> MlxResult<()> {
        // Validate key format (should be 8 hex digits for 64-bit key)
        if !Self::is_valid_key(key) {
            return Err(MlxError::InvalidKey);
        }

        let args = ["-d", device_id, "hw_access", "disable", key];

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&args)));
        }

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
            let error_msg = format!("stdout: {}\nstderr: {}", stdout.trim(), stderr.trim());
            return Err(MlxError::CommandFailed(error_msg));
        }

        Ok(())
    }

    // set_key sets a new hardware access key.
    pub fn set_key(&self, device_id: &str, key: &str) -> MlxResult<()> {
        if !Self::is_valid_key(key) {
            return Err(MlxError::InvalidKey);
        }

        let args = ["-d", device_id, "set_key", key];

        if self.dry_run {
            return Err(MlxError::DryRun(self.build_command(&args)));
        }

        let output = Command::new(&self.flint_path)
            .args(args)
            .output()
            .map_err(|e| MlxError::CommandFailed(format!("Failed to execute set_key: {e}")))?;

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let error_msg = format!("stdout: {}\nstderr: {}", stdout.trim(), stderr.trim());
            return Err(MlxError::CommandFailed(error_msg));
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

    // is_valid_key validates that the key is in the correct format (8 hex digits).
    fn is_valid_key(key: &str) -> bool {
        key.len() == 8 && key.chars().all(|c| c.is_ascii_hexdigit())
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

    // err_kind projects an MlxError onto a stable discriminant string so we can
    // distinguish *which* error a deterministic path returns without relying on
    // MlxError being PartialEq (it is not). Only the variants the deterministic
    // (no-flint-spawn) paths in this file can produce are named here.
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

    // is_valid_key accepts exactly 8 ASCII hex digits; anything else is rejected.
    // Covers: too short, too long, exactly-8 valid (upper and lower), non-hex
    // chars, empty, and embedded whitespace.
    #[test]
    fn is_valid_key_requires_eight_hex_digits() {
        value_scenarios!(
            run = FlintRunner::is_valid_key;
            "eight lowercase hex digits" {
                "0a1b2c3d" => true,
            }

            "eight uppercase hex digits" {
                "ABCDEF01" => true,
            }

            "eight digits, all numeric" {
                "12345678" => true,
            }

            "seven digits is too short" {
                "1234567" => false,
            }

            "nine digits is too long" {
                "123456789" => false,
            }

            "empty string" {
                "" => false,
            }

            "right length but non-hex char 'g'" {
                "1234567g" => false,
            }

            "right length but contains a space" {
                "1234 678" => false,
            }

            "0x-prefixed is not bare hex of length 8" {
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

    // In dry-run mode the command-issuing methods short-circuit to MlxError::DryRun
    // (carrying the would-be command string) before spawning flint, so these are
    // deterministic. Key-taking methods validate the key FIRST: an invalid key
    // yields InvalidKey even under dry-run; a valid key yields DryRun.
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
            "dry run - would have executed: flint -d dev0 hw_access enable 0a1b2c3d"
        );

        // disable_hw_access: valid key -> DryRun with the disable command string.
        let dis = runner.disable_hw_access("dev0", "0a1b2c3d").unwrap_err();
        assert_eq!(err_kind(&dis), "DryRun");
        assert_eq!(
            dis.to_string(),
            "dry run - would have executed: flint -d dev0 hw_access disable 0a1b2c3d"
        );

        // set_key: valid key -> DryRun with the set_key command string.
        let sk = runner.set_key("dev0", "0a1b2c3d").unwrap_err();
        assert_eq!(err_kind(&sk), "DryRun");
        assert_eq!(
            sk.to_string(),
            "dry run - would have executed: flint -d dev0 set_key 0a1b2c3d"
        );
    }

    // Key validation precedes the dry-run check in every key-taking method, so an
    // invalid key short-circuits to InvalidKey regardless of dry-run state.
    #[test]
    fn invalid_key_takes_precedence_over_dry_run() {
        let runner = FlintRunner::with_path("flint").with_dry_run(true);

        // Each key-taking method, fed a too-short key, must report InvalidKey.
        value_scenarios!(
            run = |which| {
                let bad = "xyz"; // not 8 hex digits
                let e = match which {
                    "enable" => runner.enable_hw_access("dev0", bad).unwrap_err(),
                    "disable" => runner.disable_hw_access("dev0", bad).unwrap_err(),
                    "set_key" => runner.set_key("dev0", bad).unwrap_err(),
                    _ => unreachable!(),
                };
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
