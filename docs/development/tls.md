# TLS Certificates in Kubernetes

## Overview

- cert-manager-spiffe uses Kubernetes `serviceAccounts`, `clusterDomain`,
  `roles`, and `rolebindings` to build the SVID, e.g., spiffe://nico.local/nico-system/nico-api
- Certificates are available in pods at `/run/secrets/spiffe.io/{tls.crt,tls.key,ca.crt}`
- To retrieve a certificate, you must first create a `serviceAccount`, `role`, and `roleBinding` (example below)
- Don't nicot to update the `namespace` to the correct value
- Helm upgrade/install generates the `Labels` you see in the
  example below; you can omit those.
- The `role` associated with the `serviceAccount` grants enough permissions to request a certificate from `cert-manager-csi-driver-spiffe`

## Cert-Manager
The `CertificateRequest` (which includes the CSR) references a
`ClusterIssuer` set up during the initial bootstrap of the site.

The `ClusterIssuer` sends CSRs to Vault for signing using the nicoCA PKI.
Before a `CertificateRequest` can be signed, it must be approved.

`cert-manager-csi-driver-spiffe-approver` runs as a `deployment` and is
responsible for verifying the `CertificateRequest` meets [specific
criteria](https://cert-manager.io/docs/projects/csi-driver-spiffe/#approver)

If all criteria are met, the `CertificateRequest` is approved, and `cert-manager` sends the CSR portion of the `CertificateRequest` to
Vault for signing.

### SPIFFE

SPIFFE is a means of identifying software systems.  The identity of the software is
cryptographically verifiable and exists within a "trust domain"  The trust domain could be a user, organization, or anything
representable in a URI.

With SPIFFE formatted `Certificates`, the only field populated is the SAN (Subject Alternative Name).  The SAN must conform to the
 [SPIFFE ID format](https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE.md#2-the-spiffe-id).

 The validation of the SPIFFE ID format and submission of `CertificateRequest` gets handled by `cert-manager-csi-driver-spiffe-approver` and `cert-manager-csi-driver-spiffe`, respectively.

 `cert-manager-csi-driver-spiffe` runs as a `DaemonSet`. It is responsible for generating the TLS key, CSR and submitting the CSR for
 approval (By way of `CertificateRequest`).

**NOTE**
> The TLS key generated in every pod never leaves the host which
> it was generated on. If a migration event occurs, the CSR/key are
> regenerated, submitted to CertManager, and then signed again.

### How to obtain a SPIFFE formatted cert

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
name: nico-api
namespace: "default"
labels:
app.kubernetes.io/name: nico-api
helm.sh/chart: nicoApi-0.0.1
app.kubernetes.io/instance: release-name
app.kubernetes.io/managed-by: Helm
app.kubernetes.io/component: nico-api
automountServiceAccountToken: true

---

kind: Role
apiVersion: rbac.authorization.k8s.io/v1
metadata:
name: nico-api
namespace: "default"
labels:
app.kubernetes.io/name: nico-api
helm.sh/chart: nicoApi-0.0.1
app.kubernetes.io/instance: release-name
app.kubernetes.io/managed-by: Helm
app.kubernetes.io/component: nico-api
rules:

- apiGroups: ["cert-manager.io"]
  resources: ["certificaterequests"]
  verbs: ["create"]

---

kind: RoleBinding
apiVersion: rbac.authorization.k8s.io/v1
metadata:
name: nico-api
namespace: default
labels:
app.kubernetes.io/name: nico-api
helm.sh/chart: nicoApi-0.0.1
app.kubernetes.io/instance: release-name
app.kubernetes.io/managed-by: Helm
app.kubernetes.io/component: nico-api
roleRef:
apiGroup: rbac.authorization.k8s.io
kind: Role
name: nico-api
subjects:

- kind: ServiceAccount
  name: nico-api
  namespace: "default"

```

After creating the `serviceAccount`, `role`, and `rolebinding`, modify your deployment/pod spec to request a `Certificate`

```yaml
spec:
  serviceAccountName: nico-api
...
      volumeMounts:
        - name: spiffe
          mountPath: "/var/run/secrets/spiffe.io"
...
    volumes:
    - name: spiffe
    csi:
      driver: spiffe.csi.cert-manager.io
      readOnly: true

```

### NON-SPIFFE

Some components in Kubernetes cannot use SPIFFE formatted certs
`ValidatingWebhooks` and `MutatingWebhooks` can not use SPIFFE formatted `CertificateRequests`

For those resources, there is a separate `ClusterIssuer` that signs `CertificateRequests` which are not SPIFFE formatted.

There is a `CertificateRequestPolicy` that enforces specific criteria for non-SPIFFE `CertificateRequests`. The policy only allows signing
requests for `Service` based TLS certs.
