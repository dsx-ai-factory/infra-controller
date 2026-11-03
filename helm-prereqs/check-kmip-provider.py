#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check that a KMIP KEK provider in the Core values reads its TLS files from a chart mount or a mounted Secret that exists."""

import json
import posixpath
import subprocess
import sys

try:
    import yaml
except ImportError:
    raise SystemExit("KMIP provider preflight requires PyYAML in the python3 environment")

NAMESPACE = "nico-system"
# setup.sh creates this Secret from NICO_KMIP_CLIENT_DIR with exactly these keys and this label.
MANAGED_SECRET = "nico-kmip-client"
MANAGED_KEYS = ("ca.pem", "client.pem", "client.key")
MANAGED_BY = "helm-prereqs"
TLS_FIELDS = ("ca_bundle", "client_cert", "client_key")
# Mount paths the nico-api chart provides on its own (helm/charts/nico-api/templates/deployment.yaml).
# A TLS path under one of them, such as the site CA at /var/run/secrets/nico-roots/ca.crt, needs no
# extraVolumes entry, so the checker accepts it without a Secret lookup.
CHART_MOUNT_PATHS = (
    "/etc/forge/carbide-api",
    "/etc/nico/nico-api",
    "/mnt/persistence",
    "/var/run/secrets/spiffe.io",
    "/var/run/secrets/forge-roots",
    "/var/run/secrets/nico-roots",
    "/opt/carbide/firmware",
    "/opt/nico/firmware",
)
# Rendered only when nico-api.bootArtifactContainers is non-empty, like the template's own test.
BOOT_ARTIFACTS_MOUNT = "/forge-boot-artifacts/blobs/internal"
# Chart mounts whose path comes from the values, with the chart's defaults.
NVSWITCH_CLIENT_MOUNT_DEFAULT = "/var/run/secrets/nvswitch-client"
CREDENTIAL_FILE_MOUNT_DEFAULT = "/var/run/secrets/nico/credentials"


def _mapping(value, label):
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a mapping")
    return value


def _sequence(value, label):
    if value is None:
        return []
    if not isinstance(value, list):
        raise ValueError(f"{label} must be a list")
    return value


class ParserUnavailable(Exception):
    """No TOML parser is importable, so whether the site config declares a kmip provider is unknown."""


def _parse_toml(text):
    """Parse the site TOML. The parser ships with Python 3.11; older interpreters need the tomli package."""
    try:
        import tomllib
    except ImportError:
        try:
            import tomli as tomllib
        except ImportError:
            raise ParserUnavailable("the site config mentions KMIP, but python3 has no TOML parser to check for a "
                                    "kmip provider; install python3 3.11 or later, or the tomli package, if the "
                                    "site config declares one") from None
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"nico-api.siteConfig.nicoApiSiteConfig is not TOML: {error}") from None


def kmip_providers(nico_api):
    """Return every [secrets.kms.providers.*] table with type = "kmip" in the rendered site TOML."""
    site_config = _mapping(nico_api.get("siteConfig"), "nico-api.siteConfig")
    # The chart renders the TOML under `if .Values.siteConfig.enabled`, which defaults to true.
    if not _helm_enabled(site_config, "enabled", True):
        return {}
    text = site_config.get("nicoApiSiteConfig")
    if text is None or text == "":
        return {}
    if not isinstance(text, str):
        raise ValueError("nico-api.siteConfig.nicoApiSiteConfig must be a string")
    # A TOML string value is spelled out in the text, so a site config without the
    # word cannot declare the provider; skip the parser and its version requirement.
    if "kmip" not in text.lower():
        return {}
    config = _parse_toml(text)
    kms = _mapping(_mapping(config.get("secrets"), "[secrets]").get("kms"), "[secrets.kms]")
    providers = _mapping(kms.get("providers"), "[secrets.kms.providers]")
    return {
        name: table for name, table in providers.items()
        if isinstance(table, dict) and table.get("type") == "kmip"
    }


def _is_under(path, directory):
    relative = posixpath.relpath(posixpath.normpath(path), posixpath.normpath(directory))
    return relative not in (".", "..") and not relative.startswith("../")


