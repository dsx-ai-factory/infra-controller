{{- define "nico-bmc-target.labels" -}}
app.kubernetes.io/name: nico-bmc-target
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "nico-bmc-target.config" -}}
{{- $clusterUID := required "clusterUID is required" .Values.clusterUID -}}
{{- $cluster := lookup "v1" "Namespace" "" "kube-system" -}}
{{- if and $cluster (ne $cluster.metadata.uid $clusterUID) -}}
{{- fail "clusterUID differs from this cluster's kube-system UID" -}}
{{- end -}}
{{- $config := dict "clusterUID" $clusterUID "owner" (printf "%s/%s" .Release.Namespace .Release.Name)
    "nodeName" (required "nodeName is required" .Values.nodeName)
    "architecture" .Values.architecture "instanceIndex" .Values.target.instanceIndex
    "memoryMiB" .Values.target.memoryMiB "cpus" .Values.target.cpus "diskGiB" .Values.target.diskGiB
    "bridgeAddress" .Values.network.bridgeAddress "bmcPort" .Values.service.targetPort
    "bmcAddress" (required "service.clusterIP is required" .Values.service.clusterIP) -}}
{{- if .Values.target.hugepages -}}
{{- $_ := set $config "hugepages" true -}}
{{- end -}}
{{- if .Values.network.bridgeAddressV6 -}}
{{- $_ := set $config "bridgeAddressV6" .Values.network.bridgeAddressV6 -}}
{{- end -}}
{{- $config | toJson -}}
{{- end -}}

{{- define "nico-bmc-target.resources" -}}
{{- $resources := deepCopy (.Values.resources | default dict) -}}
{{- $requests := $resources.requests | default dict -}}
{{- $limits := $resources.limits | default dict -}}
{{- range $section := list $requests $limits -}}
{{- range $key, $_ := $section -}}
{{- if hasPrefix "hugepages-" $key -}}
{{- fail "huge-page resources are derived from target.hugepages and target.memoryMiB" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $memoryMiB := add 1024 .Values.target.memoryMiB -}}
{{- if .Values.target.hugepages -}}
{{- $memoryMiB = 1024 -}}
{{- $guest := printf "%dMi" (int .Values.target.memoryMiB) -}}
{{- $_ := set $requests "hugepages-2Mi" $guest -}}
{{- $_ := set $limits "hugepages-2Mi" $guest -}}
{{- $_ := set $resources "limits" $limits -}}
{{- end -}}
{{- if and (not (hasKey $requests "memory")) (not (hasKey $limits "memory")) -}}
{{- $_ := set $requests "memory" (printf "%dMi" $memoryMiB) -}}
{{- end -}}
{{- $_ := set $resources "requests" $requests -}}
{{- $resources | toYaml -}}
{{- end -}}
