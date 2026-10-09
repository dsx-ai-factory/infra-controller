# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Per-resource overrides for the Terraform provider code generator.

Keys are the snake_case resource names. Values are dicts that get merged into
the auto-generated resource config.
"""

RESOURCE_OVERRIDES = {
    # Machine: no create (machines are discovered, not provisioned)
    'machine': {
        'no_create': True,
    },

    # Instance: delete can carry a body
    'instance': {
        'delete_body_fields': ['machine_health_issue', 'is_repair_tenant'],
    },

    # Allocation: scoped by site
    'allocation': {
        'scope_fields': ['site_id'],
    },

    # Allocation constraint: nested under allocation
    'allocation_constraint': {
        'scope_fields': ['allocation_id'],
    },

    # VPC: scoped by site
    'vpc': {
        'scope_fields': ['site_id'],
    },

    # VPC prefix: scoped by VPC and site
    'vpc_prefix': {
        'scope_fields': ['vpc_id', 'site_id'],
    },

    # Subnet: scoped by VPC and site
    'subnet': {
        'scope_fields': ['vpc_id', 'site_id'],
    },

    # IP Block: scoped by site
    'ip_block': {
        'scope_fields': ['site_id'],
    },

    # Expected machine: scoped by site
    'expected_machine': {
        'scope_fields': ['site_id'],
    },

    # InfiniBand Partition: scoped by site
    'infiniband_partition': {
        'scope_fields': ['site_id'],
    },

    # NVLink Logical Partition: scoped by site
    'nvlink_logical_partition': {
        'scope_fields': ['site_id'],
    },

    # Instance Type: scoped by site
    'instance_type': {
        'scope_fields': ['site_id'],
    },

    # SSH Key: scoped by SSH Key Group
    'ssh_key': {
        'scope_fields': ['ssh_key_group_id'],
    },
}

# Tags that map to read-only data sources (no POST/PATCH/DELETE on main resource).
READ_ONLY_TAGS = {
    'Service Account',
    'Infrastructure Provider',
    'Tenant',
    'User',
    'Metadata',
    'Audit',
    'Machine Capability',
    'Rack',
    'SKU',
    'Tray',
    'NVLink Domain',
}

# Tags to skip entirely.
SKIP_TAGS = {
    'Deprecations',
    'Getting Started',
}

# Map from spec tag name to snake_case resource name override.
TAG_TO_RESOURCE = {
    'SSH Key Group':                  'ssh_key_group',
    'SSH Key':                        'ssh_key',
    'IP Block':                       'ip_block',
    'DPU Extension Service':          'dpu_extension_service',
    'InfiniBand Partition':           'infiniband_partition',
    'NVLink Logical Partition':       'nvlink_logical_partition',
    'Instance Type':                  'instance_type',
    'Expected Machine':               'expected_machine',
    'Expected Power Shelf':           'expected_power_shelf',
    'Expected Rack':                  'expected_rack',
    'Expected Switch':                'expected_switch',
    'Tenant Account':                 'tenant_account',
    'Network Security Group':         'network_security_group',
    'Machine Capability':             'machine_capability',
    'Service Account':                'service_account',
    'Infrastructure Provider':        'infrastructure_provider',
    'Operating System':               'operating_system',
    'VPC Prefix':                     'vpc_prefix',
    'VPC Peering':                    'vpc_peering',
    'NVLink Domain':                  'nvlink_domain',
    'iPXE Template':                  'ipxe_template',
    'SpectrumX Partition':            'spectrumx_partition',
    'Measured Boot Trusted Machine':  'measured_boot_trusted_machine',
    'Measured Boot Trusted Profile':  'measured_boot_trusted_profile',
    'Credential Rotation':            'credential_rotation',
    'Site Explorer':                  'site_explorer',
    'BMC Credential':                 'bmc_credential',
    'UEFI Credential':                'uefi_credential',
    'BMC Reset':                      'bmc_reset',
    'DPU Reprovision':                'dpu_reprovision',
    'Machine Validation':             'machine_validation',
}

# Paths to skip (sub-resources, status-history, special endpoints).
SKIP_PATHS = {
    # Status history (read-only sub-paths)
    '/v2/org/{org}/carbide/site/{siteId}/status-history',
    '/v2/org/{org}/carbide/instance/{instanceId}/status-history',
    '/v2/org/{org}/carbide/machine/{machineId}/status-history',
    # Stats endpoints
    '/v2/org/{org}/carbide/infrastructure-provider/current/stats',
    '/v2/org/{org}/carbide/tenant/current/stats',
    '/v2/org/{org}/carbide/machine/gpu/stats',
    '/v2/org/{org}/carbide/machine/instance-type/stats/summary',
    '/v2/org/{org}/carbide/machine/instance-type/stats',
    '/v2/org/{org}/carbide/tenant/instance-type/stats',
    # VPC sub-operations
    '/v2/org/{org}/carbide/vpc/{vpcId}/virtualization',
    # Instance type machine associations
    '/v2/org/{org}/carbide/instance/type/{instanceTypeId}/machine',
    '/v2/org/{org}/carbide/instance/type/{instanceTypeId}/machine/{machineAssociationId}',
    # Instance interfaces (sub-resources)
    '/v2/org/{org}/carbide/instance/{instanceId}/interface',
    '/v2/org/{org}/carbide/instance/{instanceId}/infiniband-interface',
    '/v2/org/{org}/carbide/nvlink-interface',
    # IP Block derived
    '/v2/org/{org}/carbide/ipblock/{ipBlockId}/derived',
    # Batch endpoints (imperative, not declarative — skip for Terraform)
    '/v2/org/{org}/carbide/expected-machine/batch',
    '/v2/org/{org}/carbide/instance/batch',
    # Machine capability (handled as read-only via TAG)
    '/v2/org/{org}/carbide/machine-capability',
    # Action endpoints (imperative — skip for Terraform)
    '/v2/org/{org}/carbide/rack/validation',
    '/v2/org/{org}/carbide/rack/{id}/validation',
    '/v2/org/{org}/carbide/rack/power',
    '/v2/org/{org}/carbide/rack/{id}/power',
    '/v2/org/{org}/carbide/rack/firmware',
    '/v2/org/{org}/carbide/rack/{id}/firmware',
    '/v2/org/{org}/carbide/tray/validation',
    '/v2/org/{org}/carbide/tray/{id}/validation',
    '/v2/org/{org}/carbide/tray/power',
    '/v2/org/{org}/carbide/tray/{id}/power',
    '/v2/org/{org}/carbide/tray/firmware',
    '/v2/org/{org}/carbide/tray/{id}/firmware',
    # DPU Extension Service versioned sub-resource
    '/v2/org/{org}/carbide/dpu-extension-service/{dpuExtensionServiceId}/version/{version}',
}
