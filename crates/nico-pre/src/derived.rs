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
    /// The files say something neither option describes: a mistake to correct, whatever
    /// the pin says.
    Invalid(String),
    /// A component whose installation the files do not settle: `installed` is what they
    /// say when it is installed, `otherwise` what they say when it is not. A pin that
    /// agrees with either is consistent.
    IfInstalled {
        installed: Box<Derived>,
        otherwise: Box<Derived>,
    },
}

impl Derived {
    /// Every option the value mentions, selected or open.
    #[cfg(test)]
    pub(crate) fn options(&self) -> Vec<&String> {
        match self {
            Derived::Value(v) | Derived::Ambiguous(v) => v.iter().collect(),
            Derived::Partial {
                certain,
                candidates,
            } => certain.iter().chain(candidates).collect(),
            Derived::IfInstalled {
                installed,
                otherwise,
            } => {
                let mut v = installed.options();
                v.extend(otherwise.options());
                v
            }
            Derived::Invalid(_) => Vec::new(),
        }
    }

    /// The options as a reader of the check reads them.
    pub(crate) fn text(&self) -> String {
        let list = |v: &[String], sep: &str| {
            if v.is_empty() {
                "none".to_string()
            } else {
                v.join(sep)
            }
        };
        match self {
            Derived::Value(v) => list(v, " + "),
            Derived::Ambiguous(v) => list(v, " | "),
            Derived::Partial {
                certain,
                candidates,
            } => format!(
                "{}, and perhaps {}",
                list(certain, " + "),
                list(candidates, " | ")
            ),
            Derived::IfInstalled {
                installed,
                otherwise,
            } => format!(
                "{} if installed, else {}",
                installed.text(),
                otherwise.text()
            ),
            Derived::Invalid(reason) => format!("invalid: {reason}"),
        }
    }
}

/// The component-manager backend fields and the rack-capability role each one resolves
/// vendors from.
pub(crate) const COMPONENT_MANAGER_ROLES: [(&str, &str); 3] = [
    ("compute_tray_backend", "compute"),
    ("nv_switch_backend", "switch"),
    ("power_shelf_backend", "power_shelf"),
];

fn one(s: &str) -> Derived {
    Derived::Value(vec![s.to_string()])
}

fn none() -> Derived {
    Derived::Value(Vec::new())
}

