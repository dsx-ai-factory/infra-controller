#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../../.." && pwd)"

NAMESPACE="${LOCAL_DEV_NAMESPACE:-nico-system}"
CERT_MANAGER_NAMESPACE="${LOCAL_DEV_CERT_MANAGER_NAMESPACE:-cert-manager}"
VALUES_FILE="${LOCAL_DEV_VALUES_FILE:-${REPO_ROOT}/dev/deployment/devspace/values.generated.yaml}"

INSTALL_CERT_MANAGER="${LOCAL_DEV_INSTALL_CERT_MANAGER:-1}"
INSTALL_LOCAL_ISSUER="${LOCAL_DEV_INSTALL_LOCAL_ISSUER:-1}"
INSTALL_POSTGRES="${LOCAL_DEV_INSTALL_POSTGRES:-1}"
INSTALL_VAULT="${LOCAL_DEV_INSTALL_VAULT:-1}"
INSTALL_REST_PREREQS="${LOCAL_DEV_INSTALL_REST_PREREQS:-1}"
INSTALL_TEMPORAL="${LOCAL_DEV_INSTALL_TEMPORAL:-1}"
INSTALL_KEYCLOAK="${LOCAL_DEV_INSTALL_KEYCLOAK:-1}"

POSTGRES_NAMESPACE="${POSTGRES_NAMESPACE:-postgres}"
POSTGRES_HOST="${LOCAL_DEV_POSTGRES_HOST:-postgres.${POSTGRES_NAMESPACE}.svc.cluster.local}"
POSTGRES_PORT="${LOCAL_DEV_POSTGRES_PORT:-5432}"
POSTGRES_DB="${LOCAL_DEV_POSTGRES_DB:-nico}"
POSTGRES_USER="${LOCAL_DEV_POSTGRES_USER:-nico}"
POSTGRES_PASSWORD="${LOCAL_DEV_POSTGRES_PASSWORD:-nico}"
POSTGRES_SSL_MODE="${LOCAL_DEV_POSTGRES_SSL_MODE:-disable}"

REST_NAMESPACE="nico-rest"
REST_POSTGRES_DB="${LOCAL_DEV_REST_POSTGRES_DB:-nico_rest}"
REST_POSTGRES_USER="${LOCAL_DEV_REST_POSTGRES_USER:-nico_rest}"
REST_POSTGRES_PASSWORD="${LOCAL_DEV_REST_POSTGRES_PASSWORD:-nico_rest}"
TEMPORAL_NAMESPACE="temporal"

# Kubernetes namespace hosting the local development Vault. Keep this distinct
# from VAULT_NAMESPACE, which selects a Vault Enterprise/HCP namespace.
VAULT_KUBERNETES_NAMESPACE="${LOCAL_DEV_VAULT_KUBERNETES_NAMESPACE:-vault}"
VAULT_ADDR="${LOCAL_DEV_VAULT_ADDR:-http://vault.${VAULT_KUBERNETES_NAMESPACE}.svc.cluster.local:8200}"
VAULT_TOKEN="${LOCAL_DEV_VAULT_TOKEN:-root}"
VAULT_KV_MOUNT="${LOCAL_DEV_VAULT_KV_MOUNT:-secrets}"
VAULT_PKI_MOUNT="${LOCAL_DEV_VAULT_PKI_MOUNT:-certs}"
VAULT_PKI_ROLE_NAME="${LOCAL_DEV_VAULT_PKI_ROLE_NAME:-nico-cluster}"
VAULT_AUTH_MODE="${LOCAL_DEV_VAULT_AUTH_MODE:-root-token}"
VAULT_ADMIN_CA_FILE="${LOCAL_DEV_VAULT_ADMIN_CA_FILE:-}"
ADMIN_ROOT_CERT_PEM=""

CERT_ISSUER_KIND="${LOCAL_DEV_CERT_ISSUER_KIND:-Issuer}"
CERT_ISSUER_NAME="${LOCAL_DEV_CERT_ISSUER_NAME:-local-ca-issuer}"
CERT_ISSUER_GROUP="${LOCAL_DEV_CERT_ISSUER_GROUP:-cert-manager.io}"

log() {
  printf '[local-dev] %s\n' "$*"
}

require_bin() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'missing required binary: %s\n' "$1" >&2
    exit 1
  }
}

