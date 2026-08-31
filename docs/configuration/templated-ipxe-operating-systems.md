# Templated iPXE Operating Systems

Templated iPXE Operating Systems let NICo reuse a validated iPXE script
template while supplying the parameters and boot artifacts that vary between
operating-system definitions. They replace most uses of the legacy raw
`ipxeScript` and image-based paths. Raw `ipxeScript` Operating Systems are
deprecated. Image-based creation remains available to a Tenant Admin only when
the target Site is Registered, accessible to the Tenant, and has the
`ImageBasedOperatingSystem` capability enabled. A Provider Admin can create
only Templated iPXE Operating Systems.

This guide explains the template and Operating System resources, ownership and
site rules, synchronization between NICo REST and NICo Core, and the workflow
for creating and using a templated Operating System.

## How the Resources Fit Together

The feature has three layers:

1. **iPXE template** -- a read-only script blueprint built into NICo Core. It
   declares required parameters, reserved parameters, and required artifacts.
2. **Templated iPXE Operating System** -- a reusable definition that references
   one template and supplies its parameter values, artifact URLs, user data,
   and boot policy.
3. **Instance** -- references the Operating System by UUID. NICo Core combines
   the definition with reserved values and renders
   the final iPXE script when the machine boots.

```mermaid
flowchart LR
    CoreTemplate["Public template in NICo Core"]
    RestTemplate["Read-only template catalog in NICo REST"]
    RestOS["Templated iPXE Operating System in REST"]
    CoreOS["Operating System definition in Core"]
    Instance["Instance with operatingSystemId"]
    Script["Rendered iPXE script"]

    CoreTemplate -->|"site inventory"| RestTemplate
    RestTemplate -->|"ipxeTemplateId"| RestOS
    RestOS -->|"synchronized to one Site"| CoreOS
    Instance -->|"references"| RestOS
    CoreTemplate --> Script
    CoreOS --> Script
```

Templates and Operating Systems are different resources. A template describes
the placeholders and script structure. An Operating System provides concrete
values for one use of that template.

The examples use `nicocli` for the REST API and `nico-admin-cli` for direct
Core administration. Replace the example resource identifiers with values from
your deployment. To run the examples, set:

```bash
export NICO_BASE_URL="https://nico.example.com"
export NICO_ORG="example-org"
export NICO_TOKEN="replace-with-a-valid-jwt-bearer-token"
export SITE_ID="497f6eca-6276-4993-bfeb-53cbbbba6f08"
export TEMPLATE_ID="c4b1d4f6-69ba-5f55-90cd-ab2acd002475"
export OS_ID="42b0f982-5c61-4d2f-a018-41ece61f4641"
export TENANT_ID="f97df110-f4de-492e-8849-4a6af68026b0"
export VPC_ID="5f0b9e7a-3f4c-4d6a-8b21-7d2e9a4c6f10"
export INSTANCE_TYPE_ID="6a1c8f42-5d37-4e90-b264-3f7a9d1e5c82"
export TENANT_ORG_ID="tenant-example-org"
```

## Template Discovery and Availability

iPXE templates are compiled into NICo Core and are read-only. Only
templates with `Public` visibility are synchronized to NICo REST.

REST stores one global template record for each stable Core UUID and tracks
which Sites report it. Consequently:

- A template can be listed only at Sites where it is available.
- The same template UUID reported by multiple Sites is represented once.
- Removing the last Site association removes the template from the REST
  catalog.
- Templates cannot be created, updated, or deleted through the REST API.

List the templates available at the target Site:

```bash
nicocli ipxe-template list \
  --site-id "${SITE_ID}" \
  --all \
  --output table
```

The equivalent REST request is:

```bash
curl --fail-with-body --silent --show-error --get \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  --data-urlencode "siteId=${SITE_ID}" \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/ipxe-template"
```

Inspect the selected template:

```bash
nicocli ipxe-template get "${TEMPLATE_ID}"
```

Or retrieve it directly through REST:

