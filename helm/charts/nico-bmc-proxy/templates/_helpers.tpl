{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "nico-bmc-proxy.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Resource name prefix. Defaults to the chart name; override with nameOverride.
*/}}
{{- define "nico-bmc-proxy.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-bmc-proxy.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
What image to use: Use subchart-local image if defined, fall back on global
image. In devspace deployments, {{ include "nico-bmc-proxy.name" . }} gets its own image. In other
deployments, the main nico image contains all binaries, so we can use that.
*/}}
{{- define "nico-bmc-proxy.image" -}}
{{- if not (eq (toString (.Values.image.repository | default "")) "") }}
{{- .Values.image.repository }}:{{ .Values.image.tag | default "latest" }}
{{- else }}
{{- .Values.global.image.repository }}:{{ .Values.global.image.tag }}
{{- end }}
{{- end }}

{{/*
Validate and return the redirect handling mode.
*/}}
{{- define "nico-bmc-proxy.redirectMode" -}}
{{- $redirectMode := required "bmcProxy.redirectMode is required" .Values.bmcProxy.redirectMode -}}
{{- if not (has $redirectMode (list "follow_same_origin" "return_to_client")) -}}
{{- fail (printf "bmcProxy.redirectMode must be one of follow_same_origin or return_to_client; got %q" $redirectMode) -}}
{{- end -}}
{{- $redirectMode -}}
{{- end -}}

{{- define "nico-bmc-proxy.labels" -}}
helm.sh/chart: {{ include "nico-bmc-proxy.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-bmc-proxy.name" . }}
app.kubernetes.io/component: bmc-proxy
{{- end }}

{{- define "nico-bmc-proxy.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-bmc-proxy.name" . }}
app.kubernetes.io/component: bmc-proxy
{{- end }}

{{- define "nico-bmc-proxy.certificateSpec" -}}
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

{{- define "nico-bmc-proxy.serviceMonitorSpec" -}}
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
