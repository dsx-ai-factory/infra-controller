{{/*
Create the chart name and version as used by the chart label.
*/}}
{{- define "nico.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a namespace- and release-scoped name for the packaged Grafana
dashboards. The dashboard ConfigMap may be installed into a shared monitoring
namespace, where release name alone is not necessarily unique.
*/}}
{{- define "nico.grafanaDashboardsName" -}}
{{- printf "%s-%s-grafana-dashboards" .Release.Namespace .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Labels for the Grafana dashboard ConfigMap. User-provided global and dashboard
labels override the chart defaults, in that order.
*/}}
{{- define "nico.grafanaDashboardLabels" -}}
{{- $labels := dict
  "helm.sh/chart" (include "nico.chart" .)
  "app.kubernetes.io/name" .Chart.Name
  "app.kubernetes.io/instance" .Release.Name
  "app.kubernetes.io/component" "observability"
  "app.kubernetes.io/managed-by" .Release.Service
-}}
{{- with .Values.global.labels }}
{{- $labels = mergeOverwrite $labels . }}
{{- end }}
{{- with .Values.grafanaDashboards.labels }}
{{- $labels = mergeOverwrite $labels . }}
{{- end }}
{{- toYaml $labels -}}
{{- end -}}
