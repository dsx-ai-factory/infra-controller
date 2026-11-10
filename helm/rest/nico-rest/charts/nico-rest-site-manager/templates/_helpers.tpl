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
has to be a SAN on that service's certificate, so when nico-rest-cert-manager
is the subchart being relocated, set args.certManagerUrl explicitly.
*/}}
{{- define "nico-rest-site-manager.certManagerUrl" -}}
{{- $url := .Values.args.credsManagerUrl | default .Values.args.certManagerUrl -}}
{{- if $url -}}
{{- $url -}}
{{- else -}}
{{- printf "https://nico-rest-cert-manager.%s:8000" .Release.Namespace -}}
{{- end -}}
{{- end -}}

{{/*
REST Cert Manager trust settings. The deprecated former names
args.credsManagerCaPath and credsManagerCa override the current ones when set,
since the current names carry defaults. The current names can also be absent:
helm upgrade --reuse-values renders with the previous release's values, which
predate them.
*/}}
{{- define "nico-rest-site-manager.certManagerCaPath" -}}
{{- .Values.args.credsManagerCaPath | default .Values.args.certManagerCaPath -}}
{{- end -}}

{{- define "nico-rest-site-manager.certManagerCaSecretName" -}}
{{- (.Values.credsManagerCa | default dict).secretName | default (.Values.certManagerCa | default dict).secretName -}}
{{- end -}}

{{- define "nico-rest-site-manager.certManagerCaKey" -}}
{{- (.Values.credsManagerCa | default dict).key | default (.Values.certManagerCa | default dict).key -}}
{{- end -}}

{{- define "nico-rest-site-manager.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}
