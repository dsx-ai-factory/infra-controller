{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "nico-api.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Expand the name of the chart.
*/}}
{{- define "nico-api.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "nico-api.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-api.labels" -}}
helm.sh/chart: {{ include "nico-api.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-api.name" . }}
app.kubernetes.io/component: api
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-api.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-api.name" . }}
app.kubernetes.io/component: api
{{- end }}

{{/*
Global image reference
*/}}
{{- define "nico-api.image" -}}
{{ .Values.global.image.repository }}:{{ .Values.global.image.tag }}
{{- end }}

{{/* Validate and return the configured WebUI authentication mode. */}}
{{- define "nico-api.webAuth.configuredMode" -}}
{{- $mode := default "basic" .Values.webAuth.mode -}}
{{- if not (has $mode (list "basic" "oauth2" "none")) -}}
{{- fail (printf "nico-api.webAuth.mode must be one of basic, oauth2, or none; got %q" $mode) -}}
{{- end -}}
{{- $mode -}}
{{- end -}}

{{/* Whether extraEnv contains the legacy mode variable, including valueFrom entries. */}}
{{- define "nico-api.webAuth.hasModeOverride" -}}
{{- $found := false -}}
{{- range .Values.extraEnv -}}
  {{- if eq .name "CARBIDE_WEB_AUTH_TYPE" -}}
    {{- $found = true -}}
  {{- end -}}
{{- end -}}
{{- $found -}}
{{- end -}}

{{/* A literal legacy override, or an empty string when absent/valueFrom. */}}
{{- define "nico-api.webAuth.literalModeOverride" -}}
{{- range .Values.extraEnv -}}
  {{- if and (eq .name "CARBIDE_WEB_AUTH_TYPE") (hasKey . "value") -}}
    {{- .value -}}
  {{- end -}}
{{- end -}}
{{- end -}}

{{- define "nico-api.webAuth.hasLiteralModeOverride" -}}
{{- $found := false -}}
{{- range .Values.extraEnv -}}
  {{- if and (eq .name "CARBIDE_WEB_AUTH_TYPE") (hasKey . "value") -}}
    {{- $found = true -}}
  {{- end -}}
{{- end -}}
{{- $found -}}
{{- end -}}

{{/* Statically known effective mode. "unknown" means extraEnv uses valueFrom. */}}
{{- define "nico-api.webAuth.effectiveMode" -}}
{{- $override := include "nico-api.webAuth.literalModeOverride" . -}}
{{- if eq (include "nico-api.webAuth.hasLiteralModeOverride" .) "true" -}}
  {{- if not (has $override (list "basic" "oauth2" "none")) -}}
    {{- fail (printf "literal CARBIDE_WEB_AUTH_TYPE in nico-api.extraEnv must be one of basic, oauth2, or none; got %q" $override) -}}
  {{- end -}}
  {{- $override -}}
{{- else if eq (include "nico-api.webAuth.hasModeOverride" .) "true" -}}
unknown
{{- else -}}
{{- include "nico-api.webAuth.configuredMode" . -}}
{{- end -}}
{{- end -}}

{{/* Render a Basic password Secret for known basic mode, or conservative valueFrom mode. */}}
{{- define "nico-api.webAuth.renderBasicSecret" -}}
{{- $effective := include "nico-api.webAuth.effectiveMode" . -}}
{{- if eq $effective "basic" -}}
true
{{- else if eq $effective "unknown" -}}
  {{- if eq (include "nico-api.webAuth.configuredMode" .) "basic" -}}true{{- else -}}false{{- end -}}
{{- else -}}
false
{{- end -}}
{{- end -}}

{{- define "nico-api.webAuth.basicSecretName" -}}
{{- default "nico-api-web-basic-auth" .Values.webAuth.basic.existingSecret.name -}}
{{- end -}}

{{- define "nico-api.webAuth.basicSecretKey" -}}
{{- default "password" .Values.webAuth.basic.existingSecret.key -}}
{{- end -}}

{{- define "nico-api.webAuth.hasExistingBasicSecret" -}}
{{- if .Values.webAuth.basic.existingSecret.name -}}true{{- else -}}false{{- end -}}
{{- end -}}

{{/* Validate the optional public admin-client CA bundle. Emits no content. */}}
{{- define "nico-api.validateAdminRootCertPem" -}}
{{- $raw := .Values.siteConfig.adminRootCertPem -}}
{{- if and (not (kindIs "string" $raw)) (ne (kindOf $raw) "invalid") -}}
{{- fail "nico-api: siteConfig.adminRootCertPem must be a string containing public CA certificates" -}}
{{- end -}}
{{- $pem := $raw | default "" -}}
{{- if and $pem (not .Values.siteConfig.enabled) -}}
{{- fail "nico-api: siteConfig.adminRootCertPem requires siteConfig.enabled=true" -}}
{{- end -}}
{{- if $pem -}}
{{/* Keep these paths in sync with the site-config-files mounts in deployment.yaml. */}}
{{- $mountedPaths := list "/etc/forge/carbide-api/site/admin_root_cert_pem" "/etc/nico/nico-api/site/admin_root_cert_pem" -}}
{{- if not (has .Values.auth.adminRootCafilePath $mountedPaths) -}}
{{- fail "nico-api: siteConfig.adminRootCertPem requires auth.adminRootCafilePath to reference admin_root_cert_pem in a mounted site config directory" -}}
{{- end -}}
{{/* The remainder check also rejects keys; detect them first to return an actionable error. */}}
{{- if regexMatch `(?i)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----` $pem -}}
{{- fail "nico-api: siteConfig.adminRootCertPem must contain public CA certificates only; private keys are not allowed" -}}
{{- end -}}
{{- $certificatePattern := `(?m)^-----BEGIN CERTIFICATE-----\r?\n([A-Za-z0-9+/=]+\r?\n)+-----END CERTIFICATE-----\r?$` -}}
{{- $certificates := regexFindAll $certificatePattern $pem -1 -}}
{{- if eq (len $certificates) 0 -}}
{{- fail "nico-api: siteConfig.adminRootCertPem must contain at least one canonical PEM CERTIFICATE block" -}}
{{- end -}}
{{- $remainder := regexReplaceAll $certificatePattern $pem "" | trim -}}
{{- if ne $remainder "" -}}
{{- fail "nico-api: siteConfig.adminRootCertPem must contain only bare PEM CERTIFICATE blocks and whitespace; remove metadata/comments and fix malformed blocks" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
Certificate spec
Usage: {{ include "nico-api.certificateSpec" (dict "name" "{{ include "nico-api.name" . }}-certificate" "cert" .Values.certificate "global" .Values.global "namespace" (include "nico-api.namespace" .)) }}
*/}}
{{- define "nico-api.certificateSpec" -}}
duration: {{ .global.certificate.duration }}
renewBefore: {{ .global.certificate.renewBefore }}
commonName: {{ printf "%s.%s.svc.cluster.local" (.cert.serviceName | default .svcName) (.cert.identityNamespace | default .namespace) }}
dnsNames:
{{- if .cert.dnsNames }}
{{- range .cert.dnsNames }}
  - {{ . }}
{{- end }}
{{- else }}
  - {{ printf "%s.%s.svc.cluster.local" (.cert.serviceName | default .svcName) (.cert.identityNamespace | default .namespace) }}
{{- if ne (toString .cert.includeShortDnsName) "false" }}
  - {{ printf "%s.%s" (.cert.serviceName | default .svcName) (.cert.identityNamespace | default .namespace) }}
{{- end }}
{{- range .cert.extraDnsNames | default list }}
{{- if . }}
  - {{ . | quote }}
{{- end }}
{{- end }}
{{- end }}
{{- if .cert.ipAddresses }}
ipAddresses:
{{- range .cert.ipAddresses }}
  - {{ . }}
{{- end }}
{{- end }}
uris:
{{- if .cert.uris }}
{{- range .cert.uris }}
  - {{ . }}
{{- end }}
{{- else }}
  - {{ printf "spiffe://%s/%s/sa/%s" .global.spiffe.trustDomain (.cert.identityNamespace | default .namespace) (.cert.spiffeServiceName | default .cert.serviceName | default .svcName) }}
{{- range .cert.extraUris | default list }}
  - {{ . }}
{{- end }}
{{- end }}
privateKey:
  algorithm: {{ .global.certificate.privateKey.algorithm }}
  size: {{ .global.certificate.privateKey.size }}
issuerRef:
  kind: {{ .global.certificate.issuerRef.kind }}
  name: {{ .global.certificate.issuerRef.name }}
  group: {{ .global.certificate.issuerRef.group }}
secretName: {{ .name }}
{{- end }}

{{/*
Service monitor spec
Usage: {{ include "nico-api.serviceMonitorSpec" (dict "name" "{{ include "nico-api.name" . }}" "port" "http" "monitor" .Values.serviceMonitor "namespace" "nico-system") }}
*/}}
{{- define "nico-api.serviceMonitorSpec" -}}
endpoints:
  - honorLabels: false
    interval: {{ .monitor.interval }}
    port: {{ .port }}
    scheme: http
    scrapeTimeout: {{ .monitor.scrapeTimeout }}
namespaceSelector:
  matchNames:
    - {{ .namespace }}
selector:
  matchLabels:
    app.kubernetes.io/metrics: {{ .name }}
{{- end }}

{{/*
Whether the per-object state metrics listener, port, and Service are active:
enabled with a non-empty (and non-null) objectTypes list.
*/}}
{{- define "nico-api.perObjectStateMetricsActive" -}}
{{- if and .Values.service.perObjectStateMetrics.enabled (gt (len (default list .Values.service.perObjectStateMetrics.objectTypes)) 0) -}}true{{- end -}}
{{- end }}