def _helm_enabled(mapping, key, default):
    """Evaluate a values flag the way a template's `if` does: an absent key keeps the chart default,
    and a present one is false when it is false, null, 0, or an empty string, list, or map."""
    if key not in mapping:
        return default
    return bool(mapping[key])


def deepest_chart_mount(path, chart_mounts):
    """Return the longest chart-provided mount path above `path`, or None."""
    above = [posixpath.normpath(m) for m in chart_mounts if _is_under(path, m)]
    return max(above, key=len) if above else None


def chart_mount_paths(nico_api):
    """Return the mount paths the chart renders for nico-api from its templates and these values."""
    paths = list(CHART_MOUNT_PATHS)
    if nico_api.get("bootArtifactContainers"):
        paths.append(BOOT_ARTIFACTS_MOUNT)
    nvswitch = _mapping(_mapping(nico_api.get("nvSwitchTls"), "nico-api.nvSwitchTls").get("nicoClient"),
                        "nico-api.nvSwitchTls.nicoClient")
    # The template renders this mount under `if .Values.nvSwitchTls.nicoClient.enabled`, which
    # defaults to true in the chart.
    if _helm_enabled(nvswitch, "enabled", True):
        paths.append(str(nvswitch.get("mountPath") or NVSWITCH_CLIENT_MOUNT_DEFAULT))
    credential_file = _mapping(_mapping(nico_api.get("credentials"), "nico-api.credentials").get("file"),
                               "nico-api.credentials.file")
    if _mapping(credential_file.get("existingSecret"), "nico-api.credentials.file.existingSecret").get("name"):
        paths.append(str(credential_file.get("mountPath") or CREDENTIAL_FILE_MOUNT_DEFAULT))
    return paths


def mount_for(path, mounts):
    """Return the extraVolumeMounts entry serving `path`: the deepest mountPath above it, or a subPath mount of that very file."""
    best = None
    for mount in mounts:
        mount_path = mount.get("mountPath")
        if not isinstance(mount_path, str) or not mount_path:
            continue
        mount_path = posixpath.normpath(mount_path)
        file_mount = bool(mount.get("subPath")) and mount_path == posixpath.normpath(path)
        if (file_mount or _is_under(path, mount_path)) and (
                best is None or len(mount_path) > len(posixpath.normpath(best["mountPath"]))):
            best = mount
    return best


def secret_key_for(path, mount, items):
    """Return (key, None) for the Secret key a secret volume serves at `path`, or (None, problem).

    A subPath names the file or directory of the volume that the mount exposes, so the
    path inside the volume is the subPath plus the path below the mount.
    """
    mount_path = posixpath.normpath(mount["mountPath"])
    sub_path = str(mount["subPath"]).strip("/") if mount.get("subPath") else ""
    if posixpath.normpath(path) == mount_path:
        volume_path = sub_path
    else:
        below = posixpath.relpath(posixpath.normpath(path), mount_path)
        volume_path = f"{sub_path}/{below}" if sub_path else below
    if items:
        for item in items:
            if isinstance(item, dict) and item.get("path") == volume_path:
                return item.get("key"), None
        return None, f"the secret volume's items map nothing to {volume_path!r}"
    if "/" in volume_path:
        return None, f"{volume_path!r} is in a subdirectory of the volume, which a Secret volume cannot provide without items"
    return volume_path, None


