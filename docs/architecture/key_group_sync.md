# Key Group Synchronization

Key groups are lists of SSH Keys and groups of them in order to provide access to the SSH console for users.

The key group update and synchronization mechanism in NICo REST API works as follows:

1. Key groups are stored per tenant. They can be uniquely identified by the tenant org identifier and a unique key group name that the tenant chooses. Therefore no additional UUID based ID is required.
2. The source of truth for the content of key groups is stored in the NICo REST API.
3. Key groups are versioned. Whenever a key group is modified by a user (key added or removed), a version field for the group is changed to a unique new value. Usage of the same version format that NICo entities already use (e.g. `V1-T1666644937952267` makes sense, but is not strictly necessary).
4. The NICo REST API backend synchronizes the contents of the key groups to all NICo sites that the tenant selected (or potentially even just all sites that the tenant is enabled for).
5. The NICo REST API stores for each Site/Tenant/KeyGroupName combination, which version is already stored on a site. By having this information available, the NICo REST API can efficiently look up whether key groups have been synced to required destinations by comparing the most recent key group version (owned by the cloud) with the synchronized key group version.
6. After a NICo Tenant changed the contents of a key group in the NICo REST API, the Cloud needs to update all target sites with the latest state. There are multiple approaches for this:
    1. The NICo Tenant explicitly triggers the sync via UI. Triggering the sync will let the Cloud Backend compare the latest deployed state of a keygroup on one site with the version in the Cloud database, and update it if required. This approach is not ideal because it requires the NICo tenant to monitor the deployment status on all sites.
    2. The NICo REST API periodically syncs the state of all Key Groups to all sites. It can iterate over all the groups it has knowledge about and all sites, and update the group contents for sites where there is a mismatch. This requires some extra work for groups where no content changes occurred, but is otherwise fairly straightforward to implement and free from race conditions.
    3. NICo REST API only schedules updates for key groups if the NICo Tenant updated the state of a group. This is a bit more efficient, but harder to cover all edge-cases. E.g. the Cloud needs to account for
        - sites being temporarily offline during the sync
        - sites being restored from backups and having outdated keygroup versions or missing keygroups
        - users triggering multiple keygroup updates in rapid succession
7. NICo provides the ability to fully overwrite the content of a keygroup that is identified by a `(TenantOrg, GroupName)` tuple and indexed by a `Version`. It will echo the version of a keygroup as is back to the Cloud, and not change it by itself or interpret it in any way.
8. The NICo REST API **could** expose the version number of key groups to users - however it does not have to. By exposing the version number, it can provide update APIs with `ifVersionNotMatch` semantics - which means adding the capability for UIs to fail changes to groups if a concurrent edit occurred. This prevents NICo Tenant Admins from accidentally overwriting changes that another Tenant Admin for the same org performed.

```mermaid
sequenceDiagram
    participant U as NICo Tenant TenantY
    participant C as NICo REST API
    participant S as NICo Site SiteA
    U->>C: CreateKeyGroup(name="MyKeys")
    C->>U: KeyGroupCreationResult(Group {name="MyKeys", keys=[], version=V1-T1666644937952267})

    opt Trigger Sync of KeyGroup
        C->>S: CreateKeyGroup(tenant="TenantY", name="mykeys", version=V1-T1666644937952267)
    end

    Note over U, C: Adding the first keys

    U->>C: UpdateKeyGroup(name="MyKeys", content="[Key1, Key2]")
    C-->C: Schedule Sync of keys to all sites or affected sites
    C->>U: UpdateKeyGroupResult(Group {name="MyKeys", keys=[Key1, Key2], version=V2-T1666644937952400})

    U->>C: GetKeyGroups()
    C->>U: KeyGroups([name="MyKeys", keys=[Key1, Key2], sync=Pending])

    Note over C, S: Background Sync. Triggered periodically and/or after updates
    C->>S: FindKeyGroups(tenant="TenantY")
    S->>C: KeyGroups([])
    C->>S: UpdateKeyGroup(tenant="TenantY", name="mykeys", version=V2-T1666644937952400, keys=[Key1, Key2])
    S->>C: UpdateKeyGroupResult
    C-->C: RecordKeyGroupVersion(site="SiteA", tenant="TenantY", name="MyKeys", version="V2-T1666644937952400")

    Note over U,C: After the sync had been performed, the Cloud knows that the key group version on the site matches the latest revision in the Cloud

    U->>C: GetKeyGroups()
    C->>U: KeyGroups([name="MyKeys", keys=[Key1, Key2], sync=Done])

    Note over U, C: Adding more keys

    U->>C: UpdateKeyGroup(name="MyKeys", content="[Key1, Key2, Key3]", ifVersionMatch="V2-T1666644937952400")
    C-->C: Schedule Sync of keys to all sites or affected sites
    C->>U: UpdateKeyGroupResult(Group {name="MyKeys", keys=[Key1, Key2, Key3], version=V3-T1666644937952600})

    Note over C,S: Background Sync
    C->>S: FindKeyGroups(tenant="TenantY")
    S->>C: KeyGroups([{name="MyKeys", content="[Key1, Key2]", version="V2-T1666644937952400"}])
    C-->C: Determines an update is required
    C->>S: UpdateKeyGroup(tenant="TenantY", name="mykeys", version=V3-T1666644937952600, keys=[Key1, Key2, Key3])
    S->>C: UpdateKeyGroupResult
    C-->C: RecordKeyGroupVersion(site="SiteA", tenant="TenantY", name="MyKeys", version="V3-T1666644937952600")

    Note over U,C: Next query for key group status will know that the site has applied the latest version

```
