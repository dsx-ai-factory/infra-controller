#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Confirm every connectivity and authorization requirement MLT has, read-only.

Six checks, each reported independently so one failure does not hide the rest.
Nothing changes state: no factory reset, no force-delete, no instance create, no
power action, no BMC write. Safe against a site under bringup.

  1  Configuration            which optional sections the site supplies
  2  Site Vault access        BMC credentials, and issuing the CLI certificate
  3  OAuth credential source  the OAuth client credential
  4  NICo REST API            bearer exchange, then a read through the client
  5  admin-cli                client identity minted, staged, and accepted
  6  BMC Redfish access       per-MAC credentials, then the service root

Imports the repo's own lib/, so it exercises the same code paths the test does
rather than a parallel implementation. Run it from the checkout, or from /test
in the MLT image:

    uv run python tools/nico_readiness_probe.py
"""

from __future__ import annotations

import os
import sys
import traceback

import requests
import urllib3

from lib import admin_cli, nico_rest
from lib.config import load_config
from lib.credentials import CredentialError, resolve_client_credential
from lib.site_vault import SiteVaultClient, SiteVaultError

urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

REDFISH_TIMEOUT = 30

results: list[tuple[str, bool, str]] = []
_lines: list[str] = []


def say(message: str = "") -> None:
    print(message, flush=True)
    _lines.append(message)


def check(name: str):
    """Record a check's outcome instead of letting it end the run."""

    def decorate(function):
        def run(*args, **kwargs):
            say(f"\n=== {name} ===")
            try:
                detail = function(*args, **kwargs)
            except Exception as error:  # noqa: BLE001 - a probe reports, never aborts
                say(f"FAIL  {error.__class__.__name__}: {error}")
                say(traceback.format_exc().rstrip())
                results.append((name, False, str(error)[:120]))
                return None
            results.append((name, True, detail if isinstance(detail, str) else ""))
            say("PASS")
            return detail

        return run

    return decorate


@check("1  Configuration")
def check_config():
    config = load_config()
    say(f"  site            {config.site.name}")
    say(f"  gRPC API        {'configured' if config.grpc_api else 'not configured'}")
    say(f"  OAuth           {'configured' if config.oauth else 'not configured'}")
    return config


@check("2  Site Vault access")
def check_site_vault(config):
    say("  Per-machine BMC credentials, and issuing the client certificate the")
    say("  admin-cli presents. Authorization keys on the subject fields that PKI")
    say("  role stamps, so the role has to be the one issued for CLI clients.")
    with SiteVaultClient() as client:
        paths = [client.credential_api_path(_capability_probe_mac())]
        if config.grpc_api and config.grpc_api.client_certificate:
            certificate = config.grpc_api.client_certificate
            paths.append(
                f"{certificate.vault_pki_mount}/issue/{certificate.vault_pki_role}"
            )
        granted = client.get_capabilities(paths)
        readable = 0
        for path in paths:
            capabilities = granted.get(path) or set()
            if capabilities and "deny" not in capabilities:
                readable += 1
            say(f"  {sorted(capabilities)!s:<22} {path}")
    if readable < len(paths):
        raise SiteVaultError(f"{len(paths) - readable} of {len(paths)} path(s) not readable")
    return f"{readable}/{len(paths)} path(s) readable"


def _capability_probe_mac() -> str:
    """A MAC to test capability against, before any machine is known.

    Capabilities are a property of the policy and the path shape, so the record
    behind it need not exist.
    """
    return os.environ.get("PROBE_BMC_MAC", "00:00:00:00:00:00")


@check("3  OAuth credential source")
def check_credential(config):
    if config.oauth is None:
        say("  no [oauth] section: a bearer must come from NICO_TOKEN")
        return "not configured"
    say("  The OAuth client credential the REST bearer is exchanged from.")
    say(f"  vault           {config.oauth.vault_address} ns={config.oauth.vault_namespace}")
    say(f"  auth            {config.oauth.auth_method} mount={config.oauth.auth_mount} "
        f"role={config.oauth.auth_role or '<from token>'}")
    say(f"  secret          {config.oauth.secret_engine} {config.oauth.secret_path}")
    credential = resolve_client_credential(config.oauth)
    if credential is None:
        raise CredentialError("no credential resolved")
    say("  resolved        yes")
    nico_rest.configure_token_refresh(
        credential, config.oauth.token_url, config.oauth.scope
    )
    return "resolved"


@check("4  NICo REST API")
def check_rest(config):
    if not os.environ.get("NICO_BASE_URL"):
        raise RuntimeError("NICO_BASE_URL is not set")
    say(f"  endpoint        {os.environ['NICO_BASE_URL']}")
    say(f"  org / api       {os.environ.get('NICO_ORG')} / {os.environ.get('NICO_API_NAME')}")

    if not os.environ.get("NICO_TOKEN"):
        if config.oauth is None:
            raise RuntimeError("no bearer and no [oauth] section to obtain one")
        if not nico_rest.refresh_token():
            raise RuntimeError(
                "no OAuth credential to exchange; the credential-source check did not "
                "resolve one"
            )
        say("  bearer          exchanged")

    tenant = nico_rest.get_tenant_uuid()
    say(f"  tenant          {tenant}")
    say(f"  site            {nico_rest.get_site_uuid(config.site.name)}")
    return f"tenant {tenant}"