```bash
curl --fail-with-body --silent --show-error \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/ipxe-template/${TEMPLATE_ID}"
```

Before creating an Operating System, note these fields:

| Field | Meaning |
|---|---|
| `id` | Stable UUID to send as `ipxeTemplateId` |
| `requiredParams` | Parameter names the Operating System must provide |
| `reservedParams` | Values supplied by NICo Core at render time; do not provide them |
| `requiredArtifacts` | Artifact names the Operating System must provide |
| `visibility` | Only `Public` templates are available through REST |

## Ownership and Authorization

Ownership is inferred from the caller's organization and role. The deprecated
`tenantId` and `infrastructureProviderId` request fields should not be used.

| Caller | Create behavior | Read behavior | Update and delete |
|---|---|---|---|
| Provider Admin | May create only Templated iPXE Operating Systems at a Registered Site owned by its provider. The result is provider-owned. | Sees its provider-owned definitions. | May modify its provider-owned definitions. |
| Tenant Admin | May create a tenant-owned Templated iPXE Operating System at a Registered Site accessible to the tenant. | Sees tenant-owned definitions plus provider-owned definitions associated with an accessible Site. | May modify only tenant-owned definitions. |
| Dual-role user | Provider ownership takes priority when creating a Templated iPXE Operating System. | Sees the union of its provider and tenant views. | Authorization follows the resource owner. |
| Provider Viewer | Cannot create or modify Operating Systems. | Can discover templates available at provider Sites. | None. |

Visibility does not grant mutation rights. A Tenant can select a visible
provider-owned definition when creating or updating an Instance at an
associated Site, but only the Provider can update or delete that definition.

## Site Rules

A Templated iPXE Operating System created through REST must specify exactly one
Site:

```json
{
  "siteIds": ["497f6eca-6276-4993-bfeb-53cbbbba6f08"]
}
```

Although `siteIds` is an array for compatibility with the existing Operating
System API, zero or multiple values are rejected for this type.

The Site must:

- Exist and be in `Registered` status.
- Be owned by the Provider for a provider-owned definition, or be accessible
  to the Tenant for a tenant-owned definition.
- Report the referenced iPXE template.

The Site association is fixed at creation and cannot be updated.

## Create a Templated iPXE Operating System

The following example uses the built-in `kernel-initrd` template. That template
requires the `kernel_params` parameter and the `kernel` and `initrd` artifacts.
Always inspect the template returned by your deployment instead of assuming
that its requirements match this example.

Save the request body so it can be used with either client:

```bash
cat > templated-os.json <<EOF
{
  "name": "ubuntu-24.04-autoinstall",
  "description": "Ubuntu 24.04 automated installation",
  "siteIds": ["${SITE_ID}"],
  "ipxeTemplateId": "${TEMPLATE_ID}",
  "ipxeTemplateParameters": [
    {
      "name": "kernel_params",
      "value": "ip=dhcp autoinstall"
    }
  ],
  "ipxeTemplateArtifacts": [
    {
      "name": "kernel",
      "url": "https://artifacts.example.com/ubuntu/vmlinuz",
      "cacheStrategy": "CacheAsNeeded"
    },
    {
      "name": "initrd",
      "url": "https://artifacts.example.com/ubuntu/initrd",
      "cacheStrategy": "CacheAsNeeded"
    }
  ],
  "userData": "#cloud-config\nhostname: ubuntu-worker\n",
  "phoneHomeEnabled": false,
  "allowOverride": true
}
EOF
```

Create it with `nicocli`:

```bash
nicocli operating-system create --data-file templated-os.json
```

Or create it directly through REST:

```bash
curl --fail-with-body --silent --show-error \
  --request POST \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  --header "Content-Type: application/json" \
  --data @templated-os.json \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/operating-system"
```

The request type is inferred from `ipxeTemplateId`; do not send a separate
`type` value. `ipxeTemplateId` is mutually exclusive with `ipxeScript` and
`imageUrl`.

