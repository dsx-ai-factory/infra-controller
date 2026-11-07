{{/*
Expand the name of the chart.
*/}}
{{- define "nico-dpu-agent.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "nico-dpu-agent.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- /* DPF names the helm release <dpu-cluster>-<dpu-service>-<hash>; */}}
{{- /* use it verbatim so resource names stay short and don't get a   */}}
{{- /* redundant chart-name suffix appended.                          */}}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "nico-dpu-agent.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-dpu-agent.labels" -}}
helm.sh/chart: {{ include "nico-dpu-agent.chart" . }}
{{ include "nico-dpu-agent.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-dpu-agent.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-dpu-agent.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Metrics port for collecting agent metrics
*/}}
{{- define "nico-dpu-agent.metricsPort" -}}
{{- default 8888 .Values.metricsPort -}}
{{- end -}}
