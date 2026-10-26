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
Whether to provision a component's database on nico-pg-cluster: true unless its
useHaPostgres value is false. `auto` provisions it too, because setup.sh only
decides at install time which database the workload uses. An existing Site that
stays on the standalone StatefulSet gets an empty database it can migrate into.
*/}}
{{- define "prereqs.provisionHaPostgres" -}}
{{- $value := toString .value -}}
{{- if not (has $value (list "true" "false" "auto")) -}}
{{- fail (printf "%s.useHaPostgres must be true, false, or auto (got %s)" .component $value) -}}
{{- end -}}
{{- if ne $value "false" -}}true{{- end -}}
{{- end -}}

{{/*
Render siteCredentials as nico-api's credential file (JSON, which the file
reader accepts). A password left empty keeps the value of the same entry in the
existing Secret, or is generated when that entry is absent. Usernames may be
empty. Fails the render when an explicit password or username is not a quoted
string, and when the existing credential file or one of its entries is
malformed, so a broken Secret is never replaced by fresh passwords.
*/}}
{{- define "prereqs.siteCredentialsFile" -}}
{{- $sc := .Values.siteCredentials -}}
{{- $uefi := $sc.uefi | default dict -}}
{{- $existing := dict -}}
{{- $secretRef := printf "%s/%s" .Values.namespace $sc.secretName -}}
{{- $secret := lookup "v1" "Secret" .Values.namespace $sc.secretName -}}
{{- if and $secret $secret.data (hasKey $secret.data "credentials.yaml") -}}
{{- $existing = index $secret.data "credentials.yaml" | b64dec | fromYaml -}}
{{- if hasKey $existing "Error" -}}
{{- fail (printf "Secret %s has a credentials.yaml that does not parse; fix or remove it before rendering" $secretRef) -}}
{{- end -}}
{{- end -}}
{{- $doc := dict -}}
{{- $entries := list
      (dict "key" "bmcRoot" "entry" "bmc_site_wide_root" "cred" ($sc.bmcRoot | default dict))
      (dict "key" "uefi.dpu" "entry" "dpu_uefi_site_default" "cred" ($uefi.dpu | default dict))
      (dict "key" "uefi.host" "entry" "host_uefi_site_default" "cred" ($uefi.host | default dict)) -}}
{{- range $e := $entries -}}
{{- $password := $e.cred.password -}}
{{- if kindIs "invalid" $password -}}
{{- $password = "" -}}
{{- else if not (kindIs "string" $password) -}}
{{- fail (printf "siteCredentials.%s.password must be a quoted string" $e.key) -}}
{{- end -}}
{{- if and (not $password) (hasKey $existing $e.entry) -}}
{{- $kept := index $existing $e.entry -}}
{{- if and (kindIs "map" $kept) (kindIs "string" $kept.password) $kept.password -}}
{{- $password = $kept.password -}}
{{- else -}}
{{- fail (printf "Secret %s holds a malformed %s entry; fix or remove it before rendering" $secretRef $e.entry) -}}
{{- end -}}
{{- end -}}
{{- if not $password -}}
{{- $password = randAlphaNum 32 -}}
{{- end -}}
{{- $username := $e.cred.username | default "" -}}
{{- if not (kindIs "string" $username) -}}
{{- fail (printf "siteCredentials.%s.username must be a quoted string" $e.key) -}}
{{- end -}}
{{- $_ := set $doc $e.entry (dict "username" $username "password" $password) -}}
{{- end -}}
{{- $doc | toJson -}}
{{- end -}}
