# The site definition (`site-def.yaml`)

One file per NICo site, written by the Infrastructure Provider, read only by `nico-pre`.
`setup.sh`, the ISO builder and `nico-api` never read it. It states what the provider intends
(the decisions), where the configuration files the NICo tooling does read live (the
collaterals), and the plan values the checks compare those files against. It never carries a
secret value, only the source of one.

Validate it with `nico-pre validate --site-def <path>`; every checklist runs the same gate first.
Unknown keys anywhere are errors. Key names are camelCase. Paths are relative to the file.

Schema version: `schemaVersion: 1`. Source of truth: `src/sitedef.rs` (types) and
`src/gate.rs` (invariants); this page follows them and is checked against the two examples.

## Top level

| Key | Required | Type | Read by | Meaning |
|---|---|---|---|---|
| `schemaVersion` | yes | integer, `1` | gate | Schema generation of this file. |
| `site` | yes | table | every checklist | Identity of the site. |
| `collaterals` | yes | table | `config`, later checklists | Paths of the files NICo's tooling reads. |
| `results` | yes | path | every checklist | Output directory; the only path the tool writes to, one JSON result per checklist and run. |
| `decisions` | no, see below | table | gate, every checklist | One key per register entry; selects which checks run. |
| `hosts` | yes | table | `build`, `stage`, `host`, `fabric` | The utility hosts and the site controllers. |
| `network` | yes | table | `config`, `fabric`, `platform` | The network plan. |
| `counts` | yes | table | `config` | Fleet size for sizing checks. |
| `fleet` | no | table | `host`, `ingest` | What NICo will manage; selects BIOS profiles, firmware pins, seed entries. |
| `versions` | yes | table | `build`, `stage`, `install` | Release tags the artifacts must match. |
| `credentials` | no | table | `ingest` | Site-wide credentials by source. |
| `setup` | yes | table | `config`, `platform`, `install` | The `setup.sh` command line and environment the provider will run. |

## `site`

| Key | Required | Type | Meaning |
|---|---|---|---|
| `name` | yes | string | Site name; appears in every result file. |
| `datacenter` | yes | string | Datacenter the site belongs to. |
| `dnsZone` | yes | string | The site's DNS zone. |
| `apiHostname` | yes | string | External name of the Core API; must resolve to the API VIP. |
| `uuidSource` | yes | `setup` or `"fixed: <uuid>"` | Who mints the site UUID: setup.sh, or a fixed value. Quote the `fixed:` form, YAML would otherwise read it as a nested key. |
| `description` | no | string | Free text, never checked. |

## `collaterals`

| Key | Required | Meaning |
|---|---|---|
| `cpDpuSiteYaml` | only with a control-plane DPU | `scripts/control-plane-dpu` site yaml the ISO builder reads. Omit when `dpu-in-the-site-controllers` is `no DPU`. |
| `helmPrereqsValues` | yes | `helm-prereqs/values.yaml` for the site. |
| `coreValues` | yes | The Core values file (`values/nico-core.yaml`); carries the NICo TOML under `nicoApiSiteConfig`. |
| `restValues` | yes | The REST values file (`values/nico-rest.yaml`). |
| `siteAgentValues` | yes | The site-agent values file. |
| `metallbConfig` | yes | The MetalLB configuration passed with `--metallb-config`: a manifest file or a kustomize directory. |
| `expectedMachines` | from step 7 | The expected-machines file the ingestion checklist reads. |

## `decisions`

One key per decision in the register, value the option's **short name**, case-insensitive.
`nico-pre decisions` lists the register with the short names; `nico-pre decisions --template`
writes this section with every stated entry marked `REQUIRED` and every derived entry
commented out with its default.

- **Stated** entries have no collateral that records them and must be present unless a rule
  implies them: `staging-host`, `external-registry-access`, `build-host`,
  `dpu-in-the-site-controllers`, `managed-host-kinds-in-the-fleet`.
- **Derived** entries are read from the collaterals by the `config` checklist; stating one pins it,
  and a collateral that contradicts the pin is a failure.
- **Any of** entries take a YAML list of short names; `[]` means nothing ticked. They are
  `managed-host-kinds-in-the-fleet`, `managed-host-dpu-generation`, `pxe-and-http-boot-url`,
  `key-wrapping-for-the-postgres-store`, `certificate-issuance`,
  `autonomous-bmc-and-uefi-rotation`, `optional-components`. A list given to any other entry
  is an error.
- Implication rules fill in what a stated answer implies (an air-gapped staging host implies
  `external-registry-access: Not applicable`); a stated value that contradicts an implication,
  and any pair the register declares exclusive, fails the gate and names both values.

## `hosts`

| Key | Required | Type | Meaning |
|---|---|---|---|
| `jumphost` | no | `{name?, address}` | The jumphost of an air-gapped site. |
| `stagingHost` | yes | `{address, sshUser?}` | Where setup.sh runs. |
| `buildHost` | no | `{address}` | Where the ISO and the images are built; outside the site when air-gapped. |
| `registryHost` | no | `{url, caSource?}` | The provider registry; `caSource` names where its CA is read from (`file:`, never the key). |
| `bmcProbeHost` | no | IP address | A host in the BMC segment that runs the probe container. |
| `probeHosts` | no | `{admin?, hostInband?}` | Vantage points for the `fabric` checks on segments no utility host sits on, before the controllers are up: `admin` (IP on the admin segment), `hostInband` (list of `{name, address}`, one per `network.segments.hostInband` entry, by its `name`). Without them the DNS, NTP, MTU and DHCP rows for those segments are deferred. |
| `siteControllers` | yes | list | One entry per site controller, see below. |

