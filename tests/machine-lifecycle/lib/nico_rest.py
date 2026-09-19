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

"""Cloud operations for NICo sites, over the site-local REST API.

Every site under test is disconnected: there is no external cloud, so the
cloud half of the machine lifecycle test runs against the site-local NICo REST
API over HTTP. This module is the only cloud backend --
``tests/lifecycle/machine_lifecycle_test.py`` calls these functions directly.

Auth and endpoint come from the environment, set up by
``setup_site_config``:
  NICO_TOKEN     short-lived bearer (from lib.oauth.fetch_access_token)
  NICO_BASE_URL  site-local REST URL; https, or http only via loopback or a
                 Kubernetes service name
  NICO_ORG       org slug (e.g. ncx)
  NICO_API_NAME  API path segment (e.g. nico)

Requests go to ``{NICO_BASE_URL}/v2/org/{NICO_ORG}/{NICO_API_NAME}/<resource>``.
"""

import datetime
import json
import os
import sys
import time
from dataclasses import dataclass
from urllib.parse import quote, urlsplit

import requests

from lib import oauth

API_VERSION = "v2"
# Endpoints default to 20 items per page.
PAGE_SIZE = 100
# Backstop against a list that never terminates.
MAX_PAGES = 1000
REQUEST_TIMEOUT_SECONDS = 30
LOOPBACK_HOSTS = frozenset({"localhost", "127.0.0.1", "::1"})
# Kubernetes DNS names for a service that resolves only inside the cluster.
# Matched as exact suffixes, so foo.svc.example.com stays a remote host.
IN_CLUSTER_SUFFIXES = (".svc", ".svc.cluster.local")
INSTANCE_STATUS_POLL_INTERVAL_SECONDS = 60
MAX_CONSECUTIVE_INSTANCE_STATUS_TIMEOUTS = 10


@dataclass
class Site:
    """A NICo site under test."""

    name: str


class NicoError(Exception):
    """Exception for NICo cloud operation errors"""


__all__ = [
    "Site",
    "check_base_url",
    "configure_token_refresh",
    "refresh_token",
    "NicoError",
    "get_site_uuid",
    "list_vpcs",
    "get_vpc",
    "get_vpc_uuid",
    "create_vpc",
    "delete_vpc",
    "list_vpc_prefixes",
    "get_vpc_prefix",
    "get_vpc_prefix_uuid",
    "create_vpc_prefix",
    "delete_vpc_prefix",
    "list_ip_blocks",
    "get_operating_system_uuid",
    "list_operating_systems",
    "list_instances_for_operating_system",
    "get_tenant_uuid",
    "create_instance",
    "delete_instance",
    "get_instance_info",
    "get_instance_status",
    "wait_for_instance_ready",
    "wait_for_instance_status",
    "get_instance_ip",
    "wait_for_instance_ip",
    "get_instances",
    "wait_for_vpc_to_not_contain_instance",
    "get_machine_status",
    "wait_for_machine_ready",
    "wait_for_machine_status",
]


# ---------------------------------------------------------------------------
# Bearer-token refresh
# ---------------------------------------------------------------------------
_token_refresh: tuple[str, str, str] | None = None


def configure_token_refresh(credential: str, token_url: str, scope: str) -> None:
    """Retain what the bearer is re-exchanged from, in this process only.

    Held in module state rather than ``os.environ`` because the client
    credential is long-lived and reusable, unlike the bearer it mints: every
    kubectl and admin-cli child this run spawns inherits the environment, and a
    process environment stays readable for as long as the process does.
    """
    global _token_refresh
    _token_refresh = (credential, token_url, scope)


def refresh_token() -> bool:
    """Re-exchange the client credential for a fresh NICo bearer.

    The bearer is short-lived and a full lifecycle run (notably the ~90 min
    re-ingestion wait) can outlast it. A run given ``NICO_TOKEN`` directly
    configured no credential and has nothing to refresh with.

    Returns True if the token was refreshed.
    """
    if _token_refresh is None:
        return False
    credential, token_url, scope = _token_refresh
    os.environ["NICO_TOKEN"] = oauth.fetch_access_token(
        credential, token_url, scope
    )
    return True


