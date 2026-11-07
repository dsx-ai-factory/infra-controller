# Re-creating issuer/CA for local development

nico-api uses Vault to generate certificates that it then vends to clients, such as e.g. Scout. Here are the instructions on how to set up this process from scratch - https://developer.hashicorp.com/vault/tutorials/secrets-management/pki-engine?variants=vault-deploy%3Aselfhosted

In short, when a site or local dev environment is deployed, an issuer/CA is created inside vault. In addition, a role is created. That role points to the issuer. All client certificates are requested/created against that role. Unfortunately, in local dev environment, the TTL for that issuer/CA is set to only 3 months. Also, it is a rule that client certificates cannot outlive issuer's CA certificate, so as soon as CA certificate has less time remaining than client certificate, that we are trying to create (which typically is 30 days), we'll start getting an error like this: `cannot satisfy request, as TTL would result in notAfter 2024-... that is beyond the expiration of the CA certificate at 2024-...` The solution is to create a new issuer and make sure that the role points to it instead.

Before we begin, it is important to understand Vault's operating concept. Vault runs as https service, typically listening on port 8200. Most of vault commands, e.g. `vault list`, `vault get` are simply http requests to that service.

Vault has a concept of engines, also called secrets (just to confuse you). Engines are like modules of various types that can be installed at certain paths. This command will list all the available engines:
```bash
/run/secrets $ vault secrets list -tls-skip-verify
Path          Type         Accessor              Description
----          ----         --------              -----------
cubbyhole/    cubbyhole    cubbyhole_e271c1a0    per-token private secret storage
nicoca/      pki          pki_d82997c7          n/a
identity/     identity     identity_e32b8a0d     identity store
secrets/      kv           kv_352bcd00           n/a
sys/          system       system_17d61b86       system endpoints used for control, policy and debugging
```
Here we have e.g. engine `system` installed at path `sys`, and engine `kv` installed at path `secrets` (just to confuse you once more). Most engines will accept `vault read` and `vault write` commands, some will also accept `vault list`. The parameters to those commands are most likely URL paths (except for the domain name part) with parameters, e.g. `vault read nicoca/issuer/5da1f77a-bd24-400d-1e3b-8492b9daa1c8`. (Note, the kv engine does not accept `vault list`, e.g. `vault list secrets/`, but it has a special command `vault kv list secrets/`). It appears that it is possible to have the same type of engine installed at multiple paths.

Now, the engine responsible for generating client certificates has type pki. You need to use `vault secrets list` to see what path that engine is mapped to. In the example above it is `nicoca`. Below are the steps that are necessary to undertake in order to create a new issuer, set it as default and (maybe) remove the old issuer.

