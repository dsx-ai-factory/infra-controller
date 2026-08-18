{{- define "nico-rest-api.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-rest-api.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-api.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-api.labels" -}}
helm.sh/chart: {{ include "nico-rest-api.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-rest-api
app.kubernetes.io/component: api
{{- end }}

{{- define "nico-rest-api.selectorLabels" -}}
app: nico-rest-api
app.kubernetes.io/name: nico-rest-api
app.kubernetes.io/component: api
{{- end }}

{{- define "nico-rest-api.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}

{{- define "nico-rest-api.validateAuth" -}}
{{- if and (not .Values.config.keycloak.enabled) (not .Values.config.issuers) -}}
{{- fail "Either keycloak must be enabled or at least one JWT issuer must be configured in config.issuers" -}}
{{- end -}}
{{- if and .Values.config.keycloak.enabled .Values.config.issuers -}}
{{- fail "keycloak and issuers are mutually exclusive — enable only one" -}}
{{- end -}}
{{- end -}}

{{- define "nico-rest-api.validatePowerProvisioning" -}}
{{- if not (kindIs "bool" .Values.config.powerProvisioning.dps.enabled) -}}
{{- fail "config.powerProvisioning.dps.enabled must be a boolean" -}}
{{- end -}}
{{- if .Values.config.powerProvisioning.dps.enabled -}}
{{- if not .Values.config.powerProvisioning.dps.endpoint -}}
{{- fail "config.powerProvisioning.dps.endpoint is required when direct DPS integration is enabled" -}}
{{- end -}}
{{- if not (kindIs "string" .Values.secrets.dpsCreds) -}}
{{- fail "secrets.dpsCreds must be a string" -}}
{{- end -}}
{{- if not .Values.secrets.dpsCreds -}}
{{- fail "secrets.dpsCreds is required when direct DPS integration is enabled" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "nico-rest-api.validateExposure" -}}
{{- if and .Values.ingress.enabled .Values.nodePort.enabled -}}
{{- fail "nico-rest-api: ingress and nodePort cannot both be enabled; disable nodePort to avoid exposing plaintext HTTP alongside TLS ingress" -}}
{{- end -}}
{{- if and .Values.ingress.enabled (not .Values.ingress.hosts) -}}
{{- fail "nico-rest-api: ingress.enabled requires at least one entry in ingress.hosts" -}}
{{- end -}}
{{- end -}}