# ---------------------------------------------------------------------------
# Low-level REST client
# ---------------------------------------------------------------------------
_SESSION = requests.Session()


def _checked_base_url(base: str) -> str:
    """Return the base URL, rejecting one the bearer must not be sent to.

    ``_send`` attaches the bearer to whatever this resolves to, so https is the
    rule. The exceptions are the two places cleartext cannot leave the host or
    the cluster: the loopback a port-forward listens on, and a Kubernetes
    service name. Both are properties of the URL itself rather than an operator
    assertion, so an endpoint someone else chose cannot come with its own
    permission to be cleartext.
    """
    parsed = urlsplit(base)
    # A trailing root dot is still the same name to the resolver.
    hostname = (parsed.hostname or "").rstrip(".")
    if not hostname:
        raise NicoError(f"NICO_BASE_URL must include a host; got {base!r}")
    if parsed.scheme == "https":
        return base
    if parsed.scheme == "http" and (
        hostname in LOOPBACK_HOSTS or hostname.endswith(IN_CLUSTER_SUFFIXES)
    ):
        return base
    raise NicoError(
        "NICO_BASE_URL must use https; http is allowed only via localhost or a "
        f"Kubernetes service name ending {' or '.join(IN_CLUSTER_SUFFIXES)}; "
        f"got {base!r}"
    )


def check_base_url() -> str:
    """Validate NICO_BASE_URL up front, before the run does anything."""
    return _checked_base_url(os.environ.get("NICO_BASE_URL", "").rstrip("/"))


def _endpoint_prefix() -> str:
    """Build the org-scoped API prefix from the environment."""
    base = os.environ.get("NICO_BASE_URL", "").rstrip("/")
    org = os.environ.get("NICO_ORG", "")
    api_name = os.environ.get("NICO_API_NAME", "")
    missing = [
        name
        for name, value in (
            ("NICO_BASE_URL", base),
            ("NICO_ORG", org),
            ("NICO_API_NAME", api_name),
        )
        if not value
    ]
    if missing:
        raise NicoError(f"Missing required environment: {', '.join(missing)}")
    base = _checked_base_url(base)
    return f"{base}/{API_VERSION}/org/{quote(org, safe='')}/{quote(api_name, safe='')}"


def _send(method: str, url: str, params: dict | None, body: dict | None):
    headers = {"Accept": "application/json"}
    token = os.environ.get("NICO_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return _SESSION.request(
        method,
        url,
        params=params,
        json=body,
        headers=headers,
        timeout=REQUEST_TIMEOUT_SECONDS,
    )


def _dispatch(
    method: str,
    path: str,
    params: dict | None = None,
    body: dict | None = None,
) -> tuple[str, requests.Response]:
    """Send one authenticated request, replaying once if the bearer expired."""
    url = f"{_endpoint_prefix()}/{path.lstrip('/')}"
    print(f"Requesting {method} {url}")
    response = _send(method, url, params, body)
    # A 401 is rejected before the request is acted on, so replay is safe for
    # every verb, including create and delete.
    if response.status_code == 401 and refresh_token():
        print("NICo bearer token expired; refreshed and retrying")
        response = _send(method, url, params, body)
    return url, response


def _check(method: str, url: str, response: requests.Response) -> requests.Response:
    """Return the response, or raise NicoError describing an error status."""
    if not response.ok:
        raise NicoError(_error_message(method, url, response))
    return response


def _request(
    method: str,
    path: str,
    *,
    params: dict | None = None,
    body: dict | None = None,
) -> requests.Response:
    """Issue one authenticated request, raising NicoError on an error status."""
    url, response = _dispatch(method, path, params, body)
    return _check(method, url, response)


def _request_optional(
    method: str,
    path: str,
    *,
    params: dict | None = None,
    body: dict | None = None,
) -> requests.Response | None:
    """Like _request, but returns None when the resource does not exist."""
    url, response = _dispatch(method, path, params, body)
    if response.status_code == 404:
        return None
    return _check(method, url, response)


