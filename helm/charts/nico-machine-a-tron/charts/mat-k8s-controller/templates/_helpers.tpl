{{/*
Expand the name of the chart.
*/}}
{{- define "mat-k8s-controller.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "mat-k8s-controller.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "mat-k8s-controller.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "mat-k8s-controller.labels" -}}
helm.sh/chart: {{ include "mat-k8s-controller.chart" . }}
{{ include "mat-k8s-controller.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "mat-k8s-controller.selectorLabels" -}}
app.kubernetes.io/name: {{ include "mat-k8s-controller.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "mat-k8s-controller.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "mat-k8s-controller.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Namespace - always uses the same namespace as the parent chart.
*/}}
{{- define "mat-k8s-controller.namespace" -}}
{{- if and .Values.global .Values.global.namespaceOverride }}
{{- .Values.global.namespaceOverride }}
{{- else }}
{{- .Release.Namespace }}
{{- end }}
{{- end }}

{{/*
Discovery selector - finds machine-a-tron bmc-mock Services
*/}}
{{- define "mat-k8s-controller.discoverySelector" -}}
{{- default "nvidia-infra-controller/mat-service=true" .Values.config.discoverySelector }}
{{- end }}

{{/*
Target selector - matches machine-a-tron pods
*/}}
{{- define "mat-k8s-controller.targetSelector" -}}
{{- default "app.kubernetes.io/name=nico-machine-a-tron" .Values.config.targetSelector }}
{{- end }}

{{/*
Base name of the gateway resources (ConfigMap, Service, Certificate, TLS Secret)
*/}}
{{- define "mat-k8s-controller.gateway.fullname" -}}
{{- printf "%s-gateway" (include "mat-k8s-controller.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}
