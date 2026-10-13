{{/*
Create the chart name and version as used by the chart label.
*/}}
{{- define "nico.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a namespace- and release-scoped name for one packaged Grafana dashboard
ConfigMap. The ConfigMaps may be installed into a shared monitoring namespace,
where release name alone is not necessarily unique. Takes a dict with "root"
(the chart context) and "dashboard" (the JSON filename without its extension).
*/}}
{{- define "nico.grafanaDashboardName" -}}
{{- $prefix := printf "%s-%s" .root.Release.Namespace .root.Release.Name | trunc 32 | trimSuffix "-" -}}
{{- printf "%s-%s" $prefix .dashboard | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Labels for the Grafana dashboard ConfigMaps. User-provided global and dashboard
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
