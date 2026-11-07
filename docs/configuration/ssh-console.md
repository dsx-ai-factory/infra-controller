# SSH Console Setup and Access

SSH Console is one of the Core NICo services. It connects you to an Instance's
serial console through its BMC, providing access to boot output and the
operating system's console even when the Instance's network is unavailable.
The SSH username identifies the Instance, and an SSH key attached through a
key group authorizes the connection.

This guide covers service setup for Site operators and console access for
Tenants. Follow the [Quick Start Guide](../getting-started/quick-start.md) to
install NICo, and [Tenant Management](tenant_management.md) to provision an
Instance.

## Prerequisites

- A deployed SSH Console service with access to Core and the managed BMCs.
- An SSH endpoint reachable from the client's network, with a DNS name or IP
  address and an SSH port supplied by the Site operator.
- A Provider Admin account to advertise the endpoint on the Site resource.
- A Tenant Admin account to manage SSH keys and key groups for the Instance's
  Tenant, and an Instance on that Site.
- `nicocli`, `jq`, and an OpenSSH client on the client machine.

Configure REST authentication using the
[nicocli reference](../manuals/nicocli-reference.md). The examples use separate
Provider and Tenant configurations so each operation runs in the correct
organization. In both configurations, set `api.name` to match the deployment's
configured API name. The default is `nico`.

## Set up the service

### Deploy and expose SSH Console

The NICo umbrella Helm chart installs `nico-ssh-console-rs` as a required Core
component. Its installation is independent of the Site's REST settings.
Deployments that install components individually must also install SSH Console
and its prerequisites. See the
[Reference Installation](../getting-started/installation-options/reference-install.md)
for the prerequisite and Core installation procedures.

The chart creates an internal SSH Service on TCP port `22`. An external Service
is optional: `externalService.enabled` defaults to `false` in the subchart,
while `helm-prereqs/values/nico-core.yaml` enables it for the reference
installation. Add the following settings to your existing Core values, replacing
the example VIP with an address from your load balancer's pool:

```yaml
nico-ssh-console-rs:
  externalService:
    enabled: true
    port: 22
    annotations:
      metallb.universe.tf/loadBalancerIPs: "192.0.2.40"
```

The VIP annotation above is specific to MetalLB. Apply the values through your
Core deployment procedure. Create a DNS record such as `console.example.com`
for the external address, and allow client traffic to the configured SSH port.
The client must be able to reach the VIP even if it comes from an internal
address pool.

`externalService.port` changes the client-facing port, the backend still uses
port `22`. For example, an external port of `2222` requires `ssh -p 2222`.
Changing `service.port` alone does not change the SSH listener.

The service also requires:

- An SSH host key in the `ssh-host-key` Secret in the service's namespace. The
  private key must use OpenSSH format. The reference install creates it during
  prerequisite setup.
- The service's client certificate, private key, and trusted CA for mTLS to
  Core. The chart mounts these from its certificate Secret.
- Network access and valid credentials for the managed BMCs. SSH Console
  obtains BMC metadata and credentials from Core and uses the supported SSH or
  IPMI serial-over-LAN (SOL) transport for each BMC. See
  [BMC and Out-of-Band Setup](../getting-started/prerequisites/bmc-oob-setup.md).

The packaged configuration sets `insecure = false`, keep it disabled so the
service checks client authentication. A non-empty `configFiles.config` replaces
the complete packaged TOML configuration. If you use that override, preserve
the connection, certificate, host-key, and authentication settings your
deployment requires.

### Verify the endpoint

The following commands use the reference namespace and resource names. Adjust
them if your deployment overrides those names.

```bash
kubectl -n nico-system rollout status deployment/nico-ssh-console-rs
kubectl -n nico-system get service nico-ssh-console-rs-external
kubectl -n nico-system logs deployment/nico-ssh-console-rs -c ssh-console-rs --tail=100
```

Confirm that the external Service has an address, DNS resolves to that address,
and the client can reach its SSH port. Pod readiness checks the SSH listener;
it does not prove that a particular BMC console is available.

Provide the SSH host-key fingerprint to clients through a trusted channel. A
Site operator can read the public key's fingerprint with:

```bash
kubectl -n nico-system get secret ssh-host-key \
  -o jsonpath='{.data.ssh_host_ed25519_key_pub}' |
  base64 --decode | ssh-keygen -lf -
```

