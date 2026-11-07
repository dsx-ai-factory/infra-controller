{{- define "nico-rest-common.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-rest-common.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-common.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-common.labels" -}}
helm.sh/chart: {{ include "nico-rest-common.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-rest-common
app.kubernetes.io/component: common
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}