rest_postgres_sql() {
  local rest_postgres_db_b64
  local rest_postgres_password_b64
  local rest_postgres_user_b64

  rest_postgres_db_b64="$(printf '%s' "${REST_POSTGRES_DB}" | base64 | tr -d '\n')"
  rest_postgres_user_b64="$(printf '%s' "${REST_POSTGRES_USER}" | base64 | tr -d '\n')"
  rest_postgres_password_b64="$(printf '%s' "${REST_POSTGRES_PASSWORD}" | base64 | tr -d '\n')"

  printf '%s\n' \
    "SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', convert_from(decode('${rest_postgres_user_b64}', 'base64'), 'UTF8'), convert_from(decode('${rest_postgres_password_b64}', 'base64'), 'UTF8'))" \
    "WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = convert_from(decode('${rest_postgres_user_b64}', 'base64'), 'UTF8'))" \
    '\gexec' \
    "SELECT format('ALTER ROLE %I WITH LOGIN PASSWORD %L', convert_from(decode('${rest_postgres_user_b64}', 'base64'), 'UTF8'), convert_from(decode('${rest_postgres_password_b64}', 'base64'), 'UTF8'))" \
    '\gexec' \
    'DO $$' \
    'BEGIN' \
    "  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'keycloak') THEN" \
    "    CREATE ROLE keycloak LOGIN PASSWORD 'keycloak';" \
    '  END IF;' \
    "  ALTER ROLE keycloak WITH LOGIN PASSWORD 'keycloak';" \
    "  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'temporal') THEN" \
    "    CREATE ROLE temporal LOGIN PASSWORD 'temporal' CREATEDB;" \
    '  END IF;' \
    "  ALTER ROLE temporal WITH LOGIN PASSWORD 'temporal' CREATEDB;" \
    'END' \
    '$$;' \
    "SELECT format('CREATE DATABASE %I OWNER %I', convert_from(decode('${rest_postgres_db_b64}', 'base64'), 'UTF8'), convert_from(decode('${rest_postgres_user_b64}', 'base64'), 'UTF8'))" \
    "WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = convert_from(decode('${rest_postgres_db_b64}', 'base64'), 'UTF8'))" \
    '\gexec' \
    "SELECT format('%I', convert_from(decode('${rest_postgres_db_b64}', 'base64'), 'UTF8')) AS rest_postgres_db_quoted" \
    '\gset' \
    '\connect :rest_postgres_db_quoted' \
    "SELECT format('GRANT ALL ON SCHEMA public TO %I', convert_from(decode('${rest_postgres_user_b64}', 'base64'), 'UTF8'))" \
    '\gexec' \
    'CREATE EXTENSION IF NOT EXISTS pg_trgm;' \
    '\connect postgres' \
    "SELECT 'CREATE DATABASE keycloak OWNER keycloak'" \
    "WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'keycloak')" \
    '\gexec' \
    '\connect keycloak' \
    'GRANT ALL ON SCHEMA public TO keycloak;' \
    '\connect postgres' \
    "SELECT 'CREATE DATABASE temporal OWNER temporal'" \
    "WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'temporal')" \
    '\gexec' \
    "SELECT 'CREATE DATABASE temporal_visibility OWNER temporal'" \
    "WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'temporal_visibility')" \
    '\gexec' \
    '\connect temporal' \
    'GRANT ALL ON SCHEMA public TO temporal;' \
    '\connect temporal_visibility' \
    'GRANT ALL ON SCHEMA public TO temporal;'
}

install_cert_manager() {
  if [[ "${INSTALL_CERT_MANAGER}" != "1" ]]; then
    return
  fi

  log "Installing cert-manager into ${CERT_MANAGER_NAMESPACE}"
  helm repo add jetstack https://charts.jetstack.io --force-update >/dev/null
  helm upgrade --install cert-manager jetstack/cert-manager \
    --namespace "${CERT_MANAGER_NAMESPACE}" \
    --create-namespace \
    --set crds.enabled=true >/dev/null

  kubectl rollout status deployment/cert-manager -n "${CERT_MANAGER_NAMESPACE}" --timeout=180s >/dev/null
  kubectl rollout status deployment/cert-manager-cainjector -n "${CERT_MANAGER_NAMESPACE}" --timeout=180s >/dev/null
  kubectl rollout status deployment/cert-manager-webhook -n "${CERT_MANAGER_NAMESPACE}" --timeout=180s >/dev/null
}

