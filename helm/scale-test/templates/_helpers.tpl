{{/*
Chart name + version label.
*/}}
{{- define "scale-test.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end -}}

{{/*
Default object name, derived from the release so two releases in one namespace
own distinct objects. fullnameOverride replaces it.
*/}}
{{- define "scale-test.fullname" -}}
{{- $name := default (printf "%s-scale-test" .Release.Name) .Values.fullnameOverride -}}
{{- $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Job name for a suite. The base reserves room so the suite suffix is never
truncated away: each suite keeps a distinct Job and its retained logs.
Usage: {{ include "scale-test.jobName" (dict "root" $root "suite" $name) }}
*/}}
{{- define "scale-test.jobName" -}}
{{- printf "%s-%s" (include "scale-test.fullname" .root | trunc 55 | trimSuffix "-") .suite | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels for every scale-test object.
*/}}
{{- define "scale-test.labels" -}}
helm.sh/chart: {{ include "scale-test.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: scale-test
app.kubernetes.io/name: scale-test
{{- end -}}

{{/*
Acceptance image reference. A digest pins the image, else the tag is used; a
missing repository or missing digest/tag is rejected rather than rendered as
an invalid "repo:" or ":" reference.
*/}}
{{- define "scale-test.image" -}}
{{- $img := .Values.image -}}
{{- if not $img.repository -}}
{{- fail "image.repository is required" -}}
{{- end -}}
{{- if $img.digest -}}
{{- printf "%s@%s" $img.repository $img.digest -}}
{{- else if $img.tag -}}
{{- printf "%s:%s" $img.repository $img.tag -}}
{{- else -}}
{{- fail "image.digest or image.tag is required" -}}
{{- end -}}
{{- end -}}

{{/*
positiveSeconds validates a positive integer number of seconds, accepting both
Helm's --set int64 and YAML's float64 and rejecting null, bool, string, zero,
negative, fractional and out-of-int32-range values, so a bound can never be
dropped or overflow. Returns the value as an integer.
Usage: {{ include "scale-test.positiveSeconds" (dict "key" "testBudget.x" "value" $v) }}
*/}}
{{- define "scale-test.positiveSeconds" -}}
{{- $v := .value -}}
{{- $bad := printf "%s must be a positive integer number of seconds (1 to 2147483647)" .key -}}
{{- if or (kindIs "invalid" $v) (kindIs "bool" $v) (kindIs "string" $v) -}}
{{- fail $bad -}}
{{- end -}}
{{- $f := float64 $v -}}
{{- if or (le $f 0.0) (gt $f 2147483647.0) (ne $f (floor $f)) -}}
{{- fail $bad -}}
{{- end -}}
{{- $v | int64 -}}
{{- end -}}

{{/*
Job deadline, in seconds.
*/}}
{{- define "scale-test.jobDeadline" -}}
{{- include "scale-test.positiveSeconds" (dict "key" "testBudget.activeDeadlineSeconds" "value" .Values.testBudget.activeDeadlineSeconds) -}}
{{- end -}}

{{/*
Go test timeout, a positive integer number of seconds emitted as a Go duration
(for example 480s) so it cannot truncate to zero or overflow.
*/}}
{{- define "scale-test.testTimeout" -}}
{{- printf "%ss" (include "scale-test.positiveSeconds" (dict "key" "testBudget.testTimeoutSeconds" "value" .Values.testBudget.testTimeoutSeconds)) -}}
{{- end -}}
