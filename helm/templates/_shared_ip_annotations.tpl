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
{{- $annotations := dict -}}
{{- with $key -}}
{{- $_ := set $annotations . $.group -}}
{{- end -}}
{{- mergeOverwrite $annotations (default dict .annotations) | toYaml -}}
{{- end -}}
