{{- define "nico-postgres-exporter.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "nico-postgres-exporter.selectorLabels" -}}
app.kubernetes.io/name: nico-postgres-exporter
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "nico-postgres-exporter.labels" -}}
{{ include "nico-postgres-exporter.selectorLabels" . }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
component: nico-postgres-exporter
{{- end -}}
