{{/*
Resolve the namespace flow runs in.
*/}}
{{- define "nico-flow.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Chart name + version label.
*/}}
{{- define "nico-flow.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end -}}

{{/*
Common labels for every flow object.
*/}}
{{- define "nico-flow.labels" -}}
helm.sh/chart: {{ include "nico-flow.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: flow
app.kubernetes.io/component: orchestrator
{{- end -}}

{{/*
Pod selector labels — must match the pod template labels in deployment.yaml.
*/}}
{{- define "nico-flow.selectorLabels" -}}
app: flow
app.kubernetes.io/name: flow
{{- end -}}

{{/*
Flow image reference. If images.flow.repository is empty, fall back to
<global.image.repository>/nico-flow. Same for the tag.
Usage: {{ include "nico-flow.image" (dict "component" "flow" "Values" .Values) }}
*/}}
{{- define "nico-flow.image" -}}
{{- $component := .component -}}
{{- $values := .Values -}}
{{- $override := index $values.images $component -}}
{{- $repo := $override.repository -}}
{{- if not $repo -}}
{{- $repo = printf "%s/nico-%s" $values.global.image.repository $component -}}
{{- end -}}
{{- $tag := $override.tag -}}
{{- if not $tag -}}
{{- $tag = $values.global.image.tag -}}
{{- end -}}
{{- printf "%s:%s" $repo $tag -}}
{{- end -}}

{{/*
SPIFFE Certificate spec for Flow.
*/}}
{{- define "nico-flow.certificateSpec" -}}
duration: {{ .global.certificate.duration }}
renewBefore: {{ .global.certificate.renewBefore }}
commonName: {{ printf "%s.%s.svc.cluster.local" .cert.serviceName .namespace }}
dnsNames:
  - {{ printf "%s.%s.svc.cluster.local" .cert.serviceName .namespace }}
  - {{ printf "%s.%s" .cert.serviceName .namespace }}
{{- range .cert.extraDnsNames | default list }}
  - {{ printf "%s.%s.svc.cluster.local" . $.namespace }}
  - {{ printf "%s.%s" . $.namespace }}
{{- end }}
uris:
  ## Exactly one SPIFFE URI by design — carbide-core's authn middleware
  ## (crates/authn/src/lib.rs) rejects certificates whose SAN extension carries
  ## more than one URI. The single URI must satisfy nico-api end-to-end:
  ##   1. trust domain is one of nico-api's spiffe_trust_domain(s)
  ##   2. <apiIdentity.namespace>/sa/ matches nico-api's spiffe_service_base_paths
  ##      (decoupled from the Kubernetes namespace Flow runs in — flow's K8s
  ##      namespace `flow` is not in the allow-list; `nico-system` is)
  ##   3. <apiIdentity.serviceName> matches an InternalRBACRules principal
  ##      (the upstream rename PR teaches nico-api to accept both `nico-flow`
  ##      and `carbide-flow`, so either value works during the transition)
  - {{ printf "spiffe://%s/%s/sa/%s" .global.spiffe.trustDomain .cert.apiIdentity.namespace .cert.apiIdentity.serviceName }}
privateKey:
  algorithm: {{ .global.certificate.privateKey.algorithm }}
  size: {{ .global.certificate.privateKey.size }}
issuerRef:
  kind: {{ .global.certificate.issuerRef.kind }}
  name: {{ .global.certificate.issuerRef.name }}
  group: {{ .global.certificate.issuerRef.group }}
secretName: {{ .name }}
{{- end -}}

{{/*
Data encryption key settings, nil-safe for releases whose reused values predate
the dataEncryption block (chart 0.1.0).
*/}}
{{- define "nico-flow.dataEncryptionExistingSecret" -}}
{{- $key := default dict (get (default dict .Values.dataEncryption) "key") -}}
{{- trim (default "" (get $key "existingSecret")) -}}
{{- end -}}
{{- define "nico-flow.dataEncryptionKeyValue" -}}
{{- $key := default dict (get (default dict .Values.dataEncryption) "key") -}}
{{- trim (default "" (get $key "value")) -}}
{{- end -}}
