{{/*
Resolve a container image from repository + tag, falling back to image.
Arguments: container (the values entry), path (its chart-qualified values path).
Symlinked into nico-api, nico-pxe, and nico-hardware-health so standalone charts
share the same helper; Helm includes the file contents when packaging each chart.
*/}}
{{- define "nico.containerImage" -}}
{{- if and (not (kindIs "invalid" .container.tag)) (not (kindIs "string" .container.tag)) -}}
{{- fail (printf "%s.tag must be a string; quote numeric-looking tags (for example, \"1.10\")" .path) -}}
{{- end -}}
{{- if and .container.repository .container.tag -}}
{{- printf "%s:%s" .container.repository .container.tag -}}
{{- else -}}
{{- required (printf "%s requires either non-empty repository and tag or a non-empty image" .path) .container.image -}}
{{- end -}}
{{- end -}}
