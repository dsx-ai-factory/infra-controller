{{- define "nico-rest-cert-manager.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-rest-cert-manager.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-cert-manager.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-cert-manager.labels" -}}
helm.sh/chart: {{ include "nico-rest-cert-manager.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-rest-cert-manager
app.kubernetes.io/component: cert-manager
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "nico-rest-cert-manager.selectorLabels" -}}
app: nico-rest-cert-manager
app.kubernetes.io/name: nico-rest-cert-manager
app.kubernetes.io/component: cert-manager
{{- end }}

{{/*
SANs for the HTTPS listener's self-issued certificate. Clients reach the
Service by both its short name and its namespace-qualified forms, so all of
them have to be present or hostname verification fails for the ones that are
not.
*/}}
{{- define "nico-rest-cert-manager.dnsNames" -}}
{{- if .Values.config.dnsNames -}}
{{- toYaml .Values.config.dnsNames -}}
{{- else -}}
{{- $namespace := include "nico-rest-cert-manager.namespace" . -}}
{{- toYaml (list
    "nico-rest-cert-manager"
    (printf "nico-rest-cert-manager.%s" $namespace)
    (printf "nico-rest-cert-manager.%s.svc" $namespace)
    (printf "nico-rest-cert-manager.%s.svc.cluster.local" $namespace)
    "localhost") -}}
{{- end -}}
{{- end -}}

{{- define "nico-rest-cert-manager.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}
