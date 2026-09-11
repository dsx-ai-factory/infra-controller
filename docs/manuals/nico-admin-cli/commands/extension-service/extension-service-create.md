# `nico-admin-cli extension-service create`

*[Tenant commands](../../tenant.md) › [extension-service](./extension-service.md) › **create***

## NAME

nico-admin-cli-extension-service-create - Create an extension service

## SYNOPSIS

**nico-admin-cli extension-service create** \[**-i**\|**--id**\]
\<**-n**\|**--name**\> \<**-t**\|**--type**\> \[**--description**\]
\[**--tenant-organization-id**\] \<**-d**\|**--data**\>
\[**--registry-url**\] \[**--username**\] \[**--password**\]
\[**--observability**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Create an extension service

## OPTIONS

**-i**, **--id** *\<SERVICE_ID\>*  
The extension service ID to create (optional)

**-n**, **--name** *\<SERVICE_NAME\>*  
Extension service name

**-t**, **--type** *\<SERVICE_TYPE\>*  
Extension service type\

\
*Possible values:*

- kubernetes-pod

**--description** *\<DESCRIPTION\>*  
Extension service description (optional)

**--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization ID

**-d**, **--data** *\<DATA\>*  
Extension service data

**--registry-url** *\<REGISTRY_URL\>*  
Registry URL for the service credential (optional)

**--username** *\<USERNAME\>*  
Username for the service credential (optional)

**--password** *\<PASSWORD\>*  
Password for the service credential (optional)

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
nico-admin-cli extension-service create --name my-service --type kubernetes-pod --data '{"image":"my-registry/my-service:1.0"}'
nico-admin-cli extension-service create --id 12345678-1234-5678-90ab-cdef01234567 --name my-service --type kubernetes-pod --data '{"image":"my-registry/my-service:1.0"}' --description "Front-end telemetry agent"
nico-admin-cli extension-service create --name my-service --type kubernetes-pod --data '{"image":"my-registry/my-service:1.0"}' --tenant-organization-id fds34511233a
nico-admin-cli extension-service create --name my-service --type kubernetes-pod --data '{"image":"my-registry/my-service:1.0"}' --registry-url my-registry.example.com --username admin --password mypassword
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