def _error_message(method: str, url: str, response: requests.Response) -> str:
    """Render an API error, preferring the server's own message field."""
    detail = response.text.strip()
    try:
        payload = response.json()
    except ValueError:
        payload = None
    if isinstance(payload, dict) and payload.get("message"):
        detail = payload["message"]
        if payload.get("data") is not None:
            detail = f"{detail} (data: {payload['data']})"
    return f"{method} {url} failed with HTTP {response.status_code}: {detail[:500]}"


def _json(response: requests.Response) -> object:
    """Parse a response body as JSON, printing the raw body on failure."""
    try:
        return response.json()
    except ValueError:
        print(f"JSON decode error:\n{response.text}", file=sys.stderr)
        raise


def _pagination_total(response: requests.Response) -> int | None:
    """Total item count from the X-Pagination header, or None if absent."""
    raw = response.headers.get("X-Pagination")
    if not raw:
        return None
    try:
        total = json.loads(raw).get("total")
    except (ValueError, AttributeError):
        return None
    return total if isinstance(total, int) else None


def _list(resource: str, params: dict[str, str] | None = None) -> list[dict]:
    """Page through a list endpoint and return every item.

    A site holds more objects than the 20-item default page, so an unpaged
    request would truncate silently.
    """
    query = dict(params or {})
    query["pageSize"] = str(PAGE_SIZE)
    items: list[dict] = []
    for page_number in range(1, MAX_PAGES + 1):
        query["pageNumber"] = str(page_number)
        response = _request("GET", resource, params=query)
        page = _json(response)
        if not isinstance(page, list):
            raise NicoError(
                f"Unexpected '{resource}' list response type "
                f"({type(page).__name__}): {page!r}"
            )
        items.extend(page)
        total = _pagination_total(response)
        if total is not None and len(items) >= total:
            break
        if len(page) < PAGE_SIZE:
            break
    else:
        raise NicoError(
            f"'{resource}' list did not terminate within {MAX_PAGES} pages "
            f"({len(items)} items)"
        )
    return items


def _find_by_name(resource: str, name: str, candidates: list[dict] | None = None) -> dict:
    items = candidates if candidates is not None else _list(resource)
    for item in items:
        if item.get("name") == name:
            return item
    raise NicoError(f"No {resource} with name '{name}' found.")


def get_tenant_uuid() -> str:
    """Get the current tenant's UUID (tenant scope is the bearer token)."""
    data = _json(_request("GET", "tenant/current"))
    return data["id"]


# ---------------------------------------------------------------------------
# UUID lookups
# ---------------------------------------------------------------------------
def get_site_uuid(site_name: str) -> str:
    """Given a site name, find its UUID."""
    return _find_by_name("site", site_name)["id"]


def list_vpcs(site_uuid: str) -> list[dict]:
    """Return the current tenant's VPCs at one site."""
    return [vpc for vpc in _list("vpc") if vpc.get("siteId") == site_uuid]


def get_vpc(vpc_uuid: str) -> dict:
    """Return one VPC by UUID."""
    data = _json(_request("GET", f"vpc/{quote(vpc_uuid, safe='')}"))
    if not isinstance(data, dict):
        raise NicoError(f"Unexpected VPC response: {data!r}")
    return data


def get_vpc_uuid(vpc_name: str, site_uuid: str) -> str:
    """Given a VPC name and a site UUID, find its UUID."""
    return _find_by_name("vpc", vpc_name, candidates=list_vpcs(site_uuid))["id"]


def create_vpc(
    name: str,
    site_uuid: str,
    *,
    description: str | None = None,
) -> str:
    """Create an FNN VPC and return its UUID."""
    body = {
        "name": name,
        "siteId": site_uuid,
        "networkVirtualizationType": "FNN",
    }
    if description is not None:
        body["description"] = description
    data = _json(_request("POST", "vpc", body=body))
    if not isinstance(data, dict) or not isinstance(data.get("id"), str) or not data["id"].strip():
        raise NicoError(f"Unexpected VPC create response: {data!r}")
    return data["id"]


