/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Readers for the register's derived decisions (`stated: false`): each reads the entry's
//! `source` (the setup command and environment, the TOML, the Helm values, the CP-DPU site
//! yaml) and says which option the files select. `config.derived.decisions` compares the
//! result with what the site definition pins or implies; later checklists read it to decide
//! what applies.

use crate::collaterals;
use crate::harness::Ctx;
use crate::setup_cmd::Resolved;

/// What the files say about one derived entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Derived {
    /// The options the files select: one for a single-select, any number for an "Any of",
    /// none when a `default: None` entry has nothing selected.
    Value(Vec<String>),
    /// The files do not tell these apart; the entry has to be pinned.
    Ambiguous(Vec<String>),
    /// A multi-select the files settle only in part: `certain` members they select,
    /// `candidates` they leave open; a pin has to hold the former and stay within both.
    Partial {
        certain: Vec<String>,
        candidates: Vec<String>,
    },
}

fn one(s: &str) -> Derived {
    Derived::Value(vec![s.to_string()])
}

fn none() -> Derived {
    Derived::Value(Vec::new())
}

fn ambiguous(options: &[&str]) -> Derived {
    Derived::Ambiguous(options.iter().map(|s| s.to_string()).collect())
}

/// The inputs every reader draws on, loaded once.
struct Sources<'a> {
    ctx: &'a Ctx<'a>,
    toml: toml::Table,
    core: collaterals::Values,
    prereqs: collaterals::Values,
    rest: collaterals::Values,
    cp_dpu: Option<collaterals::CpDpuSiteFile>,
    /// The command and environment as `setup.sh` resolves them.
    cmd: crate::setup_cmd::SetupCommand,
}

/// A `setup.env` entry as a reader sees it.
enum Env<'a> {
    Plain(&'a str),
    /// Named by source only; the value is not known here.
    Secret,
    Absent,
}

impl Sources<'_> {
    /// As `setup.sh` sees it: the assignments around the invocation, then `setup.env`.
    fn env(&self, name: &str) -> Env<'_> {
        match self.cmd.variable(name, &self.ctx.sd.setup) {
            // `setup.sh` treats an empty value as unset.
            Resolved::Plain("") | Resolved::Absent => Env::Absent,
            Resolved::Plain(s) => Env::Plain(s),
            Resolved::Secret => Env::Secret,
        }
    }

    fn env_set(&self, name: &str) -> bool {
        !matches!(self.env(name), Env::Absent)
    }

    fn toml_path(&self, path: &str) -> Option<&toml::Value> {
        let mut v = self.toml.get(path.split('.').next()?)?;
        for key in path.split('.').skip(1) {
            v = v.get(key)?;
        }
        Some(v)
    }

    fn toml_bool(&self, path: &str) -> Option<bool> {
        self.toml_path(path).and_then(toml::Value::as_bool)
    }

    fn toml_str(&self, path: &str) -> Option<&str> {
        self.toml_path(path).and_then(toml::Value::as_str)
    }

    /// Whether DPF is installed: from the command when it runs `setup.sh`; otherwise
    /// another tool deploys the site and only a pinned `dpf` decision says.
    fn installs_dpf(&self) -> Option<bool> {
        if self.cmd.runs_setup_sh() {
            return Some(self.cmd.installs_dpf());
        }
        self.ctx
            .decision("dpf")
            .map(|_| self.ctx.decided("dpf", "Install"))
    }

    /// Whether nv-rms is installed in the cluster: from the command when it runs
    /// `setup.sh`; otherwise from a pinned `rms` or `optional-components` decision.
    fn installs_rms(&self) -> Option<bool> {
        if self.cmd.runs_setup_sh() {
            return Some(self.cmd.installs_rms());
        }
        if self.ctx.decision("rms").is_some() {
            return Some(self.ctx.decided("rms", "In-cluster nv-rms"));
        }
        self.ctx
            .decision("optional-components")
            .map(|_| self.ctx.decided("optional-components", "RMS"))
    }

    /// `v` when the component is installed, `Not applicable` when it is not, and both as
    /// candidates when nothing says.
    fn when(installed: Option<bool>, v: Derived) -> Derived {
        match installed {
            Some(true) => v,
            Some(false) => one("Not applicable"),
            None => {
                let mut options = match v {
                    Derived::Value(v) | Derived::Ambiguous(v) => v,
                    Derived::Partial {
                        mut certain,
                        candidates,
                    } => {
                        certain.extend(candidates);
                        certain
                    }
                };
                options.push("Not applicable".to_string());
                Derived::Ambiguous(options)
            }
        }
    }

    fn if_dpf(&self, v: Derived) -> Derived {
        Self::when(self.installs_dpf(), v)
    }

    fn if_rms(&self, v: Derived) -> Derived {
        Self::when(self.installs_rms(), v)
    }

    /// A nico-api value in whichever layout the Core values file uses: under `nico-api`
    /// in the umbrella chart's file, at the root in the subchart's own.
    fn core_api(&self, path: &str) -> Option<&str> {
        if self.core.root.get("nico-api").is_some() {
            self.core.str(&format!("nico-api.{path}"))
        } else {
            self.core.str(path)
        }
    }

    fn on_off(&self, path: &str) -> Derived {
        if self.toml_bool(path) == Some(true) {
            one("On")
        } else {
            one("Off")
        }
    }
}

