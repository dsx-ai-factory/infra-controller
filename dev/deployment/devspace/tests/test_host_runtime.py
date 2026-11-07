# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest


SCRIPT_DIR = Path(__file__).resolve().parents[1]


class NativePreparationTests(unittest.TestCase):
    def test_postgres_waits_for_final_tcp_server(self):
        script = (SCRIPT_DIR / "prepare-ubuntu-host-for-dev.sh").read_text()
        function = script[script.index("wait_for_core_postgres() {"):
                          script.index("\nfetch_dependencies() {")]
        for available, attempts in (("1", 2), ("0", 60)):
            with self.subTest(available=available), tempfile.TemporaryDirectory() as directory:
                calls = Path(directory) / "calls"
                result = subprocess.run(
                    ["bash", "-c", '''set -euo pipefail
die() { printf '%s\\n' "$*" >&2; exit 1; }
sleep() { :; }
run_as_user() {
  printf '%s\\n' "$*" >> "$CALL_LOG"
  # The initialization server accepts sockets before the final TCP listener.
  [[ "$*" == *'pg_isready -h 127.0.0.1 '* ]] || return 0
  [[ "$AVAILABLE" == 1 && "$(wc -l < "$CALL_LOG")" -ge 2 ]]
}
''' + function + '\nwait_for_core_postgres\n'],
                    env={**os.environ, "CORE_POSTGRES_CONTAINER": "fixture",
                         "CALL_LOG": str(calls), "AVAILABLE": available},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0 if available == "1" else 1, result.stderr)
                self.assertEqual(len(calls.read_text().splitlines()), attempts)
                if available == "0":
                    self.assertIn("PostgreSQL did not become ready", result.stderr)

    def test_summary_uses_existing_storage_paths(self):
        script = (SCRIPT_DIR / "setup-devspace-on-host.sh").read_text()
        function = script[script.index("show_summary() {"):script.index("\nmain() {")]
        for case in ("external", "missing", "discovery-failed"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                docker_root = root / "external disk"
                docker_root.mkdir()
                calls = root / "df-paths"
                # Neither standard runtime directory exists on this fixture host.
                summary = function.replace("/var/lib/docker", str(root / "docker"))
                summary = summary.replace("/var/lib/containerd", str(root / "containerd"))
                result = subprocess.run(
                    ["bash", "-c", '''set -euo pipefail
log() { :; }
run_as_user() { "$@"; }
kubectl() { :; }
docker() {
  case "$1" in
    inspect) printf '192.0.2.1\\n' ;;
    system) : ;;
    info) [[ "$TEST_CASE" != discovery-failed ]] || return 1
          printf '%s\\n' "$DOCKER_ROOT" ;;
    *) return 99 ;;
  esac
}
df() {
  [[ "$1" == -h ]] || return 99
  shift
  for path; do
    [[ -d "$path" ]] || return 1
    printf '%s\\n' "$path" >> "$CALL_LOG"
  done
}
''' + summary + '\nshow_summary\n'],
                    env={**os.environ, "CLUSTER_NAME": "fixture", "DEV_USER": "fixture",
                         "TEST_CASE": case, "CALL_LOG": str(calls),
                         "DOCKER_ROOT": str(docker_root if case != "missing" else root / "missing")},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                expected = ["/", "/home"] + ([str(docker_root)] if case == "external" else [])
                self.assertEqual(calls.read_text().splitlines(), expected)
                self.assertIn("REST health:", result.stdout)

    def test_kind_runtime_configuration_is_idempotent(self):
        script = (SCRIPT_DIR / "setup-devspace-on-host.sh").read_text()
        function = script.split("configure_kind_node_tls() {", 1)[1].split("\n}\n", 1)[0]
        with tempfile.TemporaryDirectory(prefix="nico-kind-tls-test-") as temporary:
            root = Path(temporary)
            config, calls = root / "config", root / "calls"
            command = '''set -eu
log() { :; }
run_as_user() {
  printf '%s\\n' "$*" >> "$CALL_LOG"
  case "$*" in
    'docker exec fixture-control-plane cat '*) cat "$CONFIG" ;;
    'docker exec -i fixture-control-plane tee '*) cat > "$CONFIG" ;;
  esac
}
configure_kind_node_tls() {''' + function + '''
}
configure_kind_node_tls
configure_kind_node_tls
'''
            subprocess.run(["bash", "-c", command], check=True, capture_output=True,
                           text=True, env=dict(os.environ, CLUSTER_NAME="fixture",
                                               CONFIG=str(config), CALL_LOG=str(calls)))
            self.assertEqual(calls.read_text().count("systemctl restart containerd"), 1)

    def test_docker_configuration_only_restarts_changed_running_services(self):
        script = (SCRIPT_DIR / "setup-devspace-on-host.sh").read_text()
        function = script.split("configure_docker() {", 1)[1].split("\n}\n", 1)[0]
        config = '[Service]\nEnvironment="GODEBUG=tlsmlkem=0"\n'
        for active, existing, restarted in (
                ("0", None, []),
                ("1", None, ["containerd.service", "docker.service"]),
                ("1", config, [])):
            with self.subTest(active=active, existing=existing), \
                    tempfile.TemporaryDirectory(prefix="nico-docker-test-") as temporary:
                root = Path(temporary)
                for service in ("containerd", "docker"):
                    directory = root / f"{service}.service.d"
                    directory.mkdir()
                    if existing is not None:
                        (directory / "10-tls-compat.conf").write_text(existing)
                calls = root / "calls"
                command = '''set -eu
log() { :; }
die() { exit 1; }
usermod() { :; }
docker() { :; }
run_as_user() { "$@"; }
systemctl() {
  printf '%s\\n' "$*" >> "$CALL_LOG"
  if [ "$1" = is-active ]; then [ "$ACTIVE" = 1 ]; fi
}
configure_docker() {''' + function.replace("/etc/systemd/system", str(root)) + '''
}
configure_docker
'''
                subprocess.run(["bash", "-c", command], check=True, capture_output=True,
                               text=True, env=dict(os.environ, DEV_USER="fixture",
                                                   ACTIVE=active, CALL_LOG=str(calls)))
                actions = calls.read_text().splitlines()
                expected = ["restart " + " ".join(restarted)] if restarted else []
                self.assertEqual([line for line in actions if line.startswith("restart ")], expected)
                self.assertEqual("daemon-reload" in actions, existing != config)
                for service in ("containerd", "docker"):
                    self.assertEqual((root / f"{service}.service.d/10-tls-compat.conf").read_text(), config)

    def test_postgres_connection_budget_for_new_and_existing_containers(self):
        script = (SCRIPT_DIR / "prepare-ubuntu-host-for-dev.sh").read_text()
        function = script.split("start_core_postgres() {", 1)[1].split("\n}\n", 1)[0]
        with tempfile.TemporaryDirectory(prefix="nico-postgres-test-") as temporary:
            root = Path(temporary)
            state = root / "connections"
            calls = root / "calls"
            command = '''set -eu
log() { :; }
die() { printf '%s\\n' "$*" >&2; exit 1; }
wait_for_core_postgres() { :; }
run_as_user() {
  printf '%s\\n' "$*" >> "$CALL_LOG"
  case "$*" in
    'docker container inspect fixture') [ "$EXISTS" = 1 ] ;;
    *PortBindings*) printf '%s\\n' '{"5432/tcp":[{"HostIp":"127.0.0.1","HostPort":"5432"}]}' ;;
    'docker run '*) printf '1000\\n' > "$CONNECTIONS" ;;
    *'SHOW max_connections'*) cat "$CONNECTIONS" ;;
    *'ALTER SYSTEM SET max_connections'*) printf '1000\\n' > "$CONNECTIONS" ;;
  esac
}
start_core_postgres() {''' + function + '\n}\nstart_core_postgres\n'
            for exists, initial, restarts in (("0", 0, 0), ("1", 100, 1), ("1", 1000, 0)):
                with self.subTest(exists=exists, initial=initial):
                    state.write_text(str(initial))
                    calls.write_text("")
                    subprocess.run(["bash", "-c", command], check=True, text=True,
                                   capture_output=True, env=dict(os.environ,
                                   CONNECTIONS=str(state), CALL_LOG=str(calls),
                                   EXISTS=exists, CORE_POSTGRES_IMAGE="postgres:fixture",
                                   SKIP_CORE_POSTGRES="0", CORE_POSTGRES_CONTAINER="fixture"))
                    self.assertEqual(int(state.read_text()), 1000)
                    actions = calls.read_text()
                    self.assertEqual(actions.count("docker restart fixture"), restarts)
                    self.assertNotIn("docker rm", actions)
                    if exists == "1":
                        self.assertNotIn("docker run", actions)
                    else:
                        self.assertIn("-c max_connections=1000", actions)

    def test_configure_hashicorp_apt_source_offline_validation(self):
        script = (SCRIPT_DIR / "prepare-ubuntu-host-for-dev.sh").read_text()
        fingerprint_def = [line for line in script.splitlines() if line.startswith("HASHICORP_GPG_FINGERPRINT=")][0]
        is_valid_helper = script[script.index("is_valid_hashicorp_keyring() {"):
                                 script.index("\nconfigure_hashicorp_apt_source() {")]
        configure_source = script[script.index("configure_hashicorp_apt_source() {"):
                                  script.index("\ninstall_base_packages() {")]

        cases = (
            ("fresh-missing-gpg", None, "0", False),
            ("fresh-missing-key-with-gpg", None, "1", False),
            ("directory-as-keyring", "dir", "1", False),
            ("empty-key", "", "1", False),
            ("wrong-key", "wrong", "1", False),
            ("corrupt-key", "corrupt", "1", False),
            ("expired-key", "expired", "1", False),
            ("revoked-key", "revoked", "1", False),
            ("loose-match-key", "loose", "1", False),
            ("whitespace-valid-key", "whitespace", "1", True),
            ("valid-key", "valid", "1", True),
        )
        for case, key_content, gpg_avail, expect_source in cases:
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="nico-apt-test-") as temporary:
                root = Path(temporary)
                keyrings_dir = root / "keyrings"
                sources_dir = root / "sources.list.d"
                keyrings_dir.mkdir(parents=True)
                sources_dir.mkdir(parents=True)
                keyring_file = keyrings_dir / "hashicorp-archive-keyring.gpg"
                if key_content == "dir":
                    keyring_file.mkdir()
                elif key_content is not None:
                    keyring_file.write_text(key_content)
                legacy_list = sources_dir / "hashicorp.list"
                legacy_list.write_text("legacy\n")
                sources_file = sources_dir / "hashicorp.sources"
                sources_file.write_text("stale\n")

                call_log = root / "calls"
                patched_helper = is_valid_helper.replace("/usr/share/keyrings", str(keyrings_dir))
                patched_source = configure_source.replace("/usr/share/keyrings", str(keyrings_dir)).replace(
                    "/etc/apt/sources.list.d", str(sources_dir)
                )

                command = f"""set -euo pipefail
log() {{ printf '%s\\n' "$*" >> "$CALL_LOG"; }}
die() {{ printf '%s\\n' "$*" >&2; exit 1; }}
curl() {{ printf 'unexpected curl\\n' >> "$CALL_LOG"; exit 99; }}
command() {{
  if [[ "$1" == "-v" && "$2" == "gpg" && "$GPG_AVAIL" == "0" ]]; then
    return 1
  fi
  builtin command "$@"
}}
gpg() {{
  if [[ "$*" == *"--show-keys"* ]]; then
    local target="${{!#}}"
    if [[ ! -f "$target" ]]; then return 2; fi
    local content
    content="$(cat "$target")"
    case "$content" in
      *corrupt*) return 2 ;;
      *wrong*|*stale*) printf 'fpr:::::::::AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:\\n' ;;
      *expired*) printf 'pub:e:4096:1:798AEC654E5C1542:1611340798:1674412798::-:::scESC::::::23:\\nfpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:\\n' ;;
      *revoked*) printf 'pub:r:4096:1:798AEC654E5C1542:1611340798:1674412798::-:::scESC::::::23:\\nfpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:\\n' ;;
      *loose*) printf 'fpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560EXTRA:\\n' ;;
      *whitespace*) printf 'fpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:  \\r\\n' ;;
      *valid*) printf 'fpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:\\n' ;;
      *) return 2 ;;
    esac
    return 0
  fi
  return 0
}}
VERSION_CODENAME="noble"
{fingerprint_def}
{patched_helper}
{patched_source}
if is_valid_hashicorp_keyring ""; then
  die "empty-string path must never be accepted as valid keyring"
fi
configure_hashicorp_apt_source
"""
                result = subprocess.run(
                    ["bash", "-c", command],
                    capture_output=True,
                    text=True,
                    env={**os.environ, "CALL_LOG": str(call_log), "GPG_AVAIL": gpg_avail},
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(legacy_list.exists())
                if expect_source:
                    self.assertTrue(sources_file.exists())
                    self.assertIn(f"Signed-By: {keyring_file}", sources_file.read_text())
                else:
                    self.assertFalse(sources_file.exists())
                    self.assertIn("deferring source configuration", call_log.read_text())

    def test_install_vault_keyring_recovery_and_idempotency(self):
        script = (SCRIPT_DIR / "prepare-ubuntu-host-for-dev.sh").read_text()
        fingerprint_def = [line for line in script.splitlines() if line.startswith("HASHICORP_GPG_FINGERPRINT=")][0]
        vault_version_def = [line for line in script.splitlines() if line.startswith("VAULT_VERSION=")][0]
        is_valid_helper = script[script.index("is_valid_hashicorp_keyring() {"):
                                 script.index("\nconfigure_hashicorp_apt_source() {")]
        configure_source = script[script.index("configure_hashicorp_apt_source() {"):
                                  script.index("\ninstall_base_packages() {")]
        install_vault = script[script.index("install_vault() {"):
                               script.index("\nconfigure_kea_apparmor() {")]

        cases = (
            # case, init_key, dl_content, curl_status, exp_return, exp_curl, exp_reuse
            ("stale-key-installed-vault", "stale", "valid", 0, 0, True, True),
            ("valid-key-installed-vault", "valid", "valid", 0, 0, False, True),
            ("failed-download-curl", "stale", "valid", 22, 1, True, False),
            ("failed-dearmor-gpg", "stale", "dearmor-fail", 0, 1, True, False),
            ("invalid-downloaded-key", "stale", "corrupt", 0, 1, True, False),
            ("expired-downloaded-key", "stale", "expired", 0, 1, True, False),
            ("revoked-downloaded-key", "stale", "revoked", 0, 1, True, False),
            ("loose-downloaded-key", "stale", "loose", 0, 1, True, False),
        )
        for case, init_key, dl_content, curl_status, exp_return, exp_curl, exp_reuse in cases:
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="nico-vault-test-") as temporary:
                root = Path(temporary)
                keyrings_dir = root / "keyrings"
                sources_dir = root / "sources.list.d"
                keyrings_dir.mkdir(parents=True)
                sources_dir.mkdir(parents=True)
                keyring_file = keyrings_dir / "hashicorp-archive-keyring.gpg"
                keyring_file.write_text(init_key)
                sources_file = sources_dir / "hashicorp.sources"

                call_log = root / "calls"
                patched_helper = is_valid_helper.replace("/usr/share/keyrings", str(keyrings_dir))
                patched_source = configure_source.replace("/usr/share/keyrings", str(keyrings_dir)).replace(
                    "/etc/apt/sources.list.d", str(sources_dir)
                )
                patched_vault = install_vault.replace("/usr/share/keyrings", str(keyrings_dir))

                command = f"""set -euo pipefail
log() {{ printf '%s\\n' "$*" >> "$CALL_LOG"; }}
die() {{ printf '%s\\n' "$*" >&2; exit 1; }}
command() {{
  if [[ "$1" == "-v" && "$2" == "vault" ]]; then
    printf '/usr/bin/vault\\n'
    return 0
  fi
  builtin command "$@"
}}
readlink() {{
  if [[ "$1" == "-f" ]]; then
    printf '%s\\n' "$2"
    return 0
  fi
  builtin readlink "$@"
}}
dpkg-query() {{
  printf '1.21.4-1\\n'
}}
curl() {{
  printf 'curl %s\\n' "$*" >> "$CALL_LOG"
  if [[ "$CURL_STATUS" != "0" ]]; then
    return "$CURL_STATUS"
  fi
  printf '%s' "$DL_CONTENT"
}}
apt-get() {{
  printf 'apt-get %s\\n' "$*" >> "$CALL_LOG"
}}
apt-mark() {{
  printf 'apt-mark %s\\n' "$*" >> "$CALL_LOG"
}}
gpg() {{
  if [[ "$*" == *"--show-keys"* ]]; then
    local target="${{!#}}"
    if [[ ! -f "$target" ]]; then return 2; fi
    local content
    content="$(cat "$target")"
    case "$content" in
      *corrupt*) return 2 ;;
      *wrong*|*stale*) printf 'fpr:::::::::AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:\\n' ;;
      *expired*) printf 'pub:e:4096:1:798AEC654E5C1542:1611340798:1674412798::-:::scESC::::::23:\\nfpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:\\n' ;;
      *revoked*) printf 'pub:r:4096:1:798AEC654E5C1542:1611340798:1674412798::-:::scESC::::::23:\\nfpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:\\n' ;;
      *loose*) printf 'fpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560EXTRA:\\n' ;;
      *valid*) printf 'fpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:\\n' ;;
      *) return 2 ;;
    esac
    return 0
  fi
  if [[ "$*" == *"--dearmor"* ]]; then
    if [[ "$DL_CONTENT" == "dearmor-fail" ]]; then
      return 1
    fi
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "-o" ]]; then
        cat > "$2"
        return 0
      fi
      shift
    done
  fi
  return 0
}}
VERSION_CODENAME="noble"
{fingerprint_def}
{vault_version_def}
{patched_helper}
{patched_source}
{patched_vault}
install_vault
"""
                result = subprocess.run(
                    ["bash", "-c", command],
                    capture_output=True,
                    text=True,
                    env={
                        **os.environ,
                        "CALL_LOG": str(call_log),
                        "CURL_STATUS": str(curl_status),
                        "DL_CONTENT": dl_content,
                    },
                )
                self.assertEqual(result.returncode, exp_return, result.stderr)
                calls = call_log.read_text() if call_log.exists() else ""
                if exp_curl and curl_status == 0:
                    self.assertIn("curl ", calls)
                elif not exp_curl:
                    self.assertNotIn("curl ", calls)

                self.assertNotIn("apt-get install", calls)

                temp_keyrings = [f.name for f in keyrings_dir.iterdir() if f.name.startswith(".hashicorp-keyring")]
                self.assertEqual(temp_keyrings, [])

                if exp_reuse:
                    self.assertIn("Reusing installed Vault", calls)
                    self.assertEqual(keyring_file.read_text(), "valid")
                    mode = oct(stat.S_IMODE(keyring_file.stat().st_mode))
                    self.assertEqual(mode, "0o644")
                    self.assertTrue(sources_file.exists())
                else:
                    self.assertNotIn("Reusing installed Vault", calls)
                    self.assertEqual(keyring_file.read_text(), init_key)
                    self.assertIn("failed to download or verify HashiCorp APT keyring", result.stderr)

    def test_install_vault_fresh_installation_and_package_hold(self):
        script = (SCRIPT_DIR / "prepare-ubuntu-host-for-dev.sh").read_text()
        fingerprint_def = [line for line in script.splitlines() if line.startswith("HASHICORP_GPG_FINGERPRINT=")][0]
        vault_version_def = [line for line in script.splitlines() if line.startswith("VAULT_VERSION=")][0]
        is_valid_helper = script[script.index("is_valid_hashicorp_keyring() {"):
                                 script.index("\nconfigure_hashicorp_apt_source() {")]
        configure_source = script[script.index("configure_hashicorp_apt_source() {"):
                                  script.index("\ninstall_base_packages() {")]
        install_vault = script[script.index("install_vault() {"):
                               script.index("\nconfigure_kea_apparmor() {")]

        cases = (
            ("fresh-missing-key", None, "valid", True),
            ("fresh-stale-key", "stale", "valid", True),
            ("fresh-valid-key", "valid", "valid", False),
        )
        for case, init_key, dl_content, exp_curl in cases:
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="nico-vault-fresh-") as temporary:
                root = Path(temporary)
                keyrings_dir = root / "keyrings"
                sources_dir = root / "sources.list.d"
                keyrings_dir.mkdir(parents=True)
                sources_dir.mkdir(parents=True)
                keyring_file = keyrings_dir / "hashicorp-archive-keyring.gpg"
                if init_key is not None:
                    keyring_file.write_text(init_key)
                sources_file = sources_dir / "hashicorp.sources"

                call_log = root / "calls"
                state_file = root / "installed"
                patched_helper = is_valid_helper.replace("/usr/share/keyrings", str(keyrings_dir))
                patched_source = configure_source.replace("/usr/share/keyrings", str(keyrings_dir)).replace(
                    "/etc/apt/sources.list.d", str(sources_dir)
                )
                patched_vault = install_vault.replace("/usr/share/keyrings", str(keyrings_dir))

                command = f"""set -euo pipefail
log() {{ printf '%s\\n' "$*" >> "$CALL_LOG"; }}
die() {{ printf '%s\\n' "$*" >&2; exit 1; }}
command() {{
  if [[ "$1" == "-v" && "$2" == "vault" ]]; then
    if [[ -f "$STATE_FILE" ]]; then
      printf '/usr/bin/vault\\n'
      return 0
    fi
    return 1
  fi
  builtin command "$@"
}}
readlink() {{
  if [[ "$1" == "-f" ]]; then
    printf '%s\\n' "$2"
    return 0
  fi
  builtin readlink "$@"
}}
dpkg-query() {{
  if [[ -f "$STATE_FILE" ]]; then
    printf '1.21.4-1\\n'
    return 0
  fi
  return 1
}}
curl() {{
  printf 'curl %s\\n' "$*" >> "$CALL_LOG"
  printf '%s' "$DL_CONTENT"
}}
apt-get() {{
  printf 'apt-get %s\\n' "$*" >> "$CALL_LOG"
  if [[ "$*" == *"install"*"vault="* ]]; then
    touch "$STATE_FILE"
  fi
}}
apt-mark() {{
  printf 'apt-mark %s\\n' "$*" >> "$CALL_LOG"
}}
gpg() {{
  if [[ "$*" == *"--show-keys"* ]]; then
    local target="${{!#}}"
    if [[ ! -f "$target" ]]; then return 2; fi
    local content
    content="$(cat "$target")"
    case "$content" in
      *stale*) printf 'fpr:::::::::AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:\\n' ;;
      *valid*) printf 'fpr:::::::::D55C0D1AC78A8D8126CB631CFC9CA96ACA026560:\\n' ;;
      *) return 2 ;;
    esac
    return 0
  fi
  if [[ "$*" == *"--dearmor"* ]]; then
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "-o" ]]; then
        cat > "$2"
        return 0
      fi
      shift
    done
  fi
  return 0
}}
VERSION_CODENAME="noble"
{fingerprint_def}
{vault_version_def}
{patched_helper}
{patched_source}
{patched_vault}
install_vault
"""
                result = subprocess.run(
                    ["bash", "-c", command],
                    capture_output=True,
                    text=True,
                    env={
                        **os.environ,
                        "CALL_LOG": str(call_log),
                        "DL_CONTENT": dl_content,
                        "STATE_FILE": str(state_file),
                    },
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = call_log.read_text() if call_log.exists() else ""
                if exp_curl:
                    self.assertIn("curl ", calls)
                else:
                    self.assertNotIn("curl ", calls)

                self.assertIn("apt-get update", calls)
                self.assertIn("apt-get install -y --allow-change-held-packages vault=1.21.4-1", calls)
                self.assertIn("apt-mark hold vault", calls)
                self.assertNotIn("Reusing installed Vault", calls)
                self.assertEqual(keyring_file.read_text(), "valid")
                mode = oct(stat.S_IMODE(keyring_file.stat().st_mode))
                self.assertEqual(mode, "0o644")
                self.assertTrue(sources_file.exists())
                temp_keyrings = [f.name for f in keyrings_dir.iterdir() if f.name.startswith(".hashicorp-keyring")]
                self.assertEqual(temp_keyrings, [])


class PurgeTests(unittest.TestCase):
    def test_reset_stops_before_any_action_when_docker_root_is_unknown(self):
        script = (SCRIPT_DIR / "reset-devspace-on-host.sh").read_text()
        resolve = script[script.index("resolve_docker_root() {"):
                         script.index("\ndetect_checkout() {")]
        main = script[script.index("main() {"):]
        # Only discovery and main are real; no destructive function is available.
        command = '''set -eu
log() { :; }
die() { printf '%s\\n' "$*" >&2; exit 1; }
docker() { return 1; }
detect_checkout() { printf 'unexpected action\\n'; }
''' + resolve + main
        result = subprocess.run(
            ["bash", "-c", command],
            env={**os.environ, "DOCKER_ROOT_EXPLICIT": "0", "APPLY": "1"},
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("pass --docker-root explicitly", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_reset_uses_active_docker_root_or_explicit_override(self):
        script = (SCRIPT_DIR / "reset-devspace-on-host.sh").read_text()
        function = script.split("resolve_docker_root() {", 1)[1].split("\n}\n", 1)[0]
        for explicit, active, status, expected in (
                ("0", "/custom/docker", "0", "/custom/docker"),
                ("0", "", "0", None),
                ("0", "/partial/output", "1", None),
                ("1", "", "1", "/explicit/docker")):
            with self.subTest(explicit=explicit, active=active, status=status):
                command = ('set -eu\nlog() { :; }\n'
                           'die() { printf "%s" "$*" >&2; exit 1; }\n'
                           'docker() { printf "%s" "$ACTIVE_ROOT"; return "$DOCKER_STATUS"; }\n'
                           'resolve_docker_root() {' + function + '\n}\n'
                           'resolve_docker_root\nprintf "%s" "$DOCKER_ROOT"')
                result = subprocess.run(["bash", "-c", command], text=True,
                                        capture_output=True, env=dict(os.environ,
                                        DOCKER_ROOT_EXPLICIT=explicit, ACTIVE_ROOT=active,
                                        DOCKER_STATUS=status, DOCKER_ROOT="/explicit/docker"))
                if expected is None:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("pass --docker-root explicitly", result.stderr)
                    self.assertEqual(result.stdout, "")
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, expected)

    def test_purge_preserves_family_and_fails_before_delete_without_node_config(self):
        with tempfile.TemporaryDirectory(prefix="nico-kind-test-") as temporary:
            root = Path(temporary)
            shutil.copyfile(SCRIPT_DIR / "reset-kind-cluster.sh", root / "reset.sh")
            (root / "bootstrap-prereqs.sh").write_text("#!/bin/sh\nexit 0\n")
            (root / "bootstrap-prereqs.sh").chmod(0o755)
            binary = root / "bin"
            binary.mkdir()
            for name, content in {
                "helm": "exit 0",
                "docker": "printf 'kindest/node:test\\n'",
                "kubectl": """case "$1" in
config) printf 'kind-fixture\\n' ;;
get) [ "$POD_CIDRS" != error ] || exit 1
     printf '{"spec":{"podCIDRs":%s}}\\n' "$POD_CIDRS" ;;
esac""",
                "kind": """printf '%s\\n' "$1" >> "$CALL_LOG"
if [ "$1" = create ]; then
  while [ "$1" != --config ]; do shift; done
  cp "$2" "$CAPTURED_CONFIG"
fi""",
            }.items():
                path = binary / name
                path.write_text("#!/bin/sh\nset -eu\n" + content + "\n")
                path.chmod(0o755)
            for family, cidrs in (("ipv4", '["10.244.0.0/24"]'),
                                  ("dual", '["10.244.0.0/24","fd00:10:244::/64"]'),
                                  ("error", "error")):
                with self.subTest(family=family):
                    calls = root / f"{family}.calls"
                    captured = root / f"{family}.yaml"
                    environment = dict(os.environ, PATH=f"{binary}:{os.environ['PATH']}",
                                       POD_CIDRS=cidrs, CALL_LOG=str(calls), CAPTURED_CONFIG=str(captured))
                    result = subprocess.run(["bash", str(root / "reset.sh")], env=environment,
                                            capture_output=True, text=True)
                    if family == "error":
                        self.assertNotEqual(result.returncode, 0)
                        self.assertFalse(calls.exists(), "cluster must survive failed discovery")
                    else:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(calls.read_text(), "delete\ncreate\n")
                        self.assertIn(f"ipFamily: {family}\n", captured.read_text())


if __name__ == "__main__":
    unittest.main()
