{{/*
Allow the release namespace to be overridden for multi-namespace deployments.
*/}}
{{- define "nico-dhcp.namespace" -}}
{{- default .Release.Namespace .Values.namespaceOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Expand the name of the chart.
*/}}
{{- define "nico-dhcp.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "nico-dhcp.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "nico-dhcp.labels" -}}
helm.sh/chart: {{ include "nico-dhcp.chart" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: site-controller
app.kubernetes.io/name: {{ include "nico-dhcp.name" . }}
app.kubernetes.io/component: dhcp
{{- end }}

{{/*
Selector labels
*/}}
{{- define "nico-dhcp.selectorLabels" -}}
app.kubernetes.io/name: {{ include "nico-dhcp.name" . }}
app.kubernetes.io/component: dhcp
{{- end }}

{{/*
Global image reference
*/}}
{{- define "nico-dhcp.image" -}}
{{ .Values.global.image.repository }}:{{ .Values.global.image.tag }}
{{- end }}

{{/*
Certificate spec
*/}}
{{- define "nico-dhcp.certificateSpec" -}}
duration: {{ .global.certificate.duration }}
renewBefore: {{ .global.certificate.renewBefore }}
commonName: {{ printf "%s.%s.svc.cluster.local" (.cert.serviceName | default .svcName) (.cert.identityNamespace | default .namespace) }}
dnsNames:
{{- if .cert.dnsNames }}
{{- range .cert.dnsNames }}
  - {{ . }}
{{- end }}
{{- else }}
  - {{ printf "%s.%s.svc.cluster.local" (.cert.serviceName | default .svcName) (.cert.identityNamespace | default .namespace) }}
{{- if ne (toString .cert.includeShortDnsName) "false" }}
  - {{ printf "%s.%s" (.cert.serviceName | default .svcName) (.cert.identityNamespace | default .namespace) }}
{{- end }}
{{- range .cert.extraDnsNames | default list }}
  - {{ . }}
{{- end }}
{{- end }}
uris:
{{- if .cert.uris }}
{{- range .cert.uris }}
  - {{ . }}
{{- end }}
{{- else }}
  - {{ printf "spiffe://%s/%s/sa/%s" .global.spiffe.trustDomain (.cert.identityNamespace | default .namespace) (.cert.spiffeServiceName | default .cert.serviceName | default .svcName) }}
{{- range .cert.extraUris | default list }}
  - {{ . }}
{{- end }}
{{- end }}
privateKey:
  algorithm: {{ .global.certificate.privateKey.algorithm }}
  size: {{ .global.certificate.privateKey.size }}
issuerRef:
  kind: {{ .global.certificate.issuerRef.kind }}
  name: {{ .global.certificate.issuerRef.name }}
  group: {{ .global.certificate.issuerRef.group }}
secretName: {{ .name }}
{{- end }}

{{/*
Resolve the API service name with a safe fallback. Both the structured
keaConfigJson helper and the raw escape-hatch substitution route through
this so an explicit-null / empty `.Values.apiServiceName` falls back to
`nico-api` instead of producing a malformed hostname like
`https://.<ns>.svc.cluster.local`. Override via `.Values.apiServiceName`
under the dual-deployment scheme (PR #2062 — e.g. set to `carbide-api`).
*/}}
{{- define "nico-dhcp.apiServiceName" -}}
{{- .Values.apiServiceName | default "nico-api" -}}
{{- end }}

{{/*
Assemble kea_config.json from the structured config.kea block in values.
Fields are renamed from camelCase YAML to the hyphenated keys kea-dhcp4
expects. nicoApiUrl defaults to the in-cluster API service when left
empty — built from `nico-dhcp.apiServiceName` (which honours
`.Values.apiServiceName`) so the dual-deployment scheme flows through
here without operators needing to override hookParameters.nicoApiUrl.
nameservers / ntpServer / provisioningServer are passed through
verbatim — operators provide the real IPs via the umbrella
helm-prereqs/values/nico-core.yaml.

Serialize structured values with `toJson` and strings with `quote`.
Required hook strings reject null values at render time while preserving
explicit empty values.

Note on toJson use: any field that is a free-form structure (subnets,
pools, lease database backend) is serialised with toJson so future
additions to those shapes don't require further template edits. Scalar
fields with hyphenated JSON keys are emitted explicitly so the template
controls the YAML→JSON name mapping.
*/}}
{{- define "nico-dhcp.keaConfigJson" -}}
{{- $ns := include "nico-dhcp.namespace" . -}}
{{- $apiSvc := include "nico-dhcp.apiServiceName" . -}}
{{- $k := default (dict) .Values.config.kea -}}
{{/* Nil-safe access: `hookParameters: null` from an overlay shouldn't panic. */}}
{{- $hp := default (dict) $k.hookParameters -}}
{{- range $name := list "nameservers" "ntpServer" "provisioningServer" -}}
{{- if kindIs "invalid" (index $hp $name) -}}
{{- fail (printf "nico-dhcp: config.kea.hookParameters.%s must be set; null is not supported" $name) -}}
{{- end -}}
{{- end -}}
{{- $apiUrl := default "" $hp.nicoApiUrl -}}
{{- if not $apiUrl -}}
{{- $apiUrl = printf "https://%s.%s.svc.cluster.local:1079" $apiSvc $ns -}}
{{- end -}}
{{- $metricsEndpoint := default "[::]:1089" $hp.nicoMetricsEndpoint -}}
{{- $libPath := default "/usr/lib/kea/hooks/libdhcp.so" $k.hookLibraryPath -}}
{{- $extraHooks := default (list) $k.additionalHooksLibraries -}}
{{- $ic := default (dict) $k.interfacesConfig -}}
{{- $interfaces := default (list "eth0") $ic.interfaces -}}
{{- $socketType := default "udp" $ic.dhcpSocketType -}}
{{- $ld := default (dict) $k.leaseDatabase -}}
{{- $subnets := default (list) $k.subnet4 -}}
{{- $subnetIds := dict -}}
{{- range $s := $subnets -}}
{{- if and (gt (len $subnets) 1) (not (hasKey $s "id")) -}}
{{- fail "nico-dhcp: multiple IPv4 subnets require explicit id values matching existing leases" -}}
{{- end -}}
{{- $id := "1" -}}
{{- if hasKey $s "id" -}}
{{- $id = toString (int64 $s.id) -}}
{{- end -}}
{{- if hasKey $subnetIds $id -}}
{{- fail (printf "nico-dhcp: duplicate IPv4 subnet id %s" $id) -}}
{{- end -}}
{{- $_ := set $subnetIds $id true -}}
{{- end -}}
{{- $loggers := default (list) $k.loggers -}}
{{- $declineProbationPeriod := 900 -}}
{{- if hasKey $k "declineProbationPeriod" -}}
{{- $declineProbationPeriod = $k.declineProbationPeriod -}}
{{- end -}}
{{/*
  DHCP server identifier (option 54) — pin to the stable DHCP VIP so client
  unicast renewals survive pod replacement (issue #3663). Without this, kea
  advertises the ephemeral pod IP: a rescheduled pod leaves every leased BMC
  unicast-renewing against a dead address until the rebind timer forces a
  broadcast. Resolution order:
    1. config.kea.serverIdentifier — explicit override, validated as IPv4
       (render fails on a malformed explicit value).
    2. externalService.annotations["metallb.universe.tf/loadBalancerIPs"] —
       the VIP the chart already advertises; first entry when comma-separated.
       Used only when it parses as a single IPv4 — a missing/odd annotation
       silently falls through so upgrades from values that predate this
       field can never fail at render time.
    3. Neither — the option is omitted entirely and kea falls back to its
       default (pod IP), which is byte-identical to the pre-#3663 rendering.
*/}}
{{- $ipv4Pattern := "^((25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\\.){3}(25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])$" -}}
{{/* printf "%v" coerces non-string overrides (bare numbers, maps) to a string
     so they reach the friendly IPv4-validation fail below instead of erroring
     inside sprig's trim with an opaque type message. */}}
{{- $serverIdentifier := trim (printf "%v" (default "" $k.serverIdentifier)) -}}
{{- if and $serverIdentifier (not (regexMatch $ipv4Pattern $serverIdentifier)) -}}
{{- fail (printf "nico-dhcp: config.kea.serverIdentifier must be a single IPv4 address (the stable DHCP VIP); got %q" $serverIdentifier) -}}
{{- end -}}
{{- if not $serverIdentifier -}}
{{- $es := default (dict) .Values.externalService -}}
{{- if $es.enabled -}}
{{- $esAnnotations := default (dict) $es.annotations -}}
{{- $vipAnnotation := index $esAnnotations "metallb.universe.tf/loadBalancerIPs" -}}
{{- $firstVip := trim (first (splitList "," (printf "%v" (default "" $vipAnnotation)))) -}}
{{- if regexMatch $ipv4Pattern $firstVip -}}
{{- $serverIdentifier = $firstVip -}}
{{- end -}}
{{- end -}}
{{- end -}}
{
  "Dhcp4": {
    "interfaces-config": {
      "interfaces": {{ $interfaces | toJson }},
      "dhcp-socket-type": {{ $socketType | quote }}
    },
    "lease-database": {
      "type": {{ default "memfile" $ld.type | quote }},
      "lfc-interval": {{ default 3600 $ld.lfcInterval | toJson }}
    },
    "cache-threshold": 0.0,
    "multi-threading": { "enable-multi-threading": false },
    "match-client-id": {{ default false $k.matchClientId | toJson }},
    "authoritative": {{ default true $k.authoritative | toJson }},
    "renew-timer": {{ default 900 $k.renewTimer | toJson }},
    "rebind-timer": {{ default 1800 $k.rebindTimer | toJson }},
    "valid-lifetime": {{ default 3600 $k.validLifetime | toJson }},
    "decline-probation-period": {{ $declineProbationPeriod | toJson }},
{{- if $serverIdentifier }}
    "option-data": [
      {
        "name": "dhcp-server-identifier",
        "data": {{ $serverIdentifier | quote }}
      }
    ],
{{- end }}
    {{/*
      Hook parameters — write both nico-* and carbide-* keys with identical
      values so the kea hook library (crates/dhcp/src/kea/loader.cc still
      reads carbide-*) and any future nico-named build both find the value
      they expect. Drop the carbide-* mirror once every consuming binary
      has been rebuilt to read nico-*.

      Required hook strings must be present. Preserve explicit empty
      strings for compatibility; operators must supply site addresses.
    */ -}}
    "hooks-libraries": [
      {
        "library": {{ $libPath | quote }},
        "parameters": {
          "nico-api-url": {{ $apiUrl | quote }},
          "carbide-api-url": {{ $apiUrl | quote }},
          "nico-metrics-endpoint": {{ $metricsEndpoint | quote }},
          "carbide-metrics-endpoint": {{ $metricsEndpoint | quote }},
          "nico-nameservers": {{ $hp.nameservers | quote }},
          "carbide-nameservers": {{ $hp.nameservers | quote }},
          "nico-ntpserver": {{ $hp.ntpServer | quote }},
          "carbide-ntpserver": {{ $hp.ntpServer | quote }},
          "nico-provisioning-server-ipv4": {{ $hp.provisioningServer | quote }},
          "carbide-provisioning-server-ipv4": {{ $hp.provisioningServer | quote }}
        }
      }
{{- range $i, $extra := $extraHooks }},
      {{ $extra | toJson }}
{{- end }}
    ],
    "subnet4": [
{{- range $i, $s := $subnets }}
{{- if $i }},{{ end }}
      {
        "id": {{ if hasKey $s "id" }}{{ $s.id | toJson }}{{ else }}1{{ end }},
        "subnet": {{ $s.subnet | quote }},
        "pools": [
{{- range $j, $p := default (list) $s.pools }}
{{- if $j }},{{ end }}
{{- if kindIs "string" $p }}
          { "pool": {{ $p | quote }} }
{{- else if kindIs "map" $p }}
          {{ $p | toJson }}
{{- else }}
{{- fail (printf "nico-dhcp: config.kea.subnet4[%d].pools[%d] must be a pool-range string (e.g. \"0.0.0.1-255.255.255.254\") or a kea pool object; got %s" $i $j (kindOf $p)) }}
{{- end }}
{{- end }}
        ]
      }
{{- end }}
    ],
    "loggers": [
{{- range $i, $l := $loggers }}
{{- if $i }},{{ end }}
      {
        "name": {{ $l.name | quote }},
        "severity": {{ default "INFO" $l.severity | quote }},
        "output_options": {{ default (list (dict "output" "stdout")) $l.outputOptions | toJson }}
      }
{{- end }}
    ]
  }
}
{{- end -}}

{{/*
Service monitor spec
*/}}
{{- define "nico-dhcp.serviceMonitorSpec" -}}
endpoints:
  - honorLabels: false
    interval: {{ .monitor.interval }}
    port: {{ .port }}
    scheme: http
    scrapeTimeout: {{ .monitor.scrapeTimeout }}
namespaceSelector:
  matchNames:
    - {{ .namespace }}
selector:
  matchLabels:
    app.kubernetes.io/metrics: {{ .name }}
{{- end }}
