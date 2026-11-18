{{/*
Annotations for an external Service that shares its LoadBalancer IP with a
sibling Service of the same chart.
Arguments: service (the externalService values), group (the built-in sharing
value), annotations (operator annotations, which override the built-in key).
The key is externalService.sharedIpAnnotation; an absent key keeps the MetalLB
default and "" omits the annotation.
Symlinked into nico-dns, nico-pxe, and unbound so standalone charts share the
same helper; Helm includes the file contents when packaging each chart.
*/}}
{{- define "nico.sharedIpAnnotations" -}}
{{- $key := "metallb.universe.tf/allow-shared-ip" -}}
{{- if hasKey .service "sharedIpAnnotation" -}}
{{- $key = .service.sharedIpAnnotation -}}
{{- end -}}
{{- $operator := default dict .annotations -}}
{{- $annotations := dict -}}
{{- /* An operator value under either MetalLB spelling replaces the built-in one. */ -}}
{{- if and $key (not (hasKey $operator "metallb.universe.tf/allow-shared-ip")) (not (hasKey $operator "metallb.io/allow-shared-ip")) -}}
{{- $_ := set $annotations $key .group -}}
{{- end -}}
{{- mergeOverwrite $annotations $operator | toYaml -}}
{{- end -}}
