{{/*
Expand the name of the chart.
*/}}
{{- define "nico-otelcol.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "nico-otelcol.fullname" -}}
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
{{- define "nico-otelcol.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-otelcol.labels" -}}
helm.sh/chart: {{ include "nico-otelcol.chart" . }}
{{ include "nico-otelcol.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-otelcol.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-otelcol.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Prometheus scrape port (must match DPUServiceConfiguration configPorts in NICo dpf_services).
*/}}
{{- define "nico-otelcol.prometheusPort" -}}
{{- default 9999 .Values.prometheusPort -}}
{{- end -}}
