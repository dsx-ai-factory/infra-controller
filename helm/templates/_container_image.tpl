{{/*
Resolve an init container image from repository + tag, falling back to image.
Arguments: container (the values entry), path (its chart-qualified values path).
Symlinked into nico-api and nico-pxe so standalone charts share the same helper;
Helm includes the file contents when packaging each chart.
*/}}
{{- define "nico.containerImage" -}}
{{- if and .container.repository .container.tag -}}
{{- printf "%s:%v" .container.repository .container.tag -}}
{{- else -}}
{{- required (printf "%s requires either non-empty repository and tag or a non-empty image" .path) .container.image -}}
{{- end -}}
{{- end -}}
