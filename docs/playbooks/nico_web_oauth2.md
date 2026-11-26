# Azure Setup

For managing client secrets and redirect URIs registered in the Entra portal.

## NICo Web

Use the [Admin web UI OAuth2 configuration](../operations/debug_webui.md#oauth2-entra-configuration)
to configure the site. Register `https://<ADMIN_WEB_HOST>/admin/auth-callback`
as a web redirect URI for the site's Microsoft Entra application.

## Alternative Auth Flow

Use the OAuth2 client credentials flow to obtain an Admin web UI session for
automation. The session expires after 10 minutes.

Use a client secret issued for the same Microsoft Entra application configured
on the site. Have your secret manager provide it in a header file readable only
by the account running `curl`. The file must contain one line in the form
`client_secret: <CLIENT_SECRET>`. Set `client_secret_header_file` to the path to
this file and `admin_url` to your Admin web UI origin over HTTPS, without a
trailing slash or `/admin` path.

In your script, create a private directory for the session cookie. Read the
header from the file and save the cookie returned by the callback without
following the redirect. Reuse the cookie file for requests to the same origin.
The commands ignore curl's default configuration. Add any required TLS options
explicitly.

```sh
umask 077
cookie_dir=$(mktemp -d) || exit 1
cookie_file="$cookie_dir/session.cookies"
response_file="$cookie_dir/managed-host.json"
trap 'rm -f -- "$cookie_file" "$response_file"; rmdir -- "$cookie_dir"' EXIT

curl --disable --fail-with-body --silent --show-error \
  --header "@$client_secret_header_file" \
  --cookie-jar "$cookie_file" \
  "$admin_url/admin/auth-callback" >&2 || exit 1

http_status=$(curl --disable --silent --show-error \
  --cookie "$cookie_file" \
  --output "$response_file" \
  --write-out '%{http_code}' \
  "$admin_url/admin/managed-host.json") || exit 1
printf 'HTTP %s\n' "$http_status" >&2
[ "$http_status" = 200 ] || exit 1
cat "$response_file"
```

The script sends callback diagnostics and the GET status to stderr. It writes
the GET response body to stdout only on HTTP 200. If the session expires, obtain
a new cookie before retrying. The script removes the temporary files and
directory on exit.
