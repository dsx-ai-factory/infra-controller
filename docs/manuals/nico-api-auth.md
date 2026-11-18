# NICo mTLS and authorization

The admin CLI uses mTLS for administrative requests. Sites may retain their
Vault-issued client certificates or use an operator-managed admin PKI; neither
path requires JWT support in the CLI.

## Keep the two trust directions separate

- The CLI's `--root-ca-path` verifies **the API server**. Keep the site's server
  CA here; an independent admin CA does not replace it.
- The API's `[tls].root_cafile_path` and optional
  `[tls].admin_root_cafile_path` form its combined **client** trust store.
  Adding an admin CA does not replace the service or machine CA.
- TLS trust is not sufficient for admin access. The certificate must also pass
  Casbin authorization and the internal RBAC rules.

## Generating client certificates

Use the site's existing Vault issuance procedure or ask the operator's PKI
owner for a PEM client certificate chain and its matching unencrypted PEM
private key (PKCS#1, PKCS#8, or SEC1).
NICo does not require a particular external PKI product or issue these
operator credentials automatically.

For an independent admin PKI:

- Use a dedicated admin-client issuing intermediate whose entire issuance
  population is intended to have admin access. Its Subject CN must be globally
  unique across the API's combined trust store and encoded as an ASN.1
  `PrintableString`; the client leaf's Issuer CN must match it.
- The leaf must be valid for TLS client authentication, with the appropriate
  `clientAuth` extended key usage and signing key usage. Include any
  intermediates needed to build its chain to the configured trust anchor.
- Include the operator's identity in Subject CN, encoded as `PrintableString`.
  Some administrative operations require this identity, not just TLS trust.
  Optional Subject O/OU values supply organization/group audit information
  and must also use `PrintableString` to be parsed. Under the default internal
  RBAC rules, the group label does not restrict an admin's permissions.

Protect private keys at their source and on the CLI host. Only public CA
certificates belong in the API's trust ConfigMap; never install a CA private
key there.

### Creating an admin CA and client cert with OpenSSL

For a local test, the following creates an admin root, a dedicated issuing
intermediate, and a client certificate. In production, use the site's PKI
procedure instead. Replace the example issuer CN with a globally unique value;
the sample lifetimes are illustrative, not a production renewal policy.

Save this configuration as `admin-client.cnf`. `string_mask = default` allows
the ASCII subject values below to be encoded as `PrintableString`.

```ini
[req]
prompt = no
distinguished_name = dn
string_mask = default
[dn]
CN = unused
[root]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
[intermediate]
basicConstraints = critical,CA:TRUE,pathlen:0
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
[client]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = clientAuth
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
```

Run in a protected working directory:

```sh
umask 077

# Generate the root CA.
openssl ecparam -name prime256v1 -genkey -noout -out admin-ca.key
openssl req -x509 -new -key admin-ca.key -sha256 -days 3650 \
  -config admin-client.cnf -extensions root -out admin-ca.crt \
  -subj "/O=ExampleCo/CN=ExampleCo NICo Admin CA"

# Generate a dedicated admin-client issuing intermediate.
openssl ecparam -name prime256v1 -genkey -noout -out admin-issuer.key
openssl req -new -key admin-issuer.key -config admin-client.cnf \
  -out admin-issuer.csr -subj "/O=ExampleCo/CN=ExampleCo NICo Admin Issuer"
openssl x509 -req -in admin-issuer.csr \
  -CA admin-ca.crt -CAkey admin-ca.key -CAcreateserial \
  -out admin-issuer.crt -days 1825 -sha256 \
  -extfile admin-client.cnf -extensions intermediate

# Generate the operator's client certificate.
openssl ecparam -name prime256v1 -genkey -noout -out client.key
openssl req -new -key client.key -config admin-client.cnf -out client.csr \
  -subj "/O=ExampleCo/OU=site-admins/CN=jdoe"
openssl x509 -req -in client.csr \
  -CA admin-issuer.crt -CAkey admin-issuer.key -CAcreateserial \
  -out client.crt -days 365 -sha256 \
  -extfile admin-client.cnf -extensions client

# Bundle the leaf and intermediate, then verify the chain and client purpose.
openssl crl2pkcs7 -nocrl -certfile client.crt -certfile admin-issuer.crt | \
  openssl pkcs7 -print_certs -out client-chain.crt
openssl verify -purpose sslclient -CAfile admin-ca.crt \
  -untrusted admin-issuer.crt client.crt
```

| File | Purpose |
|------|---------|
| `admin-ca.crt` | Public admin trust anchor for the API |
| `admin-ca.key`, `admin-issuer.key` | CA private keys; keep secured, never deploy as API or CLI credentials |
| `admin-issuer.crt` | Dedicated admin-client issuing intermediate |
| `client-chain.crt` | Operator's client certificate and intermediate for the CLI |
| `client.key` | Operator's matching private key for the CLI |

For this example, the issuer-CN mapping is `ExampleCo NICo Admin Issuer`, not
the root's CN. Only the public root goes in `adminRootCertPem`; the CLI uses
`client-chain.crt` and `client.key` alongside the site's unchanged server CA.

## Configure API trust and authorization

For Helm deployments, follow the
[admin-client certificate configuration](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/PREREQUISITES.md#admin-client-certificates):

1. Set `nico-api.siteConfig.adminRootCertPem` to the public PEM trust bundle.
   Its default is empty; the optional bundle is materialized only when set,
   and requires `nico-api.siteConfig.enabled=true`.
2. Add the leaf's dedicated issuing-intermediate CN to
   `nico-api.auth.additionalIssuerCns`. The chart default is an empty list.
   Preserve every existing issuer CN that must continue to authenticate;
   replacing this list is not an additive update.
3. Apply those overrides through the site's Helm release workflow, preserving
   its other values. Changes to these chart values roll the API Deployment,
   loading both the trust bundle and issuer mapping.

The default `nico-api.auth.adminRootCafilePath` selects
`/etc/forge/carbide-api/site/admin_root_cert_pem`. The prerequisites document
also covers the alternate mounted path and the PEM-envelope validation rules.
Helm does not validate X.509 CA constraints, validity, or chain placement;
the PKI owner must validate those before installation.

The issuer mapping is `[auth.trust].additional_issuer_cns` in the API's TOML
configuration. It classifies certificates as `ExternalUser` by issuer CN
across the combined TLS trust store; it is **not** bound cryptographically to
the optional admin bundle. Do not list a shared or root CA CN for the new
admin issuer.

Every successfully verified client certificate is also a
`TrustedCertificate`. The default chart's Casbin `forge/*` rule accepts that
principal, but internal RBAC still requires an `ExternalUser` for the
`ForgeAdminCLI` access path. Keep `bypass_rbac=false` and
`nico-api.auth.permissiveMode=false` in production. Permissive mode bypasses
Casbin only, not internal RBAC.

The issuer-CN mapping takes precedence over custom `[auth.cli_certs]` criteria:
matching certificates skip that section's field restrictions and identity
extraction. If those criteria define the site's admin boundary, do not add
the issuer to `additionalIssuerCns` in step 2. Retain or configure the custom
criteria instead, and verify them with the site's Casbin and internal RBAC
policies. Use the issuer shortcut only when its dedicated issuance population
is intended to have admin access.

### Server-side configuration (nico-api)

For direct TOML configuration, the TLS settings are:

```toml
[tls]
identity_pemfile_path = "/path/to/server.crt"
identity_keyfile_path = "/path/to/server.key"
root_cafile_path = "/path/to/internal-ca.crt"
admin_root_cafile_path = "/path/to/admin-ca.crt"
```

| Key | Description |
|-----|-------------|
| `identity_pemfile_path` | Server's own PEM certificate chain |
| `identity_keyfile_path` | Server's matching unencrypted EC private key in SEC1 PEM format |
| `root_cafile_path` | Public service/machine client trust bundle |
| `admin_root_cafile_path` | Optional additional public client trust bundle |

The API combines both client trust bundles; these are not isolated trust
stores. The optional admin path may be empty. The paths above are examples;
preserve the site's existing server identity and service/machine trust.

### Certificate subject fields and how they map to authorization

For sites using custom `[auth.cli_certs]` criteria instead of the issuer-CN
shortcut, the following fragment retains the field-based approach:

```toml
[auth.cli_certs]
required_equals = { "IssuerO" = "ExampleCo", "IssuerCN" = "ExampleCo NICo Admin Issuer" }
group_from = "SubjectOU"
username_from = "SubjectCN"
```

| Config key | Purpose |
|------------|---------|
| `required_equals` | Required map comparing encountered issuer/subject components to configured values |
| `group_from` | Optional certificate component supplying the authorization group; omitted means an empty group |
| `username_from` | Optional component supplying the user name when present |
| `username` | Optional fixed user name, used if no user name is extracted |

`required_equals` accepts `IssuerO`, `IssuerOU`, `IssuerCN`, `SubjectO`,
`SubjectOU`, and `SubjectCN`, with that exact spelling. Identity extraction
through `group_from` and `username_from` supports only the subject components;
issuer selectors do not extract an identity. Parsed values use
`PrintableString`. Missing components are not rejected by `required_equals`,
so the PKI's issuance profile must supply the required identity fields.
Without an extracted or fixed user name, operations requiring a user identity
fail. The issuer-CN shortcut takes precedence over this section as described
above; keep this issuer out of `additional_issuer_cns` when using these criteria.

### Casbin policy

The API's compiled [Casbin](https://casbin.org/) RBAC model uses `g` rules to
map principals to roles and `p` rules to allow a principal or role to call a
method. Method permissions use `forge/<Method>` and support glob matching.
The policy file is selected by:

```toml
[auth]
permissive_mode = false
casbin_policy_file = "/path/to/casbin-policy.csv"
```

`casbin_policy_file` is optional: omitting it disables the Casbin layer, not
internal RBAC. A configured file must load successfully for the listener to
start. Keep the site's policy configured when relying on its restrictions.

| Principal | Identifier |
|-----------|------------|
| External admin identity | `external-role/<group>` |
| Any TLS-verified client certificate | `trusted-certificate` |
| Request without a client credential | `anonymous` |

The external group comes from Subject OU for the issuer shortcut, or from
`group_from` for custom criteria. A client can have both external and trusted
principals; a grant for either passes Casbin, but internal RBAC still applies.

Example policy fragments, not a replacement for the site's full policy:

```csv
g, external-role/site-admins, site-admin
p, site-admin, forge/*
g, external-role/viewers, viewer
p, viewer, forge/FindMachineIds
p, viewer, forge/FindMachinesByIds
p, anonymous, forge/Version
```

Group-based restrictions do not constrain a credential while a broader
`p, trusted-certificate, forge/*` rule also grants it access. Retain required
service/machine rules and verify both Casbin and internal RBAC before relying
on restricted roles. A complete API configuration example is in
[full_config.toml](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/cfg/test_data/full_config.toml).

### Permissive mode

`[auth].permissive_mode=true` logs Casbin denials instead of rejecting them.
It does not bypass TLS verification or internal RBAC. Use it only for
development or a bounded authorization-debugging session, then restore
`false`; keep it disabled in production.

## Install and verify the CLI credential

Install the certificate chain and matching private key on the operator's CLI
host using the site's protected-file or Secret-mount procedure. Supply both
paths through the existing CLI inputs; no new authentication option is needed.
The [CLI connection guide](./nico-admin-cli.md#tls-options) documents flags,
environment variables, config-file keys, and fallback behavior.

Verify a protected, read-only operation:

```sh
nico-admin-cli \
  --api-url https://nico-api.example.com:1079 \
  --root-ca-path /etc/nico/certs/server-ca.crt \
  --client-cert-path /etc/nico/certs/admin-client.crt \
  --client-key-path /etc/nico/certs/admin-client.key \
  machine show
```

Replace the URL and paths with the site's values. Connection options precede
the subcommand. Leave `DISABLE_TLS_ENFORCEMENT` unset; setting it, even to an
empty value, disables server-certificate verification. With that override
unset, a successful query verifies server trust, client authentication, and
permission to read machines; an empty inventory is also a valid result.
`version` is only a connectivity check: it neither verifies the server
certificate nor sends a client certificate, and the API allows it anonymously.

Before relying on the new issuer, confirm that the existing Vault-issued
credential still works and that a certificate from an untrusted issuer is
rejected. A trusted certificate without an admin identity must not gain admin
access under the site's policy.

## Renewal, CA overlap, and recovery

The operator's PKI owns issuance and renewal. Before a leaf expires, obtain its
replacement, install the new chain/key together, and repeat the protected
query. If the files are lost or installation fails, repeat issuance and
installation; restore the site's configuration or trust bundle if that was
the cause. NICo does not automatically renew a CLI host's credential.

Changing the issuing CA is optional. Stage the new public trust anchor
alongside the old one and retain both issuer CN mappings, then apply the Helm
values. Verify old and new credentials, move operators to the new credentials,
and only then remove the retired admin anchor and mapping. Do not remove the
site's service/machine CA as part of an admin-only change. Verify the retired
credential is denied after the rollout; custom authorization mappings must
also stop granting it access.

## Revocation support boundary

This procedure preserves the existing CLI mTLS support level. The API listener
does not configure CRL or OCSP enforcement for client certificates; revoking a
leaf at the issuer alone does not cause the API to reject it. A leaf's
expiry or retiring its admin issuer's trust/authorization is the existing
containment mechanism, not a new per-certificate revocation feature.

For issuer retirement, remove its admin trust anchor and every mapping that
grants it admin access, apply the configuration, and verify rejection with the
old credential on a new connection. A trust-file refresh does not revalidate
already-established connections; completing the API rollout closes the old
connections. Issuer retirement affects all credentials from that issuer.
