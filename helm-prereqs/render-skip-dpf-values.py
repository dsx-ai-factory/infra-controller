#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Preserve explicit DPF configuration, otherwise disable DPF for a skipped operator install."""

import pathlib
import re
import sys

import yaml

try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        raise SystemExit("--skip-dpf requires Python 3.11 or later, or the tomli package") from None


def disable_dpf(text):
    """Insert the opt-out without discarding sibling settings or TOML comments."""
    expected = tomllib.loads(text)
    expected.setdefault("dpf", {})["enabled"] = False
    # The parser distinguishes real table headers from identical lines inside multiline strings.
    candidates = []
    for header in re.finditer(r'''(?m)^[ \t]*\[[ \t]*(?:dpf|'dpf'|"dpf")[ \t]*\][^\n]*(?:\n|$)''', text):
        candidates.append(text[:header.end()] + "\nenabled = false\n" + text[header.end():])
    for inline in re.finditer(r'''(?m)^[ \t]*(?:dpf|'dpf'|"dpf")[ \t]*=[ \t]*\{''', text):
        separator = "" if text[inline.end():].lstrip().startswith("}") else ","
        candidates.append(text[:inline.end()] + " enabled = false" + separator + " " + text[inline.end():])
    candidates.append("dpf.enabled = false\n" + text)
    for candidate in candidates:
        try:
            if tomllib.loads(candidate) == expected:
                return candidate
        except tomllib.TOMLDecodeError:
            pass
    raise ValueError("cannot insert dpf.enabled into this TOML; set it explicitly")


def render(text):
    """Disable DPF only when neither the effective site nor base sets enabled explicitly."""
    values = yaml.safe_load(text) or {}
    api = values.get("nico-api") or {}
    values["nico-api"] = api
    site = api.get("siteConfig") or {}
    files = api.get("configFiles") or {}
    site_enabled = site.get("enabled", True)
    site_text = (site.get("nicoApiSiteConfig") or "") if site_enabled else ""
    base_text = files.get("nicoApiConfig") or ""
    if ("enabled" in tomllib.loads(site_text).get("dpf", {})
            or "enabled" in tomllib.loads(base_text).get("dpf", {})):
        return text

    # A custom base replaces the chart-generated config; use it if the site file is disabled.
    if not site_enabled:
        if not base_text:
            # The mounted site ConfigMap is externally managed and cannot be inspected here.
            return text
        files["nicoApiConfig"] = disable_dpf(base_text)
        api["configFiles"] = files
    else:
        site["nicoApiSiteConfig"] = disable_dpf(site_text)
        api["siteConfig"] = site
    return yaml.safe_dump(values, sort_keys=False)


if __name__ == "__main__":
    try:
        source, destination = map(pathlib.Path, sys.argv[1:])
        destination.write_text(render(source.read_text()))
    except (ValueError, TypeError, AttributeError, yaml.YAMLError) as error:
        raise SystemExit(f"Cannot prepare --skip-dpf Core values: {error}") from error