def check_kmip_provider(stream, managed_secret=False, lookup_secret=None):
    """Check every kmip provider's TLS paths against the nico-api mounts and the Secrets behind them.

    `managed_secret` means setup.sh creates nico-system/nico-kmip-client from NICO_KMIP_CLIENT_DIR
    this run. `lookup_secret(name)` returns {"keys": set of data keys, "managed_by": label value or
    None} for an existing Secret, None when it does not exist, and raises RuntimeError when it
    cannot tell. Return (errors, warnings).
    """
    values = yaml.safe_load(stream)
    if values is None:
        values = {}
    if not isinstance(values, dict):
        raise ValueError("Core values must be a YAML mapping")
    nico_api = _mapping(values.get("nico-api"), "nico-api")
    errors = []
    warnings = []
    parser_missing = False
    try:
        providers = kmip_providers(nico_api)
    except ParserUnavailable as error:
        providers = {}
        parser_missing = True
        if managed_secret:
            # NICO_KMIP_CLIENT_DIR is an explicit opt-in, so a provider that cannot be checked is an error.
            errors.append("the site config mentions KMIP and NICO_KMIP_CLIENT_DIR is set, but python3 has no TOML "
                          "parser to check the kmip provider; install python3 3.11 or later, or the tomli package")
        else:
            warnings.append(str(error))

    if managed_secret:
        # setup.sh refuses to overwrite a Secret it does not own; catch that before Phase 1.
        try:
            existing = lookup_secret(MANAGED_SECRET)
        except RuntimeError as error:
            errors.append(f"could not inspect Secret {NAMESPACE}/{MANAGED_SECRET}, which setup.sh creates "
                          f"from NICO_KMIP_CLIENT_DIR: {error}")
        else:
            if existing is not None and existing.get("managed_by") != MANAGED_BY:
                errors.append(f"Secret {NAMESPACE}/{MANAGED_SECRET} exists but is not managed by setup.sh "
                              f"(its app.kubernetes.io/managed-by label is not {MANAGED_BY}); unset "
                              "NICO_KMIP_CLIENT_DIR to keep managing it yourself, or delete it so setup.sh can create it")
        if not providers and not parser_missing:
            warnings.append("NICO_KMIP_CLIENT_DIR is set, but the site config has no [secrets.kms.providers.*] table "
                            f"with type = \"kmip\"; setup.sh still creates Secret {NAMESPACE}/{MANAGED_SECRET}")
    if not providers:
        return errors, warnings

    mounts = [m for m in _sequence(nico_api.get("extraVolumeMounts"), "nico-api.extraVolumeMounts") if isinstance(m, dict)]
    volumes = {v.get("name"): v for v in _sequence(nico_api.get("extraVolumes"), "nico-api.extraVolumes") if isinstance(v, dict)}
    chart_mounts = chart_mount_paths(nico_api)
    required = {}  # Secret name -> key -> what needs it
    reported_mounts = set()  # mounts already reported as broken or non-Secret
    for name, provider in providers.items():
        label = f"[secrets.kms.providers.{name}]"
        for field in TLS_FIELDS:
            path = provider.get(field)
            if not isinstance(path, str) or not path:
                errors.append(f"{label} {field} is missing or not a string; the kmip provider requires all three TLS paths")
                continue
            if not path.startswith("/"):
                errors.append(f"{label} {field} = {path!r} is not an absolute path, so no mounted volume can provide it")
                continue
            mount = mount_for(path, mounts)
            chart_mount = deepest_chart_mount(path, chart_mounts)
            # The deepest mount wins, as in the Pod; a chart mount below an extra mount serves the file.
            if chart_mount is not None and (
                    mount is None or len(chart_mount) > len(posixpath.normpath(mount["mountPath"]))):
                continue  # provided by the chart itself; nothing to resolve
            if mount is None:
                errors.append(f"{label} {field} = {path!r} is not under any nico-api.extraVolumeMounts mountPath "
                              "or chart-provided mount; nico-api would refuse to start on the missing file")
                continue
            volume = volumes.get(mount.get("name"))
            if volume is None:
                if mount.get("name") not in reported_mounts:
                    reported_mounts.add(mount.get("name"))
                    errors.append(f"nico-api.extraVolumeMounts entry {mount.get('name')!r} ({mount.get('mountPath')}) has "
                                  "no nico-api.extraVolumes entry of that name; the Pod could not be created")
                continue
            secret = volume.get("secret")
            if not isinstance(secret, dict):
                if mount.get("name") not in reported_mounts:
                    reported_mounts.add(mount.get("name"))
                    kinds = [k for k in volume if k != "name"]
                    warnings.append(f"{label} {field} = {path!r} comes from volume {mount.get('name')!r} of kind "
                                    f"{kinds[0] if kinds else 'unknown'}, not a Secret; only Secret-backed TLS material is checked")
                continue
            secret_name = secret.get("secretName")
            if not isinstance(secret_name, str) or not secret_name:
                errors.append(f"nico-api.extraVolumes entry {mount.get('name')!r} has no secret.secretName")
                continue
            items = secret.get("items")
            key, problem = secret_key_for(path, mount, items)
            if problem is not None:
                errors.append(f"{label} {field} = {path!r}: {problem}")
                continue
            required.setdefault(secret_name, {}).setdefault(key, []).append(f"{label} {field}")
            # The kubelet mounts a non-optional Secret volume only when every listed item exists.
            if items and secret.get("optional") is not True:
                for item in items:
                    if isinstance(item, dict) and isinstance(item.get("key"), str):
                        required[secret_name].setdefault(item["key"], []).append(
                            f"nico-api.extraVolumes entry {mount.get('name')!r} items")

    if managed_secret and MANAGED_SECRET not in required:
        warnings.append(f"NICO_KMIP_CLIENT_DIR is set, but no nico-api.extraVolumes entry mounts the Secret "
                        f"{MANAGED_SECRET} that setup.sh creates from it")
    for secret_name, keys in sorted(required.items()):
        needs = ", ".join(sorted({need for owners in keys.values() for need in owners}))
        if managed_secret and secret_name == MANAGED_SECRET:
            for key, owners in sorted(keys.items()):
                if key not in MANAGED_KEYS:
                    errors.append(f"{', '.join(sorted(set(owners)))} need key {key!r} of Secret {MANAGED_SECRET}, but setup.sh "
                                  f"creates it with the keys {', '.join(MANAGED_KEYS)} only; use those names, or create "
                                  "the Secret yourself and unset NICO_KMIP_CLIENT_DIR")
            continue
        try:
            present = lookup_secret(secret_name)
        except RuntimeError as error:
            errors.append(f"could not inspect Secret {NAMESPACE}/{secret_name}, mounted for {needs}: {error}")
            continue
        if present is None:
            if secret_name == MANAGED_SECRET:
                hint = "set NICO_KMIP_CLIENT_DIR so setup.sh creates it before Phase 6, or create it yourself"
            else:
                hint = "create it before running setup.sh, or mount nico-kmip-client and set NICO_KMIP_CLIENT_DIR"
            errors.append(f"Secret {NAMESPACE}/{secret_name}, mounted by nico-api.extraVolumes for {needs}, "
                          f"does not exist; {hint}")
            continue
        for key, owners in sorted(keys.items()):
            if key not in present["keys"]:
                errors.append(f"Secret {NAMESPACE}/{secret_name} has no key {key!r}, which "
                              f"{', '.join(sorted(set(owners)))} need (keys present: "
                              f"{', '.join(sorted(present['keys'])) or 'none'})")
    return errors, warnings


