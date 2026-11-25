#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Preserve explicit DPF configuration, otherwise disable DPF for a skipped operator install."""

import pathlib
import sys

import yaml

try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        raise SystemExit("--skip-dpf requires Python 3.11 or later, or the tomli package") from None


def render(text):
    """Return Core values with an explicit DPF opt-out only when no effective DPF table exists."""
    values = yaml.safe_load(text) or {}
    api = values.get("nico-api") or {}
    values["nico-api"] = api
    site = api.get("siteConfig") or {}
    files = api.get("configFiles") or {}
    site_enabled = site.get("enabled", True)
    site_text = (site.get("nicoApiSiteConfig") or "") if site_enabled else ""
    base_text = files.get("nicoApiConfig") or ""
    if "dpf" in tomllib.loads(site_text) or "dpf" in tomllib.loads(base_text):
        return text

    # A custom base replaces the chart-generated config; use it if the site file is disabled.
    if not site_enabled:
        if not base_text:
            raise ValueError("--skip-dpf requires siteConfig.enabled=true or a custom base config "
                             "with [dpf].enabled=false when the site file is disabled")
        files["nicoApiConfig"] = base_text + "\n[dpf]\nenabled = false\n"
        api["configFiles"] = files
    else:
        site["nicoApiSiteConfig"] = site_text + "\n[dpf]\nenabled = false\n"
        api["siteConfig"] = site
    return yaml.safe_dump(values, sort_keys=False)


if __name__ == "__main__":
    try:
        source, destination = map(pathlib.Path, sys.argv[1:])
        destination.write_text(render(source.read_text()))
    except (ValueError, TypeError, AttributeError, yaml.YAMLError) as error:
        raise SystemExit(f"Cannot prepare --skip-dpf Core values: {error}") from error
