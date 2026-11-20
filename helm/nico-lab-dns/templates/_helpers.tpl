{{- define "nico-lab-dns.labels" -}}
app.kubernetes.io/name: nico-lab-dns
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}
