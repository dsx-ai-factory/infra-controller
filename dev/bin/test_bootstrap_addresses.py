# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT_DIR = Path(__file__).resolve().parent


class BootstrapAddressTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.fixture = Path(self.temporary.name)
        self.calls = self.fixture / "rpc-args"
        self.urls = self.fixture / "pxe-urls"
        (self.fixture / "scratch").mkdir()
        (self.fixture / "host_port.sh").write_text((SCRIPT_DIR / "host_port.sh").read_text())
        (self.fixture / "2:1079").touch()
        self.environment = {
            **os.environ,
            "PATH": f"{self.fixture}:{os.environ['PATH']}",
            "FORGE_BOOTSTRAP_KIND": "",
            "REPO_ROOT": str(self.fixture),
            "RPC_ARGS": str(self.calls),
            "PXE_URLS": str(self.urls),
        }
        self.write_command(
            "grpcurl",
            'printf "CALL\\n" >> "$RPC_ARGS"\n'
            'printf "%s\\n" "$@" >> "$RPC_ARGS"\n'
            'exit 91\n',
        )
        (self.fixture / "envrc").write_text(
            "API_SERVER_HOST=2001:db8::10\nAPI_SERVER_PORT=1079\n"
            "PXE_SERVER_HOST=2001:db8::20\nPXE_SERVER_PORT=8080\n"
        )
        (self.fixture / "dpu_dhcp_discovery.json").write_text("{}\n")

    def write_command(self, name, body):
        command = self.fixture / name
        command.parent.mkdir(parents=True, exist_ok=True)
        command.write_text("#!/bin/bash\n" + body)
        command.chmod(0o700)

    def run_script(self, name, arguments):
        # Keep hardcoded scratch paths private even if setup moves before the stubs.
        script = self.fixture / name
        script.write_text((SCRIPT_DIR / name).read_text().replace("/tmp/", f"{self.fixture}/scratch/"))
        return subprocess.run(
            ["bash", "-eo", "pipefail", str(script), *arguments],
            cwd=self.fixture, env=self.environment,
            capture_output=True, text=True, timeout=15,
        )

    def test_each_bootstrap_entry_point_uses_an_ipv6_endpoint(self):
        cases = [
            ("discover_host.sh", ["2001:db8::10", "1079", str(self.fixture), "dhcp-only"]),
            ("discover_dpu.sh", [str(self.fixture)]),
            ("reprovision_dpu.sh", ["2001:db8::10", "1079"]),
            ("instance_handling.sh", ["test", "2001:db8::10", "1079"]),
        ]
        for script, arguments in cases:
            with self.subTest(script=script):
                self.calls.write_text("")
                result = self.run_script(script, arguments)
                self.assertNotEqual(result.returncode, 0)
                calls = self.calls.read_text().splitlines()
                self.assertEqual(calls.count("CALL"), 1)
                self.assertIn("[2001:db8::10]:1079", calls)

    def test_dpu_pxe_and_agent_config_use_ipv6_urls(self):
        for name in ("dpu_machine_discovery.json", "update_dpu_bmc_metadata.json"):
            (self.fixture / name).write_text("{}\n")
        self.write_command(
            "grpcurl",
            'if [ -f "$REPO_ROOT/scratch/forge-dpu-agent-sim-config.toml" ]; then exit 91; fi\n'
            'case "${@: -1}" in\n'
            "  */DiscoverDhcp) printf '%s\\n' '{\"machineInterfaceId\":{\"value\":\"interface-id\"}}' ;;\n"
            "  */DiscoverMachine) cat >/dev/null; printf '%s\\n' '{\"machineId\":{\"id\":\"machine-id\"}}' ;;\n"
            "  */FindMachines) printf '%s\\n' '{\"machines\":[{\"state\":\"DPUInitializing/WaitingForNetworkConfig\"}]}' ;;\n"
            "  *) printf '{}\\n' ;;\n"
            'esac\n',
        )
        self.write_command("cargo", "exit 0\n")
        self.write_command("dev/bin/psql.sh", "printf '192.0.2.10\\n'\n")
        self.write_command(
            "curl",
            'printf "%s\\n" "${@: -1}" >> "$PXE_URLS"\n',
        )
        result = self.run_script("discover_dpu.sh", [str(self.fixture)])
        self.assertEqual(result.returncode, 91, result.stderr)
        self.assertEqual(self.urls.read_text().splitlines(), [
            "http://[2001:db8::20]:8080/api/v0/pxe/boot?uuid=interface-id&buildarch=arm64",
            "http://[2001:db8::20]:8080/api/v0/cloud-init/dpu/user-data",
        ] * 2)
        config = self.fixture / "scratch/forge-dpu-agent-sim-config.toml"
        self.assertIn(
            'api-server = "https://[2001:db8::10]:1079"',
            config.read_text().splitlines(),
        )

    def test_host_port_preserves_existing_address_forms(self):
        for host in ("192.0.2.10", "api.example.test", "[2001:db8::10]"):
            with self.subTest(host=host):
                result = subprocess.run(
                    ["bash", "-c", 'source "$1"; host_port "$2" 1079',
                     "bash", str(SCRIPT_DIR / "host_port.sh"), host],
                    check=True, capture_output=True, text=True, timeout=15,
                )
                self.assertEqual(result.stdout, f"{host}:1079")

    def test_psql_preserves_database_settings_and_query(self):
        database_call = self.fixture / "psql-call"
        self.environment.update({
            "FORGE_BOOTSTRAP_KIND": "kube",
            "PSQL_CALL": str(database_call),
            "DATASTORE_PORT": "6432",
            "DATASTORE_USER": "bootstrap-user",
            "DATASTORE_PASSWORD": "password with @:/?# and 'quotes'",
            "DATASTORE_NAME": "bootstrap-db",
            "PGSSLMODE": "disable",
            "REMOTE_PGSSLMODE": "verify-full",
        })
        self.write_command(
            "kubectl",
            'set -e\n'
            'while [ "$1" != "--" ]; do shift; done\n'
            'shift\nPGSSLMODE="$REMOTE_PGSSLMODE" exec "$@"\n',
        )
        self.write_command(
            "psql",
            'printf "%s\\0" "$PGHOST" "$PGPORT" "$PGUSER" "$PGPASSWORD" '
            '"$PGDATABASE" "$PGSSLMODE" "$@" > "$PSQL_CALL"\n',
        )
        query = "select 'literal $HOME and \"quotes\"';"
        cases = [
            ("2001:db8::10", "2001:db8::10"),
            ("[2001:db8::10]", "2001:db8::10"),
            ("192.0.2.10", "192.0.2.10"),
            ("database.example.test", "database.example.test"),
        ]
        for host, expected_host in cases:
            with self.subTest(host=host):
                self.environment.update({
                    "DATASTORE_HOST": host,
                    "PGHOST": "local.example.test",
                    "PGPORT": "5432",
                    "PGUSER": "local-user",
                    "PGPASSWORD": "local-password",
                    "PGDATABASE": "local-db",
                })
                result = self.run_script("psql.sh", [query])
                self.assertEqual(result.returncode, 0, result.stderr)
                received = database_call.read_bytes().decode().split("\0")[:-1]
                expected = [expected_host, "6432", "bootstrap-user",
                            self.environment["DATASTORE_PASSWORD"], "bootstrap-db",
                            "verify-full", "-P", "pager=off", "-t", "-c", query]
                self.assertEqual(received, expected)


if __name__ == "__main__":
    unittest.main()
