# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "setup-devspace-on-host.sh"


class SetupDevSpaceOnHostTest(unittest.TestCase):
    def run_parser(self, args):
        source = SCRIPT.read_text()
        # Stop before privilege escalation even if argument validation regresses.
        parser = source[:source.index('\nif [[ "${EUID}" -ne 0 ]]; then')]
        return subprocess.run(
            ["bash", "-c", parser + "\nexit 99\n", "bash", *args],
            capture_output=True, text=True, timeout=10,
        )

    def test_cluster_setup_delegates_before_tls_and_wait(self):
        source = SCRIPT.read_text()
        prepare = source[source.index("prepare_cluster() {"):
                         source.index("\nkind_node_has_image() {")]
        # Temporary-config cleanup is exercised through the shared public CLI.
        for status in (0, 42):
            with self.subTest(status=status):
                result = subprocess.run(
                    ["bash", "-c", """set -euo pipefail
configure_kind_node_tls() { printf 'TLS\\n'; }
run_as_user() {
  printf '<%s>\\n' "$@"
  if [[ "$1" == bash ]]; then return "$CREATE_STATUS"; fi
}
""" + prepare + "\nprepare_cluster\n"],
                    env={**os.environ, "CREATE_STATUS": str(status), "CLUSTER_NAME": "fixture",
                         "IP_FAMILY": "dual", "REPO_DIR": "/repo with spaces", "USER_HOME": "/user home"},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertIn("</repo with spaces/dev/deployment/setup-kind.sh>\n"
                              "<--name>\n<fixture>\n<--ip-family>\n<dual>\n"
                              "<--kubeconfig>\n</user home/.kube/config>\n", result.stdout)
                self.assertEqual("TLS\n" in result.stdout, status == 0)
                self.assertEqual("<kubectl>\n<--context>\n<kind-fixture>\n" in result.stdout, status == 0)

    def test_selected_checkout_requires_shared_bootstrap(self):
        source = SCRIPT.read_text()
        prepare = source[source.index("prepare_checkout() {"):
                         source.index("\nconfigure_kind_node_tls() {")]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "devspace.yaml").touch()
            for missing in ('setup-kind.sh', 'install-tools.py', 'docker-host.sh', None):
                with self.subTest(missing=missing):
                    helper = root / 'dev/deployment'
                    helper.mkdir(parents=True, exist_ok=True)
                    for name in ('setup-kind.sh', 'install-tools.py', 'docker-host.sh'):
                        path = helper / name
                        if name == missing:
                            path.unlink(missing_ok=True)
                        else:
                            path.touch()
                    result = subprocess.run(
                        ["bash", "-c", "set -euo pipefail\ndie() { echo \"$*\" >&2; exit 1; }\n"
                         + prepare + "\nprepare_checkout\n"],
                        env={**os.environ, "REPO_DIR": directory, "REPO_REF": ""},
                        capture_output=True, text=True, timeout=10,
                    )
                    self.assertEqual(result.returncode == 0, missing is None, result.stderr)
                    if missing:
                        self.assertIn('selected checkout lacks dev/deployment/' + missing, result.stderr)

    def test_standalone_script_clones_before_using_selected_checkout_tools(self):
        source = SCRIPT.read_text()
        main = source[source.index('main() {'):source.index('\nmain "$@"')]
        installer = source[source.index('install_cli_tools() {'):source.index('\nconfigure_user_shell() {')]
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory) / 'selected checkout'
            result = subprocess.run(['bash', '-c', 'set -euo pipefail\n' + installer + main + """
log() { :; }
install_host_packages() { :; }
configure_docker() { :; }
prepare_user_tool_directories() { :; }
prepare_checkout() {
  mkdir -p "$REPO_DIR/dev/deployment"
  printf 'install_docker() { :; }\\n' > "$REPO_DIR/dev/deployment/docker-host.sh"
  printf 'import sys; print("SELECTED_CHECKOUT", sys.argv[1:])\\n' > "$REPO_DIR/dev/deployment/install-tools.py"
}
run_as_user() { if [[ "$1" == python3 ]]; then "$@"; fi; }
configure_user_shell() { :; }
prepare_cluster() { :; }
preload_postgres_image() { :; }
show_summary() { :; }
main
"""], env={**os.environ, 'REPO_DIR': str(checkout), 'USER_HOME': directory, 'SKIP_DEPLOY': '1'}, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('SELECTED_CHECKOUT', result.stdout)
            self.assertIn("'devspace', 'kind', 'kubectl', 'helm'", result.stdout)

    def test_invalid_cluster_name_fails_before_host_setup(self):
        for name in ("bad.name", "a" * 50):
            with self.subTest(name=name):
                result = self.run_parser(["--cluster-name", name])
                self.assertEqual(result.returncode, 1)
                self.assertIn("--cluster-name must be a lowercase DNS label", result.stderr)

    def test_user_commands_use_the_exported_kubeconfig(self):
        source = SCRIPT.read_text()
        runner = source[source.index("run_as_user() {"):
                        source.index("\nensure_user_directory() {")]
        result = subprocess.run(
            ["bash", "-c", runner + """
runuser() { shift 3; "$@"; }
run_as_user sh -c 'printf "%s" "$KUBECONFIG"'
"""],
            env={**os.environ, "DEV_USER": "fixture", "USER_HOME": "/user home",
                 "KUBECONFIG": "/unrelated/config"},
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "/user home/.kube/config")

    def test_profile_selection(self):
        source = SCRIPT.read_text()
        # Exercise the real defaults, parser and deployment function without host setup.
        parser = source[:source.index('\nif [[ "${EUID}" -ne 0 ]]; then')]
        deploy = source[source.index("deploy_stack() {"):
                        source.index("\ntemporal_namespace_is_healthy() {")]
        for args, profile in (([], "full"),
                              (["--profile", "full", "--profile", "dsx-exchange"], "dsx-exchange")):
            with self.subTest(args=args), tempfile.TemporaryDirectory(prefix="repo space ") as directory:
                bootstrap = Path(directory) / "dev/deployment/devspace/bootstrap-prereqs.sh"
                bootstrap.parent.mkdir(parents=True)
                bootstrap.write_text("#!/bin/sh\nexit 0\n")
                bootstrap.chmod(0o755)
                result = subprocess.run(
                    ["bash", "-c", parser + deploy + '''
run_as_user() { "$@"; }
cache_postgres_wait_image() { :; }
devspace() { printf '<%s>\\n' "$@"; }
export -f devspace
deploy_stack
''', "bash", "--repo-dir", directory, *args],
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(f"<deploy>\n<-n>\n<nico-system>\n<--profile>\n<{profile}>\n",
                              result.stdout)

    @unittest.skipIf(os.geteuid() == 0, "sudo forwarding is only used by non-root callers")
    def test_profile_forwarded_through_sudo(self):
        with tempfile.TemporaryDirectory() as directory:
            sudo = Path(directory) / "sudo"
            sudo.write_text("#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n")
            sudo.chmod(0o755)
            result = subprocess.run(
                ["bash", str(SCRIPT), "--profile", "dsx-exchange"],
                env={**os.environ, "PATH": f"{directory}:{os.environ['PATH']}"},
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("<--profile>\n<dsx-exchange>\n", result.stdout)

    def test_missing_profile(self):
        for args in (["--profile"], ["--profile", ""], ["--profile", "--skip-deploy"]):
            with self.subTest(args=args):
                result = self.run_parser(args)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--profile requires a value", result.stderr)


if __name__ == "__main__":
    unittest.main()
