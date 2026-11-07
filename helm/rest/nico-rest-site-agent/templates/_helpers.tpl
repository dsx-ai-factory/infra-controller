{{- define "nico-rest-site-agent.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-rest-site-agent.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-site-agent.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-site-agent.labels" -}}
helm.sh/chart: {{ include "nico-rest-site-agent.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-rest-site-agent
app.kubernetes.io/component: site-agent
{{- end }}

{{- define "nico-rest-site-agent.selectorLabels" -}}
app: nico-rest-site-agent
app.kubernetes.io/name: nico-rest-site-agent
app.kubernetes.io/component: site-agent
{{- end }}

{{- define "nico-rest-site-agent.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}
