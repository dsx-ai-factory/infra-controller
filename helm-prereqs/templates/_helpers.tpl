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
Render siteCredentials as nico-api's credential file (JSON, which the file
reader accepts). Fails the render unless every password is a non-empty quoted
string; usernames may be empty.
*/}}
{{- define "prereqs.siteCredentialsFile" -}}
{{- $sc := .Values.siteCredentials -}}
{{- $uefi := $sc.uefi | default dict -}}
{{- $doc := dict -}}
{{- $entries := list
      (dict "key" "bmcRoot" "entry" "bmc_site_wide_root" "cred" ($sc.bmcRoot | default dict))
      (dict "key" "uefi.dpu" "entry" "dpu_uefi_site_default" "cred" ($uefi.dpu | default dict))
      (dict "key" "uefi.host" "entry" "host_uefi_site_default" "cred" ($uefi.host | default dict)) -}}
{{- range $e := $entries -}}
{{- $password := required (printf "siteCredentials.%s.password must be set when siteCredentials.enabled is true" $e.key) $e.cred.password -}}
{{- if not (kindIs "string" $password) -}}
{{- fail (printf "siteCredentials.%s.password must be a quoted string" $e.key) -}}
{{- end -}}
{{- $username := $e.cred.username | default "" -}}
{{- if not (kindIs "string" $username) -}}
{{- fail (printf "siteCredentials.%s.username must be a quoted string" $e.key) -}}
{{- end -}}
{{- $_ := set $doc $e.entry (dict "username" $username "password" $password) -}}
{{- end -}}
{{- $doc | toJson -}}
{{- end -}}
