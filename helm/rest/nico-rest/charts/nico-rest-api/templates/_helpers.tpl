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
{{- if .Values.ingress.enabled -}}
{{- range .Values.ingress.hosts -}}
{{- if not .host -}}
{{- fail "nico-rest-api: every ingress.hosts entry requires a non-empty host; an entry without one renders an empty ingress host and an empty certificate commonName" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{/*
Both ingress.tls and certificate.dnsNames replace a list the chart otherwise
derives from ingress.hosts, and neither replacement has to mention every host.
The two are checked under different rules on purpose.

ingress.tls needs an exact match. Contour keys a secure virtual host by the
literal string in spec.tls[].hosts and attaches a rule only on an exact lookup
of the rule host, so tls.hosts: ["*.example.com"] gives api.example.com no HTTPS
route at all. A wildcard certificate still works; list the concrete host.

certificate.dnsNames follows X.509 SAN matching, where a wildcard does cover one
label, so *.example.com is a valid SAN for api.example.com.
*/}}
{{- if and .Values.ingress.enabled .Values.ingress.tls -}}
{{- $tlsHosts := list -}}
{{- range .Values.ingress.tls -}}
{{- $tlsHosts = concat $tlsHosts (.hosts | default list) -}}
{{- end -}}
{{- range .Values.ingress.hosts -}}
{{- if not (has .host $tlsHosts) -}}
{{- fail (printf "nico-rest-api: ingress.hosts entry %q is not listed exactly in ingress.tls[].hosts, so Contour would attach no HTTPS route for it and serve it over plaintext HTTP; a wildcard entry does not match, list the host itself (a wildcard certificate Secret still works), or clear ingress.tls to derive the block from ingress.hosts" .host) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if and .Values.ingress.enabled .Values.ingress.certificate.enabled .Values.ingress.certificate.dnsNames -}}
{{- $sans := .Values.ingress.certificate.dnsNames -}}
{{- range .Values.ingress.hosts -}}
{{- if not (include "nico-rest-api.sanCoversHost" (dict "host" .host "sans" $sans)) -}}
{{- fail (printf "nico-rest-api: ingress.hosts entry %q is not covered by any ingress.certificate.dnsNames entry, so the issued certificate would have no SAN for it and clients would reject the connection; add it to dnsNames, or clear dnsNames to derive them from ingress.hosts" .host) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
Reports whether a host is covered by a list of X.509 SANs, emitting a non-empty
string when it is. A wildcard SAN covers exactly one label, so *.example.com
covers api.example.com but neither a.b.example.com nor example.com itself.

Deliberately not reusable for ingress.tls[].hosts, which Contour matches
exactly. Conflating the two is what lets a wildcard TLS entry look valid while
leaving the route on plaintext HTTP.
Call as: include "nico-rest-api.sanCoversHost" (dict "host" $h "sans" $list)
*/}}
{{- define "nico-rest-api.sanCoversHost" -}}
{{- $host := .host -}}
{{- range .sans -}}
{{- if eq . $host -}}
covered
{{- else if hasPrefix "*." . -}}
{{- $suffix := trimPrefix "*" . -}}
{{- if hasSuffix $suffix $host -}}
{{- $label := trimSuffix $suffix $host -}}
{{- if and $label (not (contains "." $label)) -}}
covered
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