### Parameters

Each parameter has a `name` and `value`. Names are matched case-insensitively.

- Supply a non-empty parameter value for every required occurrence in
  `requiredParams`. An artifact with the same name does not substitute for a
  required parameter during rendering.
- Do not supply names in `reservedParams`; NICo Core provides those values.
- Extra parameters are accepted only when the template contains its `{{extra}}`
  placeholder.
- Duplicate names are allowed and consumed occurrence by occurrence in the
  order declared by the template.

### Artifacts

Each artifact requires `name`, `url`, and `cacheStrategy`. Optional fields are:

- `sha` -- expected SHA-256 checksum.
- `authType` -- `Basic` or `Bearer`; requires `authToken`.
- `authToken` -- credential used to retrieve the artifact; requires `authType`.

Artifact names are matched case-insensitively. Duplicate names are allowed when
the template requires multiple occurrences.

| Strategy | URL used at boot |
|---|---|
| `CacheAsNeeded` | Prefer the Site's cached URL; fall back to the supplied URL. |
| `RemoteOnly` | Always use the supplied URL. |
| `LocalOnly` | Use the supplied URL directly; it is already Site-local. |
| `CachedOnly` | Require a cached URL at the Site; rendering cannot proceed until one exists. |

NICo does not provide the process that downloads artifacts into a Site-local
cache and populates `cachedUrl`. To use `CachedOnly`, operators must provide an
external process that downloads each artifact and records its Site-local URL
through Core's cache-management API. Without that process, a definition that
contains a `CachedOnly` artifact cannot become ready or render its iPXE script.

Artifact `authToken` values are accepted on create and update but are
structurally omitted from REST API responses. Treat them as write-only
credentials.

The per-Site `cachedUrl` value is managed by NICo Core and is not stored or
returned by NICo REST. Core's cache-management gRPC operations are the only
supported way to set or clear it. A `CachedOnly` artifact without a cached URL
keeps the Core definition from becoming ready.

### Populate `CachedOnly` Artifacts

The external cache process does not need to update the Operating System
definition or its status. It must:

1. Query the Operating System's artifacts and select each `cached_only`
   artifact whose `cached_url` is empty.
2. Download each source `url` into storage reachable from the Site. The process
   is responsible for using the supplied authentication and verifying `sha`
   when those fields are present.
3. After the cached artifact is reachable, record its Site-local URL through
   Core's cache-management API.

Core treats a non-empty `cachedUrl` as the cache-completion signal; it does not
download the artifact as part of this update. When every `CachedOnly` artifact
occurrence has a non-empty `cachedUrl`, Core automatically changes the
Operating System definition from `PROVISIONING` to `READY`. No separate status
update is required.

For example, inspect the artifacts that still need to be cached:

```bash
nico-admin-cli -f json operating-system get-artifacts "${OS_ID}" |
  jq -r '.[] |
    select(.cache_strategy == "cached_only" and (.cached_url // "") == "") |
    [.name, .url, (.sha // "")] | @tsv'
```

After the external process has downloaded and published the artifacts, record
their Site-local URLs and verify the resulting status:

```bash
nico-admin-cli operating-system set-cached-url "${OS_ID}" \
  --set kernel=https://cache.example.com/ubuntu/vmlinuz \
  --set initrd=https://cache.example.com/ubuntu/initrd

nico-admin-cli -f json operating-system show "${OS_ID}" |
  jq '{id, name, status}'
```

Repeat `--set` for multiple artifacts. If a name occurs multiple times in the
definition, repeat that name to update each occurrence in definition order. Use
`--set NAME=` to clear a cached URL. Clearing any `CachedOnly` URL changes a
ready definition back to `PROVISIONING`.

## Create a Site-Local Definition in Core

The REST API is the preferred interface for normal operations. A Site
administrator can also create a definition directly in Core with
`nico-admin-cli`. For example, the built-in `qcow-image` template requires the
`image_url` parameter:

