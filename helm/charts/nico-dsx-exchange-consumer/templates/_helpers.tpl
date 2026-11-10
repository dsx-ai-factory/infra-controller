{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "nico-dsx-exchange-consumer.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Expand the name of the chart.
*/}}
{{- define "nico-dsx-exchange-consumer.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "nico-dsx-exchange-consumer.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-dsx-exchange-consumer.labels" -}}
helm.sh/chart: {{ include "nico-dsx-exchange-consumer.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-dsx-exchange-consumer.name" . }}
app.kubernetes.io/component: dsx-exchange-consumer
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-dsx-exchange-consumer.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-dsx-exchange-consumer.name" . }}
app.kubernetes.io/component: dsx-exchange-consumer
{{- end }}

{{/*
Global image reference
*/}}
{{- define "nico-dsx-exchange-consumer.image" -}}
{{ .Values.global.image.repository }}:{{ .Values.global.image.tag }}
{{- end }}

{{/*
Certificate spec
*/}}
{{- define "nico-dsx-exchange-consumer.certificateSpec" -}}
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
*/}}
{{- define "nico-dsx-exchange-consumer.serviceMonitorSpec" -}}
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
Whether a credentials Secret is configured. secretName and key are required
together.
*/}}
{{- define "nico-dsx-exchange-consumer.credentialsEnabled" -}}
{{- $c := .Values.credentials | default dict -}}
{{- if and $c.secretName (not $c.key) -}}
{{- fail "credentials.key is required when credentials.secretName is set" -}}
{{- end -}}
{{- if and $c.key (not $c.secretName) -}}
{{- fail "credentials.secretName is required when credentials.key is set" -}}
{{- end -}}
{{- if and $c.secretName $c.key -}}true{{- end -}}
{{- end }}

{{/* Path of the credentials file inside the container. */}}
{{- define "nico-dsx-exchange-consumer.credentialsFilePath" -}}
{{- printf "%s/%s" (.Values.credentials.mountPath | trimSuffix "/") .Values.credentials.key -}}
{{- end }}