apply_core_objects() {
  log "Applying Core namespaces and connection objects in ${NAMESPACE}"
  kubectl apply -f - <<EOF
apiVersion: v1
kind: Namespace
metadata:
  name: ${NAMESPACE}
---
apiVersion: v1
kind: Namespace
metadata:
  name: forge-system
---
apiVersion: v1
kind: Secret
metadata:
  name: nico-system.nico.nico-pg-cluster.credentials
  namespace: ${NAMESPACE}
type: Opaque
stringData:
  username: ${POSTGRES_USER}
  password: ${POSTGRES_PASSWORD}
  host: ${POSTGRES_HOST}
  port: "${POSTGRES_PORT}"
  dbname: ${POSTGRES_DB}
  uri: postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@${POSTGRES_HOST}:${POSTGRES_PORT}/${POSTGRES_DB}
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: nico-system-nico-database-config
  namespace: ${NAMESPACE}
data:
  DB_HOST: ${POSTGRES_HOST}
  DB_PORT: "${POSTGRES_PORT}"
  DB_NAME: ${POSTGRES_DB}
---
apiVersion: v1
kind: Secret
metadata:
  name: nico-vault-token
  namespace: ${NAMESPACE}
type: Opaque
stringData:
  token: ${VAULT_TOKEN}
---
apiVersion: v1
kind: Secret
metadata:
  name: nico-vault-approle-tokens
  namespace: ${NAMESPACE}
type: Opaque
stringData:
  VAULT_ROLE_ID: local-dev
  VAULT_SECRET_ID: local-dev
EOF
}

apply_local_postgres() {
  local rest_postgres_init=""

  if [[ "${INSTALL_POSTGRES}" != "1" ]]; then
    return
  fi

  if [[ "${INSTALL_REST_PREREQS}" == "1" ]]; then
    rest_postgres_init="$(
      printf '  002-create-rest-databases.sql: |\n'
      rest_postgres_sql | sed 's/^/    /'
    )"
  fi

  log "Applying local PostgreSQL deployment"
  kubectl apply -f - <<EOF
apiVersion: v1
kind: Namespace
metadata:
  name: ${POSTGRES_NAMESPACE}
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: postgres-init
  namespace: ${POSTGRES_NAMESPACE}
data:
  001-create-user-and-db.sql: |
    DO \$\$
    BEGIN
      IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '${POSTGRES_USER}') THEN
        CREATE USER ${POSTGRES_USER} WITH PASSWORD '${POSTGRES_PASSWORD}';
      END IF;
    END
    \$\$;
    SELECT 'CREATE DATABASE ${POSTGRES_DB} OWNER ${POSTGRES_USER}'
    WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = '${POSTGRES_DB}')\gexec
${rest_postgres_init}
---
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: postgres
  namespace: ${POSTGRES_NAMESPACE}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: postgres
  template:
    metadata:
      labels:
        app: postgres
    spec:
      containers:
        - name: postgres
          image: postgres:14.5-alpine
          imagePullPolicy: IfNotPresent
          env:
            - name: POSTGRES_PASSWORD
              value: admin
            - name: POSTGRES_HOST_AUTH_METHOD
              value: trust
          ports:
            - containerPort: 5432
              name: postgres
          readinessProbe:
            tcpSocket:
              port: 5432
            initialDelaySeconds: 5
            periodSeconds: 5
          volumeMounts:
            - name: data
              mountPath: /var/lib/postgresql/data
            - name: init
              mountPath: /docker-entrypoint-initdb.d
      volumes:
        - name: data
          emptyDir: {}
        - name: init
          configMap:
            name: postgres-init
---
apiVersion: v1
kind: Service
metadata:
  name: postgres
  namespace: ${POSTGRES_NAMESPACE}
spec:
  selector:
    app: postgres
  ports:
    - name: postgres
      port: 5432
      targetPort: 5432
EOF

  kubectl rollout status statefulset/postgres -n "${POSTGRES_NAMESPACE}" --timeout=180s >/dev/null

  if [[ "${INSTALL_REST_PREREQS}" == "1" ]]; then
    log "Ensuring REST, Temporal, and Keycloak databases exist"
    rest_postgres_sql | kubectl exec -i -n "${POSTGRES_NAMESPACE}" statefulset/postgres -- \
      psql -v ON_ERROR_STOP=1 -U postgres -d postgres >/dev/null
  fi
}

