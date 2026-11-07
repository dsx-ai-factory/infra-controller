{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "nico-ntp.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Expand the name of the chart.
*/}}
{{- define "nico-ntp.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "nico-ntp.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-ntp.labels" -}}
helm.sh/chart: {{ include "nico-ntp.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-ntp.name" . }}
app.kubernetes.io/component: ntp
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-ntp.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-ntp.name" . }}
app.kubernetes.io/component: ntp
{{- end }}

{{/*
Comma-separated NTP_SERVERS value for the chrony container.

Combines the configured upstream servers with cluster-local peer hostnames
(one per pod, derived from the StatefulSet's headless service) so the
replicas peer with each other in addition to the upstreams.
*/}}
{{- define "nico-ntp.ntpServers" -}}
{{- $upstreams := .Values.ntp.upstreamServers | default (list) -}}
{{- $name := include "nico-ntp.name" . -}}
{{- $ns := include "nico-ntp.namespace" . -}}
{{- $replicas := int .Values.replicas -}}
{{- $peers := list -}}
{{- range $i, $_ := until $replicas -}}
{{- $peers = append $peers (printf "%s-%d.%s.%s.svc.cluster.local" $name $i $name $ns) -}}
{{- end -}}
{{- $all := concat $upstreams $peers -}}
{{- join "," $all -}}
{{- end -}}

{{/*
Comma-separated NTP_DIRECTIVES value for the chrony container.
*/}}
{{- define "nico-ntp.ntpDirectives" -}}
{{- join "," (.Values.ntp.directives | default (list)) -}}
{{- end -}}
