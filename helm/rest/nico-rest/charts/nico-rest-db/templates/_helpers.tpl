{{- define "nico-rest-db.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-rest-db.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-db.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-db.labels" -}}
helm.sh/chart: {{ include "nico-rest-db.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-rest-db
app.kubernetes.io/component: migrations
{{- end }}

{{- define "nico-rest-db.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}