/// Every derived entry with what the files say, in register order.
pub(crate) fn derive_all(ctx: &Ctx) -> eyre::Result<Vec<(String, Derived)>> {
    let core_file = collaterals::core_values(ctx)?;
    let prereqs = collaterals::helm_prereqs_values(ctx)?;
    let src = Sources {
        ctx,
        toml: core_file.toml,
        core: collaterals::core_values_tree(ctx)?,
        // A REST file a site does not deploy (the group unticked, or skipped by the
        // command when nothing pins it) is not required to load: its readers then see
        // an empty tree and leave their entries open.
        rest: match collaterals::rest_values(ctx) {
            Ok(v) => v,
            Err(e) => {
                let cmd = crate::setup_cmd::SetupCommand::parse(&ctx.sd.setup);
                // The rule `config.helm-prereqs.rules` applies: the pin, else the command
                // when it runs `setup.sh`, else what the prerequisites enable.
                let rest_ticked = match ctx.decision("optional-components") {
                    Some(_) => ctx.decided("optional-components", "REST group"),
                    None if cmd.runs_setup_sh() => cmd.installs_rest(),
                    None => prereqs.bool("rest.enabled").unwrap_or(true),
                };
                if rest_ticked {
                    return Err(e);
                }
                collaterals::Values {
                    path: ctx.resolve(&ctx.sd.collaterals.rest_values),
                    root: serde_yaml::Value::Null,
                }
            }
        },
        prereqs,
        cp_dpu: collaterals::cp_dpu_site_yaml(ctx)?,
        cmd: crate::setup_cmd::SetupCommand::parse(&ctx.sd.setup),
    };
    Ok(READERS
        .iter()
        .map(|(id, read)| (id.to_string(), read(&src)))
        .collect())
}

type Reader = fn(&Sources) -> Derived;

/// The NGC registry; a provider registry named `nvcr.io.example.net` is not it.
const NGC_HOST: &str = "nvcr.io";

/// Where `helm-prereqs/keycloak/` deploys the bundled development Keycloak.
const BUNDLED_KEYCLOAK_HOSTS: [&str; 3] = [
    "keycloak.nico-rest",
    "keycloak.nico-rest.svc",
    "keycloak.nico-rest.svc.cluster.local",
];

/// The host of a registry reference (`host[:port][/path]`), lower-cased.
fn registry_host(reference: &str) -> String {
    let authority = reference.split('/').next().unwrap_or("");
    authority
        .rsplit_once(':')
        .map_or(authority, |(h, _)| h)
        .to_ascii_lowercase()
}

/// The host of a URL (`scheme://host[:port][/path]`), lower-cased.
fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    registry_host(rest)
}

