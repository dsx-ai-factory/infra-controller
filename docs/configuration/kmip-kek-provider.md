# KMIP KEK Provider

A KMIP key encryption key (KEK) provider keeps the key that protects the NICo credentials store inside a KMIP 1.4 server such as a hardware security module or an enterprise key manager. NICo generates each per-credential data encryption key (DEK) locally, sends only that DEK to the server's `Encrypt` operation, and stores the returned ciphertext, IV, and authentication tag next to the credential. Unwrapping uses the matching `Decrypt` operation. The KEK is marked non-exportable on the server and never enters NICo configuration or memory, which is the same custody model as Vault/OpenBao Transit without the Vault dependency.

This page covers what the server must provide, how to create the key and the client identity, how to mount the TLS material through Helm, the `[secrets.kms]` configuration, what `nico-api` verifies at startup, day-to-day operation, and the qualification status of server products. The surrounding credentials-store model, routing, rotation, and migration walk are in [Secrets Storage](secrets-storage.md).

## What the Server Must Provide

The client is deliberately narrow. It speaks the KMIP 1.4 TTLV protocol over mutually authenticated TLS and uses five operations:

| Operation | When | Purpose |
|---|---|---|
| `Discover Versions` | startup | The server must list protocol version 1.4. |
| `Query` | startup | The server must list the `Encrypt` and `Decrypt` operations. |
| `Get Attributes` | startup | Each configured key must be `Active`, algorithm `AES`, length 256 bits, with `Encrypt` and `Decrypt` in its cryptographic usage mask. |
| `Encrypt` | every credential write and re-wrap | Single-part AES-GCM with no padding. The request asks for a server-generated IV (`Random IV`, 96-bit `IV Length`) and a 16-byte tag. The response must carry a non-empty IV in `IV/Counter/Nonce`, exactly 16 tag bytes in `Authenticated Encryption Tag`, and ciphertext as long as the plaintext. The IV is stored as returned, whatever its length. |
| `Decrypt` | every credential read and re-wrap | The stored IV and tag are sent as separate fields together with the ciphertext. |