### Advertise the endpoint in REST

Use the Provider configuration to find the Site:

```bash
PROVIDER_CONFIG="$HOME/.nico/provider.yaml"

nicocli --config "$PROVIDER_CONFIG" site list --all |
  jq '.[] | {id, name, status, isOnline, serialConsoleHostname}'
```

Choose the Site that owns the Instance. A paired Site has status `Registered`;
use `isOnline` to check whether it is communicating with the REST control plane.
Set its SSH hostname using the returned Site ID:

```bash
SITE_ID='<site-id>'

nicocli --config "$PROVIDER_CONFIG" site update \
  --serial-console-hostname console.example.com "$SITE_ID"
```

`serialConsoleHostname` accepts a hostname or IP address. Supply the host only,
without `ssh://`, a username, or a port. REST uses this value to construct each
Instance's `serialConsoleUrl`. It does not create DNS records, deploy the
service, or authorize connections.

The Site fields `isSerialConsoleEnabled`, `serialConsoleIdleTimeout`, and
`serialConsoleMaxSessionLength` are deprecated settings with no effect on SOL.
Do not use them to enable access, revoke access, or enforce session limits.
`isSerialConsoleSSHKeysEnabled` is also deprecated and cannot be changed to
control access. Manage the Instance's key-group attachments and the key group's
keys and Site associations instead.

## Connect as a Tenant

Use the Tenant configuration for all commands in this section. The SSH key,
key group, and Instance must belong to the same Tenant. The key group must also
be associated with the Instance's Site.

### Register a public key

Use an existing SSH key pair, or generate one with `ssh-keygen`. Keep the
private key on the client. Register only its public key:

```bash
TENANT_CONFIG="$HOME/.nico/tenant.yaml"
SSH_KEY_PATH="$HOME/.ssh/id_ed25519"

nicocli --config "$TENANT_CONFIG" ssh-key create \
  --name console-access \
  --public-key "$(cat "${SSH_KEY_PATH}.pub")"
```

Save the returned `id` as `SSH_KEY_ID`. Registering this key authorizes its use
through SSH Console after the following attachment steps. It does not install
the key in the Instance operating system's `authorized_keys`.

### Create and synchronize a key group

Create a group containing the public key and associate it with the Site:

```bash
SSH_KEY_ID='<ssh-key-id>'

jq -n --arg site "$SITE_ID" --arg key "$SSH_KEY_ID" \
  '{name: "console-access", siteIds: [$site], sshKeyIds: [$key]}' |
  nicocli --config "$TENANT_CONFIG" ssh-key-group create --data-file -
```

Save the returned group `id` as `SSH_KEY_GROUP_ID`. Synchronization to Core is
asynchronous. Check the association for the target Site:

```bash
SSH_KEY_GROUP_ID='<ssh-key-group-id>'

nicocli --config "$TENANT_CONFIG" ssh-key-group get "$SSH_KEY_GROUP_ID" |
  jq --arg site "$SITE_ID" \
    '{version, status, siteAssociations: [.siteAssociations[] | select(.site.id == $site)]}'
```

Wait for that association's `status` to become `Synced` and its `version` to
match the group's top-level `version`. If synchronization fails, inspect the
group's `statusHistory` and the Site's `isOnline` value before attempting to
connect.

### Attach the group to an Instance

Inspect the Instance and its existing attachments:

```bash
INSTANCE_ID='<instance-id>'

nicocli --config "$TENANT_CONFIG" instance get "$INSTANCE_ID" |
  jq '{id, name, siteId, status, sshKeyGroupIds, serialConsoleUrl}'
```

The following update sets the Instance's complete list of SSH key groups to
the selected group. If the Instance already has groups that should retain
access, include all their IDs in `sshKeyGroupIds` as well.

```bash
jq -n --arg group "$SSH_KEY_GROUP_ID" '{sshKeyGroupIds: [$group]}' |
  nicocli --config "$TENANT_CONFIG" instance update --data-file - "$INSTANCE_ID"
```

You can also supply `sshKeyGroupIds` when creating an Instance. On an update,
omitting the field or sending `null` preserves existing attachments, while
`[]` removes all attachments. After an update, retrieve the Instance again to
check `sshKeyGroupIds` and any `statusHistory` errors.

### Open the serial console

Read the URL from the Instance response and connect with the matching private
key. Run the SSH command only after the URL command succeeds:

