{{- define "nico-rest-site-manager.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-rest-site-manager.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-site-manager.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-site-manager.labels" -}}
helm.sh/chart: {{ include "nico-rest-site-manager.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-rest-site-manager
app.kubernetes.io/component: site-manager
{{- end }}

{{- define "nico-rest-site-manager.selectorLabels" -}}
app: nico-rest-site-manager
app.kubernetes.io/name: nico-rest-site-manager
app.kubernetes.io/component: site-manager
{{- end }}

{{/*
Endpoint for nico-rest-cert-manager. Built from the release namespace rather
than this chart's, because nico-rest-cert-manager has its own independent
namespaceOverride and does not move when site-manager's is set. The hostname
has to be a SAN on that service's certificate, so when cert-manager is the
subchart being relocated, set args.credsManagerUrl explicitly.
*/}}
{{- define "nico-rest-site-manager.credsManagerUrl" -}}
{{- if .Values.args.credsManagerUrl -}}
{{- .Values.args.credsManagerUrl -}}
{{- else -}}
{{- printf "https://nico-rest-cert-manager.%s:8000" .Release.Namespace -}}
{{- end -}}
{{- end -}}

{{- define "nico-rest-site-manager.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}
