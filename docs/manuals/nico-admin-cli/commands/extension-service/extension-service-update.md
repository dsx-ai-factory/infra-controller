# `nico-admin-cli extension-service update`

*[Tenant commands](../../tenant.md) › [extension-service](./extension-service.md) › **update***

## NAME

nico-admin-cli-extension-service-update - Update an extension service

## SYNOPSIS

**nico-admin-cli extension-service update** \<**-i**\|**--id**\>
\[**-n**\|**--name**\] \[**--description**\] \<**-d**\|**--data**\>
\[**--registry-url**\] \[**-u**\|**--username**\]
\[**-p**\|**--password**\] \[**--if-version-ctr-match**\]
\[**--observability**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update an extension service

## OPTIONS

**-i**, **--id** *\<SERVICE_ID\>*  
The extension service ID to update

**-n**, **--name** *\<SERVICE_NAME\>*  
New extension service name (optional)

**--description** *\<DESCRIPTION\>*  
New extension service description (optional)

**-d**, **--data** *\<DATA\>*  
New extension service data

**--registry-url** *\<REGISTRY_URL\>*  
New registry URL for the service credential (optional)

**-u**, **--username** *\<USERNAME\>*  
New username for the service credential (optional)

**-p**, **--password** *\<PASSWORD\>*  
New password for the service credential (optional)

**--if-version-ctr-match** *\<IF_VERSION_CTR_MATCH\>*  
Update only if current number of versions matches this number (optional)

**--observability** *\<OBSERVABILITY\>*  
JSON array containing a defined set of extension observability configs
(optional)

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 --data '{"image":"my-registry/my-service:2.0"}'
nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 --name my-renamed-service --description "Updated telemetry agent" --data '{"image":"my-registry/my-service:2.0"}'
nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 --data '{"image":"my-registry/my-service:2.0"}' --registry-url my-registry.example.com --username admin --password mynewpassword
nico-admin-cli extension-service update --id 12345678-1234-5678-90ab-cdef01234567 --data '{"image":"my-registry/my-service:2.0"}' --if-version-ctr-match 3
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
