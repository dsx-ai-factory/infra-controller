{{/*
Generate an ed25519 SSH host private key (PKCS8 PEM format).
Reuses the existing secret on helm upgrade (idempotent).
No Job required — avoids network dependency for apk.
*/}}
{{- define "prereqs.sshPrivateKey" -}}
{{- $existing := (lookup "v1" "Secret" .Values.namespace "ssh-host-key") -}}
{{- if $existing -}}
  {{- index $existing.data "ssh_host_ed25519_key" | b64dec -}}
{{- else -}}
  {{- genPrivateKey "ed25519" -}}
{{- end -}}
{{- end -}}

{{/*
Vault KV payload for one vault.siteCredentials entry:
{"UsernamePassword":{"username":"...","password":"..."}}.
Takes a dict with `key` (the values path used in error messages), `cred`
(the username/password map) and `requireUsername`. Both fields are coerced
to strings so an unquoted numeric or boolean YAML scalar still deserializes
into the String fields NICo reads the entry into.
*/}}
{{- define "prereqs.siteCredentialJson" -}}
{{- $password := required (printf "vault.siteCredentials.%s.password must be set when vault.siteCredentials.enabled is true" .key) .cred.password -}}
{{- $username := .cred.username -}}
{{- if .requireUsername -}}
{{- $username = required (printf "vault.siteCredentials.%s.username must be set when vault.siteCredentials.enabled is true" .key) $username -}}
{{- else if kindIs "invalid" $username -}}
{{- $username = "" -}}
{{- end -}}
{{- dict "UsernamePassword" (dict "username" (toString $username) "password" (toString $password)) | toJson -}}
{{- end -}}