apply_local_vault() {
  if [[ "${INSTALL_VAULT}" != "1" ]]; then
    return
  fi

  log "Applying local Vault dev server"
  kubectl apply -f - <<EOF
apiVersion: v1
kind: Namespace
metadata:
  name: ${VAULT_KUBERNETES_NAMESPACE}
---
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: vault
  namespace: ${VAULT_KUBERNETES_NAMESPACE}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: vault
  template:
    metadata:
      labels:
        app: vault
    spec:
      containers:
        - name: vault
          image: hashicorp/vault:1.20.2
          imagePullPolicy: IfNotPresent
          args:
            - server
            - -dev
            - -dev-listen-address=0.0.0.0:8200
            - -dev-root-token-id=${VAULT_TOKEN}
          env:
            - name: VAULT_DEV_LISTEN_ADDRESS
              value: 0.0.0.0:8200
          ports:
            - containerPort: 8200
              name: http
          readinessProbe:
            httpGet:
              path: /v1/sys/health?standbyok=true&sealedcode=204&uninitcode=204
              port: 8200
            initialDelaySeconds: 5
            periodSeconds: 5
---
apiVersion: v1
kind: Service
metadata:
  name: vault
  namespace: ${VAULT_KUBERNETES_NAMESPACE}
spec:
  selector:
    app: vault
  ports:
    - name: http
      port: 8200
      targetPort: 8200
EOF

  kubectl rollout status statefulset/vault -n "${VAULT_KUBERNETES_NAMESPACE}" --timeout=180s >/dev/null

  log "Configuring Vault mounts and local role"
  kubectl exec -n "${VAULT_KUBERNETES_NAMESPACE}" statefulset/vault -- sh -lc "
    set -euo pipefail
    export VAULT_ADDR=http://127.0.0.1:8200
    export VAULT_TOKEN='${VAULT_TOKEN}'

    vault secrets list -format=json | grep -q '\"${VAULT_KV_MOUNT}/\"' || \
      vault secrets enable -path='${VAULT_KV_MOUNT}' -version=2 kv

    vault secrets list -format=json | grep -q '\"${VAULT_PKI_MOUNT}/\"' || \
      vault secrets enable -path='${VAULT_PKI_MOUNT}' pki

    vault read '${VAULT_PKI_MOUNT}/cert/ca' >/dev/null 2>&1 || \
      vault write '${VAULT_PKI_MOUNT}/root/generate/internal' common_name='local-vault-ca' ttl='87600h' >/dev/null

    vault write '${VAULT_PKI_MOUNT}/config/urls' \
      issuing_certificates='${VAULT_ADDR}/v1/${VAULT_PKI_MOUNT}/ca' \
      crl_distribution_points='${VAULT_ADDR}/v1/${VAULT_PKI_MOUNT}/crl' >/dev/null

    vault write '${VAULT_PKI_MOUNT}/roles/${VAULT_PKI_ROLE_NAME}' \
      allow_any_name=true \
      allow_bare_domains=true \
      allow_subdomains=true \
      allow_localhost=true \
      require_cn=false \
      max_ttl='72h' \
      allowed_uri_sans='spiffe://nico.local/*' >/dev/null

    vault kv get '${VAULT_KV_MOUNT}/machines/bmc/site/root' >/dev/null 2>&1 || \
      echo '{\"UsernamePassword\":{\"username\":\"root\",\"password\":\"vault-password\"}}' | \
      vault kv put '${VAULT_KV_MOUNT}/machines/bmc/site/root' - >/dev/null

    vault kv get '${VAULT_KV_MOUNT}/machines/all_dpus/site_default/uefi-metadata-items/auth' >/dev/null 2>&1 || \
      echo '{\"UsernamePassword\":{\"username\":\"root\",\"password\":\"vault-password\"}}' | \
      vault kv put '${VAULT_KV_MOUNT}/machines/all_dpus/site_default/uefi-metadata-items/auth' - >/dev/null

    vault kv get '${VAULT_KV_MOUNT}/machines/all_hosts/site_default/uefi-metadata-items/auth' >/dev/null 2>&1 || \
      echo '{\"UsernamePassword\":{\"username\":\"root\",\"password\":\"vault-password\"}}' | \
      vault kv put '${VAULT_KV_MOUNT}/machines/all_hosts/site_default/uefi-metadata-items/auth' - >/dev/null
  " >/dev/null
}