def kubectl_secret(name):
    """Describe Secret nico-system/<name> as {"keys", "managed_by"}, None when it does not exist, or raise RuntimeError."""
    command = ["kubectl", "get", "secret", name, "-n", NAMESPACE, "-o", "json"]
    try:
        result = subprocess.run(command, capture_output=True, text=True, check=False)
    except OSError as error:
        raise RuntimeError(f"kubectl could not run: {error}") from None
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"kubectl exited {result.returncode}"
        if "NotFound" in detail or "not found" in detail:
            return None
        raise RuntimeError(detail)
    try:
        secret = json.loads(result.stdout)
    except ValueError as error:
        raise RuntimeError(f"kubectl returned invalid JSON: {error}") from None
    if not isinstance(secret, dict):
        raise RuntimeError("kubectl returned something other than a Secret")
    data = secret.get("data")
    labels = _mapping(secret.get("metadata"), "metadata").get("labels")
    return {
        "keys": set(data) if isinstance(data, dict) else set(),
        "managed_by": labels.get("app.kubernetes.io/managed-by") if isinstance(labels, dict) else None,
    }


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != "--managed-secret"):
        raise SystemExit("usage: check-kmip-provider.py CORE_VALUES_FILE [--managed-secret]")
    try:
        with open(sys.argv[1], encoding="utf-8") as stream:
            errors, warnings = check_kmip_provider(
                stream, managed_secret=len(sys.argv) == 3, lookup_secret=kubectl_secret)
        for error in errors:
            print(f"ERROR: {error}")
        for warning in warnings:
            print(f"WARNING: {warning}")
    except (OSError, ValueError, yaml.YAMLError) as error:
        raise SystemExit(f"Cannot check the KMIP KEK provider: {error}") from error
