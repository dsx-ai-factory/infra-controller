{{- define "nico-mcp.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-mcp.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-mcp.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-mcp.labels" -}}
helm.sh/chart: {{ include "nico-mcp.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-mcp
app.kubernetes.io/component: mcp
{{- end }}

{{- define "nico-mcp.selectorLabels" -}}
app: nico-mcp
app.kubernetes.io/name: nico-mcp
app.kubernetes.io/component: mcp
{{- end }}

{{- define "nico-mcp.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}
