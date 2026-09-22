{{/*
Effective namespace for chart resources.
Uses global.namespaceOverride if set, otherwise Release.Namespace.
*/}}
{{- define "nico-machine-a-tron.namespace" -}}
{{- if and .Values.global .Values.global.namespaceOverride -}}
{{- .Values.global.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- .Release.Namespace -}}
{{- end -}}
{{- end -}}

{{/*
Resource name prefix. Defaults to the chart name; override with nameOverride.
*/}}
{{- define "nico-machine-a-tron.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-machine-a-tron.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
What image to use: Use subchart-local image if defined, fall back on global
image. In devspace deployments, {{ include "nico-machine-a-tron.name" . }} gets its own image.
In other deployments, the main nico image contains all binaries.
*/}}
{{- define "nico-machine-a-tron.image" -}}
{{- if not (eq (toString (.Values.image.repository | default "")) "") }}
{{- .Values.image.repository }}:{{ .Values.image.tag | default "latest" }}
{{- else if and .Values.global.image (not (eq (toString (.Values.global.image.repository | default "")) "")) (not (eq (toString (.Values.global.image.tag | default "")) "")) }}
{{- .Values.global.image.repository }}:{{ .Values.global.image.tag }}
{{- else }}
{{- "nico:latest" }}
{{- end }}
{{- end }}

{{- define "nico-machine-a-tron.labels" -}}
helm.sh/chart: {{ include "nico-machine-a-tron.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-machine-a-tron.name" . }}
app.kubernetes.io/component: machine-a-tron
{{- end }}

{{- define "nico-machine-a-tron.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-machine-a-tron.name" . }}
app.kubernetes.io/component: machine-a-tron
{{- end }}

{{/*
Count configured pods.
*/}}
{{- define "nico-machine-a-tron.activePods" -}}
{{- $activePods := 0 -}}
{{- range $podName, $podConfig := .Values.pods -}}
{{- if and $podConfig (or (gt (len ($podConfig.machines | default dict)) 0) (gt (len ($podConfig.racks | default dict)) 0)) -}}
{{- $activePods = add $activePods 1 -}}
{{- end -}}
{{- end -}}
{{- print $activePods -}}
{{- end }}

{{- define "nico-machine-a-tron.certificateSpec" -}}
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
secretName: {{ .secretName | default .name }}
{{- end }}

{{- define "nico-machine-a-tron.serviceMonitorSpec" -}}
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
Calculate DHCP relay ClusterIP for a pod.
Adds podIndex to the base IP's last octet.
Usage: include "nico-machine-a-tron.dhcpRelayIP" (dict "baseIP" "10.96.127.10" "podIndex" 0)
Returns: "10.96.127.10" for podIndex 0, "10.96.127.11" for podIndex 1, etc.
*/}}
{{- define "nico-machine-a-tron.dhcpRelayIP" -}}
{{- $parts := splitList "." .baseIP -}}
{{- $lastOctet := index $parts 3 | int -}}
{{- $newLastOctet := add $lastOctet .podIndex -}}
{{- if gt $newLastOctet 255 -}}
{{- fail (printf "DHCP relay IP overflow: base %s + pod index %d exceeds .255" .baseIP .podIndex) -}}
{{- end -}}
{{- printf "%s.%s.%s.%d" (index $parts 0) (index $parts 1) (index $parts 2) $newLastOctet -}}
{{- end }}

{{/*
Check if DHCP relay mode is enabled.
Returns "true" if dhcpRelay.baseIP is set.
*/}}
{{- define "nico-machine-a-tron.dhcpRelayEnabled" -}}
{{- if and .Values.dhcpRelay .Values.dhcpRelay.baseIP -}}
true
{{- end -}}
{{- end }}

{{/*
Get DHCP server address.
Uses dhcpRelay.serverAddress if set, otherwise defaults to nico-dhcp.nico-system.
Returns: "<host>:<port>"
*/}}
{{- define "nico-machine-a-tron.dhcpServerAddress" -}}
{{- if and .Values.dhcpRelay .Values.dhcpRelay.serverAddress -}}
{{- .Values.dhcpRelay.serverAddress -}}
{{- else -}}
{{- print "nico-dhcp.nico-system.svc.cluster.local:67" -}}
{{- end -}}
{{- end }}