/// One reader per `stated: false` entry of the register; a test keeps the two lists equal.
const READERS: &[(&str, Reader)] = &[
    ("source-of-the-nico-images", |s| {
        match s.env("NICO_IMAGE_REGISTRY") {
            Env::Plain(r) if registry_host(r) == NGC_HOST => one("Pulled from NGC"),
            Env::Plain(_) => one("Pulled from a local registry"),
            Env::Secret | Env::Absent => {
                ambiguous(&["Pulled from a local registry", "Pulled from NGC"])
            }
        }
    }),
    ("registry-host", |s| match s.env("NICO_IMAGE_REGISTRY") {
        Env::Plain(r) if registry_host(r) == NGC_HOST => one("None"),
        Env::Plain(_) => one("Provider-run"),
        Env::Secret | Env::Absent => ambiguous(&["Provider-run", "None"]),
    }),
    ("registry-authentication", |s| {
        if s.env_set("REGISTRY_PULL_SECRET") {
            one("Credentialed pulls")
        } else {
            one("No secret from setup")
        }
    }),
    ("smn", |s| {
        if s.ctx.decided("dpu-in-the-site-controllers", "no DPU") {
            return one("SMN on the ToR");
        }
        match &s.cp_dpu {
            Some(cp) if cp.site.fnn.is_some() => one("SMN on the control-plane DPU"),
            Some(_) => ambiguous(&["SMN on the ToR", "No SMN"]),
            None => ambiguous(&["SMN on the control-plane DPU", "SMN on the ToR", "No SMN"]),
        }
    }),
    ("dpf", |s| {
        if !s.cmd.runs_setup_sh() {
            return ambiguous(&["Install", "Skip"]);
        }
        if s.cmd.installs_dpf() {
            one("Install")
        } else {
            one("Skip")
        }
    }),
    ("managed-host-dpu-generation", |s| {
        let fleet = s.ctx.sd.fleet.as_ref().and_then(|f| f.dpu.as_ref());
        // Without DPF the iPXE path still installs BF3, and only BF3: the fleet section
        // says whether such DPUs exist; a BF4 fleet has nothing NICo manages that way.
        if s.installs_dpf() == Some(false) {
            return match fleet {
                Some(d) if d.generation.eq_ignore_ascii_case("BF3") => one("BF3"),
                Some(_) => one("Not applicable"),
                None => ambiguous(&["BF3", "Not applicable"]),
            };
        }
        let deployments = s
            .toml_path("dpf.deployments")
            .and_then(toml::Value::as_table);
        // A BF4 deployment's key names its variant (`bf4_astra`, `bf4_generic`).
        let mut found: Vec<&str> = Vec::new();
        for (key, d) in deployments.into_iter().flatten() {
            if d.get("bfb_url").is_some() {
                found.push("BF3");
            }
            if d.get("bluefield_software").is_some() {
                found.push(if key.to_ascii_lowercase().contains("astra") {
                    "BF4 Astra"
                } else {
                    "BF4 generic"
                });
            }
        }
        found.sort_unstable();
        found.dedup();
        // Core keeps a default `[dpf.deployments.bf3]` when the block is not written: with
        // other deployments present, BF3 is deployed too, whether or not the fleet has
        // BF3 DPUs, so it stays a candidate beside what the TOML names.
        let default_bf3 = deployments.is_some() && !found.is_empty() && !found.contains(&"BF3");
        // The TOML decides when it has deployments; else the fleet section of the site
        // definition names the generation NICo will manage.
        s.if_dpf(match found.as_slice() {
            [one_kind] if default_bf3 => Derived::Partial {
                certain: vec![(*one_kind).to_string()],
                candidates: vec!["BF3".to_string()],
            },
            [one_kind] => one(one_kind),
            [_, ..] if default_bf3 => Derived::Partial {
                certain: found.iter().map(|k| k.to_string()).collect(),
                candidates: vec!["BF3".to_string()],
            },
            // The entry is a multi-select: every deployment's generation is managed.
            [_, ..] => Derived::Value(found.iter().map(|k| k.to_string()).collect()),
            [] => match fleet {
                Some(d) if d.generation.eq_ignore_ascii_case("BF3") => one("BF3"),
                Some(d) if d.generation.to_ascii_uppercase().starts_with("BF4") => {
                    if d.model.to_ascii_lowercase().contains("astra") {
                        one("BF4 Astra")
                    } else {
                        ambiguous(&["BF4 generic", "BF4 Astra"])
                    }
                }
                _ => ambiguous(&["BF3", "BF4 generic", "BF4 Astra"]),
            },
        })
    }),
    ("custom-default-storageclass", |s| {
        match s.env("NICO_MANAGE_DEFAULT_STORAGE_CLASS") {
            Env::Plain("false") => one("Yes"),
            Env::Secret => ambiguous(&["Yes", "No"]),
            _ => one("No"),
        }
    }),
    ("custom-storage-classes-for-components", |s| {
        // Both inputs resolve to the published default when unset; only another value is
        // a custom class.
        const DEFAULT_CLASS: &str = "local-path-persistent";
        let pg_custom = s
            .prereqs
            .str("postgresql.storageClass")
            .is_some_and(|c| c != DEFAULT_CLASS);
        match s.env("NICO_STORAGE_CLASS") {
            Env::Plain(c) if c != DEFAULT_CLASS => one("Yes"),
            Env::Secret if !pg_custom => ambiguous(&["Yes", "No"]),
            _ if pg_custom => one("Yes"),
            _ => one("No"),
        }
    }),
    ("evpn-overlay-peering", |s| {
        if s.toml_bool("enable_route_servers") == Some(true) {
            one("Option 2")
        } else {
            one("Option 1")
        }
    }),
    ("recursive-resolver-for-managed-machines", |s| {
        if s.core.bool("unbound.externalService.enabled") == Some(true) {
            one("Built-in unbound")
        } else {
            one("External DNS")
        }
    }),
    ("pxe-and-http-boot-url", |s| {
        let mut v = Vec::new();
        if [
            "x86_pxe_boot_url_override",
            "arm_pxe_boot_url_override",
            "pxe_public_base_url",
        ]
        .iter()
        .any(|k| s.toml_str(k).is_some_and(|u| !u.is_empty()))
        {
            v.push("Custom PXE URL".to_string());
        }
        if s.toml_path("set_http_boot_uri_for_vendors")
            .and_then(toml::Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            v.push("BMC-pinned HTTP boot URL for some vendors".to_string());
        }
        Derived::Value(v)
    }),
    ("credential-store", |s| {
        // The writer selects the store; its documented default is vault whatever the read
        // backends list says.
        let writer = s.toml_str("secrets.writer").map(str::to_lowercase);
        match (s.toml_path("secrets").is_some(), writer.as_deref()) {
            (false, _) => ambiguous(&["Vault KV", "Postgres envelope-encrypted"]),
            (true, Some("postgres")) => one("Postgres envelope-encrypted"),
            (true, Some("vault") | None) => one("Vault KV"),
            (true, Some(_)) => ambiguous(&["Vault KV", "Postgres envelope-encrypted"]),
        }
    }),
    ("key-wrapping-for-the-postgres-store", |s| {
        // The writer alone selects the store, as `credential-store` reads it.
        let postgres_store = s
            .toml_str("secrets.writer")
            .map(str::to_lowercase)
            .as_deref()
            == Some("postgres");
        if s.toml_path("secrets").is_some() && !postgres_store {
            return one("Not applicable");
        }
        let Some(active) = s.toml_str("secrets.kms.active") else {
            // No `[secrets]` table: the store is whatever `credential-store` resolves to,
            // so every option, `Not applicable` included, stays a candidate.
            return if s.toml_path("secrets").is_some() {
                none()
            } else {
                ambiguous(&["Integrated", "Transit", "KMIP", "Not applicable"])
            };
        };
        // The provider table is looked up by its exact key: a dotted name is a valid key.
        match s
            .toml_path("secrets.kms.providers")
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get(active))
            .and_then(|p| p.get("type"))
            .and_then(toml::Value::as_str)
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("integrated") => one("Integrated"),
            Some("transit") => one("Transit"),
            Some("kmip") => one("KMIP"),
            _ => ambiguous(&["Integrated", "Transit", "KMIP"]),
        }
    }),
    ("certificate-issuance", |s| {
        let mut v = Vec::new();
        if s.toml_bool("certificates.use_cert_manager") == Some(true) {
            v.push("cert-manager for machine certificates".to_string());
        }
        if s.toml_bool("certificates.use_cert_manager_ufm") == Some(true) {
            v.push("cert-manager for UFM certificates".to_string());
        }
        Derived::Value(v)
    }),
    ("root-ca", |s| {
        if s.prereqs.bool("certManager.siteRoot.create") == Some(false) {
            one("Supplied CA")
        } else {
            one("Self-signed by setup.sh")
        }
    }),
    ("admin-client-ca", |s| {
        let pem = s
            .core_api("siteConfig.adminRootCertPem")
            .is_some_and(|p| !p.trim().is_empty());
        if pem || s.toml_str("tls.admin_root_cafile_path").is_some() {
            one("Yes")
        } else {
            one("No")
        }
    }),
    ("autonomous-bmc-and-uefi-rotation", |s| {
        let mut v = Vec::new();
        if s.toml_bool("bmc_rotation_enabled") == Some(true) {
            v.push("BMC rotation".to_string());
        }
        if s.toml_bool("uefi_rotation_enabled") == Some(true) {
            v.push("UEFI rotation".to_string());
        }
        Derived::Value(v)
    }),
    ("dpu-bootstrap-ca-non-dpf-path", |s| {
        // With DPF installed a host can still take the iPXE path (`dpf_enabled: false`);
        // the `[dpu_config]` table then configures it, with Core's default `legacy_download`
        // when the key is omitted. Absent that table, the path is unused.
        match (
            s.toml_str("dpu_config.bootstrap_ca_source"),
            s.toml_path("dpu_config").is_some(),
            s.installs_dpf(),
        ) {
            (Some(v), _, _) => one(v),
            (None, true, _) | (None, false, Some(false)) => one("legacy_download"),
            (None, false, Some(true)) => one("Not applicable"),
            (None, false, None) => ambiguous(&["Not applicable", "legacy_download"]),
        }
    }),
    ("dpu-agent-bootstrap-ca-dpf-path", |s| {
        s.if_dpf(match s.toml_str("dpf.dpu_agent_bootstrap_ca.source") {
            Some(v) => one(v),
            None => one("legacy_download"),
        })
    }),
    ("dpu-os-login-password-dpf", |s| {
        // A login password is a `<user>_PASSWORD=` bf.cfg parameter; other parameters in
        // the same lists are not one.
        let sets_password = |v: &toml::Value| {
            v.as_array().is_some_and(|a| {
                a.iter()
                    .filter_map(toml::Value::as_str)
                    .any(|p| p.to_ascii_uppercase().contains("_PASSWORD="))
            })
        };
        let in_deployments = s
            .toml_path("dpf.deployments")
            .and_then(toml::Value::as_table)
            .is_some_and(|t| {
                t.values()
                    .filter_map(|d| d.get("extra_bfcfg_parameters"))
                    .any(sets_password)
            });
        s.if_dpf(
            if s.toml_path("dpf.extra_bfcfg_parameters")
                .is_some_and(sets_password)
                || in_deployments
            {
                one("Yes")
            } else {
                one("No")
            },
        )
    }),
    ("machine-identity-spiffe-jwt-svid", |s| {
        // The documented default is disabled.
        if s.toml_bool("machine_identity.enabled") == Some(true) {
            one("Enabled")
        } else {
            one("Disabled")
        }
    }),
    ("rest-api-auth", |s| {
        match (
            s.rest.bool("nico-rest-api.config.keycloak.enabled"),
            s.rest.str("nico-rest-api.config.keycloak.baseURL"),
        ) {
            (Some(false), _) => one("Generic OIDC issuers"),
            (_, Some(url)) if BUNDLED_KEYCLOAK_HOSTS.contains(&url_host(url).as_str()) => {
                one("Bundled dev Keycloak")
            }
            (_, Some(_)) => one("Your own Keycloak"),
            // No Keycloak block: the chart's defaults decide, which the file does not show.
            (_, None) => ambiguous(&["Bundled dev Keycloak", "Your own Keycloak"]),
        }
    }),
    ("optional-components", |s| {
        // The command selects five of the components; when it does not run `setup.sh`
        // they stay candidates and the TOML-selected ones are the only certain members.
        let from_command = s.cmd.runs_setup_sh();
        let mut v = Vec::new();
        let mut candidates = Vec::new();
        for (on, name) in [
            (s.cmd.installs_rest(), "REST group"),
            (s.cmd.installs_rms(), "RMS"),
            (s.cmd.with_observability(), "Observability stack"),
            (s.cmd.installs_contour(), "Contour ingress"),
        ] {
            if !from_command {
                candidates.push(name);
            } else if on {
                v.push(name);
            }
        }
        if s.toml_bool("ib_config.enabled") == Some(true) {
            v.push("UFM");
        }
        if s.toml_bool("nvlink_config.enabled") == Some(true) {
            v.push("NVLink partitioning");
        }
        if s.toml_bool("tracing.enabled") == Some(true) {
            v.push("Tracing");
        }
        if !from_command {
            candidates.push("Site overlay");
        } else if s.cmd.site_overlay.is_some() {
            v.push("Site overlay");
        }
        let names = |v: Vec<&str>| v.into_iter().map(str::to_string).collect();
        if from_command {
            Derived::Value(names(v))
        } else {
            Derived::Partial {
                certain: names(v),
                candidates: names(candidates),
            }
        }
    }),
    ("rms", |s| match s.installs_rms() {
        Some(true) => one("In-cluster nv-rms"),
        Some(false) => {
            if s.core_api("rms.apiUrl").is_some_and(|u| !u.is_empty()) {
                one("External RMS")
            } else {
                one("Not applicable")
            }
        }
        None => ambiguous(&["In-cluster nv-rms", "External RMS", "Not applicable"]),
    }),
    ("rms-chart-source", |s| {
        s.if_rms(if s.env_set("NICO_RMS_CHART") {
            one("Local chart path")
        } else {
            one("Pinned submodule")
        })
    }),
    ("core-postgres", |s| {
        if s.prereqs.bool("postgresql.enabled") == Some(false) {
            one("External Postgres")
        } else if s.prereqs.u64("postgresql.instances") == Some(1) {
            one("Single instance for development")
        } else {
            one("Zalando HA in-cluster")
        }
    }),
    ("rack-profiles", |s| {
        s.if_rms(if s.toml_path("rack_profiles").is_some() {
            one("Site-specific profile")
        } else {
            one("Built-in profiles")
        })
    }),
    ("observability", |s| {
        // Without the local stack, `NICO_SERVICEMONITORS` decides; a secret value leaves
        // both of those options open.
        let without: &[&str] = match s.env("NICO_SERVICEMONITORS") {
            Env::Plain("true") => &["External stack"],
            Env::Secret => &["External stack", "No pipeline"],
            _ => &["No pipeline"],
        };
        if !s.cmd.runs_setup_sh() {
            let mut all = vec!["Full local stack"];
            all.extend(without);
            ambiguous(&all)
        } else if s.cmd.with_observability() {
            one("Full local stack")
        } else if let [only] = without {
            one(only)
        } else {
            ambiguous(without)
        }
    }),
    ("measured-boot-attestation", |s| {
        s.on_off("attestation_enabled")
    }),
    ("spdm-and-nras", |s| s.on_off("spdm.enabled")),
    ("firmware-automation", |s| {
        s.on_off("firmware_global.autoupdate")
    }),
    ("provider-internal-tenant", |s| {
        let profiles = s
            .toml_path("fnn.routing_profiles")
            .and_then(toml::Value::as_table);
        let internal = profiles.is_some_and(|t| {
            t.iter().any(|(name, p)| {
                name != "MAINTENANCE"
                    && p.get("internal").and_then(toml::Value::as_bool) == Some(true)
                    && p.get("access_tier").is_some()
            })
        });
        let repair = profiles.is_some_and(|t| t.contains_key("MAINTENANCE"));
        match (internal, repair) {
            (true, true) => ambiguous(&[
                "Internal tenant",
                "a repair tenant on the MAINTENANCE profile",
            ]),
            (true, false) => one("Internal tenant"),
            (false, true) => one("a repair tenant on the MAINTENANCE profile"),
            (false, false) => none(),
        }
    }),
    ("break-fix-and-repair-workflow", |s| {
        if s.toml_bool("auto_machine_repair_plugin.enabled") == Some(true) {
            one("Auto-repair")
        } else {
            one("Outside NICo")
        }
    }),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::Register;

    /// Every `stated: false` entry has a reader, every reader names a register entry, and
    /// every option a reader can return exists on that entry.
    #[test]
    fn readers_cover_the_derived_register() {
        let reg = Register::builtin();
        let derived: Vec<&str> = reg
            .decisions
            .iter()
            .filter(|d| !d.stated)
            .map(|d| d.id.as_str())
            .collect();
        let readers: Vec<&str> = READERS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            derived, readers,
            "readers must follow the register, in order"
        );
    }

    #[test]
    fn the_example_derives_without_contradiction() {
        let ex = crate::fixture::Example::new();
        let values = ex.derive();
        let reg = Register::builtin();
        for (id, d) in &values {
            let entry = reg.get(id).unwrap();
            let names: Vec<&String> = match d {
                Derived::Value(v) | Derived::Ambiguous(v) => v.iter().collect(),
                Derived::Partial {
                    certain,
                    candidates,
                } => certain.iter().chain(candidates).collect(),
            };
            for n in names {
                assert!(
                    entry.options.iter().any(|o| &o.name == n),
                    "{id}: reader returned `{n}`, not an option of the register entry"
                );
            }
        }
        let get = |id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d)
                .unwrap()
        };
        assert_eq!(get("dpf"), &one("Install"));
        assert_eq!(
            get("optional-components"),
            &Derived::Value(vec!["REST group".to_string()])
        );
        assert_eq!(get("credential-store"), &one("Vault KV"));
        assert_eq!(get("smn"), &one("SMN on the control-plane DPU"));
        assert_eq!(get("core-postgres"), &one("Zalando HA in-cluster"));
    }

    /// A command that does not run `setup.sh` (another tool deploys the site) says nothing
    /// about what is installed: the command-read entries become candidates, and the entries
    /// that depend on them follow the pinned decision instead.
    #[test]
    fn a_command_that_does_not_run_setup_sh_defers_to_the_pins() {
        const COMMAND: &str = "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms";
        let get = |values: &[(String, Derived)], id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d.clone())
                .unwrap()
        };
        let baseline = crate::fixture::Example::new().derive();
        let ex = crate::fixture::Example::new();
        ex.splice(
            "site-def.yaml",
            COMMAND,
            "command: python3 tools/deploy.py --site dc1",
        );
        let unpinned = ex.derive();
        assert_eq!(get(&unpinned, "dpf"), ambiguous(&["Install", "Skip"]));
        assert!(
            matches!(get(&unpinned, "optional-components"), Derived::Partial { candidates, .. } if candidates.contains(&"REST group".to_string())),
            "{:?}",
            get(&unpinned, "optional-components")
        );
        assert!(
            matches!(get(&unpinned, "managed-host-dpu-generation"), Derived::Ambiguous(v) if v.contains(&"Not applicable".to_string())),
            "{:?}",
            get(&unpinned, "managed-host-dpu-generation")
        );
        ex.splice("site-def.yaml", "  # dpf: Install ", "  dpf: Install ");
        assert_eq!(
            get(&ex.derive(), "managed-host-dpu-generation"),
            get(&baseline, "managed-host-dpu-generation")
        );
        // Without DPF the iPXE path still installs BF3, which the example's fleet has.
        ex.splice("site-def.yaml", "  dpf: Install ", "  dpf: Skip ");
        assert_eq!(get(&ex.derive(), "managed-host-dpu-generation"), one("BF3"));
    }

    /// Core keeps its default BF3 deployment beside a written BF4 one: BF4 is selected and
    /// BF3 stays a candidate for the pin to confirm or leave out.
    #[test]
    fn a_bf4_only_toml_leaves_the_default_bf3_a_candidate() {
        let get = |values: &[(String, Derived)], id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| panic!("{id} derived"))
        };
        let ex = crate::fixture::Example::new();
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [secrets]\n",
            "      [dpf.deployments.bf4_generic]\n      bluefield_software = \"x\"\n\n      [secrets]\n",
        );
        let values = ex.derive();
        assert_eq!(
            get(&values, "managed-host-dpu-generation"),
            Derived::Partial {
                certain: vec!["BF4 generic".to_string()],
                candidates: vec!["BF3".to_string()],
            }
        );
    }

    /// With another deployer and nothing pinning the REST group, the prerequisites say
    /// whether REST applies; a REST file that does not parse is then not required.
    #[test]
    fn another_deployer_with_rest_disabled_does_not_need_the_rest_file() {
        let get = |values: &[(String, Derived)], id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| panic!("{id} derived"))
        };
        let ex = crate::fixture::Example::new();
        ex.splice(
            "site-def.yaml",
            "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
            "command: python3 tools/deploy-site.py --site site1",
        );
        ex.splice(
            "site-def.yaml",
            "  optional-components: [REST group]",
            "  # optional-components left to the files",
        );
        ex.splice(
            "helm-prereqs/values.yaml",
            "rest:\n  enabled: true\n",
            "rest:\n  enabled: false\n",
        );
        std::fs::write(ex.path("helm-prereqs/values/nico-rest.yaml"), "{{ not yaml")
            .expect("the copy is writable");
        assert_eq!(get(&ex.derive(), "dpf"), ambiguous(&["Install", "Skip"]));
    }

    /// Deployments of two generations select both; a pin has to name both.
    #[test]
    fn deployments_of_two_generations_select_both() {
        let ex = crate::fixture::Example::new();
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [secrets]\n",
            "      [dpf.deployments.bf3]\n      bfb_url = \"http://x/bf3.bfb\"\n\n      [dpf.deployments.bf4_generic]\n      bluefield_software = \"x\"\n\n      [secrets]\n",
        );
        let values = ex.derive();
        let (_, d) = values
            .iter()
            .find(|(i, _)| i == "managed-host-dpu-generation")
            .unwrap();
        assert_eq!(
            d,
            &Derived::Value(vec!["BF3".to_string(), "BF4 generic".to_string()])
        );
    }

    /// A `[dpu_config]` table without `bootstrap_ca_source` takes Core's default, with DPF
    /// installed or not.
    #[test]
    fn a_dpu_config_table_without_the_key_is_the_default_source() {
        let ex = crate::fixture::Example::new();
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [secrets]\n",
            "      [dpu_config]\n      bootstrap_retries = 3\n\n      [secrets]\n",
        );
        let values = ex.derive();
        let (_, d) = values
            .iter()
            .find(|(i, _)| i == "dpu-bootstrap-ca-non-dpf-path")
            .unwrap();
        assert_eq!(d, &one("legacy_download"));
    }

    /// A secret-sourced control is set at run time: the reader keeps both outcomes open
    /// instead of reading it as absent.
    #[test]
    fn a_secret_sourced_control_leaves_its_entry_ambiguous() {
        let ex = crate::fixture::Example::new();
        ex.splice(
            "site-def.yaml",
            "    REGISTRY_PULL_SECRET: { from: env }",
            "    REGISTRY_PULL_SECRET: { from: env }\n    NICO_SERVICEMONITORS: { from: env }",
        );
        let values = ex.derive();
        let (_, d) = values.iter().find(|(i, _)| i == "observability").unwrap();
        assert_eq!(d, &ambiguous(&["External stack", "No pipeline"]));
    }
}
