{{/*
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
*/}}

{{- define "nico-site-health-probe.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Uses global.namespaceOverride if set, otherwise Release.Namespace — the same
precedence as the parent chart, so the probe lands in the same namespace as
the rest of machine-a-tron.
*/}}
{{- define "nico-site-health-probe.namespace" -}}
{{- if and .Values.global .Values.global.namespaceOverride -}}
{{- .Values.global.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- .Release.Namespace -}}
{{- end -}}
{{- end }}

{{- define "nico-site-health-probe.image" -}}
{{- printf "%s:%s" .Values.image.repository .Values.image.tag }}
{{- end }}

{{- define "nico-site-health-probe.labels" -}}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-site-health-probe.name" . }}
app.kubernetes.io/component: monitoring
{{- end }}

{{- define "nico-site-health-probe.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-site-health-probe.name" . }}
{{- end }}

{{/*
The ServiceMonitor's scrape interval: serviceMonitor.interval, else 5s with
state on, else empty for Prometheus's global interval. Fails on a value the
ServiceMonitor CRD refuses (Prometheus durations have whole numbers only).
*/}}
{{- define "nico-site-health-probe.scrapeInterval" -}}
{{- $interval := toString (.Values.serviceMonitor.interval | default (ternary "5s" "" (eq (toString .Values.state.enabled) "true"))) -}}
{{- if not (regexMatch "^(0|(([0-9]+)y)?(([0-9]+)w)?(([0-9]+)d)?(([0-9]+)h)?(([0-9]+)m)?(([0-9]+)s)?(([0-9]+)ms)?)$" $interval) -}}
{{- fail (printf "serviceMonitor.interval %q must be a Prometheus duration, like 5s, 500ms or 1m30s" $interval) -}}
{{- end -}}
{{- $interval -}}
{{- end }}

{{/*
A Prometheus duration ("500ms", "5s", "1m30s", "1d") in seconds.
*/}}
{{- define "nico-site-health-probe.promSeconds" -}}
{{- $units := dict "ms" 0.001 "s" 1.0 "m" 60.0 "h" 3600.0 "d" 86400.0 "w" 604800.0 "y" 31536000.0 -}}
{{- $total := 0.0 -}}
{{- range regexFindAll "[0-9]+(ms|y|w|d|h|m|s)" . -1 -}}
{{- $unit := regexReplaceAll "^[0-9]+" . "" -}}
{{- $total = addf $total (mulf (regexFind "^[0-9]+" . | float64) (index $units $unit)) -}}
{{- end -}}
{{- $total -}}
{{- end }}

{{/*
A duration value ("500ms", "5s", "2m", "1h", "1m30s") in seconds.
*/}}
{{- define "nico-site-health-probe.seconds" -}}
{{- $d := toString . -}}
{{- $parts := regexFindAll "[0-9]+(\\.[0-9]+)?(ms|s|m|h)" $d -1 -}}
{{- if or (not $parts) (ne (join "" $parts) $d) -}}
{{- fail (printf "duration %q must be numbers with units (ms, s, m, h), like 90s or 1m30s" $d) -}}
{{- end -}}
{{- $total := 0.0 -}}
{{- range $parts -}}
{{- if hasSuffix "ms" . -}}
{{- $total = addf $total (divf (trimSuffix "ms" . | float64) 1000) -}}
{{- else if hasSuffix "s" . -}}
{{- $total = addf $total (trimSuffix "s" . | float64) -}}
{{- else if hasSuffix "m" . -}}
{{- $total = addf $total (mulf (trimSuffix "m" . | float64) 60) -}}
{{- else -}}
{{- $total = addf $total (mulf (trimSuffix "h" . | float64) 3600) -}}
{{- end -}}
{{- end -}}
{{- $total -}}
{{- end }}

{{/*
"true" while a state reader reads nico-api.
*/}}
{{- define "nico-site-health-probe.nicoReaders" -}}
{{- with .Values.state -}}
{{- if and .enabled .readers.devices.enabled -}}true{{- end -}}
{{- end -}}
{{- end }}

{{/*
"true" while a state reader reads nico-rest-api.
*/}}
{{- define "nico-site-health-probe.restReaders" -}}
{{- with .Values.state -}}
{{- if and .enabled .readers.restInventory.enabled -}}true{{- end -}}
{{- end -}}
{{- end }}