def delete_vpc(vpc_uuid: str) -> None:
    """Delete one VPC by UUID."""
    _request("DELETE", f"vpc/{quote(vpc_uuid, safe='')}")


def list_vpc_prefixes(site_uuid: str) -> list[dict]:
    """Return the current tenant's VPC prefixes at one site."""
    return [prefix for prefix in _list("vpc-prefix") if prefix.get("siteId") == site_uuid]


def get_vpc_prefix(
    vpc_prefix_uuid: str, *, allow_missing: bool = False
) -> dict | None:
    """Return one VPC prefix by UUID, optionally returning None for 404."""
    path = f"vpc-prefix/{quote(vpc_prefix_uuid, safe='')}"
    response = _request_optional("GET", path) if allow_missing else _request("GET", path)
    if response is None:
        return None
    data = _json(response)
    if not isinstance(data, dict):
        raise NicoError(f"Unexpected VPC-prefix response: {data!r}")
    return data


def get_vpc_prefix_uuid(vpc_prefix_name: str, site_uuid: str) -> str:
    """Given a VPC prefix name and a site UUID, find its UUID."""
    prefixes = list_vpc_prefixes(site_uuid)
    return _find_by_name("vpc-prefix", vpc_prefix_name, candidates=prefixes)["id"]


def create_vpc_prefix(
    name: str,
    vpc_uuid: str,
    ip_block_uuid: str,
    prefix_length: int,
) -> str:
    """Create an IPv4 VPC prefix and return its UUID."""
    body = {
        "name": name,
        "vpcId": vpc_uuid,
        "ipBlockId": ip_block_uuid,
        "prefixLength": prefix_length,
    }
    data = _json(_request("POST", "vpc-prefix", body=body))
    if not isinstance(data, dict) or not isinstance(data.get("id"), str) or not data["id"].strip():
        raise NicoError(f"Unexpected VPC-prefix create response: {data!r}")
    return data["id"]


def delete_vpc_prefix(vpc_prefix_uuid: str) -> None:
    """Delete one VPC prefix by UUID."""
    _request("DELETE", f"vpc-prefix/{quote(vpc_prefix_uuid, safe='')}")


def list_ip_blocks(site_uuid: str) -> list[dict]:
    """Return IP blocks visible to the tenant at one site."""
    return [ip_block for ip_block in _list("ipblock") if ip_block.get("siteId") == site_uuid]


def get_operating_system_uuid(operating_system_name: str) -> str:
    """Given an operating system name, find its UUID."""
    return _find_by_name("operating-system", operating_system_name)["id"]


def list_operating_systems(
    *,
    query: str | None = None,
    operating_system_type: str | None = None,
) -> list[dict]:
    """Return operating systems narrowed by optional search text and type."""
    params = {}
    if query is not None:
        params["query"] = query
    if operating_system_type is not None:
        params["type"] = operating_system_type
    return _list("operating-system", params=params)


def list_instances_for_operating_system(operating_system_uuid: str) -> list[dict]:
    """Return every instance that references one operating system.
    """
    instances = _list(
        "instance",
        params={"operatingSystemId": operating_system_uuid},
    )
    unexpected = [
        instance.get("id")
        for instance in instances
        if instance.get("operatingSystemId") != operating_system_uuid
    ]
    if unexpected:
        raise NicoError(
            "Instance lookup for operating system "
            f"{operating_system_uuid} returned unrelated or malformed instances: "
            f"{unexpected}"
        )
    return instances