load_admin_root_cert_pem() {
  if [[ -n "${VAULT_ADMIN_CA_FILE}" ]]; then
    if [[ ! -f "${VAULT_ADMIN_CA_FILE}" || ! -r "${VAULT_ADMIN_CA_FILE}" ]]; then
      printf 'Vault admin CA file is not a readable regular file: %s\n' \
        "${VAULT_ADMIN_CA_FILE}" >&2
      exit 1
    fi
    ADMIN_ROOT_CERT_PEM="$(<"${VAULT_ADMIN_CA_FILE}")"
  elif [[ "${INSTALL_VAULT}" == "1" ]]; then
    log "Reading the local Vault PKI CA for Core admin-client trust"
    ADMIN_ROOT_CERT_PEM="$(
      kubectl exec -n "${VAULT_KUBERNETES_NAMESPACE}" statefulset/vault -- \
        env VAULT_ADDR=http://127.0.0.1:8200 VAULT_TOKEN="${VAULT_TOKEN}" \
        vault read -field=certificate "${VAULT_PKI_MOUNT}/cert/ca"
    )"
  else
    return
  fi

  require_bin awk
  require_bin openssl

  if ! printf '%s\n' "${ADMIN_ROOT_CERT_PEM}" | awk '
    BEGIN {
      in_certificate = 0
      certificate_count = 0
      certificate_has_data = 0
      invalid = 0
    }
    {
      sub(/\r$/, "")
      if ($0 == "-----BEGIN CERTIFICATE-----") {
        if (in_certificate) {
          invalid = 1
          exit
        }
        in_certificate = 1
        certificate_has_data = 0
        next
      }
      if ($0 == "-----END CERTIFICATE-----") {
        if (!in_certificate || !certificate_has_data) {
          invalid = 1
          exit
        }
        in_certificate = 0
        certificate_count++
        next
      }
      if (in_certificate) {
        if ($0 !~ /^[A-Za-z0-9+\/=]+$/) {
          invalid = 1
          exit
        }
        certificate_has_data = 1
        next
      }
      if ($0 !~ /^[[:space:]]*$/) {
        invalid = 1
        exit
      }
    }
    END {
      exit(invalid || in_certificate || certificate_count == 0)
    }
  '; then
    printf '%s\n' \
      'Vault admin CA must contain only bare PEM CERTIFICATE blocks and whitespace' >&2
    exit 1
  fi

  local certificate_pem=""
  local line
  while IFS= read -r line || [[ -n "${line}" ]]; do
    line="${line%$'\r'}"
    if [[ "${line}" == "-----BEGIN CERTIFICATE-----" ]]; then
      certificate_pem="${line}"$'\n'
      continue
    fi
    if [[ -z "${certificate_pem}" ]]; then
      continue
    fi

    certificate_pem+="${line}"$'\n'
    if [[ "${line}" != "-----END CERTIFICATE-----" ]]; then
      continue
    fi
    if ! openssl x509 -noout <<<"${certificate_pem}" >/dev/null 2>&1; then
      printf '%s\n' \
        'Vault admin CA contains a CERTIFICATE block that is not valid X.509' >&2
      exit 1
    fi
    certificate_pem=""
  done <<<"${ADMIN_ROOT_CERT_PEM}"
}

apply_local_issuer() {
  if [[ "${INSTALL_LOCAL_ISSUER}" != "1" ]]; then
    return
  fi

  log "Applying local cert-manager issuer resources"
  kubectl apply -f - <<EOF
apiVersion: cert-manager.io/v1
kind: ClusterIssuer
metadata:
  name: local-selfsigned
spec:
  selfSigned: {}
---
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: nico-local-ca
  namespace: ${NAMESPACE}
spec:
  isCA: true
  commonName: nico-local-ca
  secretName: nico-local-ca
  privateKey:
    algorithm: ECDSA
    size: 384
  issuerRef:
    name: local-selfsigned
    kind: ClusterIssuer
    group: cert-manager.io
---
apiVersion: cert-manager.io/v1
kind: Issuer
metadata:
  name: ${CERT_ISSUER_NAME}
  namespace: ${NAMESPACE}
spec:
  ca:
    secretName: nico-local-ca
EOF

  kubectl wait --for=condition=Ready certificate/nico-local-ca -n "${NAMESPACE}" --timeout=180s >/dev/null
}

