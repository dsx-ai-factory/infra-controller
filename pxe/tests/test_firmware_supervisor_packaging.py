# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Exercise the Scout build and payload paths without compiling Rust or booting."""

import json
import os
from pathlib import Path
import re
import shutil
import struct
from string import Template
import subprocess
import sys
import tempfile
import textwrap
import tomllib
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
SUPERVISOR = "mlx-firmware-supervisor"


def task(path, name):
    """Load the shipped task so tests execute the packaging source of truth."""
    with (REPO_ROOT / path).open("rb") as source:
        return tomllib.load(source)["tasks"][name]


class FirmwareSupervisorPackagingTest(unittest.TestCase):
    """Scout's helper must reach every boot path beside the matching Scout."""

    def setUp(self):
        """Record builders and dependency checks while retaining real copy and Debian tools."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        commands = self.directory / "commands"
        commands.mkdir()
        recorder = commands / "record"
        recorder.write_text(f"#!{sys.executable}\n" + textwrap.dedent('''\
            import json
            import os
            from pathlib import Path
            import sys

            name = Path(sys.argv[0]).name
            with open(os.environ["BUILD_COMMAND_LOG"], "a") as log:
                log.write(json.dumps([name, *sys.argv[1:]]) + "\\n")
            if name == "docker" and "build" in sys.argv[1:]:
                sys.exit(int(os.environ.get("DOCKER_BUILD_EXIT_CODE", "0")))
            if name == "cargo" and sys.argv[1:2] == ["metadata"]:
                print(json.dumps({"packages": [{
                    "name": "carbide-scout",
                    "targets": [{"kind": ["bin"], "name": "forge-scout"}],
                }]}))
            if name == "ldd" and sys.argv[1] == os.environ.get("MISSING_LIBRARY_FOR"):
                print("libfixture.so => not found")
        '''))
        recorder.chmod(0o755)
        for command in ("cargo", "docker", "ldd", "dpkg", "systemctl"):
            (commands / command).symlink_to(recorder)
        self.env = {
            **os.environ,
            "PATH": str(commands) + os.pathsep + os.environ["PATH"],
            "BUILD_COMMAND_LOG": str(self.directory / "commands.jsonl"),
            "REPO_ROOT": str(self.directory),
            "CARGO_HOME": str(self.directory / "cargo"),
            "CARBIDE_CRATE": "carbide-scout",
            "CARBIDE_PACKAGE": "scout",
            "CARBIDE_CRATE_DIR": "crates/scout",
            "CARBIDE_PROFILE": "release",
            "CARBIDE_ARCH": "x86_64",
            "PKG_VERSION": "1.0.0",
            "INSTALL_DIR": "/opt/forge",
            "pkg_args": "",
        }

    def run_task_script(self, path, name, **env):
        """Run one task body with fixture-owned paths, without task dependencies."""
        return subprocess.run(
            ["bash", "-c", task(path, name)["script"]],
            cwd=self.directory,
            env={**self.env, **env},
            capture_output=True,
            text=True,
            timeout=20,
        )

    def stage_package_inputs(self, arch="x86_64"):
        """Stage distinct local ELF payloads with the service configuration that ships."""
        release = self.directory / f"target/{arch}-unknown-linux-gnu/release"
        release.mkdir(parents=True)
        # Different ELF payloads make a swapped or duplicated copy observable.
        for name, source in (("forge-scout", "/bin/true"), (SUPERVISOR, "/bin/false")):
            shutil.copyfile(source, release / name)
            (release / name).chmod(0o755)
        misc = self.directory / "crates/scout/misc"
        (misc / "DEBIAN").mkdir(parents=True)
        shutil.copyfile(REPO_ROOT / "crates/scout/misc/forge-scout.service", misc / "forge-scout.service")
        return release

    def test_scout_builds_select_the_supervisor_binary(self):
        """Each distinct builder must request the helper, not only its library."""
        cases = [
            # Both native targets catch a target triple hard-coded to either architecture.
            ("Makefile-build.toml", "build-component-local", "x86_64", "release", "carbide-scout", ["forge-scout", SUPERVISOR], False),
            ("Makefile-build.toml", "build-component-local", "aarch64", "release", "carbide-scout", ["forge-scout", SUPERVISOR], False),
            # The x86 container also strips binaries; ARM skips that step here.
            ("Makefile-build.toml", "build-component-in-container", "x86_64", "release", "carbide-scout", ["forge-scout", SUPERVISOR], True),
            ("Makefile-build.toml", "build-component-in-container", "aarch64", "release", "carbide-scout", ["forge-scout", SUPERVISOR], False),
            # The standalone cross task additionally builds the admin CLI.
            ("Makefile.toml", "build-cli-cross-aarch64", "aarch64", "release", "carbide-scout", ["nico-admin-cli", "forge-scout", SUPERVISOR], True),
            # Debug Scout still needs its helper but must not strip release output.
            ("Makefile-build.toml", "build-component-in-container", "x86_64", "debug", "carbide-scout", ["forge-scout", SUPERVISOR], False),
            # Neither independent component builder adds Scout binaries to other crates.
            ("Makefile-build.toml", "build-component-local", "x86_64", "debug", "nico-api", [], False),
            ("Makefile-build.toml", "build-component-in-container", "x86_64", "debug", "nico-api", [], False),
        ]
        for path, name, arch, profile, crate, expected_bins, expect_strip in cases:
            with self.subTest(task=name, arch=arch, profile=profile, crate=crate):
                log = self.directory / "commands.jsonl"
                log.write_text("")
                result = self.run_task_script(path, name, CARBIDE_ARCH=arch, CARBIDE_PROFILE=profile, CARBIDE_CRATE=crate)
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = [json.loads(line) for line in log.read_text().splitlines()]
                build = next(command for command in commands if "build" in command)
                expected_packages = (
                    ["nico-admin-cli", "carbide-scout", "carbide-libmlx"]
                    if name == "build-cli-cross-aarch64"
                    else [crate] + (["carbide-libmlx"] if crate == "carbide-scout" else [])
                )
                self.assertEqual(
                    [build[index + 1] for index, arg in enumerate(build) if arg == "-p"],
                    expected_packages,
                )
                self.assertEqual("--release" in build, profile == "release")
                self.assertIn(f"--target={arch}-unknown-linux-gnu", build)
                self.assertEqual(
                    [build[index + 1] for index, arg in enumerate(build) if arg == "--bin"],
                    expected_bins,
                )
                strip_commands = [command for command in commands if any(arg.endswith("-strip") for arg in command)]
                self.assertEqual(len(strip_commands), 1 if expect_strip else 0)
                for command in strip_commands:
                    for binary in expected_bins:
                        self.assertIn(f"/carbide/target/{arch}-unknown-linux-gnu/release/{binary}", command)

    def test_failed_container_build_preserves_the_build_error(self):
        """A failed build must not let packaging reuse stale release binaries."""
        # x86 release must not strip stale output; ARM must not hide the failure
        # behind the successful echo in its skip-strip path.
        for arch in ("x86_64", "aarch64"):
            with self.subTest(arch=arch):
                log = self.directory / "commands.jsonl"
                log.write_text("")
                result = self.run_task_script(
                    "Makefile-build.toml", "build-component-in-container",
                    CARBIDE_ARCH=arch, DOCKER_BUILD_EXIT_CODE="42",
                )
                self.assertEqual(result.returncode, 42, result.stdout + result.stderr)
                commands = [json.loads(line) for line in log.read_text().splitlines()]
                self.assertEqual(len(commands), 1, commands)
                self.assertEqual(commands[0][:2], ["docker", "run"])
                self.assertIn("build", commands[0])

    def test_debug_test_prerequisites_build_the_supervisor(self):
        """The supported test task must build the helper before any unit tests run."""
        self.assertIn("build-debug-test-prerequisites", task("Makefile.toml", "correctly-execute-tests")["dependencies"])
        self.assertIn("build-nic-firmware-test-helper", task("Makefile.toml", "build-debug-test-prerequisites")["dependencies"])
        helper = task("Makefile.toml", "build-nic-firmware-test-helper")
        subprocess.run([helper["command"], *helper["args"]], cwd=self.directory, env=self.env, check=True, timeout=20)
        commands = [json.loads(line) for line in (self.directory / "commands.jsonl").read_text().splitlines()]
        self.assertEqual(commands, [["cargo", "build", "-p", "carbide-libmlx", "--bin", SUPERVISOR]])

    def test_debian_package_installs_matching_executable_siblings(self):
        """Inspect the built archive, not merely the copy command's argument list."""
        release = self.stage_package_inputs()
        self.assertNotEqual((release / "forge-scout").read_bytes(), (release / SUPERVISOR).read_bytes())
        result = self.run_task_script("Makefile-package.toml", "package-component")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        package = self.directory / "target/debs/forge-scout_1.0.0_amd64.deb"
        extracted = self.directory / "extracted"
        subprocess.run(["dpkg-deb", "--extract", str(package), str(extracted)], check=True, timeout=20)
        for name in ("forge-scout", SUPERVISOR):
            installed = extracted / "opt/forge" / name
            self.assertEqual(installed.read_bytes(), (release / name).read_bytes())
            self.assertTrue(os.access(installed, os.X_OK), name)
        service = extracted / "lib/systemd/system/forge-scout.service"
        self.assertEqual(service.read_bytes(), (REPO_ROOT / "crates/scout/misc/forge-scout.service").read_bytes())
        settings = service.read_text().splitlines()
        scope = "Environment=MLX_FIRMWARE_SUPERVISOR_LAUNCH=systemd-scope"
        overrides = "EnvironmentFile=-/opt/forge/forge-scout.env"
        self.assertIn(scope, settings)
        self.assertIn(overrides, settings)
        self.assertLess(settings.index(scope), settings.index(overrides))
        package_name = subprocess.check_output(["dpkg-deb", "--field", str(package), "Package"], text=True, timeout=20)
        self.assertEqual(package_name.strip(), "scout")

    def test_missing_or_nonexecutable_helper_rejects_packaging(self):
        """A package that cannot launch the helper must never be published."""
        release = self.stage_package_inputs()
        helper = release / SUPERVISOR
        # An existing file without its executable bit is not a usable helper.
        helper.chmod(0o644)
        result = self.run_task_script("Makefile-package.toml", "package-component")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.directory / "target/debs").exists())
        # A missing build output must fail too, rather than reuse stale staging.
        staged_helper = self.directory / "target/x86_64-unknown-linux-gnu/forge-scout_1.0.0_amd64/opt/forge" / SUPERVISOR
        staged_helper.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(helper, staged_helper)
        staged_helper.chmod(0o755)
        helper.unlink()
        result = self.run_task_script("Makefile-package.toml", "package-component")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.directory / "target/debs").exists())

    def test_arm_package_rejects_a_non_elf_supervisor(self):
        """An executable filename alone does not prove an ARM helper was built."""
        # The script fixture must fail the real ARM ELF/page-size check even though
        # both expected executable paths exist and have their executable bits.
        release = self.stage_package_inputs(arch="aarch64")
        for name in ("forge-scout", SUPERVISOR):
            (release / name).write_text("#!/bin/sh\nexit 0\n")
        scripts = self.directory / "scripts"
        scripts.mkdir()
        shutil.copyfile(REPO_ROOT / "scripts/check-aarch64-pagesize.sh", scripts / "check-aarch64-pagesize.sh")
        result = self.run_task_script("Makefile-package.toml", "package-component", CARBIDE_ARCH="aarch64")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("FAIL no aarch64 ELF executable/shared object(s) found", result.stderr)
        self.assertFalse((self.directory / "target/debs").exists())

    def test_arm_package_checks_supervisor_load_segments(self):
        """A real ELF header must pass both page alignment and load-address checks."""
        release = self.stage_package_inputs(arch="aarch64")
        scripts = self.directory / "scripts"
        scripts.mkdir()
        shutil.copyfile(REPO_ROOT / "scripts/check-aarch64-pagesize.sh", scripts / "check-aarch64-pagesize.sh")

        def elf(alignment, address):
            # One ELF64 AArch64 PT_LOAD segment is enough for the shipped readelf
            # validator. These bytes are inspected and packaged, never executed.
            header = struct.pack(
                "<16sHHIQQQIHHHHHH", b"\x7fELF\x02\x01\x01" + bytes(9),
                2, 183, 1, address, 64, 0, 0, 64, 56, 1, 0, 0, 0,
            )
            segment = struct.pack("<IIQQQQQQ", 1, 5, 0, address, address, 120, 120, alignment)
            return header + segment

        (release / "forge-scout").write_bytes(elf(65536, 0x400000))
        for name, alignment, address, expected_error in [
            ("small alignment", 4096, 0x400000, "p_align=0x1000 below required 0x10000"),
            ("incongruent address", 65536, 0x401000, "not congruent modulo 0x10000"),
            ("valid ARM helper", 65536, 0x400000, None),
        ]:
            with self.subTest(case=name):
                (release / SUPERVISOR).write_bytes(elf(alignment, address))
                result = self.run_task_script("Makefile-package.toml", "package-component", CARBIDE_ARCH="aarch64")
                if expected_error:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(expected_error, result.stdout + result.stderr)
                    self.assertIn(str(release / SUPERVISOR), result.stdout + result.stderr)
                    self.assertFalse((self.directory / "target/debs").exists())
                else:
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    package = self.directory / "target/debs/forge-scout_1.0.0_arm64.deb"
                    extracted = self.directory / "arm-extracted"
                    subprocess.run(["dpkg-deb", "--extract", str(package), str(extracted)], check=True, timeout=20)
                    self.assertEqual((extracted / "opt/forge" / SUPERVISOR).read_bytes(), (release / SUPERVISOR).read_bytes())

    def test_scout_launch_override_survives_environment_regeneration(self):
        """The pre-start generator must retain the override from its template."""
        for arch in ("aarch64", "x86_64"):
            with self.subTest(arch=arch):
                source = (REPO_ROOT / f"pxe/mkosi.profiles/scout-oss-{arch}/mkosi.extra/opt/forge/forge-scout-pre.sh").read_text()
                generator = source[source.index('rm "/opt/forge/forge-scout.env"'):source.index("# Use the PXE URL")]
                install = self.directory / arch / "opt/forge"
                install.mkdir(parents=True)
                template = install / "forge-scout.env.template"
                template.write_text("MLX_FIRMWARE_SUPERVISOR_LAUNCH=standalone\n")
                generated = install / "forge-scout.env"
                generated.write_text("MLX_FIRMWARE_SUPERVISOR_LAUNCH=systemd-scope\n")
                for start in (1, 2):
                    with self.subTest(start=start):
                        result = subprocess.run(
                            ["sh", "-c", generator.replace("/opt/forge/", str(install) + "/")],
                            env={**self.env, "server_uri": "https://core.test", "machine_id": f"fixture-host-{start}", "cli_cmd": "fixture-command"},
                            capture_output=True, text=True, timeout=20,
                        )
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertEqual(generated.read_text().splitlines(), [
                            "MLX_FIRMWARE_SUPERVISOR_LAUNCH=standalone",
                            "server_uri=https://core.test",
                            f"machine_id=fixture-host-{start}",
                            "cli_cmd=fixture-command",
                        ])

    def test_images_install_the_scout_service_and_check_the_helper(self):
        """Exercise both postinst sources without claiming to build or boot either image."""
        # These profiles are independent files; checking only ARM would miss an
        # x86 image that no longer installs the package containing the service.
        for arch in ("aarch64", "x86_64"):
            with self.subTest(arch=arch):
                source = (REPO_ROOT / f"pxe/mkosi.profiles/scout-oss-{arch}/mkosi.postinst.chroot").read_text()
                service_install = re.findall(
                    r"(?m)^dpkg -i /build-output/forge-scout\.deb\nsystemctl enable forge-scout$",
                    source,
                )
                self.assertEqual(len(service_install), 1)
                log = self.directory / "commands.jsonl"
                log.write_text("")
                result = subprocess.run(
                    ["bash", "-c", "set -Ee\n" + service_install[0]],
                    env=self.env,
                    capture_output=True,
                    text=True,
                    timeout=20,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(
                    [json.loads(line) for line in log.read_text().splitlines()],
                    [["dpkg", "-i", "/build-output/forge-scout.deb"], ["systemctl", "enable", "forge-scout"]],
                )

                loops = re.findall(r"(?ms)^for bin in (.*?)\ndo\n(.*?)\ndone$", source)
                checks = []
                image = self.directory / f"{arch}-image"
                helper = image / "opt/forge" / SUPERVISOR
                for guard in ("if [ ! -e $bin ]", "$(ldd ${bin} | fgrep -c 'not found')"):
                    matching = [(binaries, body) for binaries, body in loops if guard in body]
                    self.assertEqual(len(matching), 1, guard)
                    binaries, body = matching[0]
                    paths = binaries.replace("\\\n", " ").split()
                    self.assertIn(f"/opt/forge/{SUPERVISOR}", paths)
                    block = f"for bin in {binaries}\ndo\n{body}\ndone"
                    for path in paths:
                        installed = image / path.removeprefix("/")
                        installed.parent.mkdir(parents=True, exist_ok=True)
                        installed.write_bytes(b"fixture command")
                        block = block.replace(path, str(installed))
                    checks.append(block)

                cases = [
                    # A complete install reaches the dependency check for the helper.
                    ("present", True, False, 0),
                    # Missing helper output must fail even when Scout is installed.
                    ("missing helper", False, False, 1),
                    # An installed helper is unusable when its loader lacks a library.
                    ("missing helper library", True, True, 1),
                ]
                for name, present, missing_library, expected_exit in cases:
                    with self.subTest(check=name):
                        if present:
                            helper.write_bytes(b"fixture command")
                        else:
                            helper.unlink()
                        log.write_text("")
                        result = subprocess.run(
                            ["bash", "-c", "set -Ee\n" + "\n".join(checks)],
                            env={**self.env, "MISSING_LIBRARY_FOR": str(helper) if missing_library else ""},
                            capture_output=True,
                            text=True,
                            timeout=20,
                        )
                        self.assertEqual(result.returncode, expected_exit, result.stdout + result.stderr)
                        if present:
                            commands = [json.loads(line) for line in log.read_text().splitlines()]
                            self.assertIn(["ldd", str(helper)], commands)
                        if expected_exit:
                            self.assertIn(str(helper), result.stdout)

    def test_raw_bfb_payload_installs_current_and_legacy_executables(self):
        """New BFBs install both binaries; older Scout-only payloads still work."""
        release = self.directory / "target/aarch64-unknown-linux-gnu/release"
        release.mkdir(parents=True)
        for name in ("forge-scout", SUPERVISOR):
            (release / name).write_bytes(name.encode())
            (release / name).chmod(0o755)
        # Local and CI have separate copy arguments; either can omit the helper.
        for name in ("bfb-add-scout-to-bfb", "bfb-add-scout-to-bfb-ci"):
            with self.subTest(task=name):
                destination = self.directory / name
                destination.mkdir()
                copy = task("pxe/Makefile.toml", name)
                variables = {**self.env, "BUILD_LOCATION": str(destination)}
                args = [Template(arg).substitute(variables) for arg in copy["args"]]
                subprocess.run([copy["command"], *args], check=True, timeout=20)
                self.assertEqual({entry.name for entry in destination.iterdir()}, {"forge-scout", SUPERVISOR})
                for binary in ("forge-scout", SUPERVISOR):
                    self.assertEqual((destination / binary).read_bytes(), (release / binary).read_bytes())
                    self.assertTrue(os.access(destination / binary, os.X_OK))

                # A new BFB build must fail when its matching helper is absent,
                # even though retained legacy payloads remain installable below.
                helper = release / SUPERVISOR
                helper.rename(release / "retained-helper")
                result = subprocess.run([copy["command"], *args], capture_output=True, text=True, timeout=20)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(SUPERVISOR, result.stderr)
                (release / "retained-helper").rename(helper)

        source = (REPO_ROOT / "pxe/templates/user-data").read_text()
        block = source[source.index('local forge_path="/mnt/opt/forge"'):source.index("local forge_path_debs=")]
        block = block.replace('local forge_path="/mnt/opt/forge"', 'local forge_path="${INSTALL_ROOT}"')
        block = re.sub(r"(?m)^  /(\S+)$", r'  "${PAYLOAD_ROOT}/\1"', block)
        # The optional certificate is irrelevant here; absent optional artifacts
        # follow the installer's existing skip path after template directives go.
        block = re.sub(r"(?m)^  {%.*%}\n", "", block)
        legacy_payload = self.directory / "legacy-bfb"
        legacy_payload.mkdir()
        shutil.copyfile(release / "forge-scout", legacy_payload / "forge-scout")
        cases = [
            # Exercise the actual current-build copy output with both executables.
            ("current", destination, ("forge-scout", SUPERVISOR)),
            # A retained BFB may use the newer PXE template without the new helper.
            ("legacy", legacy_payload, ("forge-scout",)),
        ]
        for name, payload, binaries in cases:
            with self.subTest(payload=name):
                # Clear execute bits so omitting either chmod cannot pass accidentally.
                for binary in binaries:
                    (payload / binary).chmod(0o644)
                installed = self.directory / "installed" / name / "opt/forge"
                result = subprocess.run(
                    ["bash", "-c", 'set -e\nilog() { :; }\ninstall_payload() {\n' + block + '\n}\ninstall_payload\necho installer-continued'],
                    env={**self.env, "PAYLOAD_ROOT": str(payload), "INSTALL_ROOT": str(installed)},
                    capture_output=True,
                    text=True,
                    timeout=20,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("installer-continued", result.stdout.splitlines())
                self.assertEqual({entry.name for entry in installed.iterdir()}, set(binaries))
                for binary in binaries:
                    self.assertEqual((installed / binary).read_bytes(), (release / binary).read_bytes())
                    self.assertTrue(os.access(installed / binary, os.X_OK), binary)
                    self.assertFalse((payload / binary).exists(), binary)

    @unittest.skipUnless(shutil.which("mksquashfs") and shutil.which("unsquashfs"), "squashfs tools are required")
    def test_ci_checks_supervisor_from_a_squashfs_fixture(self):
        """Run the shipped image check against tiny fixtures with distinct file types."""
        workflow = (REPO_ROOT / ".github/workflows/build-boot-artifacts.yml").read_text()
        step = workflow.split("      - name: Verify firmware supervisor in built Scout image\n", 1)[1].split("      - name:", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        for name in ("executable", "missing", "symlink", "nonexecutable", "corrupt-image"):
            with self.subTest(payload=name):
                root = self.directory / name
                source = root / "image-root"
                helper = source / "opt/forge" / SUPERVISOR
                helper.parent.mkdir(parents=True)
                if name == "symlink":
                    # The target exists and is executable, so only the explicit
                    # symlink guard rejects this extracted helper.
                    helper.symlink_to("/bin/true")
                elif name != "missing":
                    helper.write_bytes(b"fixture helper")
                    helper.chmod(0o755 if name == "executable" else 0o644)
                artifacts = root / "pxe/static/blobs/internal/x86_64"
                artifacts.mkdir(parents=True)
                image = artifacts / "scout.squashfs"
                if name == "corrupt-image":
                    image.write_bytes(b"invalid squashfs header")
                else:
                    subprocess.run(
                        ["mksquashfs", str(source), str(image), "-noappend", "-no-progress", "-processors", "1"],
                        check=True, capture_output=True, timeout=20,
                    )
                validation_temp = root / "validation-tmp"
                validation_temp.mkdir()
                result = subprocess.run(
                    ["bash", "-c", script], cwd=root,
                    env={**self.env, "ARCH": "x86_64", "TMPDIR": str(validation_temp)},
                    capture_output=True, text=True, timeout=20,
                )
                self.assertEqual(result.returncode == 0, name == "executable", result.stdout + result.stderr)
                self.assertEqual(list(validation_temp.iterdir()), [], "CI must remove its extraction directory after success or failure")
                if name == "executable":
                    self.assertIn(f"OK: /opt/forge/{SUPERVISOR} is executable", result.stdout)
                elif name == "corrupt-image":
                    self.assertIn("superblock", result.stderr)


if __name__ == "__main__":
    unittest.main()