1. Obtain root login token for the vault: `kubectl get secret -n nico-system nico-vault-token -o yaml` (don't nicot to do base64 decode!).
2. Exec into vault-0 container: `kubectl exec -n vault vault-0 -it  -- /bin/sh`.
3. Inside the vault container login using that token: `vault login --tls-skip-verify <token>`. (Without this, you will not have root permission to carry out steps below)
4. Figure out what path pki engine is mapped to: `vault secrets list -tls-skip-verify`. In this example it is `nicoca` (it will also be the value of `VAULT_PKI_MOUNT_LOCATION` env var in nico-api deployment/pod).
5. List certificate issuers created by the engine `nicoca`:
    ```bash
    /run/secrets $ vault list -tls-skip-verify nicoca/issuers/
    Keys
    ----
    447e5fb7-65d8-3829-d1b4-416a3d795ede
    ```
6. Have a look at the issuer itself: `vault read -tls-skip-verify nicoca/issuer/447e5fb7-65d8-3829-d1b4-416a3d795ed` (one can add -format json for a JSON output). Parse the cert displayed with `openssl x509 -in mycert.pem -text` to double check it's the actual culprit by looking at the `NotAfter` field.
7. Check the role (the name of the role nico-cluster is the value of VAULT_PKI_ROLE_NAME env var in nico-api deployment/pod)
    <details>
    <summary>Get Issuer Role</summary>

    ```bash
    /run/secrets $ vault read -format json -tls-skip-verify nicoca/roles/nico-cluster
    {
    "request_id": "752222cf-97db-d63f-d1cb-59c74d7f9143",
    "lease_id": "",
    "lease_duration": 0,
    "renewable": false,
    "data": {
        "allow_any_name": false,
        "allow_bare_domains": false,
        "allow_glob_domains": true,
        "allow_ip_sans": true,
        "allow_localhost": true,
        "allow_subdomains": false,
        "allow_token_displayname": false,
        "allow_wildcard_certificates": false,
        "allowed_domains": [
        "*.nico",
        "cluster.local",
        "*.svc",
        "*.svc.cluster.local",
        "*.nico.example.com"
        ],
        "allowed_domains_template": false,
        "allowed_other_sans": [],
        "allowed_serial_numbers": [],
        "allowed_uri_sans": [
        "spiffe://*"
        ],
        "allowed_uri_sans_template": false,
        "allowed_user_ids": [],
        "basic_constraints_valid_for_non_ca": false,
        "client_flag": true,
        "cn_validations": [
        "email",
        "hostname"
        ],
        "code_signing_flag": false,
        "country": [],
        "email_protection_flag": false,
        "enforce_hostnames": true,
        "ext_key_usage": [],
        "ext_key_usage_oids": [],
        "generate_lease": false,
        "issuer_ref": "default",
        "key_bits": 256,
        "key_type": "ec",
        "key_usage": [
        "DigitalSignature",
        "KeyAgreement",
        "KeyEncipherment"
        ],
        "locality": [],
        "max_ttl": 2592000,
        "no_store": false,
        "not_after": "",
        "not_before_duration": 30,
        "organization": [],
        "ou": [],
        "policy_identifiers": [],
        "postal_code": [],
        "province": [],
        "require_cn": false,
        "server_flag": true,
        "signature_bits": 0,
        "street_address": [],
        "ttl": 2592000,
        "use_csr_common_name": true,
        "use_csr_sans": true,
        "use_pss": false
    },
    "warnings": null
    }
    ```
    </details>
8. Check the value of `issuer_ref` field in the role description. In this instance it is `default`, meaning this role will be tied to whatever issuer is set as default.
9. Try and generate a new client cert manually now with TTL greater than CA cert's NotAfter date, e.g.: `vault write -tls-skip-verify nicoca/issue/nico-cluster common_name="" ttl="30d"`. This should reproduce the original error: `cannot satisfy request, as TTL would result in notAfter of 2024-11-29T11:04:57.198383711Z that is beyond the expiration of the CA certificate at 2024-11-13T12:36:56Z`
10. Before generating a new issuer/CA, we need to set the upper bound for allowable TTLs, e.g.: `vault secrets tune -max-lease-ttl=87600h nicoca` (87600h=10 years, because I don't want to recreate issuers every three months, but feel free to choose your own value). It is possible to specify TTL for a role also, see https://groups.google.com/g/vault-tool/c/sYbWxiTzgcw.
11. Now, create the new issuer: `vault write -field=certificate -tls-skip-verify nicoca/root/generate/internal common_name="site-root" issuer_name="site-root"  ttl=87600h`. The CA cert for this issuer will be printed. While you are at it, grab it and insert it into `/opt/nico/nico_root.pem` on your client machine (e.g. the one that is running scout). Without this, all communication from nico-api to Scout will be rejected by Scout as it will have no way to check the authenticity of certs supplied by nico-api in the TLS session.
12. Set that issuer as the default one: `vault write -tls-skip-verify nicoca/root/replace default=site-root`. Now, the role will "point" to this issuer.
13. You can also delete the old one if you want to: `vault delete -tls-skip-verify nicoca/issuer/447e5fb7-65d8-3829-d1b4-416a3d795ed`
14. In order to verify that the change has worked, try repeating step 9. This time, it should not produce any errors and should generate a certificate without a problem.

As a side note, we are also using Vault to generate certificate for various services inside a Site, i.e. not for vending to Scout. This is done using Kubernetes' cert-manager. One needs to create certificate objects that describe certificates, e.g. `nico-api-certificate` in the `nico-system namespace`. That object will point to objects of type `Issuer` or `ClusterIssuer`, e.g. `vault-nico-issuer`, that will point to a concrete Vault service generating certificates. The result of that is that there will always be a secret automatically created for each certificate object containing all certificates ready to be consumed by Kubernetes components (pods etc).
