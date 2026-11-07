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
The metrics Service, which the ServiceMonitor's scrape job is named after.
*/}}
{{- define "nico-site-health-probe.metricsService" -}}
{{- printf "%s-metrics" (include "nico-site-health-probe.name" .) -}}
{{- end }}

{{/*
Every label the probe's series carry, which a selector must not set. Keep in
step with series_labels() in crates/site-health-probe-api/src/metrics.rs; a
probe config test compares them.
*/}}
{{- define "nico-site-health-probe.seriesLabels" -}}
allocatable eligible family initiator instance_type kind linked metric mode model predicate reader reason resource result stage state type update_complete
{{- end }}

{{/*
The label matchers that select the probe's series in the metrics store, as
config.yaml lines: state.metrics.selector, else the job the ServiceMonitor
scrapes (the metrics Service) and the release namespace. Fails on a label
the probe's series carry or the store never holds, and on a value the probe
refuses.
*/}}
{{- define "nico-site-health-probe.metricsSelector" -}}
{{- $metrics := .Values.state.metrics | default dict -}}
{{- $selector := $metrics.selector | default (dict "job" (include "nico-site-health-probe.metricsService" .) "namespace" (include "nico-site-health-probe.namespace" .)) -}}
{{- $intrinsic := include "nico-site-health-probe.seriesLabels" . | trim | splitList " " -}}
{{- range $label, $value := $selector }}
{{- if not (regexMatch "^[a-zA-Z_][a-zA-Z0-9_]*$" $label) }}
{{- fail (printf "state.metrics.selector label %q is not a label name" $label) }}
{{- end }}
{{- if hasPrefix "__" $label }}
{{- fail (printf "state.metrics.selector must not set %s: labels starting with __ are reserved" $label) }}
{{- end }}
{{- if has $label $intrinsic }}
{{- fail (printf "state.metrics.selector must not set %s: the probe's series carry it" $label) }}
{{- end }}
{{- if not (kindIs "string" $value) }}
{{- fail (printf "state.metrics.selector value of %s must be a string" $label) }}
{{- end }}
{{- if not (trim $value) }}
{{- fail (printf "state.metrics.selector value of %s must not be blank" $label) }}
{{- end }}
{{- if regexMatch "\\p{Cc}" $value }}
{{- fail (printf "state.metrics.selector value of %s must not hold control characters" $label) }}
{{- end }}
{{- if gt (len $value) 256 }}
{{- fail (printf "state.metrics.selector value of %s is longer than 256 bytes" $label) }}
{{- end }}
{{ $label }}: {{ $value | quote }}
{{- end }}
{{- end }}

{{/*
A Prometheus duration setting, or empty. Fails on a number (YAML reads an
unquoted 0 as one) and on a value the ServiceMonitor CRD refuses (Prometheus
durations have whole numbers only). Takes the setting's name and value.
*/}}
{{- define "nico-site-health-probe.promDuration" -}}
{{- $value := .value -}}
{{- if not (or (kindIs "invalid" $value) (kindIs "string" $value)) -}}
{{- fail (printf "%s must be a quoted Prometheus duration, like \"5s\", not %v" .name $value) -}}
{{- end -}}
{{- $value = $value | default "" -}}
{{- if not (regexMatch "^(|0|(([0-9]+)y)?(([0-9]+)w)?(([0-9]+)d)?(([0-9]+)h)?(([0-9]+)m)?(([0-9]+)s)?(([0-9]+)ms)?)$" $value) -}}
{{- fail (printf "%s %q must be a Prometheus duration, like 5s, 500ms or 1m30s" .name $value) -}}
{{- end -}}
{{- $value -}}
{{- end }}

{{/*
The ServiceMonitor's scrape interval: serviceMonitor.interval, else 5s with
state on, else empty for Prometheus's global interval.
*/}}
{{- define "nico-site-health-probe.scrapeInterval" -}}
{{- $interval := include "nico-site-health-probe.promDuration" (dict "name" "serviceMonitor.interval" "value" .Values.serviceMonitor.interval) -}}
{{- $interval | default (ternary "5s" "" (eq (toString .Values.state.enabled) "true")) -}}
{{- end }}

{{/*
The declared cadence of the scrapes that store the probe's series, as YAML
with the setting's name and value: the ServiceMonitor's interval, else
state.metrics.scrapeInterval for an external scraper, which needs an
explicit selector. Empty value without either. Fails on conflicting or
incomplete settings rather than leave /v1/config without them.
*/}}
{{- define "nico-site-health-probe.metricsCadence" -}}
{{- $metrics := .Values.state.metrics | default dict -}}
{{- $external := include "nico-site-health-probe.promDuration" (dict "name" "state.metrics.scrapeInterval" "value" $metrics.scrapeInterval) -}}
{{- $cadence := dict "name" "" "value" "" -}}
{{- if .Values.serviceMonitor.enabled -}}
{{- if $external -}}
{{- fail "state.metrics.scrapeInterval is for a scraper other than the ServiceMonitor: with serviceMonitor.enabled, set serviceMonitor.interval instead" -}}
{{- end -}}
{{- $cadence = dict "name" "serviceMonitor.interval" "value" (include "nico-site-health-probe.scrapeInterval" .) -}}
{{- else if $external -}}
{{- if not $metrics.selector -}}
{{- fail "state.metrics.scrapeInterval needs state.metrics.selector: without the ServiceMonitor the chart cannot name the scrape job" -}}
{{- end -}}
{{- $cadence = dict "name" "state.metrics.scrapeInterval" "value" $external -}}
{{- else if $metrics.selector -}}
{{- fail "state.metrics.selector needs serviceMonitor.enabled or state.metrics.scrapeInterval for the scrape cadence" -}}
{{- end -}}
{{- toYaml $cadence -}}
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
{{- if and .enabled (or .readers.devices.enabled .readers.nicoConfig.enabled) -}}true{{- end -}}
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
{{- fail "nicoApiClientCert.existingSecret is required while a nico-api state reader is on: the probe's SPIFFE identity may call only FindMachineIds, FindMachinesByIds and FindHostStatesByIds" -}}
{{- end -}}
{{- if not (has (toString .nicoApi.hostRead) (list "auto" "full" "compact")) -}}
{{- fail (printf "state.nicoApi.hostRead must be auto, full or compact, not %q" (toString .nicoApi.hostRead)) -}}
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
{{- $cadence := include "nico-site-health-probe.metricsCadence" $ | fromYaml -}}
{{- if $cadence.value -}}
{{- $scrape := include "nico-site-health-probe.promSeconds" $cadence.value | float64 -}}
{{- if not (gt $scrape 0.0) -}}
{{- fail (printf "%s (%s) must be positive with state on" $cadence.name $cadence.value) -}}
{{- end -}}
{{- range $name, $reader := .readers -}}
{{- if $reader.enabled -}}
{{- $interval := include "nico-site-health-probe.seconds" $reader.interval | float64 -}}
{{- if gt (mulf 2 $scrape) $interval -}}
{{- fail (printf "%s (%s) must be at most half of state.readers.%s.interval (%s)" $cadence.name $cadence.value $name $reader.interval) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end }}
