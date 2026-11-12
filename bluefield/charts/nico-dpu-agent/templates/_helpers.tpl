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

{{/* Service names must be consistent across DPUClusters. */}}
{{- define "nico-dpu-agent.serviceName" -}}
{{- if dig "dpf" "multiClusterSupport" false .Values.AsMap -}}
{{- dig "dpf" "dpuService" "name" "" .Values.AsMap | default (include "nico-dpu-agent.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- include "nico-dpu-agent.fullname" . -}}
{{- end -}}
{{- end }}

{{- define "nico-dpu-agent.serviceNamespace" -}}
{{- if dig "dpf" "multiClusterSupport" false .Values.AsMap -}}
{{- dig "dpf" "dpuService" "namespace" "" .Values.AsMap | default .Release.Namespace -}}
{{- else -}}
dpf-operator-system
{{- end -}}
{{- end }}

{{- define "nico-dpu-agent.serviceHost" -}}
{{- $name := .name -}}
{{- if not (dig "dpf" "multiClusterSupport" false .context.Values.AsMap) -}}
{{- $name = printf "carbide-dpf-cluster-%s" $name -}}
{{- end -}}
{{- printf "%s.%s.svc.cluster.local" $name (include "nico-dpu-agent.serviceNamespace" .context) -}}
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