@check("5  admin-cli")
def check_admin_cli(config):
    if config.grpc_api is None:
        say("  no [grpc_api] section: invoked as it is without one")
    else:
        say(f"  address         {config.grpc_api.url}")
        certificate = config.grpc_api.client_certificate
        if certificate is None:
            say("  identity        none")
        else:
            say(f"  identity        {certificate.vault_pki_mount}/issue/"
                f"{certificate.vault_pki_role} as {certificate.common_name}, "
                f"ttl {certificate.ttl}")
    with admin_cli.client_identity(config.grpc_api):
        machine = admin_cli.get_machine_from_mh_show(config.target.machine_id)
        say(f"  managed host    {config.target.machine_id}")
        say(f"  host BMC        {machine.get('host_bmc_ip')}  {machine.get('host_bmc_mac')}")
        for dpu in machine.get("dpus", []):
            say(f"  DPU BMC         {dpu.get('bmc_ip')}  {dpu.get('bmc_mac')}")
    if admin_cli._grpc_api_target is not None:
        raise RuntimeError("the staged identity outlived the context manager")
    say("  staged identity removed")
    return machine


_BMC_CHECK = "6  BMC Redfish access"


@check(_BMC_CHECK)
def check_bmc(config, machine):
    say("  The service root is open by specification; the Systems collection it")
    say("  advertises requires credentials.")

    endpoints = [("host", machine.get("host_bmc_ip"), machine.get("host_bmc_mac"))]
    for dpu in machine.get("dpus", []):
        endpoints.append((dpu.get("machine_id", "dpu"), dpu.get("bmc_ip"), dpu.get("bmc_mac")))

    read = 0
    with SiteVaultClient() as vault:
        for name, ip, mac in endpoints:
            say(f"  {name}")
            try:
                credentials = vault.get_bmc_credentials(mac)
                auth = (credentials.username, credentials.password)

                root = _redfish(ip, "/redfish/v1/", None)
                root.raise_for_status()
                body = root.json()
                say(f"    GET /redfish/v1/          HTTP {root.status_code}  "
                    f"{body.get('Product') or body.get('Name')}, "
                    f"Redfish {body.get('RedfishVersion')}")

                protected = (body.get("Systems") or {}).get("@odata.id")
                if not protected:
                    raise RuntimeError("service root advertises no Systems collection")

                authenticated = _redfish(ip, protected, auth)
                authenticated.raise_for_status()
                members = authenticated.json().get("Members", [])
                say(f"    GET {protected:<22} HTTP {authenticated.status_code}  "
                    f"{len(members)} member(s)")
            except Exception as error:  # noqa: BLE001 - report every endpoint
                say(f"    FAILED  {error.__class__.__name__}: {error}")
                continue
            read += 1

    if read < len(endpoints):
        raise RuntimeError(f"{len(endpoints) - read} of {len(endpoints)} endpoint(s) unreadable")
    return f"{read}/{len(endpoints)} endpoint(s) readable"


def _redfish(ip: str, path: str, auth: tuple[str, str] | None):
    """One Redfish GET. Unverified TLS, as the test does."""
    return requests.get(
        f"https://{ip}{path}", auth=auth, verify=False, timeout=REDFISH_TIMEOUT
    )


def main() -> int:
    say("NICo readiness probe -- read-only, no state is changed.")
    say("Redfish calls are unverified TLS (BMC self-signed certificates).")

    config = check_config()
    if config is None:
        return 2

    check_site_vault(config)
    check_credential(config)
    check_rest(config)
    machine = check_admin_cli(config)
    if machine is not None:
        check_bmc(config, machine)
    else:
        say(f"\n=== {_BMC_CHECK} ===")
        say("SKIP  no machine record from check 5")
        results.append((_BMC_CHECK, False, "skipped"))

    say("\n" + "=" * 62)
    failed = [name for name, ok, _ in results if not ok]
    for name, ok, detail in results:
        say(f"{'PASS' if ok else 'FAIL'}  {name}{('  -- ' + detail) if detail else ''}")
    say(f"{len(results) - len(failed)}/{len(results)} checks passed")

    artifacts = os.environ.get("ARTIFACT_DIR")
    if artifacts:
        try:
            os.makedirs(artifacts, exist_ok=True)
            with open(os.path.join(artifacts, "test-report.txt"), "w") as report:
                report.write("\n".join(_lines) + "\n")
        except OSError as error:
            print(f"could not write artifact report: {error}", file=sys.stderr)

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
