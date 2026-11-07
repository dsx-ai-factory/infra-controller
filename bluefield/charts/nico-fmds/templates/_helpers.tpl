{{/*
Expand the name of the chart.
*/}}
{{- define "nico-fmds.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "nico-fmds.fullname" -}}
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
{{- define "nico-fmds.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-fmds.labels" -}}
helm.sh/chart: {{ include "nico-fmds.chart" . }}
{{ include "nico-fmds.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-fmds.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-fmds.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Server Port
*/}}
{{- define "nico-fmds.serverPort" -}}
{{- default 50052 .Values.serverPort -}}
{{- end -}}

{{/*
Metrics port for collecting FMDS metrics
*/}}
{{- define "nico-fmds.metricsPort" -}}
{{- default 8888 .Values.metricsPort -}}
{{- end -}}