/// A stated value Core refuses: a mistake to correct, whatever the pin says.
fn invalid_value(key: &str, v: &str) -> Derived {
    Derived::Invalid(format!("{key} = \"{v}\" is not a value Core accepts"))
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
    /// Named by source only; present at run time, the value not known here.
    Secret,
    /// A shell expression; neither value nor presence known here.
    Unreadable,
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
            Resolved::Expression => Env::Unreadable,
        }
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
    /// `setup.sh`; otherwise from a pinned `rms` decision, or an `optional-components` pin
    /// that leaves RMS out. One ticking RMS says it is in use, not where: another deployer
    /// may point Core at an external RMS, so the location stays open.
    fn installs_rms(&self) -> Option<bool> {
        if self.cmd.runs_setup_sh() {
            return Some(self.cmd.installs_rms());
        }
        if self.ctx.decision("rms").is_some() {
            return Some(self.ctx.decided("rms", "In-cluster nv-rms"));
        }
        self.ctx
            .decision("optional-components")
            .and_then(|_| (!self.ctx.decided("optional-components", "RMS")).then_some(false))
    }

    /// `v` when the component is installed, `Not applicable` when it is not, and the two
    /// candidates when nothing says.
    fn when(installed: Option<bool>, v: Derived) -> Derived {
        Self::when_else(installed, v, one("Not applicable"))
    }

    /// `v` when the component is installed, `otherwise` when it is not, and the two held
    /// as alternatives when nothing says.
    fn when_else(installed: Option<bool>, v: Derived, otherwise: Derived) -> Derived {
        match installed {
            Some(true) => v,
            Some(false) => otherwise,
            None if v == otherwise => v,
            None => Derived::IfInstalled {
                installed: Box::new(v),
                otherwise: Box::new(otherwise),
            },
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
    let src = sources(ctx, true, true)?;
    Ok(READERS
        .iter()
        .map(|(id, read)| (id.to_string(), read(&src)))
        .collect())
}

/// One entry, read from the files it needs: the CP-DPU yaml is loaded for `smn` alone, so
/// a check reading another entry does not stop on that file.
pub(crate) fn derive_one(ctx: &Ctx, id: &str) -> eyre::Result<Option<Derived>> {
    let Some((_, read)) = READERS.iter().find(|(i, _)| *i == id) else {
        return Ok(None);
    };
    let src = sources(ctx, id == "smn", id == "rest-api-auth")?;
    Ok(Some(read(&src)))
}

/// The inputs the readers draw on, loaded once; the CP-DPU yaml and the REST values only
/// when asked for (`rest-api-auth` alone reads the latter).
fn sources<'a>(ctx: &'a Ctx<'a>, with_cp_dpu: bool, with_rest: bool) -> eyre::Result<Sources<'a>> {
    let core_file = collaterals::core_values(ctx)?;
    let prereqs = collaterals::helm_prereqs_values(ctx)?;
    let src = Sources {
        ctx,
        // The TOML and the tree from one read of the file, so they cannot disagree.
        toml: core_file.toml,
        core: core_file.tree,
        // A REST file a site does not deploy (the group unticked, or skipped by the
        // command when nothing pins it) is not required to load: its readers then see
        // an empty tree and leave their entries open.
        rest: if !with_rest {
            collaterals::Values {
                path: ctx.resolve(&ctx.sd.collaterals.rest_values),
                root: serde_yaml::Value::Null,
            }
        } else {
            // The rule `config.helm-prereqs.rules` applies: the pin, else the command when
            // it runs `setup.sh`, else what the prerequisites enable. A REST file the site
            // does not deploy is neither required to load nor read: its readers see an
            // empty tree and leave their entries open.
            let cmd = crate::setup_cmd::SetupCommand::parse(&ctx.sd.setup);
            let rest_ticked = match ctx.decision("optional-components") {
                Some(_) => ctx.decided("optional-components", "REST group"),
                None if cmd.runs_setup_sh() => cmd.installs_rest(),
                None => prereqs.bool("rest.enabled").unwrap_or(true),
            };
            if rest_ticked {
                collaterals::rest_values(ctx)?
            } else {
                collaterals::Values {
                    path: ctx.resolve(&ctx.sd.collaterals.rest_values),
                    root: serde_yaml::Value::Null,
                }
            }
        },
        prereqs,
        // Read only when the site controllers carry a DPU: the `smn` reader is its one
        // consumer, and a plain-NIC site's stale yaml must not stop the other readers.
        cp_dpu: if with_cp_dpu {
            collaterals::cp_dpu_site_yaml_if_dpu(ctx)?
        } else {
            None
        },
        cmd: crate::setup_cmd::SetupCommand::parse(&ctx.sd.setup),
    };
    Ok(src)
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
            Env::Plain(_) => one("Pulled from a provider-run registry"),
            Env::Secret | Env::Unreadable | Env::Absent => {
                ambiguous(&["Pulled from a provider-run registry", "Pulled from NGC"])
            }
        }
    }),
    ("provider-run-registry-host", |s| {
        match s.env("NICO_IMAGE_REGISTRY") {
            Env::Plain(r) if registry_host(r) == NGC_HOST => one("Not applicable"),
            Env::Plain(_) => one("Provider-run"),
            Env::Secret | Env::Unreadable | Env::Absent => {
                ambiguous(&["Provider-run", "Not applicable"])
            }
        }
    }),
    ("registry-authentication", |s| {
        match s.env("REGISTRY_PULL_SECRET") {
            // A `{from: …}` source is a secret the provider supplies at run time.
            Env::Plain(_) | Env::Secret => one("Credentialed pulls"),
            Env::Absent => one("No secret from setup"),
            // A shell expression may be empty at run time, when setup.sh creates no secret.
            Env::Unreadable => ambiguous(&["Credentialed pulls", "No secret from setup"]),
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
        let without_dpf = match fleet {
            Some(d) if d.generation.eq_ignore_ascii_case("BF3") => one("BF3"),
            Some(_) => one("Not applicable"),
            None => ambiguous(&["BF3", "Not applicable"]),
        };
        if s.installs_dpf() == Some(false) {
            return without_dpf;
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
        // Core keeps its default `[dpf.deployments.bf3]` beside the written deployments
        // while none of them states a `bfb_url`: BF3 is then deployed and managed too,
        // whether or not the fleet has BF3 DPUs, so it is certain, not a candidate.
        let default_bf3 = deployments
            .is_some_and(|t| !t.is_empty() && !t.values().any(|d| d.get("bfb_url").is_some()));
        if default_bf3 && !found.contains(&"BF3") {
            found.push("BF3");
            found.sort_unstable();
        }
        // The TOML decides when it has deployments; else the fleet section of the site
        // definition names the generation NICo will manage.
        let with_dpf = match found.as_slice() {
            [one_kind] => one(one_kind),
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
        };
        Sources::when_else(s.installs_dpf(), with_dpf, without_dpf)
    }),
    ("custom-default-storageclass", |s| {
        match s.env("NICO_MANAGE_DEFAULT_STORAGE_CLASS") {
            Env::Plain("false") => one("Yes"),
            Env::Secret | Env::Unreadable => ambiguous(&["Yes", "No"]),
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
            Env::Secret | Env::Unreadable if !pg_custom => ambiguous(&["Yes", "No"]),
            _ if pg_custom => one("Yes"),
            _ => one("No"),
        }
    }),
    ("evpn-overlay-peering", |s| {
        if s.toml_bool("enable_route_servers") == Some(true) {
            one("Route servers")
        } else {
            one("ToR peering")
        }
    }),
    ("recursive-resolver-for-managed-machines", |s| {
        // The umbrella chart deploys the subchart only with `unbound.enabled: true`, and
        // its Service only with the Service's own flag: both for the built-in resolver.
        if s.core.bool("unbound.enabled") == Some(true)
            && s.core.bool("unbound.externalService.enabled") == Some(true)
        {
            one("Built-in unbound")
        } else {
            one("External DNS")
        }
    }),
    ("custom-pxe-and-http-boot-url", |s| {
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
        // No `[secrets]` table: Core reads env -> file -> vault and writes to vault.
        match (s.toml_path("secrets").is_some(), writer.as_deref()) {
            (false, _) => one("Vault KV"),
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
            // No `[secrets]` table: the store is Vault, as `credential-store` reads it, so
            // no key wrapping applies; a table with a Postgres writer and no `kms.active`
            // names no provider.
            return if s.toml_path("secrets").is_some() {
                none()
            } else {
                one("Not applicable")
            };
        };
        // Every configured provider's type is in use (a rotation keeps the old provider for
        // existing entries), so the value is the set of them; the provider table is looked
        // up by its exact key, a dotted name being a valid key. An `active` naming no
        // provider is a broken reference no pin can satisfy, so nothing is selected.
        let providers = s
            .toml_path("secrets.kms.providers")
            .and_then(toml::Value::as_table);
        if !providers.is_some_and(|t| t.contains_key(active)) {
            return none();
        }
        let name = |t: &str| match t {
            "integrated" => Some("Integrated"),
            "transit" => Some("Transit"),
            "kmip" => Some("KMIP"),
            _ => None,
        };
        let mut types: Vec<String> = Vec::new();
        let mut unknown = false;
        for p in providers.into_iter().flat_map(toml::Table::values) {
            match p
                .get("type")
                .and_then(toml::Value::as_str)
                .map(str::to_lowercase)
                .as_deref()
                .and_then(name)
            {
                Some(n) if !types.iter().any(|t| t == n) => types.push(n.to_string()),
                Some(_) => {}
                None => unknown = true,
            }
        }
        if unknown || types.is_empty() {
            ambiguous(&["Integrated", "Transit", "KMIP"])
        } else {
            Derived::Value(types)
        }
    }),
    ("certificate-issuance-overrides", |s| {
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
    ("custom-admin-client-ca", |s| {
        let pem = s
            .core_api("siteConfig.adminRootCertPem")
            .is_some_and(|p| !p.trim().is_empty());
        if pem || s.toml_str("tls.admin_root_cafile_path").is_some() {
            one("Yes")
        } else {
            one("No")
        }
    }),
    ("autonomous-bmc-and-uefi-credential-rotation", |s| {
        let mut v = Vec::new();
        if s.toml_bool("bmc_rotation_enabled") == Some(true) {
            v.push("BMC rotation".to_string());
        }
        if s.toml_bool("uefi_rotation_enabled") == Some(true) {
            v.push("UEFI rotation".to_string());
        }
        Derived::Value(v)
    }),
    ("dpu-bootstrap-ca-ipxe-provisioned", |s| {
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
    ("dpu-agent-bootstrap-ca-dpf-provisioned", |s| {
        s.if_dpf(match s.toml_str("dpf.dpu_agent_bootstrap_ca.source") {
            Some(v) => one(v),
            None => one("legacy_download"),
        })
    }),
    ("dpu-os-login-password-dpf", |s| {
        // The register's `Yes` is `ubuntu_PASSWORD='$6$…'`, the crypt(3) SHA-512 hash
        // single-quoted; a `<user>_PASSWORD=` entry in any other shape, or a `{{` anywhere
        // in a list, is a mistake neither option describes.
        let is_password = |p: &str| p.to_ascii_uppercase().contains("_PASSWORD=");
        // crypt(3) SHA-512: `$6$`, an optional `rounds=N$`, a salt of up to 16 and a digest
        // of exactly 86 characters, all over `./0-9A-Za-z`. A shorter digest is a truncated
        // or placeholder hash no login ever matches.
        let crypt_sha512 = |h: &str| {
            let b64 = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '/')
            };
            let Some(rest) = h.strip_prefix("$6$") else {
                return false;
            };
            let rest = match rest.strip_prefix("rounds=") {
                Some(r) => match r.split_once('$') {
                    Some((n, r)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => r,
                    _ => return false,
                },
                None => rest,
            };
            // The digest's last character carries two bits, so it is one of `.`, `/`, `0`, `1`.
            matches!(rest.split_once('$'), Some((salt, digest))
                if salt.len() <= 16 && b64(salt) && digest.len() == 86 && b64(digest)
                    && digest.ends_with(['.', '/', '0', '1']))
        };
        let well_formed = |p: &str| {
            p.strip_prefix("ubuntu_PASSWORD='")
                .and_then(|v| v.strip_suffix('\''))
                .is_some_and(crypt_sha512)
        };
        // Each active deployment resolves the site-wide list plus its own; the register
        // wants exactly one assignment per deployment, and none anywhere for `No`.
        let mut malformed = false;
        let mut count = |v: Option<&toml::Value>| {
            let mut n = 0;
            for p in v
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
            {
                if p.contains("{{") {
                    malformed = true;
                }
                if well_formed(p) {
                    n += 1;
                } else if is_password(p) {
                    malformed = true;
                }
            }
            n
        };
        let site_wide = count(s.toml_path("dpf.extra_bfcfg_parameters"));
        let deployments = s
            .toml_path("dpf.deployments")
            .and_then(toml::Value::as_table);
        let mut counts: Vec<usize> = deployments
            .into_iter()
            .flatten()
            .map(|(_, d)| site_wide + count(d.get("extra_bfcfg_parameters")))
            .collect();
        // Core's default BF3 deployment, kept when no written deployment states a
        // `bfb_url` (and the only one when none is written), carries the site-wide list.
        if deployments.is_none_or(|t| !t.values().any(|d| d.get("bfb_url").is_some())) {
            counts.push(site_wide);
        }
        s.if_dpf(if malformed {
            Derived::Invalid(
                "a `<user>_PASSWORD=` entry in extra_bfcfg_parameters is not `ubuntu_PASSWORD='$6$…'` with a full crypt SHA-512 hash, or a list carries a `{{` template marker"
                    .to_string(),
            )
        } else if counts.iter().all(|c| *c == 0) {
            one("No")
        } else if counts.iter().all(|c| *c == 1) {
            one("Yes")
        } else {
            // A deployment without a password beside one with, or one resolving two: not
            // a configuration either option describes.
            Derived::Invalid(format!(
                "the active DPF deployments resolve {} ubuntu_PASSWORD assignments each; exactly one per deployment is Yes, none anywhere is No",
                counts
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
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
        // Local REST not applying (the file left unread): the issuer is whoever runs the
        // REST service elsewhere, which these files do not show.
        if s.rest.root.is_null() {
            return ambiguous(&[
                "Bundled dev Keycloak",
                "Your own Keycloak",
                "Generic OIDC issuers",
            ]);
        }
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
        // RMS is in use when setup.sh installs it or when Core points at an external one.
        let external_rms = s.core_api("rms.apiUrl").is_some_and(|u| !u.is_empty());
        for (on, name) in [
            (s.cmd.installs_rest(), "REST group"),
            (s.cmd.installs_rms() || external_rms, "RMS"),
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
        // setup.sh uses the submodule when the variable is unset or empty; a shell
        // expression (`${SITE_RMS_CHART:-}`) may be either, so both sources stay open.
        s.if_rms(match s.env("NICO_RMS_CHART") {
            Env::Plain(_) | Env::Secret => one("Local chart path"),
            Env::Unreadable => ambiguous(&["Local chart path", "Pinned submodule"]),
            Env::Absent => one("Pinned submodule"),
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
        // Rack profiles place hosts in racks with or without RMS. A site `[rack_profiles]`
        // table is a site-specific profile; without one the files cannot tell the chart's
        // built-ins from a site with no rack-scale systems.
        if s.toml_path("rack_profiles").is_some() {
            one("Site-specific profile")
        } else {
            ambiguous(&["Built-in profiles", "Not applicable"])
        }
    }),
    ("observability", |s| {
        // Without the local stack, `NICO_SERVICEMONITORS` decides; a secret value leaves
        // both of those options open.
        // Without ServiceMonitors the files do not say whether an external pipeline takes
        // the telemetry (an OTLP endpoint is a Helm value another deployer may set), so
        // both options stay open until pinned.
        let without: &[&str] = match s.env("NICO_SERVICEMONITORS") {
            Env::Plain("true") => &["External stack"],
            _ => &["External stack", "No pipeline"],
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
    // Core keys that are provider decisions, each read from one TOML key; absent is the
    // code's default, a spelling Core would refuse is a mistake reported whatever the pin
    // (`config.toml.keys` reports the type, Core the value).
    ("vpc-isolation-behavior", |s| {
        match s
            .toml_str("vpc_isolation_behavior")
            .map(str::to_lowercase)
            .as_deref()
        {
            None | Some("mutual_isolation") => one("Mutual isolation"),
            Some("open") => one("Open"),
            Some(v) => invalid_value("vpc_isolation_behavior", v),
        }
    }),
    ("vpc-peering-policy", |s| {
        match s
            .toml_str("vpc_peering_policy")
            .map(str::to_lowercase)
            .as_deref()
        {
            None => one("Unset"),
            Some("exclusive") => one("Exclusive"),
            Some("mixed") => one("Mixed"),
            Some("none") => one("None"),
            Some(v) => invalid_value("vpc_peering_policy", v),
        }
    }),
    ("default-tenant-routing-profile", |s| {
        match s.toml_str("default_tenant_routing_profile_type") {
            None | Some("EXTERNAL") => one("EXTERNAL"),
            Some("INTERNAL") => one("INTERNAL"),
            Some(_) => one("Another named profile"),
        }
    }),
    ("host-naming-strategy", |s| {
        match s
            .toml_str("host_naming_strategy")
            .map(str::to_lowercase)
            .as_deref()
        {
            None | Some("ip_address") => one("IP address"),
            Some("serial_number") => one("Serial number"),
            Some("mac_address") => one("MAC address"),
            Some("fun") => one("Fun names"),
            Some(v) => invalid_value("host_naming_strategy", v),
        }
    }),
    ("compute-allocation-enforcement", |s| {
        match s
            .toml_str("compute_allocation_enforcement")
            .map(str::to_lowercase)
            .as_deref()
        {
            None | Some("warn_only") => one("Warn only"),
            Some("enforce_if_present") => one("Enforce if present"),
            Some("always") => one("Always"),
            Some(v) => invalid_value("compute_allocation_enforcement", v),
        }
    }),
    ("bmc-basic-auth-fallback", |s| {
        match s.toml_bool("allow_bmc_basic_auth_fallback") {
            Some(true) => one("On"),
            _ => one("Off"),
        }
    }),
    ("tpm-required", |s| match s.toml_bool("tpm_required") {
        Some(false) => one("Not required"),
        _ => one("Required"),
    }),
    ("dpu-agent-upgrade-policy", |s| {
        match s
            .toml_str("initial_dpu_agent_upgrade_policy")
            .map(str::to_lowercase)
            .as_deref()
        {
            None => one("Unset"),
            Some("off") => one("Off"),
            Some("up_only") => one("Up only"),
            Some("up_down") => one("Up and down"),
            Some(v) => invalid_value("initial_dpu_agent_upgrade_policy", v),
        }
    }),
    ("admin-web-ui", |s| match s.toml_bool("enable_admin_ui") {
        Some(false) => one("Off"),
        _ => one("On"),
    }),
    ("api-listen-mode", |s| {
        // Core matches the spelling exactly, so `PLAINTEXT_HTTP2` is the TLS fallback.
        match s.toml_str("listen_mode") {
            None | Some("tls") => one("TLS"),
            Some("plaintext_http2") => one("Plaintext HTTP/2"),
            Some("plaintext_http1") => one("Plaintext HTTP/1.1"),
            // Core reads any other spelling as TLS (`#[serde(other)]`).
            Some(_) => one("TLS"),
        }
    }),
    ("component-manager-backends", |s| {
        let Some(cm) = s
            .toml_path("component_manager")
            .and_then(toml::Value::as_table)
        else {
            return one("Not applicable");
        };
        // Each field defaults to `rms` when the table is present and the field absent.
        let backends: Vec<String> = COMPONENT_MANAGER_ROLES
            .iter()
            .map(|(field, _)| {
                cm.get(*field)
                    .and_then(toml::Value::as_str)
                    .map_or("rms".to_string(), str::to_lowercase)
            })
            .collect();
        let rms = backends.iter().filter(|b| *b == "rms").count();
        if rms == backends.len() {
            one("RMS for every role")
        } else if rms > 0 {
            one("Mixed backends")
        } else {
            one("No RMS backend")
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
            for n in d.options() {
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
        // Both paths install BF3 for the BF3 fleet, so DPF undecided settles it.
        assert_eq!(get(&unpinned, "managed-host-dpu-generation"), one("BF3"));
        ex.splice("site-def.yaml", "  # dpf: Install ", "  dpf: Install ");
        assert_eq!(
            get(&ex.derive(), "managed-host-dpu-generation"),
            get(&baseline, "managed-host-dpu-generation")
        );
        // Without DPF the iPXE path still installs BF3, which the example's fleet has.
        ex.splice("site-def.yaml", "  dpf: Install ", "  dpf: Skip ");
        assert_eq!(get(&ex.derive(), "managed-host-dpu-generation"), one("BF3"));
    }

    /// The built-in resolver needs the subchart deployed, not only its Service switched on.
    #[test]
    fn the_resolver_is_built_in_only_with_the_unbound_subchart_enabled() {
        let get = |values: &[(String, Derived)], id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| panic!("{id} derived"))
        };
        let ex = crate::fixture::Example::new();
        assert_eq!(
            get(&ex.derive(), "recursive-resolver-for-managed-machines"),
            one("Built-in unbound")
        );
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "unbound:\n  enabled: true\n",
            "unbound:\n",
        );
        assert_eq!(
            get(&ex.derive(), "recursive-resolver-for-managed-machines"),
            one("External DNS")
        );
    }

    /// With another deployer, the component pin says RMS is in use, not where: the `rms`
    /// entry stays open; a pin leaving RMS out settles it as not in the cluster.
    #[test]
    fn the_component_pin_does_not_place_rms_for_another_deployer() {
        let get = |values: &[(String, Derived)], id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| panic!("{id} derived"))
        };
        let other_deployer = "command: make deploy";
        let setup_sh = "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms";
        let ex = crate::fixture::Example::new();
        ex.splice("site-def.yaml", setup_sh, other_deployer);
        ex.splice(
            "site-def.yaml",
            "  optional-components: [REST group]",
            "  optional-components: [REST group, RMS]",
        );
        assert_eq!(
            get(&ex.derive(), "rms"),
            ambiguous(&["In-cluster nv-rms", "External RMS", "Not applicable"])
        );
        let ex = crate::fixture::Example::new();
        ex.splice("site-def.yaml", setup_sh, other_deployer);
        assert_eq!(get(&ex.derive(), "rms"), one("Not applicable"));
    }

    /// Core keeps its default BF3 deployment beside a written BF4 one while no deployment
    /// states a `bfb_url`: both generations are deployed and managed, so both are certain.
    #[test]
    fn a_bf4_only_toml_still_deploys_the_default_bf3() {
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
            Derived::Value(vec!["BF3".to_string(), "BF4 generic".to_string()])
        );
    }

    /// With another deployer and nothing pinning the REST group, the prerequisites say
    /// whether REST applies; a REST file that does not parse is then not required.
    /// Every configured KMS provider's type is in use; an `active` naming no provider
    /// selects nothing; an unreadable pull-secret expression leaves its entry open.
    #[test]
    fn kms_types_and_an_unreadable_secret() {
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
            "      [secrets]\n      backends = [\"vault\"]\n      writer = \"vault\"\n",
            "      [secrets]\n      backends = [\"vault\"]\n      writer = \"postgres\"\n      [secrets.kms]\n      active = \"site\"\n      [secrets.kms.providers.site]\n      type = \"integrated\"\n      [secrets.kms.providers.old]\n      type = \"transit\"\n",
        );
        ex.splice(
            "site-def.yaml",
            "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
            "command: REGISTRY_PULL_SECRET=\"${TOKEN:-}\" ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
        );
        let values = ex.derive();
        let Derived::Value(mut types) = get(&values, "key-wrapping-for-the-postgres-store") else {
            panic!("a value");
        };
        types.sort();
        assert_eq!(types, ["Integrated", "Transit"]);
        assert_eq!(
            get(&values, "registry-authentication"),
            ambiguous(&["Credentialed pulls", "No secret from setup"])
        );
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      active = \"site\"",
            "      active = \"typo\"",
        );
        assert_eq!(
            get(&ex.derive(), "key-wrapping-for-the-postgres-store"),
            none()
        );
    }

    /// The nine Core-key entries: absent keys read as the code's defaults, set keys as the
    /// spelled option.
    #[test]
    fn core_key_entries_read_their_keys_and_defaults() {
        let get = |values: &[(String, Derived)], id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| panic!("{id} derived"))
        };
        let ex = crate::fixture::Example::new();
        let defaults = ex.derive();
        for (id, want) in [
            ("vpc-isolation-behavior", "Mutual isolation"),
            ("vpc-peering-policy", "Unset"),
            ("default-tenant-routing-profile", "EXTERNAL"),
            ("host-naming-strategy", "IP address"),
            ("compute-allocation-enforcement", "Warn only"),
            ("bmc-basic-auth-fallback", "Off"),
            ("tpm-required", "Required"),
            ("dpu-agent-upgrade-policy", "Unset"),
            ("admin-web-ui", "On"),
            ("api-listen-mode", "TLS"),
            ("component-manager-backends", "Not applicable"),
        ] {
            assert_eq!(get(&defaults, id), one(want), "{id} absent");
        }
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      sitename = \"site1\"\n",
            "      sitename = \"site1\"\n      vpc_isolation_behavior = \"open\"\n      vpc_peering_policy = \"mixed\"\n      default_tenant_routing_profile_type = \"INTERNAL\"\n      host_naming_strategy = \"fun\"\n      compute_allocation_enforcement = \"always\"\n      allow_bmc_basic_auth_fallback = true\n      tpm_required = false\n      initial_dpu_agent_upgrade_policy = \"up_only\"\n      enable_admin_ui = false\n      listen_mode = \"plaintext_http2\"\n",
        );
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [dpf]\n      enabled = true\n",
            "      [component_manager]\n      compute_tray_backend = \"core\"\n      nv_switch_backend = \"nsm\"\n\n      [component_manager.nsm]\n      url = \"http://nsm.site1.dc1.example.net:50052\"\n\n      [dpf]\n      enabled = true\n",
        );
        let set = ex.derive();
        for (id, want) in [
            ("vpc-isolation-behavior", "Open"),
            ("vpc-peering-policy", "Mixed"),
            ("default-tenant-routing-profile", "INTERNAL"),
            ("host-naming-strategy", "Fun names"),
            ("compute-allocation-enforcement", "Always"),
            ("bmc-basic-auth-fallback", "On"),
            ("tpm-required", "Not required"),
            ("dpu-agent-upgrade-policy", "Up only"),
            ("admin-web-ui", "Off"),
            ("api-listen-mode", "Plaintext HTTP/2"),
            // compute core, switch nsm, power shelf absent and so rms: mixed.
            ("component-manager-backends", "Mixed backends"),
        ] {
            assert_eq!(get(&set, id), one(want), "{id} set");
        }
    }

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
            .find(|(i, _)| i == "dpu-bootstrap-ca-ipxe-provisioned")
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

    /// No `[secrets]` table is Core's Vault default, so no key wrapping applies either.
    #[test]
    fn an_absent_secrets_table_reads_as_vault() {
        let ex = crate::fixture::Example::new();
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [secrets]\n      backends = [\"vault\"]\n      writer = \"vault\"\n",
            "",
        );
        let values = ex.derive();
        let get = |id: &str| {
            values
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, d)| d.clone())
                .unwrap()
        };
        assert_eq!(get("credential-store"), one("Vault KV"));
        assert_eq!(
            get("key-wrapping-for-the-postgres-store"),
            one("Not applicable")
        );
    }

    /// Core matches `listen_mode` exactly and reads any other spelling as TLS.
    #[test]
    fn listen_mode_is_matched_exactly() {
        let ex = crate::fixture::Example::new();
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      sitename = \"site1\"\n",
            "      sitename = \"site1\"\n      listen_mode = \"PLAINTEXT_HTTP2\"\n",
        );
        let values = ex.derive();
        let d = values
            .iter()
            .find(|(i, _)| i == "api-listen-mode")
            .map(|(_, d)| d.clone())
            .unwrap();
        assert_eq!(d, one("TLS"));
    }

    /// The DPF login password resolves per active deployment, the site-wide list plus the
    /// deployment's own: exactly one each is `Yes`, none anywhere `No`, anything else
    /// neither.
    #[test]
    fn dpf_login_password_is_counted_per_deployment() {
        const HASH: &str = "$6$saltsalt$ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789./ABCDEFGHIJKLMNOPQRSTU.";
        let pw = format!("extra_bfcfg_parameters = [\"ubuntu_PASSWORD='{HASH}'\"]\n");
        let read = |ex: &crate::fixture::Example| {
            ex.derive()
                .iter()
                .find(|(i, _)| i == "dpu-os-login-password-dpf")
                .map(|(_, d)| d.clone())
                .unwrap()
        };
        let none_set = crate::fixture::Example::new();
        assert_eq!(read(&none_set), one("No"));

        // Site-wide only: the default BF3 deployment resolves the one assignment.
        let site_wide = crate::fixture::Example::new();
        site_wide.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [dpf]\n      enabled = true\n",
            &format!("      [dpf]\n      enabled = true\n      {pw}"),
        );
        assert_eq!(read(&site_wide), one("Yes"));

        // A rounds parameter is part of the hash's shape.
        let with_rounds = crate::fixture::Example::new();
        with_rounds.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [dpf]\n      enabled = true\n",
            &format!(
                "      [dpf]\n      enabled = true\n      extra_bfcfg_parameters = [\"ubuntu_PASSWORD='$6$rounds=5000$saltsalt${}'\"]\n",
                &HASH[12..]
            ),
        );
        assert_eq!(read(&with_rounds), one("Yes"));

        // Only in a BF4 deployment: the default BF3 deployment resolves none.
        let one_of_two = crate::fixture::Example::new();
        one_of_two.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [secrets]\n",
            &format!("      [dpf.deployments.bf4_generic]\n      bluefield_software = \"x\"\n      {pw}\n      [secrets]\n"),
        );
        assert!(
            matches!(read(&one_of_two), Derived::Invalid(_)),
            "{:?}",
            read(&one_of_two)
        );

        // Site-wide and in the written BF3 deployment: that deployment resolves two.
        let twice = crate::fixture::Example::new();
        twice.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [dpf]\n      enabled = true\n",
            &format!("      [dpf]\n      enabled = true\n      {pw}"),
        );
        twice.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [secrets]\n",
            &format!("      [dpf.deployments.bf3]\n      bfb_url = \"http://x/bf3.bfb\"\n      {pw}\n      [secrets]\n"),
        );
        assert!(
            matches!(read(&twice), Derived::Invalid(_)),
            "{:?}",
            read(&twice)
        );

        // Another user's password, a hash not single-quoted, not `$6$` or with a truncated
        // digest, or a template marker left in a list: a mistake neither option describes.
        for bad in [
            "extra_bfcfg_parameters = [\"admin_PASSWORD=placeholder\"]\n".to_string(),
            format!("extra_bfcfg_parameters = [\"ubuntu_PASSWORD={HASH}\"]\n"),
            format!(
                "extra_bfcfg_parameters = [\"ubuntu_PASSWORD='{HASH}'\", \"ENABLE_X={{{{ .x }}}}\"]\n"
            ),
            "extra_bfcfg_parameters = [\"ubuntu_PASSWORD='$6$salt$hash'\"]\n".to_string(),
            "extra_bfcfg_parameters = [\"ubuntu_PASSWORD='$5$saltsalt$hash'\"]\n".to_string(),
        ] {
            let ex = crate::fixture::Example::new();
            ex.splice(
                "helm-prereqs/values/nico-core.yaml",
                "      [dpf]\n      enabled = true\n",
                &format!("      [dpf]\n      enabled = true\n      {bad}"),
            );
            assert!(
                matches!(read(&ex), Derived::Invalid(_)),
                "{bad}: {:?}",
                read(&ex)
            );
        }
    }

    /// A plain-NIC site's CP-DPU yaml is not read: a stale or broken file stops no reader.
    #[test]
    fn a_no_dpu_site_does_not_read_the_cp_dpu_yaml() {
        let ex = crate::fixture::Example::new();
        ex.splice(
            "site-def.yaml",
            "  dpu-in-the-site-controllers: BF3 DPU in DPU mode\n",
            "  dpu-in-the-site-controllers: no DPU\n",
        );
        ex.splice(
            "control-plane-dpu/site.yaml",
            "bgpAsnStart: 4244766860",
            "bgpAsnStart: [",
        );
        let smn = ex
            .derive()
            .iter()
            .find(|(i, _)| i == "smn")
            .map(|(_, d)| d.clone())
            .unwrap();
        assert_eq!(smn, one("SMN on the ToR"));
    }
}