# ---------------------------------------------------------------------------
# Operating systems
# ---------------------------------------------------------------------------
def create_operating_system(
    name: str,
    ipxe_script: str,
    user_data: str,
    *,
    description: str | None = None,
    phone_home_enabled: bool = True,
) -> dict:
    """Create a tenant-owned iPXE OS definition and return its JSON response."""
    body = {
        "name": name,
        "tenantId": get_tenant_uuid(),
        "ipxeScript": ipxe_script,
        "userData": user_data,
        "allowOverride": True,
        "phoneHomeEnabled": phone_home_enabled,
    }
    if description is not None:
        body["description"] = description

    response = _request("POST", "operating-system", body=body)
    print("operating-system create response:")
    print(response.text)
    data = _json(response)
    if not isinstance(data, dict):
        raise NicoError(f"Unexpected operating-system create response: {data!r}")
    operating_system_uuid = data.get("id")
    if not isinstance(operating_system_uuid, str) or not operating_system_uuid.strip():
        raise NicoError(
            "Unexpected operating-system create response: missing a non-empty "
            f"'id': {data!r}"
        )
    return data


def get_operating_system(operating_system_uuid: str) -> dict:
    """Get the current representation of an operating system."""
    response = _request(
        "GET", f"operating-system/{quote(operating_system_uuid, safe='')}"
    )
    data = _json(response)
    if not isinstance(data, dict):
        raise NicoError(f"Unexpected operating-system get response: {data!r}")
    return data


def wait_for_operating_system_ready(
    operating_system_uuid: str, timeout: int = 5 * 60, poll_interval: int = 5
) -> None:
    """Wait until an operating system reaches Ready or a terminal failure."""
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        operating_system = get_operating_system(operating_system_uuid)
        status = operating_system.get("status")
        if not isinstance(status, str) or not status.strip():
            raise NicoError(
                f"Operating system {operating_system_uuid} returned invalid status "
                f"payload: {operating_system}"
            )
        if status == "Ready":
            return
        if status in {"Error", "Failed", "Deactivated"}:
            raise NicoError(
                f"Operating system {operating_system_uuid} entered terminal status {status}: "
                f"{operating_system}"
            )
        print(
            f"{now.strftime('%Y-%m-%d %H:%M:%S')}: operating system "
            f"{operating_system_uuid} is {status!r}; waiting for Ready"
        )
        time.sleep(poll_interval)
    raise TimeoutError(
        f"Operating system {operating_system_uuid} did not reach Ready within {timeout} seconds"
    )


def delete_operating_system(operating_system_uuid: str, strict: bool = True) -> bool:
    """Delete an operating system, optionally making the operation best-effort.

    The best-effort path reports the reason before returning False. Swallowing
    it silently leaves the caller able to say only that cleanup failed, which
    hides the common and expected case: a run that keeps its instance (a
    failure held for analysis, or debug.keep_instance) still references this
    definition, so the delete is refused.
    """
    path = f"operating-system/{quote(operating_system_uuid, safe='')}"
    try:
        _request("DELETE", path)
    except NicoError as delete_error:
        if strict:
            raise
        print(
            f"WARNING: operating system {operating_system_uuid} delete was "
            f"refused: {delete_error}",
            file=sys.stderr,
        )
        return False
    return True


# ---------------------------------------------------------------------------
# Instances
# ---------------------------------------------------------------------------
def create_instance(
    instance_name: str,
    machine_id: str,
    network_interface: str | dict[str, object],
    operating_system_uuid: str,
    virtual_private_cloud_uuid: str,
) -> dict:
    """Create an instance and return the JSON response.

    ``network_interface`` is either a NICo interface request object or the
    legacy ``"vpcPrefixId=<uuid>"`` string. The object form supports
    Controller-managed selection with
    ``{"vpcId": <uuid>, "ipFamilies": ["IPv4"]}``.

    Body fields match the NICo ``InstanceCreateRequest`` schema (required:
    name, tenantId, vpcId). Phone-home is enabled with ``phoneHomeEnabled``;
    without it the instance never phones home and never reaches Ready.

    ``machine_id`` pins the instance to a specific machine via the ``machineId``
    field. MLT does not use instance-type-based placement.
    """
    if isinstance(network_interface, dict):
        interface = dict(network_interface)
    else:
        key, _, value = network_interface.partition("=")
        if not value:
            # Bare UUID passed; it names a VPC prefix.
            key, value = "vpcPrefixId", network_interface
        interface = {key: value}

    body = {
        "name": instance_name,
        "tenantId": get_tenant_uuid(),
        "machineId": machine_id,
        "vpcId": virtual_private_cloud_uuid,
        "operatingSystemId": operating_system_uuid,
        "interfaces": [interface],
        # NICo's InstanceCreateRequest field enabling phone-home.
        "phoneHomeEnabled": True,
    }
    response = _request("POST", "instance", body=body)
    print("instance create response:")
    print(response.text)
    return _json(response)


