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
Job name for a suite, kept within the 63-character bound with the suite suffix
intact. When the base fits, it is used whole. When it must be truncated, an
8-character hash of the full base is inserted before the suffix, so two
releases that share a long prefix but differ later get distinct Job names
(before-hook-creation would otherwise delete the other release's Job).
Usage: {{ include "scale-test.jobName" (dict "root" $root "suite" $name) }}
*/}}
{{- define "scale-test.jobName" -}}
{{- $base := include "scale-test.fullname" .root -}}
{{- $reserve := int (sub 62 (len .suite)) -}}
{{- if le (len $base) $reserve -}}
{{- printf "%s-%s" $base .suite -}}
{{- else -}}
{{- $hash := $base | sha256sum | trunc 8 -}}
{{- $keep := int (sub $reserve 9) -}}
{{- printf "%s-%s-%s" ($base | trunc $keep | trimSuffix "-") $hash .suite -}}
{{- end -}}
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
Name of the shared run-data claim, derived from the release unless
persistence.claimName overrides it. The journal Jobs and the PVC resolve the
same name through this helper.
*/}}
{{- define "scale-test.runDataClaim" -}}
{{- default (printf "%s-run-data" (include "scale-test.fullname" .)) .Values.persistence.claimName | trunc 63 | trimSuffix "-" -}}
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
count validates a whole count of 0 or more, accepting Helm's --set int64,
YAML's float64 and a string of digits, and rejecting an omitted (empty or
null), fractional, negative or non-numeric one: an explicit 0 is kept and
nothing is coerced. Returns the count as canonical digits.
Usage: {{ include "scale-test.count" (dict "key" "inventory.expected.hosts" "value" $v) }}
*/}}
{{- define "scale-test.count" -}}
{{- $s := toString .value -}}
{{- if kindIs "float64" .value -}}
{{- $s = ternary (printf "%.0f" .value) "-" (eq .value (floor .value)) -}}
{{- end -}}
{{- if not (regexMatch "^(0|[1-9][0-9]{0,18})$" $s) -}}
{{- fail (printf "%s must be a whole count of 0 or more" .key) -}}
{{- end -}}
{{- $s -}}
{{- end -}}

{{/*
The store ingestion delay, whole seconds from 0 to 44: the store client needs
it under half its 90 s summary timeout.
*/}}
{{- define "scale-test.ingestionDelay" -}}
{{- $delay := toString .Values.store.ingestionDelaySeconds -}}
{{- if not (regexMatch "^([0-9]|[1-3][0-9]|4[0-4])$" $delay) -}}
{{- fail (printf "store.ingestionDelaySeconds must be a whole number of seconds from 0 to 44, got %q" $delay) -}}
{{- end -}}
{{- $delay -}}
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