sync_nico_roots_secret() {
  local ca_b64=""

  if kubectl get secret nico-local-ca -n "${NAMESPACE}" >/dev/null 2>&1; then
    ca_b64="$(kubectl get secret nico-local-ca -n "${NAMESPACE}" -o jsonpath='{.data.tls\.crt}')"
  fi

  if [[ -z "${ca_b64}" ]]; then
    ca_b64="$(printf 'placeholder' | base64 | tr -d '\n')"
  fi

  kubectl apply -f - <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: nico-roots
  namespace: ${NAMESPACE}
type: Opaque
data:
  ca.crt: ${ca_b64}
EOF
}

apply_rest_ca() {
  local ca_crt_b64=""
  local ca_key_b64=""

  if [[ "${INSTALL_REST_PREREQS}" != "1" ]]; then
    return
  fi

  ca_crt_b64="$(kubectl get secret nico-local-ca -n "${NAMESPACE}" -o jsonpath='{.data.tls\.crt}')"
  ca_key_b64="$(kubectl get secret nico-local-ca -n "${NAMESPACE}" -o jsonpath='{.data.tls\.key}')"
  if [[ -z "${ca_crt_b64}" || -z "${ca_key_b64}" ]]; then
    printf 'missing local Core CA secret in namespace %s\n' "${NAMESPACE}" >&2
    exit 1
  fi

  log "Sharing the local Core CA with REST services"
  kubectl apply -f - <<EOF
apiVersion: v1
kind: Namespace
metadata:
  name: ${REST_NAMESPACE}
---
apiVersion: v1
kind: Secret
metadata:
  name: ca-signing-secret
  namespace: ${CERT_MANAGER_NAMESPACE}
type: kubernetes.io/tls
data:
  tls.crt: ${ca_crt_b64}
  tls.key: ${ca_key_b64}
---
apiVersion: v1
kind: Secret
metadata:
  name: ca-signing-secret
  namespace: ${REST_NAMESPACE}
type: kubernetes.io/tls
data:
  tls.crt: ${ca_crt_b64}
  tls.key: ${ca_key_b64}
---
EOF

  if ! kubectl get secret site-registration -n "${REST_NAMESPACE}" >/dev/null 2>&1; then
    kubectl apply -f - <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: site-registration
  namespace: ${REST_NAMESPACE}
type: Opaque
data:
  cacert: ${ca_crt_b64}
stringData:
  site-uuid: 00000000-0000-4000-8000-000000000001
  otp: local-dev-otp-token
  creds-url: https://nico-rest-site-manager:8100/v1/sitecreds
EOF
  fi

  kubectl apply -k "${REPO_ROOT}/rest-api/deploy/kustomize/base/cert-manager-io"
  kubectl wait --for=condition=Ready clusterissuer/nico-rest-ca-issuer --timeout=180s >/dev/null
}

install_temporal() {
  if [[ "${INSTALL_REST_PREREQS}" != "1" || "${INSTALL_TEMPORAL}" != "1" ]]; then
    return
  fi

  log "Installing Temporal"
  kubectl create namespace "${TEMPORAL_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
  kubectl apply -k "${REPO_ROOT}/rest-api/deploy/kustomize/base/temporal-helm" >/dev/null
  kubectl wait --for=condition=Ready certificate/server-interservice-cert \
    -n "${TEMPORAL_NAMESPACE}" --timeout=240s >/dev/null
  kubectl wait --for=condition=Ready certificate/server-cloud-cert \
    -n "${TEMPORAL_NAMESPACE}" --timeout=240s >/dev/null
  kubectl wait --for=condition=Ready certificate/server-site-cert \
    -n "${TEMPORAL_NAMESPACE}" --timeout=240s >/dev/null

  helm upgrade --install temporal "${REPO_ROOT}/rest-api/temporal-helm/temporal" \
    --namespace "${TEMPORAL_NAMESPACE}" \
    --values "${REPO_ROOT}/rest-api/temporal-helm/temporal/values-kind.yaml" \
    --wait \
    --timeout 16m >/dev/null

  kubectl rollout status deployment/temporal-frontend -n "${TEMPORAL_NAMESPACE}" --timeout=360s >/dev/null
  kubectl rollout status deployment/temporal-history -n "${TEMPORAL_NAMESPACE}" --timeout=360s >/dev/null
  kubectl rollout status deployment/temporal-matching -n "${TEMPORAL_NAMESPACE}" --timeout=360s >/dev/null
  kubectl rollout status deployment/temporal-worker -n "${TEMPORAL_NAMESPACE}" --timeout=360s >/dev/null

  for temporal_namespace in cloud site; do
    "${REPO_ROOT}/rest-api/scripts/setup-local.sh" temporal-namespace "${temporal_namespace}"
  done
}