The provider never creates, activates, rotates, revokes, or destroys keys. Multi-part operations, additional authenticated data, and KMIP 2.x wire encodings are not used. The client checks the shape of every `Encrypt` response, so a server that omits the IV, returns a tag of another length, or appends the tag to the ciphertext instead of returning it in its own field fails the startup probe described below. The client cannot tell whether the IV it receives is fresh: a server that ignores `Random IV` and returns a fixed IV passes the probe, and a repeated IV under the same key breaks AES-GCM. Fresh IVs on every `Encrypt` are therefore a qualification requirement, checked as described in [Qualification Checklist](#qualification-checklist), not a startup check.

## Create the KEK and the Client Identity

Perform these steps on the KMIP server with its own tools. Names and menus differ by product; the KMIP concepts do not.

1. Create a symmetric key with algorithm `AES`, length `256`, a cryptographic usage mask containing `Encrypt` and `Decrypt`, and the server's export protection turned on. In KMIP terms that is the `Extractable` attribute set to `False`; a server sets it to `True` when the request omits it (section 3.50 of the [KMIP 1.4 specification](https://docs.oasis-open.org/kmip/spec/v1.4/os/kmip-spec-v1.4-os.html)), and products often expose it as their own export or extractability control. Confirm the setting with the product's own tools after creation, because NICo's startup checks read the key's state, algorithm, length, and usage mask, not its extractability. Do not set usage limits on the key: the client may repeat an `Encrypt` once on a stale connection, and a usage counter would count that repeat.
2. Activate the key and record its KMIP Unique Identifier. That identifier, not a name, goes into the NICo configuration.
3. Issue a TLS client certificate for `nico-api` from a CA the server trusts, and map that certificate to a server-side identity whose permissions on the key are limited to `Encrypt`, `Decrypt`, `Get Attributes`, `Query`, and `Discover Versions`. The identity must not be able to export, rotate, revoke, or destroy the key.
4. Collect the PEM bundle of the CA that signed the server certificate. The provider trusts only that bundle, never system roots.

Rotation is a NICo routing change, as for every provider, and it takes two rollouts because a rolling update runs old and new pods side by side, as described in [Before You Start](secrets-storage.md#before-you-start). Create and activate a second key on the server and add it to `keys` under a new `kek_id`, leaving `[secrets.routing]` unchanged; restart and wait for every old pod to terminate, so that no replica can meet a `kek_id` it does not know. Then point `[secrets.routing]` at the new `kek_id`, restart again, and run `nico-admin-cli secrets re-wrap` as described in [Rotating and Retiring Keys](secrets-storage.md#rotating-and-retiring-keys). Keep the old key active on the server until the re-wrap reports no stale rows and every retained database dump that references it has aged out.

## Mount the TLS Material

Put the CA bundle, the client certificate, and the client key in a Kubernetes Secret and mount it into `nico-api` with the chart's `extraVolumes` and `extraVolumeMounts` values. The example below uses the reference installation's nesting under `nico-api`:

```yaml
nico-api:
  extraVolumes:
    - name: kmip-client
      secret:
        secretName: nico-kmip-client
        defaultMode: 0400
  extraVolumeMounts:
    - name: kmip-client
      mountPath: /etc/nico/kmip
      readOnly: true
```

```bash
kubectl -n nico-system create secret generic nico-kmip-client \
  --from-file=ca.pem=./kmip-ca.pem \
  --from-file=client.pem=./nico-api-kmip.pem \
  --from-file=client.key=./nico-api-kmip.key
```

The files are read again on every new connection, so a renewed certificate takes effect at the next reconnect without a restart. The provider reconnects after a connection failure and before reusing a connection that has been idle for 30 seconds, so a renewed certificate is picked up within that time once traffic pauses. A connection in continuous use keeps the identity it was opened with until then or until `nico-api` restarts.

## Configure the Provider

Add a `kmip` provider to `[secrets.kms]` in the site TOML that the chart renders from `siteConfig.nicoApiSiteConfig`. The example makes the KMIP server the active provider for a new site; a site migrating from Transit keeps both providers configured, as the migration section below describes.

```toml
[secrets]
backends = ["postgres"]
writer = "postgres"

[secrets.kms]
active = "hsm"

[secrets.kms.providers.hsm]
type = "kmip"
endpoint = "kms.example.com:5696"
ca_bundle = "/etc/nico/kmip/ca.pem"
client_cert = "/etc/nico/kmip/client.pem"
client_key = "/etc/nico/kmip/client.key"
keys = { "site-kek-1" = "f0a1c2d3-5e6f-4a7b-8c9d-0e1f2a3b4c5d" }

[secrets.routing]
"/" = "site-kek-1"
```

| Field | Required | Default | Meaning |
|---|---|---|---|
| `endpoint` | yes | | `host:port` or `[v6-literal]:port` of the KMIP TLS listener. The host must be a DNS name or an IP address, and an IPv6 literal must be bracketed. Without a port, the registered KMIP port `5696` is used. |
| `server_name` | no | host part of `endpoint` | The name the server certificate must match. A DNS name is also sent as SNI; an IP address is matched against the certificate's IP SANs and no SNI is sent. Set it when the endpoint is an IP address whose certificate carries only a DNS name, or a load balancer whose certificate carries a different name. |
| `ca_bundle` | yes | | Path of the PEM bundle trusted to sign the server certificate. |
| `client_cert` | yes | | Path of the PEM client certificate chain. |
| `client_key` | yes | | Path of the PEM private key (PKCS#8, PKCS#1, or SEC1) for `client_cert`. |
| `keys` | yes, non-empty | | Each `kek_id` and the KMIP Unique Identifier of its key. Every `kek_id` routed to this provider must appear here, and a `kek_id` cannot appear in two providers. |
| `connect_timeout` | no | `5s` | Per-attempt bound on reading the TLS files, the TCP connect, and the TLS handshake together. At most `60s`. |
| `request_timeout` | no | `10s` | Per-attempt bound on writing a request and reading its complete response. It also bounds how long a caller waits for the single connection behind another caller. At most `120s`. |

Every value in this table is read at startup, so a change to the TOML requires a `nico-api` restart, as for the rest of `[secrets]`. The files behind `ca_bundle`, `client_cert`, and `client_key` are the exception: the provider reads them again whenever it opens a connection, as described above.

## What Startup Verifies

Before `nico-api` serves traffic it connects to the server and checks, in this order, failing the boot with a message that names the field at fault:

1. The TLS files are readable and valid PEM, the certificate matches the key, and the endpoint host and `server_name` are valid DNS names or IP addresses.
2. `Discover Versions` lists KMIP 1.4.
3. `Query` lists `Encrypt` and `Decrypt`.
4. For every entry in `keys`, `Get Attributes` reports state `Active`, algorithm `AES`, length `256`, and a usage mask that permits `Encrypt` and `Decrypt`.
5. For every entry in `keys`, a throwaway 32-byte value wraps and unwraps through the server. This proves that the server answers `Encrypt` with a non-empty IV, a 16-byte tag in its own field, and ciphertext of the plaintext's length, and that `Decrypt` returns the original value. It does not prove that the IV is fresh on every call; see [Qualification Checklist](#qualification-checklist).

An unreachable server therefore stops `nico-api` from starting rather than failing credential operations later. Kubernetes restarts the pod until the server is reachable again. These checks run in addition to the routing checks in [Secrets Storage](secrets-storage.md#kms-providers).

## Operating the Provider

- **Availability.** Every credential read served from Postgres needs one `Decrypt`, and every write needs one `Encrypt`, so a KMIP outage has the same synchronous impact as a Transit outage: reads and writes of Postgres-stored credentials fail until the server returns. Credentials already cached by consumers, such as `bmc-proxy`, keep working for their cache lifetime.
- **Connections and deadlines.** The provider holds one TLS connection and sends one request at a time. A request is retried exactly once, only when a cached connection turns out to have been closed before the request was answered. A request that exceeds `request_timeout` is reported as a failure and not retried. Every response must echo the request's batch item identifier, protocol version, operation, and key identifier; a response that does not is reported as a protocol violation and its connection is closed. A connection idle for 30 seconds is closed before the next request rather than reused.
- **Metrics.** Calls appear on the shared outbound-call histogram `carbide_external_call_duration_milliseconds` with `backend="kmip"` and `operation` set to `encrypt_dek`, `decrypt_dek`, or one of the startup checks; the `outcome` label separates successes from errors. Failures also log at `warn` with the server's Result Reason and Result Message.
- **Logging.** No key material, plaintext, ciphertext, or IV is ever logged; log lines carry the endpoint, the operation, and the server's reason text.
- **Backup.** The KEK lives only in the KMIP server. A database dump holds only ciphertext and is unreadable without every key its rows reference, so back the keys up through the product's own procedure and include that step in the pre-upgrade backup described in [Upgrading NICo](../manuals/upgrade.md).

### Migrating from Transit

The migration reuses the multi-provider, routing, and re-wrap mechanics of [Secrets Storage](secrets-storage.md#rotating-and-retiring-keys). It takes two rollouts, because a rolling update runs old and new pods side by side, as described in [Before You Start](secrets-storage.md#before-you-start):

1. **Distribute the provider.** Add the `kmip` provider, its TLS mounts, and its `keys` next to the existing `transit` provider, leaving `active` and `[secrets.routing]` on Transit. Restart and wait for every old pod to terminate. Every replica now holds both providers and can unwrap rows written under either, but no replica writes under a KMIP `kek_id` yet. Startup validation runs against the KMIP server in this step, so a misconfigured provider fails here, before any row depends on it.
2. **Switch writes.** Set `active` to the KMIP provider, route every prefix to a KMIP `kek_id` that step 1 distributed, restart, and wait for the rollout to finish. A `kek_id` first added in this step would bring back the problem step 1 avoids. A pod still on the previous configuration can read everything the new pods write, because it already holds the KMIP provider.
3. **Re-wrap.** Run `nico-admin-cli secrets re-wrap`. Transit stays configured and unwraps old rows until `stale_remaining` reaches zero and the rollback and backup-retention windows have passed.

Skipping the first rollout lets a new pod write a credential under a KMIP `kek_id` while an old pod that knows only Transit is still serving; that pod cannot read the new row until it terminates. Rolling back is the mirror image: set `active` back to Transit, reverse the routing, restart, and re-wrap again, keeping the KMIP provider configured until its rows are gone. Because every replica already holds both providers, the rollback needs one rollout.

## Qualified Server Products

A server product is supported only after it passes the checklist below. No product has completed it yet; [#3253](https://github.com/dsx-ai-factory/infra-controller/issues/3253) tracks qualification, and this table is updated as products complete it.

| Product | Version | Negotiated KMIP version | Cipher profile | Authentication | Status |
|---|---|---|---|---|---|
| | | | | | none qualified yet; see #3253 |

### Qualification Checklist

Run the whole list against the product, and record the product name and version, the firmware or software release, the negotiated protocol version, the `Query` operation list, the IV and tag lengths the server returned, the authentication mode and how the client certificate maps to a server identity, and the high-availability topology used.

1. Startup validation passes with the key created as described above, and fails with a message naming the field when the Unique Identifier is wrong or the identity lacks a permission.
2. A credential written with `nico-admin-cli credential add-*` reads back, and the row's wrapped DEK unwraps after a `nico-api` restart.
3. The vendor's documentation or support states that `Random IV` yields a unique IV for every `Encrypt` under a key, across connections and failover; record that statement. As a smoke test, write several credentials that route to the same `kek_id`, some before and some after a `nico-api` restart and a server failover, and check that their `dek_nonce` values in the `secrets` table are all different and have the IV length recorded above. The samples rule out a fixed IV or a counter that restarts with each connection; they do not prove uniqueness, which is why the vendor statement is required.
4. The product's own tools show the key as non-extractable, as step 1 of key creation requires.
5. Stopping one server node, or forcing a cluster failover, produces `carbide_external_call_duration_milliseconds{backend="kmip",outcome="error"}` observations that stop without a `nico-api` restart once the service is back.
6. Removing the identity's `Encrypt` and `Decrypt` permissions on the key, a change the server can undo, makes writes fail with an encryption error and reads with a decryption error without crashing `nico-api`, and restoring the permissions restores both without a restart. Do not use key deactivation for this step: a conforming server cannot return a Deactivated key to Active, and may still decrypt with it.
7. A migration from Transit and its rollback complete with `secrets re-wrap` reporting zero stale rows in each direction.
8. A database dump restored against the same key reads back.
9. The table above gains the product's row.

## Known Limits

- Only the KMIP 1.4 wire protocol is spoken. A server that lists only 2.x versions is rejected at startup.
- Only single-part AES-GCM with a server-generated IV and a separate authentication tag is used; other modes, client-supplied IVs, and additional authenticated data are not.
- The client does not verify that the server generates a fresh IV for every `Encrypt`; that property comes from the server and is confirmed during qualification.
- A `Get Attributes` response that includes an unrequested attribute of TTLV type Interval or Big Integer cannot be decoded. The client requests only the four attributes it needs, which real servers honor.
- The provider does not fall back to another provider when the server is unreachable; availability is the KMIP deployment's responsibility.