{{/*
Where the state readers find their nico-api client certificate: the admin
certificate when set, else the SPIFFE certificate.
*/}}
{{- define "nico-site-health-probe.stateTlsDir" -}}
{{- if .Values.nicoApiClientCert.existingSecret -}}/var/run/secrets/nico-admin{{- else -}}/var/run/secrets/spiffe.io{{- end -}}
{{- end }}

{{/*
Whether the pod carries the SPIFFE certificate: for the gRPC latency probe
only. The nico-api state readers use the admin certificate, which
validateState requires. REST-only deployments carry none.
*/}}
{{- define "nico-site-health-probe.spiffe" -}}
{{- if .Values.probes.grpcMachines.enabled -}}true{{- end -}}
{{- end }}

{{/*
One REST identity of the state readers, as config.yaml `auth`. Takes a dict
with `auth` (the values) and `dir` (where its existingSecret is mounted).
*/}}
{{- define "nico-site-health-probe.restAuth" -}}
{{- $auth := .auth -}}
token_url: {{ $auth.tokenUrl | quote }}
client_id: {{ $auth.clientId | quote }}
{{- if $auth.tokenFile }}
token_file: {{ $auth.tokenFile | quote }}
{{- else if $auth.tokenUrl }}
client_secret_path: {{ printf "%s/%s" .dir $auth.existingSecret.key | quote }}
{{- else if $auth.existingSecret.name }}
token_file: {{ printf "%s/%s" .dir $auth.existingSecret.key | quote }}
{{- end }}
{{- end }}

{{/*
Fails on a state section the probe would refuse at startup in ways a
deployer should hear about at install time.
*/}}
{{- define "nico-site-health-probe.validateState" -}}
{{- with .Values.state -}}
{{- if .enabled -}}
{{- $_ := required "state.siteId is required when state is enabled" .siteId -}}
{{- if and (include "nico-site-health-probe.nicoReaders" $) (not $.Values.nicoApiClientCert.existingSecret) -}}
{{- fail "nicoApiClientCert.existingSecret is required while a nico-api state reader is on: the probe's SPIFFE identity may call only FindMachineIds and FindMachinesByIds" -}}
{{- end -}}
{{- if and (include "nico-site-health-probe.nicoReaders" $) (ne (toString .nicoApi.hostRead) "full") -}}
{{- fail "state.nicoApi.hostRead must be full: nico-api has no compact host read yet" -}}
{{- end -}}
{{- if include "nico-site-health-probe.restReaders" $ -}}
{{- if not (hasPrefix "https://" (toString .rest.target)) -}}
{{- fail "state.rest.target must be an https:// URL while the REST inventory reader is on" -}}
{{- end -}}
{{- $_ := required "state.rest.provider.org is required while the REST inventory reader is on" .rest.provider.org -}}
{{- with .rest.provider.auth -}}
{{- if or .tokenFile (and (not .tokenUrl) .existingSecret.name) -}}
{{- if or .tokenUrl .clientId -}}
{{- fail "state.rest.provider.auth takes a token (tokenFile, or existingSecret without tokenUrl) or client credentials (tokenUrl, clientId, existingSecret), not both" -}}
{{- end -}}
{{- else if not (and (hasPrefix "https://" (toString .tokenUrl)) .clientId .existingSecret.name) -}}
{{- fail "state.rest.provider.auth needs an https:// tokenUrl, clientId and existingSecret.name, or a token" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if $.Values.serviceMonitor.enabled -}}
{{- $scrapeInterval := include "nico-site-health-probe.scrapeInterval" $ -}}
{{- $scrape := include "nico-site-health-probe.promSeconds" $scrapeInterval | float64 -}}
{{- range $name, $reader := .readers -}}
{{- if $reader.enabled -}}
{{- $interval := include "nico-site-health-probe.seconds" $reader.interval | float64 -}}
{{- if gt (mulf 2 $scrape) $interval -}}
{{- fail (printf "serviceMonitor.interval (%s) must be at most half of state.readers.%s.interval (%s)" $scrapeInterval $name $reader.interval) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end }}
