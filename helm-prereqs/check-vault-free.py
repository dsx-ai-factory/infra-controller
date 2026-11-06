#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check that the Core values never make nico-api build a Vault client when setup.sh installs without Vault.

setup.sh --skip-vault installs no Vault, renders no vault-nico-issuer, and deploys Core with
global.certificate.issuerRef pinned to the site-issuer ClusterIssuer (name, kind, and group),
global.certificate.useCertManager=true, global.certificate.useCertManagerUfm=true, and
nico-api.vault.enabled=false. This checker covers what those --set values do not reach, following
the startup rules of nico-api. The effective TOML config (the site TOML over the base config, merged
the way nico-api merges them) must have a [secrets] section without the Vault credential backend or a
Transit KMS provider, both [certificates] cert-manager flags true, and no [certificates.cert_manager]
signer that names vault-nico-issuer or makes site-issuer a namespaced Issuer. Issuer references below
the top level of the values must not name vault-nico-issuer, and nico-api.vaultClusterInfo must be
empty, because the chart rejects it without Vault.
"""

import sys

try:
    import yaml
except ImportError:
    raise SystemExit("Vault-free preflight requires PyYAML in the python3 environment")

VAULT_ISSUER = "vault-nico-issuer"
SITE_ISSUER = "site-issuer"
CLUSTER_ISSUER = "ClusterIssuer"
# The issuer reference setup.sh --skip-vault overrides. Helm gives the parent chart's global values
# precedence over a subchart's copy, so a <subchart>.global.certificate.issuerRef is overridden too.
GLOBAL_ISSUER_REF = ("global", "certificate", "issuerRef")
# The [certificates] flags with the chart value each one mirrors in the generated base config.
CERT_MANAGER_FLAGS = (("use_cert_manager", "useCertManager"), ("use_cert_manager_ufm", "useCertManagerUfm"))
SITE_TOML = "the site TOML (nico-api.siteConfig.nicoApiSiteConfig)"
BASE_CONFIG = "nico-api.configFiles.nicoApiConfig"
SECRETS_DOC = "docs/configuration/secrets-storage.md"
PARSER_HINT = "python3 3.11 or later, or the tomli package"


def _mapping(value, label):
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a mapping")
    return value


def _helm_enabled(mapping, key, default):
    """Evaluate a values flag the way a template's `if` does: an absent key keeps the chart default,
    and a present one is false when it is false, null, 0, or an empty string, list, or map."""
    if key not in mapping:
        return default
    return bool(mapping[key])


class ParserUnavailable(Exception):
    """No TOML parser is importable, so the nico-api TOML config cannot be read."""


def _parse_toml(text, label):
    """Parse a nico-api TOML config. The parser ships with Python 3.11; older interpreters need the tomli package."""
    try:
        import tomllib
    except ImportError:
        try:
            import tomli as tomllib
        except ImportError:
            raise ParserUnavailable("python3 has no TOML parser to read the nico-api config ([secrets] and "
                                    "[certificates] in the site TOML or a custom base config), which --skip-vault "
                                    f"requires; install {PARSER_HINT}") from None
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"{label} is not TOML: {error}") from None


def site_toml(nico_api):
    """Return the parsed site TOML, or None when the chart renders none (siteConfig disabled or empty)."""
    site_config = _mapping(nico_api.get("siteConfig"), "nico-api.siteConfig")
    # The chart renders the TOML under `if .Values.siteConfig.enabled`, which defaults to true.
    if not _helm_enabled(site_config, "enabled", True):
        return None
    text = site_config.get("nicoApiSiteConfig")
    if text is None or text == "":
        return None
    if not isinstance(text, str):
        raise ValueError("nico-api.siteConfig.nicoApiSiteConfig must be a string")
    return _parse_toml(text, SITE_TOML)


def base_toml(nico_api):
    """Return (base config, custom) as nico-api reads its --config-path file.

    A non-empty nico-api.configFiles.nicoApiConfig replaces the chart's generated config verbatim
    (custom is True). Otherwise the generated config applies (custom is False); of it, only the parts
    that matter here are reproduced: [certificates] renders the global flags that setup.sh --skip-vault
    sets to true, [certificates.cert_manager] renders nico-api.machineCertificates.issuerRef (chart
    default site-issuer, ClusterIssuer), and there is no [secrets] section.
    """
    config_files = _mapping(nico_api.get("configFiles"), "nico-api.configFiles")
    text = config_files.get("nicoApiConfig")
    if text is None or text == "":
        machine = _mapping(_mapping(nico_api.get("machineCertificates"), "nico-api.machineCertificates").get("issuerRef"),
                           "nico-api.machineCertificates.issuerRef")
        generated = {"certificates": {"use_cert_manager": True, "use_cert_manager_ufm": True,
                                      "cert_manager": {"issuer_name": machine.get("name") or SITE_ISSUER,
                                                       "issuer_kind": machine.get("kind") or CLUSTER_ISSUER}}}
        return generated, False
    if not isinstance(text, str):
        raise ValueError(f"{BASE_CONFIG} must be a string")
    return _parse_toml(text, BASE_CONFIG), True


def _deep_merge(base, overlay):
    """Merge two TOML documents the way nico-api merges its config files (crates/api-core/src/cfg/load.rs,
    merge_carbide_config_providers: Figment merges the base, then the site TOML): the later document wins key by
    key, tables merge recursively, and every other value, lists included, is replaced."""
    merged = dict(base)
    for key, value in overlay.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _sets(config, path):
    """True when the TOML document has a key at `path`, whatever its value."""
    node = config
    for part in path:
        if not isinstance(node, dict) or part not in node:
            return False
        node = node[part]
    return True


def _source(site, custom_base, path):
    """Name the file whose value at `path` wins: the site TOML over a custom base config; None when neither sets it."""
    if site is not None and _sets(site, path):
        return SITE_TOML
    if custom_base is not None and _sets(custom_base, path):
        return BASE_CONFIG
    return None


def _toml_literal(value):
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return f'"{value}"'
    return repr(value)


def _dotted(path):
    text = ""
    for part in path:
        if isinstance(part, int):
            text += f"[{part}]"
        else:
            text += ("." if text else "") + str(part)
    return text


def issuer_refs(node, path=()):
    """Yield (path, issuerRef mapping) for every issuerRef mapping in the document, lists included."""
    if isinstance(node, dict):
        for key, value in node.items():
            here = path + (str(key),)
            if key == "issuerRef" and isinstance(value, dict):
                yield here, value
            yield from issuer_refs(value, here)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            yield from issuer_refs(value, path + (index,))


def check_secrets(effective, site, custom_base):
    """Errors for every effective [secrets] setting that keeps a Vault client required at startup."""
    errors = []
    secrets = effective.get("secrets")
    if secrets is None:
        remedy = ('add [secrets] with backends = ["postgres"], writer = "postgres", and an integrated or kmip KMS '
                  f"provider ({SECRETS_DOC})")
        if custom_base is not None:
            errors.append(f"neither {BASE_CONFIG} nor {SITE_TOML} has a [secrets] section, so nico-api would start "
                          f"with the Vault credential store; {remedy}")
        elif site is None:
            errors.append("nico-api.siteConfig renders no site TOML, so there is no [secrets] section and nico-api "
                          f"would start with the Vault credential store; enable siteConfig and {remedy}")
        else:
            errors.append(f"{SITE_TOML} has no [secrets] section, so nico-api would start with the Vault credential "
                          f"store; {remedy}")
        return errors
    secrets = _mapping(secrets, "[secrets]")
    backends = secrets.get("backends")
    if backends is None:
        errors.append('[secrets] backends is omitted and defaults to ["vault"], which keeps the Vault credential backend '
                      'in the read chain; set backends = ["postgres"]')
    elif isinstance(backends, list) and "vault" in backends:
        errors.append('[secrets] backends lists "vault", which keeps the Vault credential backend in the read chain; '
                      'set backends = ["postgres"]')
    writer = secrets.get("writer")
    if writer is None:
        errors.append('[secrets] writer is omitted and defaults to "vault", which sends credential writes to Vault; '
                      'set writer = "postgres"')
    elif writer == "vault":
        errors.append('[secrets] writer = "vault" sends credential writes to Vault; set writer = "postgres"')
    if secrets.get("import_from") == "vault":
        errors.append('[secrets] import_from = "vault" keeps the Vault client required at startup, even after the '
                      "import has completed; remove it (the import must already have run with Vault installed)")
    kms = _mapping(secrets.get("kms"), "[secrets.kms]")
    providers = _mapping(kms.get("providers"), "[secrets.kms.providers]")
    for name, table in providers.items():
        if isinstance(table, dict) and table.get("type") == "transit":
            errors.append(f'[secrets.kms.providers.{name}] type = "transit" builds its own Vault client at startup '
                          "whether or not it is the active provider; re-wrap its KEKs onto an integrated or kmip "
                          f"provider and remove it ({SECRETS_DOC})")
    return errors


def check_certificates(effective, site, custom_base):
    """Errors for an effective [certificates] cert-manager flag that is not true."""
    errors = []
    certificates = _mapping(effective.get("certificates"), "[certificates]")
    for toml_key, chart_key in CERT_MANAGER_FLAGS:
        value = certificates.get(toml_key)
        if value is True:
            continue
        source = _source(site, custom_base, ("certificates", toml_key))
        if source is None:
            # Only a custom base config gets here: the generated config carried the flag that setup.sh set.
            errors.append(f"[certificates] {toml_key} is set in neither {BASE_CONFIG} nor {SITE_TOML}, and nico-api "
                          "defaults it to false; the custom base config replaces the chart's generated config that "
                          f"carried setup.sh's --set global.certificate.{chart_key}=true, so set {toml_key} = true in "
                          "one of them")
        elif source == BASE_CONFIG:
            errors.append(f"[certificates] {toml_key} = {_toml_literal(value)} in {BASE_CONFIG} keeps the Vault PKI "
                          "backend for that certificate type; the custom base config replaces the chart's generated "
                          f"config, so setup.sh's --set global.certificate.{chart_key}=true has no effect there; set it "
                          "to true")
        else:
            errors.append(f"[certificates] {toml_key} = {_toml_literal(value)} in the site TOML overrides the "
                          f"global.certificate.{chart_key}=true that setup.sh --skip-vault sets, so that certificate "
                          "type would use the Vault PKI backend; remove it or set it to true")
    return errors


def check_cert_manager_signer(effective, site, custom_base):
    """Errors and warnings for a [certificates.cert_manager] signer written in a TOML file.

    The generated base config takes the signer from the values (checked by check_issuer_refs), so only
    issuer_name and issuer_kind that a TOML file sets are judged here, against the effective values.
    """
    errors = []
    warnings = []
    cm_path = ("certificates", "cert_manager")
    signer = _mapping(_mapping(effective.get("certificates"), "[certificates]").get("cert_manager"),
                      "[certificates.cert_manager]")
    name_source = _source(site, custom_base, cm_path + ("issuer_name",))
    if name_source is not None:
        name = signer.get("issuer_name")
        if name == VAULT_ISSUER:
            errors.append(f'[certificates.cert_manager] issuer_name = "{VAULT_ISSUER}" in {name_source} overrides the '
                          f"chart's {SITE_ISSUER} with a ClusterIssuer that exists only when Vault is installed; set it "
                          f'to "{SITE_ISSUER}" or remove it')
        elif name != SITE_ISSUER:
            warnings.append(f"[certificates.cert_manager] issuer_name = {_toml_literal(name)} in {name_source} names a "
                            "custom signer; it must chain to the site-root CA that NICo, Scout, and the DPU agents trust")
    kind_source = _source(site, custom_base, cm_path + ("issuer_kind",))
    if kind_source is not None and signer.get("issuer_kind") != CLUSTER_ISSUER and signer.get("issuer_name") == SITE_ISSUER:
        errors.append(f"[certificates.cert_manager] issuer_kind = {_toml_literal(signer.get('issuer_kind'))} in "
                      f'{kind_source} with issuer_name "{SITE_ISSUER}": the bootstrap creates {SITE_ISSUER} as a '
                      f'{CLUSTER_ISSUER}; set issuer_kind = "{CLUSTER_ISSUER}" or remove it')
    return errors, warnings


def check_issuer_refs(values):
    """Errors for issuer references that name the Vault PKI issuer, and warnings for ones setup.sh overrides."""
    errors = []
    warnings = []
    bmc_proxy = _mapping(values.get("nico-bmc-proxy"), "nico-bmc-proxy")
    switch_tls = _mapping(bmc_proxy.get("switchTls"), "nico-bmc-proxy.switchTls")
    # The chart renders the switch client Certificate under `if .Values.switchTls.enabled` (default
    # false) from switchTls.issuerRef alone, whose chart default is vault-nico-issuer: there is no
    # fallback to global.certificate.issuerRef, so an absent name is the Vault issuer too.
    switch_tls_issuer_path = ("nico-bmc-proxy", "switchTls", "issuerRef")
    switch_tls_enabled = _helm_enabled(switch_tls, "enabled", False)
    # The only nested reference whose chart default already names site-issuer (used as the
    # [certificates.cert_manager] signer in the generated config); elsewhere an absent name
    # inherits the pinned global reference.
    machine_issuer_path = ("nico-api", "machineCertificates", "issuerRef")
    if switch_tls_enabled:
        switch_issuer = _mapping(switch_tls.get("issuerRef"), "nico-bmc-proxy.switchTls.issuerRef").get("name")
        if switch_issuer is None:
            errors.append(f'nico-bmc-proxy.switchTls.issuerRef.name is absent, so the chart default "{VAULT_ISSUER}" '
                          "applies, a ClusterIssuer that exists only when Vault is installed; set it to "
                          f'"{SITE_ISSUER}"')
        elif switch_issuer == VAULT_ISSUER:
            errors.append(f'nico-bmc-proxy.switchTls.issuerRef.name is "{VAULT_ISSUER}", a ClusterIssuer that exists '
                          f'only when Vault is installed; set it to "{SITE_ISSUER}"')
    for path, ref in issuer_refs(values):
        name = ref.get("name")
        label = f"{_dotted(path)}.name"
        if path == GLOBAL_ISSUER_REF:
            # The shipped values name vault-nico-issuer here, and the --set replaces it; only an unknown
            # issuer deserves a note, because the override may be unexpected.
            if name not in (None, VAULT_ISSUER, SITE_ISSUER):
                warnings.append(f"{label} is {_toml_literal(name)}; setup.sh --skip-vault overrides it with "
                                f"--set {label}={SITE_ISSUER}")
            continue
        if len(path) == 4 and path[1:] == GLOBAL_ISSUER_REF:
            if name is not None and name != SITE_ISSUER:
                warnings.append(f"{label} is {_toml_literal(name)}, but Helm gives the parent chart's global values "
                                f"precedence, so the --set global.certificate.issuerRef.name={SITE_ISSUER} from "
                                f"setup.sh --skip-vault applies to {path[0]} too; remove the subchart copy")
            continue
        # A nested reference the installer does not pin must not turn site-issuer into a
        # namespaced Issuer: the bootstrap creates it as a ClusterIssuer. An absent kind is
        # the chart default and fine.
        effective_name = name if name is not None or path != machine_issuer_path else SITE_ISSUER
        rendered = path != switch_tls_issuer_path or switch_tls_enabled
        if rendered and effective_name == SITE_ISSUER and "kind" in ref and ref["kind"] != CLUSTER_ISSUER:
            default_note = "" if name is not None else " (the chart default)"
            errors.append(f'{_dotted(path)}.kind is {_toml_literal(ref["kind"])} with name "{SITE_ISSUER}"'
                          f"{default_note}; the bootstrap creates {SITE_ISSUER} as a {CLUSTER_ISSUER}, so set kind "
                          f'to "{CLUSTER_ISSUER}"')
        if path == switch_tls_issuer_path:
            continue  # the name was judged above, and is ignored while switchTls is disabled
        if name != VAULT_ISSUER:
            continue
        errors.append(f'{label} is "{VAULT_ISSUER}", a ClusterIssuer that exists only when Vault is installed; '
                      f'set it to "{SITE_ISSUER}"')
    return errors, warnings


def check_overridden_values(values, nico_api):
    """Warnings for values the file sets explicitly and setup.sh --skip-vault overrides with --set."""
    warnings = []
    global_certificate = _mapping(_mapping(values.get("global"), "global").get("certificate"), "global.certificate")
    for key in ("useCertManager", "useCertManagerUfm"):
        if key in global_certificate and global_certificate[key] is not True:
            warnings.append(f"global.certificate.{key} is {_toml_literal(global_certificate[key])}; setup.sh "
                            f"--skip-vault overrides it with --set global.certificate.{key}=true")
    vault = _mapping(nico_api.get("vault"), "nico-api.vault")
    if "enabled" in vault and _helm_enabled(vault, "enabled", True):
        warnings.append("nico-api.vault.enabled is true; setup.sh --skip-vault overrides it with "
                        "--set nico-api.vault.enabled=false")
    return warnings


def check_vault_free(stream):
    """Check the Core values for anything that would make nico-api need Vault under --skip-vault.

    Return (errors, warnings). Errors name the field and what to change; warnings name values the
    installer's --set list overrides, so the override is visible, and custom signers.
    """
    values = yaml.safe_load(stream)
    if values is None:
        values = {}
    if not isinstance(values, dict):
        raise ValueError("Core values must be a YAML mapping")
    nico_api = _mapping(values.get("nico-api"), "nico-api")
    errors = []
    warnings = []
    try:
        site = site_toml(nico_api)
        base, custom = base_toml(nico_api)
    except ParserUnavailable as error:
        # The [secrets] section must be read in this mode, so an unreadable config is an error.
        errors.append(str(error))
    else:
        custom_base = base if custom else None
        effective = _deep_merge(base, site or {})
        errors.extend(check_secrets(effective, site, custom_base))
        errors.extend(check_certificates(effective, site, custom_base))
        signer_errors, signer_warnings = check_cert_manager_signer(effective, site, custom_base)
        errors.extend(signer_errors)
        warnings.extend(signer_warnings)
    issuer_errors, issuer_warnings = check_issuer_refs(values)
    errors.extend(issuer_errors)
    warnings.extend(issuer_warnings)
    if nico_api.get("vaultClusterInfo"):
        errors.append("nico-api.vaultClusterInfo is set, but setup.sh --skip-vault sets nico-api.vault.enabled=false "
                      "and the nico-api chart refuses to render a Vault ConfigMap without Vault; remove it")
    warnings.extend(check_overridden_values(values, nico_api))
    return errors, warnings


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: check-vault-free.py CORE_VALUES_FILE")
    try:
        with open(sys.argv[1], encoding="utf-8") as stream:
            errors, warnings = check_vault_free(stream)
        for error in errors:
            print(f"ERROR: {error}")
        for warning in warnings:
            print(f"WARNING: {warning}")
    except (OSError, ValueError, yaml.YAMLError) as error:
        raise SystemExit(f"Cannot check the Vault-free Core values: {error}") from error
