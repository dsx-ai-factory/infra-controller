# NICo mTLS and authorization

`nico-api` uses mutual TLS (mTLS) for all connections: the server presents a
certificate the client verifies against a root CA, and the client presents a
certificate the server verifies against a separate admin CA. Authorization is
then handled by a Casbin RBAC policy that maps certificate fields to roles and
gRPC method permissions.

## Generating client certificates

### Creating an admin CA and client cert with OpenSSL

The following creates a self-contained CA and client certificate. In
production you would typically use your organization's existing PKI
instead of a self-signed CA.

```sh
# 1. Generate the CA key and self-signed certificate
openssl ecparam -name prime256v1 -genkey -noout -out admin-ca.key
openssl req -x509 -new -key admin-ca.key -sha256 -days 3650 \
  -out admin-ca.crt \
  -subj "/O=ExampleCo/CN=ExampleCo NICo Admin CA"

# 2. Generate a client key
openssl ecparam -name prime256v1 -genkey -noout -out client.key

# 3. Create a CSR with operator identity in the subject
#    - O  = organization (matched by required_equals if configured)
#    - OU = group (used for role-based authorization via group_from)
#    - CN = username (used for audit logging via username_from)
openssl req -new -key client.key -out client.csr \
  -subj "/O=ExampleCo/OU=site-admins/CN=jdoe"

# 4. Create an extensions file for clientAuth
cat > client_ext.cnf <<EOF
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = clientAuth
EOF

# 5. Sign the client certificate with the CA
openssl x509 -req -in client.csr \
  -CA admin-ca.crt -CAkey admin-ca.key -CAcreateserial \
  -out client.crt -days 365 -sha256 \
  -extfile client_ext.cnf

# 6. Clean up intermediate files
rm -f client.csr client_ext.cnf admin-ca.srl
```

This produces:

| File | Purpose |
|------|---------|
| `admin-ca.crt` | Root CA -- configure as `admin_root_cafile_path` in nico-api |
| `admin-ca.key` | CA private key -- keep offline/secured |
| `client.crt` | Operator's client certificate |
| `client.key` | Operator's client private key |

### Certificate subject fields and how they map to authorization

The `[auth.cli_certs]` section in `nico-api-config.toml` controls how
certificate fields are interpreted:

| Config key | Purpose | Example |
|------------|---------|---------|
| `required_equals` | Issuer/subject fields that **must** match exactly for the cert to be accepted | `{ "IssuerO" = "ExampleCo", "IssuerCN" = "ExampleCo NICo Admin CA" }` |
| `group_from` | Which cert field to extract the authorization group from | `"SubjectOU"` |
| `username_from` | Which cert field to extract the username from (for audit trails) | `"SubjectCN"` |
| `username` | Fixed username for all certs of this type (alternative to `username_from`) | `"shared-admin"` |

The available `CertComponent` values are:

- `IssuerO`, `IssuerOU`, `IssuerCN` -- from the certificate issuer
- `SubjectO`, `SubjectOU`, `SubjectCN` -- from the certificate subject

## Server-side configuration (nico-api)

### TLS section

The `[tls]` section of `nico-api-config.toml` tells nico-api
where to find its own server certificate and which CAs to trust for
client authentication:

```toml
[tls]
identity_pemfile_path = "/path/to/server.crt"
identity_keyfile_path = "/path/to/server.key"
root_cafile_path = "/path/to/internal-ca.crt"
admin_root_cafile_path = "/path/to/admin-ca.crt"
```

| Key | Description |
|-----|-------------|
| `identity_pemfile_path` | Server's own TLS certificate (PEM) |
| `identity_keyfile_path` | Server's private key (PEM) |
| `root_cafile_path` | CA used to verify internal client certs |
| `admin_root_cafile_path` | CA used to verify external admin client certs |

nico-api loads both `root_cafile_path` and `admin_root_cafile_path`
into its TLS trust store. A client presenting a certificate signed by
either CA will pass the TLS handshake.

### Configuring authorization

Authorization is configured in the `[auth]` section of
`nico-api-config.toml`.

#### Casbin policy

