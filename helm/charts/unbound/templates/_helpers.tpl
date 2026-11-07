{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "unbound.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Expand the name of the chart.
*/}}
{{- define "unbound.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "unbound.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels (metadata)
*/}}
{{- define "unbound.labels" -}}
helm.sh/chart: {{ include "unbound.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/name: unbound
app.kubernetes.io/component: recursive-dns
{{- end }}

{{/*
Common annotations
*/}}
{{- define "unbound.annotations" -}}
managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels — used in Deployment matchLabels and Service selector
*/}}
{{- define "unbound.selectorLabels" -}}
app.kubernetes.io/component: recursive-dns
app.kubernetes.io/instance: nico-unbound
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/name: unbound
{{- end }}

{{/*
Pod template labels
*/}}
{{- define "unbound.podLabels" -}}
app.kubernetes.io/component: recursive-dns
app.kubernetes.io/instance: nico-unbound
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/name: unbound
{{- end }}