```bash
nico-admin-cli operating-system create \
  --name site-qcow-image \
  --ipxe-template-id ea756ddd-add3-5e42-a202-44bfc2d5aac2 \
  --param image_url=https://artifacts.example.com/images/ubuntu.qcow2
```

The optional `--org` flag controls how REST assigns ownership when it discovers
the definition:

- Omit `--org` to create a provider-owned definition for the reporting Site.
- Set `--org "${TENANT_ORG_ID}"` to create a tenant-owned definition.
  REST must be able to resolve that organization to an existing Tenant.
- An explicitly empty organization is invalid.

The discovered definition has one Site association and therefore participates
in bidirectional synchronization.

## Verify Synchronization

Capture the returned Operating System UUID and inspect it:

```bash
nicocli operating-system get "${OS_ID}"
```

The equivalent REST request is:

```bash
curl --fail-with-body --silent --show-error \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/operating-system/${OS_ID}"
```

This response contains the definition, aggregate `status`, and `statusHistory`.
The get-by-ID response does not expand Site associations for
Templated iPXE definitions. Use the Site-filtered list response to inspect the
association:

```bash
nicocli operating-system list \
  --site-id "${SITE_ID}" \
  --type TemplatedIpxe \
  --all
```

The REST equivalent is:

```bash
curl --fail-with-body --silent --show-error --get \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  --data-urlencode "siteId=${SITE_ID}" \
  --data-urlencode "type=TemplatedIpxe" \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/operating-system"
```

Wait for the target `siteAssociations` entry to become `Synced` before
selecting the Operating System for an Instance.

Common association states are:

| State | Meaning |
|---|---|
| `Syncing` | REST is sending or re-sending the definition to the Site. |
| `Synced` | The Site accepted the definition. |
| `Error` | The Site proxy operation or association bookkeeping failed; inspect status history. |
| `Deleting` | Site cleanup is in progress. |

Updates set the association to `Syncing` until synchronization finishes.
Instance create and update reject a Templated iPXE Operating System unless its
association with the Instance's Site is `Synced`.

## Use the Operating System for an Instance

For either a tenant-owned definition or a visible provider-owned definition,
pass its UUID as `operatingSystemId`. The Instance's VPC and the Operating
System must resolve to the same Site, and the Site association must be
`Synced`.

```bash
nicocli instance create \
  --name ubuntu-worker-01 \
  --tenant-id "${TENANT_ID}" \
  --vpc-id "${VPC_ID}" \
  --instance-type-id "${INSTANCE_TYPE_ID}" \
  --operating-system-id "${OS_ID}"
```

The equivalent REST request is:

```bash
curl --fail-with-body --silent --show-error \
  --request POST \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  --header "Content-Type: application/json" \
  --data @- \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/instance" <<EOF
{
    "name": "ubuntu-worker-01",
    "tenantId": "${TENANT_ID}",
    "vpcId": "${VPC_ID}",
    "instanceTypeId": "${INSTANCE_TYPE_ID}",
    "operatingSystemId": "${OS_ID}"
}
EOF
```

NICo sends the Operating System UUID to Core rather than expanding the template
in REST. Core retrieves the synchronized definition, supplies reserved
machine/Site values, validates the definition hash, resolves artifact URLs,
and renders the final iPXE script.