install_keycloak() {
  if [[ "${INSTALL_REST_PREREQS}" != "1" || "${INSTALL_KEYCLOAK}" != "1" ]]; then
    return
  fi

  log "Installing Keycloak"
  kubectl apply -k "${REPO_ROOT}/rest-api/deploy/kustomize/base/keycloak" >/dev/null
  kubectl rollout status deployment/keycloak -n "${REST_NAMESPACE}" --timeout=240s >/dev/null
}

write_generated_values() {
  local disable_tls_enforcement=""
  local automount="true"

  if [[ "${POSTGRES_SSL_MODE}" == "disable" ]]; then
    disable_tls_enforcement=$'  extraEnv:\n    - name: DISABLE_TLS_ENFORCEMENT\n      value: "1"'
  fi

  if [[ "${VAULT_AUTH_MODE}" == "root-token" ]]; then
    automount="false"
  fi

  mkdir -p "$(dirname -- "${VALUES_FILE}")"
  {
    cat <<EOF
global:
  certificate:
    issuerRef:
      kind: ${CERT_ISSUER_KIND}
      name: ${CERT_ISSUER_NAME}
      group: ${CERT_ISSUER_GROUP}

nico-api:
EOF
    if [[ -n "${ADMIN_ROOT_CERT_PEM}" ]]; then
      printf '%s\n' \
        '  siteConfig:' \
        '    adminRootCertPem: |'
      printf '%s\n' "${ADMIN_ROOT_CERT_PEM}" | sed 's/^/      /'
    fi
    cat <<EOF
  automountServiceAccountToken: ${automount}
  migrationJob:
    enabled: true
    sslMode: ${POSTGRES_SSL_MODE}
  vaultClusterInfo:
    VAULT_SERVICE: ${VAULT_ADDR}
    NICO_VAULT_MOUNT: ${VAULT_KV_MOUNT}
    NICO_VAULT_PKI_MOUNT: ${VAULT_PKI_MOUNT}
  databaseConfig: {}
${disable_tls_enforcement}

nico-bmc-proxy:
  automountServiceAccountToken: ${automount}
  vaultClusterInfo:
    VAULT_SERVICE: ${VAULT_ADDR}
    NICO_VAULT_MOUNT: ${VAULT_KV_MOUNT}
    NICO_VAULT_PKI_MOUNT: ${VAULT_PKI_MOUNT}
  databaseConfig: {}
${disable_tls_enforcement}
EOF
  } > "${VALUES_FILE}"
}

print_summary() {
  cat <<EOF

Bootstrap complete.

Namespace: ${NAMESPACE}
Generated values: ${VALUES_FILE}
Postgres endpoint: ${POSTGRES_HOST}:${POSTGRES_PORT}/${POSTGRES_DB}
Vault address: ${VAULT_ADDR}
Cert issuer: ${CERT_ISSUER_KIND}/${CERT_ISSUER_NAME}
REST prerequisites: ${INSTALL_REST_PREREQS}

Next step:
  cd ${REPO_ROOT} && devspace deploy -n ${NAMESPACE}
EOF
}

main() {
  require_bin kubectl
  require_bin helm
  require_bin base64

  install_cert_manager
  apply_core_objects
  apply_local_postgres
  apply_local_vault
  load_admin_root_cert_pem
  apply_local_issuer
  sync_nico_roots_secret
  apply_rest_ca
  install_temporal
  install_keycloak
  write_generated_values
  print_summary
}

main "$@"
