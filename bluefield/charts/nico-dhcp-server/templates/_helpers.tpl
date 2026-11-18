{{/*
Expand the name of the chart.
*/}}
{{- define "nico-dhcp-server.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "nico-dhcp-server.fullname" -}}
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
{{- define "nico-dhcp-server.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-dhcp-server.labels" -}}
helm.sh/chart: {{ include "nico-dhcp-server.chart" . }}
{{ include "nico-dhcp-server.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-dhcp-server.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-dhcp-server.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Server Port
*/}}
{{- define "nico-dhcp-server.serverPort" -}}
{{- default 10079 .Values.serverPort -}}
{{- end -}}

{{/*
Keep workload rendering enabled for values retained from chart versions without the switch.
*/}}
{{- define "nico-dhcp-server.serviceDaemonSetEnabled" -}}
{{- if hasKey .Values.serviceDaemonSet "enabled" -}}
{{- .Values.serviceDaemonSet.enabled -}}
{{- else -}}
true
{{- end -}}
{{- end -}}
