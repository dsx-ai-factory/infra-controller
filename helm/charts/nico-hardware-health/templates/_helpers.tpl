{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "nico-hardware-health.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Expand the name of the chart.
*/}}
{{- define "nico-hardware-health.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "nico-hardware-health.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-hardware-health.labels" -}}
helm.sh/chart: {{ include "nico-hardware-health.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-hardware-health.name" . }}
app.kubernetes.io/component: hardware-health
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-hardware-health.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-hardware-health.name" . }}
app.kubernetes.io/component: hardware-health
{{- end }}

{{/*
Global image reference
*/}}
{{- define "nico-hardware-health.image" -}}
{{ .Values.global.image.repository }}:{{ .Values.global.image.tag }}
{{- end }}

{{/*
Certificate spec
*/}}
{{- define "nico-hardware-health.certificateSpec" -}}
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
{{- define "nico-hardware-health.serviceMonitorSpec" -}}
endpoints:
  - honorLabels: false
    interval: {{ .monitor.interval }}
    port: {{ .port }}
    path: /metrics
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
Telemetry service monitor spec (/telemetry — Prometheus sink sensor gauges).
*/}}
{{- define "nico-hardware-health.telemetryServiceMonitorSpec" -}}
endpoints:
  - honorLabels: false
    interval: {{ .monitor.interval }}
    port: {{ .port }}
    path: /telemetry
    scheme: http
    scrapeTimeout: {{ .monitor.scrapeTimeout }}
namespaceSelector:
  matchNames:
    - {{ .namespace }}
selector:
  matchLabels:
    app.kubernetes.io/metrics: {{ .name }}
{{- end }}

{{/* Fail unless terminationGracePeriodSeconds exceeds hardware-health's shutdown_timeout. */}}
{{- define "nico-hardware-health.validateTerminationGracePeriod" -}}
{{- $shutdownTimeout := "20s" -}}
{{- if and (kindIs "map" .Values.env) (hasKey .Values.env "CARBIDE_HEALTH__SHUTDOWN_TIMEOUT") -}}
{{- $shutdownTimeout = toString (index .Values.env "CARBIDE_HEALTH__SHUTDOWN_TIMEOUT") -}}
{{- end -}}
{{- if and (regexMatch "^[0-9]+s$" $shutdownTimeout) (le (int .Values.terminationGracePeriodSeconds) (int (trimSuffix "s" $shutdownTimeout))) -}}
{{- fail (printf "terminationGracePeriodSeconds (%v) must exceed hardware-health's shutdown_timeout (%s)" .Values.terminationGracePeriodSeconds $shutdownTimeout) -}}
{{- end -}}
{{- end -}}