```bash
CONSOLE_URL=$(nicocli --config "$TENANT_CONFIG" instance get "$INSTANCE_ID" |
  jq -er '.serialConsoleUrl | select(type == "string" and length > 0)')

ssh -o IdentitiesOnly=yes -i "$SSH_KEY_PATH" "$CONSOLE_URL"
```

The URL has the form `ssh://<instance-id>@<console-host>`. The Instance ID is
the same in REST and Core. Use the returned URL to connect to the endpoint
advertised for the Site. The URL is `null` until REST records the Instance's
creation in Core and the Site has a console hostname. Its presence does not
prove that the service is reachable or that your key is authorized.

REST does not include a custom SSH port in the URL. If the Site operator
configured an external port such as `2222`, use:

```bash
ssh -o IdentitiesOnly=yes -i "$SSH_KEY_PATH" -p 2222 "$CONSOLE_URL"
```

On the first connection, compare the host-key fingerprint with the value from
the Site operator before accepting it. Once connected, press Enter to request
the console prompt. The operating system may require its own login credentials;
SSH Console authentication does not log you into the operating system.
With the default OpenSSH escape character, press Enter followed by `~.` to
disconnect.

## Remove or change access

To remove one group's access to an Instance, update `sshKeyGroupIds` with only
the groups that should remain. To remove all Tenant key-group access to that
Instance:

```bash
nicocli --config "$TENANT_CONFIG" instance update \
  --data '{"sshKeyGroupIds": []}' "$INSTANCE_ID"
```

To remove a key across a group's Instances, update the group's `sshKeyIds`.
To remove the group from a Site, update its `siteIds`. Each supplied list
replaces the corresponding list, include every ID you want to retain.
Group updates require the latest `version` from `ssh-key-group get`; fetch
the group again if an update reports a version mismatch. Wait for the relevant
Site synchronization before checking a new connection.

Key authorization happens when a client authenticates. Removing a key or
attachment does not terminate an already authenticated SSH session. Changing
`serialConsoleHostname` or `isSerialConsoleEnabled` does not revoke access.

## Operator access

Site operators can configure OpenSSH certificate authentication independently
of Tenant key groups. The chart's
`sshConsole.opensshCertificateCaFingerprints` setting supplies trusted signing
CA fingerprints, and `sshConsole.adminCertificateRole` supplies the role
required in the certificate's key ID. Both are empty by default. A configured
operator certificate can authenticate using a Machine ID or Instance ID.

Certificate issuance and distribution belong to the Site's SSH CA procedure.
These chart settings are ignored when `configFiles.config` supplies the complete
TOML file; configure the corresponding `openssh_certificate_ca_fingerprints`
and `admin_certificate_role` fields in that file instead. Operator certificate
access is unaffected by removing Tenant key-group attachments.

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| `serialConsoleUrl` is `null` | Ask the Provider to check `serialConsoleHostname`, and check whether the Instance has been created in Core. |
| DNS failure, connection timeout, or connection refused | Check the external Service address, DNS, client route, firewall, and SSH port. A REST hostname or enablement flag does not verify service availability. |
| `Permission denied (publickey)` | Use the returned URL and matching private key. Check the Instance's complete `sshKeyGroupIds` list and the target Site's synchronized key-group version. A Tenant key authenticates with the Instance ID as the SSH username. |
| Key group cannot be attached | Check that the group belongs to the Instance's Tenant and has an association with the Instance's Site. |
| Key group stays unsynchronized | Inspect its `statusHistory`, Site connectivity, and Site Agent logs. Wait for `Synced` before retrying authentication. |
| SSH succeeds but the console reports it is disconnected | Check SSH Console logs, BMC network access, credentials, and serial-over-LAN setup. Press Enter to request connection status. |
| IPMI SOL reports an existing session | Close the conflicting session through the Site's BMC procedure. Automatic forced deactivation is disabled by default because it can disconnect another operator. |
| Console is connected but has no login prompt | Check host power, boot progress, and the operating system's serial-console configuration. |
| Host-key fingerprint changes | Ask the Site operator to verify whether the SSH host key changed before updating the client's saved key. |

For service and console log collection, see
[Logging](../observability/logging.md) and
[Machine and DPU Logs](../observability/machine-dpu-logs.md). SSH Console
maintains BMC connections and captures output independently of interactive
client sessions when console logging is enabled.
