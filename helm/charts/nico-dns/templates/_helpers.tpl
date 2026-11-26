{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "nico-dns.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Expand the name of the chart.
*/}}
{{- define "nico-dns.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "nico-dns.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-dns.labels" -}}
helm.sh/chart: {{ include "nico-dns.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-dns.name" . }}
app.kubernetes.io/component: dns
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-dns.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-dns.name" . }}
app.kubernetes.io/component: dns
{{- end }}

{{/*
Global image reference
*/}}
{{- define "nico-dns.image" -}}
{{ .Values.global.image.repository }}:{{ .Values.global.image.tag }}
{{- end }}

{{/*
Certificate spec
*/}}
{{- define "nico-dns.certificateSpec" -}}
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
Resolve the pod listener port for a named Service entry using the same
precedence as the Service template: the per-entry ports[].targetPort
wins, then the chart-level service.targetPort, then the entry's own
port, then 53. Keeps the StatefulSet's named container ports describing
where nico-dns actually binds when entries are overridden individually.
Usage: include "nico-dns.listenerPort" (dict "name" "udp" "context" $)
*/}}
{{- define "nico-dns.listenerPort" -}}
{{- $entry := dict -}}
{{- range .context.Values.service.ports -}}
{{- if eq (default "" .name) $.name -}}
{{- $entry = . -}}
{{- end -}}
{{- end -}}
{{- $target := $entry.targetPort -}}
{{- if not $target -}}
{{- $target = .context.Values.service.targetPort -}}
{{- end -}}
{{- if not $target -}}
{{- $target = $entry.port -}}
{{- end -}}
{{- default 53 $target -}}
{{- end -}}
