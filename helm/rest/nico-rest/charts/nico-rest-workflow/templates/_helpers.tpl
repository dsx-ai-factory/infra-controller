{{- define "nico-rest-workflow.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nico-rest-workflow.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-workflow.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "nico-rest-workflow.labels" -}}
helm.sh/chart: {{ include "nico-rest-workflow.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: nico-rest
app.kubernetes.io/name: nico-rest-workflow
{{- end }}

{{- define "nico-rest-workflow.image" -}}
{{ .Values.global.image.repository }}/{{ .Values.image.name }}:{{ .Values.global.image.tag }}
{{- end }}

{{- define "nico-rest-workflow.dbCredsVolumeMount" -}}
{{- if .Values.secrets.dbCreds }}
- name: db-creds
  mountPath: /var/secrets/db
  readOnly: true
{{- end }}
{{- end }}

{{- define "nico-rest-workflow.dbCredsVolume" -}}
{{- if .Values.secrets.dbCreds }}
- name: db-creds
  secret:
    secretName: {{ .Values.secrets.dbCreds }}
    items:
      - key: password
        path: password
{{- end }}
{{- end }}

{{/*
GOMEMLIMIT value for a worker's resources: 85 percent of limits.memory, rounded
down to whole MiB. Empty unless the limit is a whole number of Mi or Gi, of at
most nine digits, and the result is at least 1 MiB.
*/}}
{{- define "nico-rest-workflow.goMemLimit" -}}
{{- $limit := ((. | default dict).limits | default dict).memory | default "" | toString -}}
{{- if regexMatch "^[0-9]{1,9}(Mi|Gi)$" $limit -}}
{{- $mib := regexFind "^[0-9]+" $limit | atoi -}}
{{- if hasSuffix "Gi" $limit -}}
{{- $mib = mul $mib 1024 -}}
{{- end -}}
{{- $value := div (mul $mib 85) 100 -}}
{{- if gt $value 0 -}}
{{- printf "%dMiB" $value -}}
{{- end -}}
{{- end -}}
{{- end }}