Each site controller: `hostName` (unique), `hostSerial` (unique), `model` (selects the HCL
firmware pins), `nodeId` (from 1, no gaps), `bmc` (IP), `dpuBmc` (IP, with a DPU), `p0Mac`
(parsable MAC, with a DPU), `torPeers` (`[{address, asn}]`, plain-NIC controllers only, since
MetalLB then peers with the ToRs directly), `dpuSsh` (`{address, user?}`, optional: the
control-plane DPU's OS over its OOB address, so the `fabric` checks can read FRR and LLDP on it;
without it those checks run only from media on the controller), `uplinks` (optional list of
`{port, tor, torPort?}`: the local port, the ToR's LLDP system name and port id, what
`fabric.uplinks.lldp` compares with; without it the row only counts neighbours).

## `network`

| Key | Required | Type | Meaning |
|---|---|---|---|
| `datacenterAsn` | yes | u32 | The fabric's ASN. |
| `bgpAsnStart`, `bgpAsnEnd` | yes | u32 | The site's BGP ASN range, one block. By convention MetalLB advertises from `bgpAsnStart`, site controller `nodeId` *i* peers as `bgpAsnStart + i`, and the rest is the per-DPU pool (`[pools.fnn-asn]`). Must not contain `datacenterAsn` and must leave at least one ASN for DPUs. |
| `asnLayout` | no | table | Overrides for a site that did not follow the convention: `siteControllerRoutes` (the MetalLB ASN) and `siteControllers` (`nodeId` to ASN). Each key is optional; the convention stands for what is absent. Overridden ASNs inside the range are carved out of the per-DPU pool; outside it they are simply recorded. |
| `siteControllerMtuSize` | yes | u32 | MTU of the site controller uplinks. |
| `forgeControlPlanePrefix` | yes | prefix | Control-plane /31s to the DPUs. |
| `forgeServiceVipPrefix` | yes | prefix | The service VIP pool. |
| `forgeDpuLoopbackPrefix` | yes | prefix | DPU loopbacks. |
| `forgeOverlayPrefixes` | yes | list of prefixes | Tenant overlay space. |
| `segments` | yes | table | `admin` and `controlPlaneOob` (each `{prefix, gateway, mtu}`), `managedHostBmc` (list, each with `switchName`), `hostInband` (list, each with `name`). Gateways must lie inside their prefix. |
| `kubernetes` | yes | `{podCidr, serviceCidr, apiVip}` | The cluster's CIDRs and API VIP. |
| `nameServer` | yes | IP | Upstream resolver. |
| `unboundForwardAddresses` | no | list of IPs | Forwarders of the built-in unbound. |
| `ntpServers` | yes | list of IPs | Site NTP. |
| `pathMtu` | yes | u32 | Path MTU the segments must respect. |
| `defaultRoute` | no | `bgp` or `static` | How the site controllers and DPUs get their default route; `fabric.default-route` checks the stated mechanism, and skips when absent. |
| `pools` | yes | `{vlan, vni, externalVpcVni?}` | Each `{start, end}`, start at or below end. |
| `fnn` | no | table | `controlPlaneVni`, `adminVpcVni?`, `vpcVrfLoopbackPrefix`, `poolsVpcVni` (list of ranges), `routeTargets` (name to number). |
| `denyPrefixes` | no | list of prefixes | Pin of a derived value; a collateral that contradicts it fails. |
| `serviceVips` | no | `{api, dhcp, dns0, dns1, pxe, sshConsole}` | Pin of the derived VIP layout. |

Every prefix in this table must be aligned to its mask and no two may overlap.

## `counts`, `fleet`, `versions`, `credentials`, `setup`

- `counts`: `hosts`, `dpus`, `tenants`, `racks`, all integers; size the prefixes, pools and
  Postgres against them.
- `fleet`: `hosts` is a list of `{model, gpu?, count}`; `dpu` is `{model, generation}`.
- `versions`: `infraController` (release tag of the checkout and the Core image), `nicoRest`,
  `kubernetesBaseline` required; `nvRms`, `docaPlatform`, `bfb`, `doca`, `hbn` optional.
- `credentials`: `bmcRoot` required when present, `uefi` optional; each is a secret source
  `{from: env|file|vault, path?, name?}`. Never the value.
- `setup`: `command` is the `setup.sh` command line the provider will run; `env` maps each
  variable to a plain string or to a secret source table such as `{from: env}`.
  The switches are read from the simple command that runs `setup.sh`: the script as the program,
  or behind a wrapper (`env`, `sudo`, `nohup`, `time`, `timeout`, an interpreter, `bash -c '…'`
  included) and its
  options, with assignments before either, after any `cd … &&`. A command that merely names it (`test -x ./setup.sh &&`) is not
  it; when no command qualifies, the first one naming it is read. A line that does not run
  `setup.sh` (a developer site deployed by other scripts) makes no decision and is not compared.

## Examples

- `examples/site-def.yaml`: an illustrative air-gapped site with three site controllers
  carrying BF3 DPUs.
- `examples/site-def-dev1.yaml`: the nico-dev `dev1` developer VM, written from the site's own
  records, with stand-ins marked where the simulation has no real counterpart.

Both pass `nico-pre validate`; the crate's tests splice broken values into the first one and
assert the gate names the offending row.
