# Deprecations

NICo REST API maintains backward compatibility with the previous versions. Any breaking changes are announced using deprecation API objects.

## Deprecation API Object

A deprecation API object is a JSON object that contains the details of a particular deprecation in the API. It is used to announce deprecations to clients of the API.

| Field | Description |
|-------|-------------|
| attribute | Name of the attribute that is deprecated. Omitted if queryParam or endpoint is being deprecated. |
| queryParam | Name of the query parameter that is deprecated. Omitted if attribute or endpoint is being deprecated. |
| endpoint | API endpoint that is deprecated. Omitted if attribute or queryParam is being deprecated. |
| replacedBy | Name of the attribute, query parameter, or endpoint that replaces the deprecated item. Omitted if no replacement is available. |
| takeActionBy | ISO datetime string for when the deprecated field will no longer be accepted or available in the API. |
| notice | Message describing the deprecation. If the takeActionBy date hasn't passed, yet the notice will end with `Please take action prior to the specified date`. If the takeActionBy date has passed, the notice will end with `Please take action immediately`. |

## Attribute Deprecation

When an attribute of an API object is being deprecated:

- Each API object that contains the deprecated attribute will include a `deprecations` attribute containing an array of deprecation API objects
- When an attribute is being deprecated, only the `attribute` field will be included in the deprecation API object

```json
{
    "id": "123e4567-e89b-12d3-a456-426614174000",
    "org": "example-org",
    "displayName": "Example Org", // Deprecated in favor of orgDisplayName
    "orgDisplayName": "Example Org",
    "status": "Pending",
    "deprecations": [
        {
            "attribute": "displayName",
            "replacedBy": "orgDisplayName",
            "takeActionBy": "2026-06-08T00:00:00Z",
            "notice": "`displayName` has been deprecated in favor of `orgDisplayName`. Please take action prior to the specified date"
        }
    ]
}
```

## Endpoint Deprecation

When an API endpoint is being deprecated:

- Each API endpoint will include a `deprecations` attribute containing an array of deprecation API objects
- When an endpoint is being deprecated, only the `endpoint` field will be included in the deprecation API object

```json
{
    "id": "123e4567-e89b-12d3-a456-426614174000",
    "name": "example-endpoint-resource",
    "description": "Example endpoint resource",
    "status": "Pending",
    "deprecations": [
        {
            "endpoint": "POST /org/:orgName/nico/resource/example",
            "takeActionBy": "2026-06-08T00:00:00Z",
            "notice": "`POST /org/:orgName/nico/resource/example` has been deprecated. Please take action prior to the specified date"
        }
    ]
}
```

If a deprecated attribute/endpoint or query param has no replacement, the `replacedBy` field will be omitted from the response.

## Query Param Deprecation

When a query param is being deprecated:

- Each API endpoint that accepts the query param will include a `deprecations` attribute containing an array of deprecation API objects
- When a query param is being deprecated, only the `queryParam` field will be included in the deprecation API object

```json
{
    "id": "123e4567-e89b-12d3-a456-426614174000",
    "name": "example-endpoint-resource",
    "description": "Example endpoint resource",
    "status": "Pending",
    "deprecations": [
        {
            "queryParam": "includeUsageStats",
            "replacedBy": "includeUsage",
            "takeActionBy": "2026-06-08T00:00:00Z",
            "notice": "`includeUsageStats` has been deprecated in favor of `includeUsage`. Please take action prior to the specified date"
        }
    ]
}
```

## Guidance for Users

If the deprecated item is an attribute that belongs to a request object used for create/update API endpoints:
  - If a new attribute is introduced, either the new or deprecated attribute can be specified in request until expiry date
  - If both new and deprecated attributes are specified in request data at the same time, an HTTP 400 response is returned informing preference for the new attribute
  - Once the take action by date has passed and the deprecated attribute is included in create/update request, an HTTP 400 response is returned informing that the attribute has been deprecated

Deprecation notices continue to be returned for one more release cycle after the take action by date.

## Active Deprecations

Endpoints that have deprecations will be grouped here. Following deprecations are in effect:

### Operating System

- `isCloudInit` on create and update requests is deprecated and ignored. The value returned in API responses is derived from whether `userData` is non-empty.

### Tenant Account

- `accountNumber`, `subscriptionId`, and `subscriptionTier` attributes were deprecated and will be removed on **September 10th, 2026 0:00 UTC**. Please update your usage accordingly.

### Allocation

- `ResourceTypeID` attribute on Allocation Constraint was deprecated in favor of `resourceTypeId` and will be removed on **September 10th, 2026 0:00 UTC**. Please use `resourceTypeId` instead.

### Instance Type

- `id` attribute on Machine Instance Type association was deprecated in favor of `machineId` and will be removed on **September 10th, 2026 0:00 UTC**. Please use `machineId` instead.

### NVLink Logical Partition

- `nvLinklogicalPartitionId` attribute on NVLink Interface was deprecated in favor of `nvLinkLogicalPartitionId` and will be removed on **September 10th, 2026 0:00 UTC**. Please use `nvLinkLogicalPartitionId` instead.

### Network Security Group

- `object_id` attribute on Network Security Group propagation details was deprecated in favor of `objectId` and will be removed on **September 10th, 2026 0:00 UTC**. Please use `objectId` instead.

### Instance

- `infrastructureProviderId` query parameter on the list Instances endpoint was deprecated and will be removed on **October 10th, 2026 0:00 UTC**. Instances will no longer be filtered by Infrastructure Provider; results are scoped to the org's Tenant. Use the `siteId` parameter to scope results to a specific Infrastructure Provider's Sites.

## Recent Deprecations

Following deprecations were removed from the API in the recent past:

### Site

- `rackLevelAdministration` capability attribute was deprecated in favor of `flow` and was removed on **May 13th, 2026 0:00 UTC**. Please use `flow` instead.
- `isRackLevelAdministrationEnabled` query parameter was deprecated in favor of `isFlowEnabled` and was removed on **May 13th, 2026 0:00 UTC**. Please use `isFlowEnabled` instead.
