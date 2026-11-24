# Azure Setup

For managing client secrets and redirect URIs registered in the Entra portal.

## NICo Web

The oauth2 in nico-web has defaults for most settings:

|ENV|DESCRIPTION|
|----|----|
|NICO_WEB_ALLOWED_ACCESS_GROUPS|The list of DL groups allowed to access nico-web|
|NICO_WEB_ALLOWED_ACCESS_GROUPS_ID_LIST|The list of UUIDs in Azure that correspond to the DL groups allowed to access nico-web|
|NICO_WEB_OAUTH2_CLIENT_ID|The app ID of nico-web in Azure/Entra|
|NICO_WEB_OAUTH2_TOKEN_ENDPOINT|  The URI for our tenant ID |
|NICO_WEB_OAUTH2_CLIENT_SECRET|A secret used to talk to MS entra/graph.|
|NICO_WEB_PRIVATE_COOKIEJAR_KEY|A secret used for encrypting the cookie values used for sessions.|
|NICO_WEB_HOSTNAME|A hostname specific for each site that's needed for redirects.  The value must match what's set in the Azure/Entra portal for the URL of the nico-api web interface|

## Alternative Auth Flow

Use the OAuth2 client credentials flow to obtain an Admin web UI session for
automation. The session expires after 10 minutes.

Use your secret manager to provide a header file readable only by the account
running `curl`. The file must contain one line in the form
`client_secret: <CLIENT_SECRET>`. Set `client_secret_header_file` to the path to
this file and `admin_url` to your Admin web UI origin over HTTPS, without a
trailing slash or `/admin` path.

In your script, create a private directory for the session cookie. Read the
header from the file and save the cookie returned by the callback without
following the redirect. Reuse the cookie file for requests to the same origin.

```sh
umask 077
cookie_dir=$(mktemp -d) || exit 1
cookie_file="$cookie_dir/session.cookies"
trap 'rm -f -- "$cookie_file"; rmdir -- "$cookie_dir"' EXIT

curl --fail-with-body --silent --show-error \
  --header "@$client_secret_header_file" \
  --cookie-jar "$cookie_file" \
  "$admin_url/admin/auth-callback" || exit 1

curl --fail-with-body --silent --show-error \
  --cookie "$cookie_file" \
  --write-out '\nHTTP %{http_code}\n' \
  "$admin_url/admin/managed-host.json"
```

Verify HTTP 200 from the `managed-host.json` request. If the session expires,
obtain a new cookie before retrying. The script removes the cookie file and its
directory on exit.
