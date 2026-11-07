# API Resource Overview

The diagram below shows the main top-level API resources returned by the REST API and the most important ownership or dependency relationships between them. It is intentionally selective: certain resources are omitted so the core Provider, Tenant, Site, network, compute, and Instance lifecycle remains readable.

```mermaid
erDiagram
    InfrastructureProvider ||--o{ Site : owns
    InfrastructureProvider ||--o{ TenantAccount : invites
    Tenant ||--o{ TenantAccount : accepts

    InfrastructureProvider ||--o{ Allocation : grants
    Tenant ||--o{ Allocation : receives
    Site ||--o{ Allocation : scopes
    Allocation ||--o{ AllocationConstraint : contains
    AllocationConstraint }o--|| InstanceType : references
    AllocationConstraint }o--|| IPBlock : references

    Site ||--o{ IPBlock : contains
    Site ||--o{ Machine : contains
    Site ||--o{ InstanceType : offers
    InstanceType ||--o{ Machine : groups

    Tenant ||--o{ VPC : owns
    Site ||--o{ VPC : hosts
    VPC ||--o{ VpcPrefix : contains
    VPC ||--o{ Subnet : contains
    VPC }o--o{ VPC : peers
    IPBlock ||--o{ VpcPrefix : supplies
    IPBlock ||--o{ Subnet : supplies

    Tenant ||--o{ Instance : owns
    VPC ||--o{ Instance : connects
    InstanceType ||--o{ Instance : shapes
    Machine ||--o| Instance : backs
    OperatingSystem ||--o{ Instance : boots

    Tenant ||--o{ NetworkSecurityGroup : owns
    Site ||--o{ NetworkSecurityGroup : scopes
    NetworkSecurityGroup }o--o{ VPC : attaches
    NetworkSecurityGroup }o--o{ Instance : attaches

    Tenant ||--o{ InfiniBandPartition : owns
    Site ||--o{ InfiniBandPartition : hosts
    InfiniBandPartition }o--o{ Instance : connects

    Tenant ||--o{ NVLinkLogicalPartition : owns
    Site ||--o{ NVLinkLogicalPartition : hosts
    NVLinkLogicalPartition }o--o{ VPC : defaults
    NVLinkLogicalPartition }o--o{ Instance : connects

    Tenant ||--o{ SshKeyGroup : owns
    SshKeyGroup }o--o{ Instance : authorizes

    Site ||--o{ DpuExtensionService : hosts
    DpuExtensionService }o--o{ Instance : deploys
```