def get_instance_info(instance_uuid: str) -> dict:
    """Get up-to-date info on the instance with the given UUID."""
    return _json(_request("GET", f"instance/{quote(instance_uuid, safe='')}"))


def delete_instance(instance_uuid: str) -> None:
    """Delete the instance with the given UUID."""
    _request("DELETE", f"instance/{quote(instance_uuid, safe='')}")


def get_instance_status(instance_uuid: str, site: Site) -> str:
    """Get the current status of the specified instance (tenant view)."""
    return get_instance_info(instance_uuid)["status"]


def wait_for_instance_ready(instance_uuid: str, site: Site, timeout: int) -> None:
    """Check repeatedly until the specified instance has status Ready."""
    wait_for_instance_status(instance_uuid, site, "Ready", timeout)


def _sleep_before_next_instance_status_poll(deadline: datetime.datetime) -> None:
    """Sleep for at most the time remaining before the polling deadline."""
    remaining = (deadline - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
    if remaining > 0:
        time.sleep(min(INSTANCE_STATUS_POLL_INTERVAL_SECONDS, remaining))


def wait_for_instance_status(
    instance_uuid: str, site: Site, desired_status: str, timeout: int
) -> None:
    """Check repeatedly until the instance reaches a specific status."""
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    consecutive_timeouts = 0
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        now_formatted = now.strftime("%Y-%m-%d %H:%M:%S")
        try:
            status = get_instance_status(instance_uuid, site)
        except requests.Timeout as error:
            consecutive_timeouts += 1
            if consecutive_timeouts >= MAX_CONSECUTIVE_INSTANCE_STATUS_TIMEOUTS:
                raise TimeoutError(
                    f"Instance {instance_uuid} status request timed out "
                    f"{consecutive_timeouts} consecutive times"
                ) from error
            print(
                f"{now_formatted}: transient REST transport failure while checking instance "
                f"{instance_uuid} status ({type(error).__name__}: {error}); "
                f"attempt {consecutive_timeouts}/"
                f"{MAX_CONSECUTIVE_INSTANCE_STATUS_TIMEOUTS}; retrying"
            )
            _sleep_before_next_instance_status_poll(end)
            continue
        consecutive_timeouts = 0
        if status == desired_status:
            print(
                f"{now_formatted}: instance {instance_uuid} reached desired status "
                f"({desired_status})!"
            )
            return
        print(
            f"{now_formatted}: instance {instance_uuid} not in desired status "
            f"({desired_status}) yet, current status: {status}"
        )
        _sleep_before_next_instance_status_poll(end)
    else:
        raise TimeoutError(
            f"Instance {instance_uuid} did not get to desired status ({desired_status}) "
            f"within {timeout} seconds"
        )


def get_instance_ip(
    instance_uuid: str, network_interface: str | dict[str, object]
) -> str | None:
    """Get an instance's IP address for the requested network selector.

    The selector may be a request object, bare UUID, or ``key=uuid`` string.
    """
    if isinstance(network_interface, dict):
        selector = {
            key: value
            for key, value in network_interface.items()
            if key in {"subnetId", "vpcPrefixId", "vpcId"}
        }
    else:
        key, separator, value = network_interface.partition("=")
        selector = {key if separator else "vpcPrefixId": value or key}
    data = get_instance_info(instance_uuid)
    interfaces = data.get("interfaces", [])
    for interface in interfaces:
        if any(interface.get(key) == value for key, value in selector.items()):
            ips = interface.get("ipAddresses")
            return ips[0] if ips else None

    # A VPC-selected create request uses vpcId, but NICo resolves that request
    # to an interface whose read model identifies only the chosen vpcPrefixId.
    # MLT requests exactly one interface, so the sole response interface is
    # unambiguous once the Instance itself confirms the requested VPC.
    requested_vpc_id = selector.get("vpcId")
    if requested_vpc_id is not None and data.get("vpcId") == requested_vpc_id:
        if len(interfaces) == 1:
            ips = interfaces[0].get("ipAddresses")
            return ips[0] if ips else None

    print(
        f"Couldn't find details for network interface {selector!r}.\n{data}",
        file=sys.stderr,
    )
    return None


def wait_for_instance_ip(
    instance_uuid: str,
    network_interface: str | dict[str, object],
    timeout: int,
) -> str:
    """Wait until the given instance has an IP address on the interface."""
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        now_formatted = now.strftime("%Y-%m-%d %H:%M:%S")
        ip_address = get_instance_ip(instance_uuid, network_interface)
        if ip_address:
            return ip_address
        print(f"{now_formatted}: Instance {instance_uuid} doesn't have an IP address yet")
        time.sleep(60)
    else:
        raise TimeoutError(
            f"Instance {instance_uuid} did not get an IP address within {timeout} seconds."
        )


def get_instances(site_uuid: str, vpc_uuid: str | None = None) -> list[dict]:
    """Get instances for a site, optionally filtered to a VPC."""
    instances = [i for i in _list("instance") if i.get("siteId") in (None, site_uuid)]
    if vpc_uuid is not None:
        instances = [i for i in instances if i.get("vpcId") == vpc_uuid]
    return instances


def wait_for_vpc_to_not_contain_instance(
    site_uuid: str, vpc_uuid: str, instance_uuid: str, timeout: int
) -> None:
    """Wait until the VPC no longer contains the given instance."""
    instances = None
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        now_formatted = now.strftime("%Y-%m-%d %H:%M:%S")
        instances = get_instances(site_uuid, vpc_uuid)
        if not any(instance["id"] == instance_uuid for instance in instances):
            print(f"{now_formatted}: VPC {vpc_uuid} no longer contains instance {instance_uuid}!")
            return
        print(f"{now_formatted}: VPC {vpc_uuid} still contains instance {instance_uuid}")
        time.sleep(60)
    else:
        raise TimeoutError(
            f"VPC {vpc_uuid} still contains instance {instance_uuid} after {timeout} seconds."
            f"\n{instances=}"
        )


# ---------------------------------------------------------------------------
# Machines (provider view)
# ---------------------------------------------------------------------------
def get_machine_status(machine_id: str, site: Site, allow_missing_machine: bool = False) -> str:
    """Get the current status of the specified machine (provider view).

    Returns "<Missing>" if the machine is absent and allow_missing_machine.
    """
    path = f"machine/{quote(machine_id, safe='')}"
    if allow_missing_machine:
        response = _request_optional("GET", path)
        if response is None:
            return "<Missing>"
    else:
        response = _request("GET", path)
    return _json(response)["status"]


def wait_for_machine_ready(machine_id: str, site: Site, timeout: int) -> None:
    """Check repeatedly until the specified machine has status Ready."""
    wait_for_machine_status(machine_id, site, "Ready", timeout)


def wait_for_machine_status(
    machine_id: str,
    site: Site,
    desired_status: str,
    timeout: int,
    allow_missing_machine: bool = False,
) -> None:
    """Check repeatedly until the machine reaches a specific status."""
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        now_formatted = now.strftime("%Y-%m-%d %H:%M:%S")
        status = get_machine_status(machine_id, site, allow_missing_machine)
        if status == desired_status:
            print(
                f"{now_formatted}: machine {machine_id} reached desired status "
                f"({desired_status})!"
            )
            return
        print(
            f"{now_formatted}: machine {machine_id} not in desired status ({desired_status}) "
            f"yet, current status: {status}"
        )
        time.sleep(60)
    else:
        raise TimeoutError(
            f"Machine id {machine_id} did not get to desired status ({desired_status}) within "
            f"{timeout} seconds."
        )