If `allowOverride` is enabled, an Instance request can override the Operating
System's user data. See
[Tenant Management](tenant_management.md#launching-an-instance) for the full
Instance workflow.

## Update a Definition

Update only the fields that should change. Omitted fields retain their current
values. Supplying a parameter or artifact array replaces the complete
corresponding list; an explicit empty array clears it if the resulting
definition remains valid. Save the update body:

```bash
cat > templated-os-update.json <<'EOF'
{
  "description": "Ubuntu 24.04 automated installation, revision 2",
  "ipxeTemplateParameters": [
    {
      "name": "kernel_params",
      "value": "ip=dhcp autoinstall console=ttyS0,115200"
    }
  ]
}
EOF
```

Apply it with `nicocli`:

```bash
nicocli operating-system update \
  --data-file templated-os-update.json \
  "${OS_ID}"
```

Or apply it directly through REST:

```bash
curl --fail-with-body --silent --show-error \
  --request PATCH \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  --header "Content-Type: application/json" \
  --data @templated-os-update.json \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/operating-system/${OS_ID}"
```

The Operating System type and Site association cannot be changed. To target a
different Site, create another Operating System definition.

## Delete a Definition

```bash
nicocli operating-system delete "${OS_ID}"
```

The REST equivalent is:

```bash
curl --fail-with-body --silent --show-error \
  --request DELETE \
  --header "Authorization: Bearer ${NICO_TOKEN}" \
  "${NICO_BASE_URL}/v2/org/${NICO_ORG}/nico/operating-system/${OS_ID}"
```

REST marks the definition and its Site association as deleting, asks Core to
remove the Site copy, and records an actionable error state if Site cleanup
fails. Deletion is rejected while an Instance references the definition.

## Synchronization and Source of Truth

Templates and Operating Systems use different synchronization rules:

- **Templates:** one-way from Core to REST. REST aggregates the Public
  templates reported by authorized Sites.
- **Operating Systems with one Site association:** definition changes are
  bidirectional. REST compares update timestamps during inventory
  reconciliation.
- **Operating Systems with multiple Site associations:** REST is the source of
  truth so divergent Site definitions cannot overwrite one another.

REST creates a Templated iPXE Operating System with exactly one Site,
but the multi-Site rule protects existing or administratively-created records.
Reconciliation-by-absence applies only to single-Site definitions: if Core no
longer reports one at that Site, REST soft-deletes it. A multi-Site definition
is not deleted merely because one Site omits it.

When Core reports a previously unknown Operating System:

- A present `tenant_organization_id` resolves it as tenant-owned.
- An omitted `tenant_organization_id` resolves it as provider-owned using the
  reporting Site's Infrastructure Provider.

## Troubleshooting

| Symptom | Cause and resolution |
|---|---|
| No templates are returned for the Site | Confirm the Site is Registered, the template is Public, Core reports it, and the caller is authorized for the Site. |
| `exactly one siteId must be specified` | Send one UUID in `siteIds`; do not omit it or send multiple Sites. |
| Template is not available at the target Site | List templates with `--site-id "${SITE_ID}"` and use an ID from that response. |
| Missing or reserved parameter error | Compare the request with `requiredParams` and `reservedParams` from `ipxe-template get`. |
| Missing artifact error | Supply every occurrence in `requiredArtifacts`; names are matched case-insensitively. |
| Operating System is not synchronized to the Instance's Site | Wait for the Site association to become `Synced`, or create the definition for the Instance's Site. |
| Definition remains non-ready with `CachedOnly` | NICo does not populate the cache. Provide an external process that downloads every `CachedOnly` artifact and records its Site-local URL through Core's cache-management API, or use another strategy. |
| `authToken` is absent from GET responses | This is intentional redaction. Submit a replacement token on update when credentials rotate. |
| Tenant cannot select a provider-owned definition | Confirm the definition is associated and `Synced` with the Instance's Site and that the Tenant has access to that Site. |

Use `nicocli --debug` to inspect REST requests and responses, and inspect the
Operating System's `statusHistory` and `siteAssociations` for synchronization
failures.

## Related Documentation

- [Tenant Management](tenant_management.md) -- Tenant setup and Instance
  provisioning.
- [Phone-home](tenant_management.md#phone-home) -- Readiness behavior controlled
  by `phoneHomeEnabled`.
- [nico-admin-cli Operating System reference](../manuals/nico-admin-cli/commands/operating-system/operating-system.md)
  -- Direct Core gRPC administration.
- [nico-admin-cli iPXE template reference](../manuals/nico-admin-cli/commands/ipxe-template/ipxe-template.md)
  -- Inspect templates directly in Core.