nico-api uses [Casbin](https://casbin.org/) with an RBAC model for
authorization. The model is compiled into the binary and uses two rule
types:

- **`g` (grouping) rules** -- map a principal identifier to a role name
- **`p` (policy) rules** -- allow a principal or role to call a gRPC
  method (glob matching is supported on the method name)

The policy file is a CSV referenced by `casbin_policy_file`:

```toml
[auth]
permissive_mode = false
casbin_policy_file = "/path/to/casbin-policy.csv"
```

##### How principals are identified

| Certificate type | Principal identifier format | Example |
|-----------------|---------------------------|---------|
| External admin cert | `external-role/<group>` | `external-role/site-admins` |
| Any trusted cert | `trusted-certificate` | |
| No cert | `anonymous` | |

The `<group>` in `external-role/<group>` comes from the certificate
field specified by `group_from` in `[auth.cli_certs]`.

##### Writing policy rules

Sample policy file:

```csv
# On `g` rules: These associate a principal (second column) with a role name
# (third column). This causes the named role to also be looked up as if it were
# a principal.
#
# On `p` rules: These allow a principal or role (second column) to perform the
# named action (third column). Glob matching is available on the action field.
#


# Map the nico-dhcp SPIFFE ID to the nico-dhcp role.
# FIXME: verify that this is how these SPIFFE service identifiers look in reality.
g, spiffe-service-id/nico-dhcp, nico-dhcp
g, spiffe-service-id/nico-dns, nico-dns
g, spiffe-machine-id, machine

# Allow the nico-dhcp role to call its methods.
p, nico-dhcp, nico/DiscoverDhcp

# Same idea for nico-dns.
p, nico-dns, nico/LookupRecord

# Anonymous access to endpoints that don't modify state or expose any customer
# or site data should be fine.
p, anonymous, nico/Version

# Allow anonymous access to methods used by machines that may not have their
# certificates from us yet.
p, anonymous, nico/DiscoverMachine
p, anonymous, nico/ReportNicoScoutError
p, anonymous, nico/AttestQuote

# Allow anonymous access to methods used by dpu-agent. As of 2023-09-28 there
# are probably a fair amount of agents across the environments that don't have a
# certificate and are not ready for strict enforcement.
p, anonymous, nico/FindInstanceByMachineID
p, anonymous, nico/GetManagedHostNetworkConfig
p, anonymous, nico/RecordDpuNetworkStatus

# The client cert generated above has OU=site-admins in its subject.
# With group_from = "SubjectOU" in [auth.cli_certs], that becomes the
# principal "external-role/site-admins". Map it to a role and grant access.
g, external-role/site-admins, site-admin
p, site-admin, nico/*

# Example of a restricted role: a cert with OU=viewers would only get
# read access to a handful of methods.
g, external-role/viewers, viewer
p, viewer, nico/Version
p, viewer, nico/GetMachine
p, viewer, nico/ListMachines
p, viewer, nico/GetInstance
p, viewer, nico/ListInstances


# Allow any certificate we trust to hit any NICo method.
# FIXME: This should be removed once we have more fine-grained rule coverage.
p, trusted-certificate, nico/*
```

The method names in the `nico/<Method>` column correspond to the gRPC
method names defined in the protobuf service definitions. Glob matching
(`*`) is supported.

##### Full example: nico-api config with external admin certs

```toml
[tls]
identity_pemfile_path = "/path/to/server.crt"
identity_keyfile_path = "/path/to/server.key"
root_cafile_path = "/path/to/internal-ca.crt"
admin_root_cafile_path = "/path/to/admin-ca.crt"

[auth]
permissive_mode = false
casbin_policy_file = "/path/to/casbin-policy.csv"

[auth.cli_certs]
required_equals = { "IssuerO" = "ExampleCo", "IssuerCN" = "ExampleCo NICo Admin CA" }
group_from = "SubjectOU"
username_from = "SubjectCN"

[auth.trust]
spiffe_trust_domain = "nico.local"
spiffe_service_base_paths = [
  "/nico-system/sa/",
  "/default/sa/",
  "/elektra-site-agent/sa/",
]
spiffe_machine_base_path = "/nico-system/machine/"
additional_issuer_cns = []
```

With this configuration, a client certificate with subject
`/O=ExampleCo/OU=site-admins/CN=jdoe` and issuer
`/O=ExampleCo/CN=ExampleCo NICo Admin CA` would:

1. Pass the `required_equals` check (IssuerO and IssuerCN match)
2. Be assigned group `site-admins` (from SubjectOU)
3. Be identified as user `jdoe` (from SubjectCN)
4. Receive the principal `external-role/site-admins`
5. Be authorized according to whatever casbin policy rules match that
   principal

You can see an example of a complete nico-api configuration file in [full_config.toml](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/cfg/test_data/full_config.toml)

### Permissive mode

Setting `permissive_mode = true` in the `[auth]` section causes the
authorization engine to **allow all requests**, even when the casbin
policy would deny them. Denied requests are logged with a warning
instead of being rejected:

```toml
[auth]
permissive_mode = true
```

When permissive mode is active, nico-api logs messages like:

```text
WARN The policy engine denied this request, but --auth-permissive-mode overrides it.
```

**Use permissive mode only for:**

- Initial deployment and bring-up, before certificates are fully
  configured
- Debugging authorization issues (enable temporarily, check logs, then
  disable)
- Development environments

**Do not leave permissive mode enabled in production.** It bypasses all
authorization checks. Any client that can complete the TLS handshake
(or any client at all, if TLS is also disabled) can call any API method.

You can also set permissive mode via environment variable without
editing the config file:

```sh
NICO_API_AUTH="{permissive_mode=true}"
```
