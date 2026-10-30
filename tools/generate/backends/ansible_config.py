# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Per-resource overrides for the code generator.

These overrides handle edge cases that cannot be inferred from the OpenAPI spec alone.
Keys are the snake_case module names. Values are dicts that get merged into (and override)
the auto-generated resource config.
"""

RESOURCE_OVERRIDES = {
    # Machine: update/delete only (no create; machines are discovered)
    'machine': {
        'no_create': True,
        'name_field': None,
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Instance: delete can have a body.  A valid create needs an operating
    # system (or an iPXE script) and at least one interface unless autoNetwork
    # is set — an explicit Subnet interface with isPhysical is the portable
    # form across network virtualization types.
    'instance': {
        'delete_body_fields': ['machine_health_issue', 'is_repair_tenant'],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
        'example_extras': [
            'operating_system_id: "{{ operating_system_id }}"',
            'interfaces:',
            '  - subnet_id: "{{ subnet_id }}"',
            '    is_physical: true',
        ],
    },

    # Instance batch: batch create only, no state parameter.  Same conditional
    # create requirements as instance: operating system (or iPXE script) and a
    # Subnet-backed physical interface (batch requests cannot use ipAddress).
    'instance_batch': {
        'module_type': 'batch',
        'example_extras': [
            'operating_system_id: "{{ operating_system_id }}"',
            'interfaces:',
            '  - subnet_id: "{{ subnet_id }}"',
            '    is_physical: true',
        ],
    },

    # Host firmware config: `ordering` is required on first create (and when
    # adding components) but the schema cannot mark it required because it is
    # optional on update, so the create example carries it explicitly.
    'firmware_config_host': {
        'example_extras': ['ordering: "{{ ordering }}"'],
    },

    # Allocation constraint: nested under allocation
    'allocation_constraint': {
        'scope_fields': ['allocation_id'],
    },

    # VPC prefix: scope by VPC
    'vpc_prefix': {
        'scope_fields': ['vpc_id', 'site_id'],
    },

    # Subnet: scope by VPC.  The list API requires siteId whenever vpcId is
    # absent — a conditional the spec cannot mark required — so the list
    # example carries site_id explicitly.
    'subnet': {
        'scope_fields': ['vpc_id', 'site_id'],
    },
    'subnet_info': {
        'example_extras': ['site_id: "{{ site_id }}"'],
    },

    # SSH Key: scope by SSH Key Group
    'ssh_key': {
        'scope_fields': ['ssh_key_group_id'],
    },

    # DPU Extension Service: has versioned sub-resource, but we treat the
    # main resource as CRUD
    'dpu_extension_service': {
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Expected machine: batch endpoint exists but is separate
    'expected_machine': {
        'scope_fields': ['site_id'],
    },

    # InfiniBand Partition: scope by site
    'infiniband_partition': {
        'scope_fields': ['site_id'],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # NVLink Logical Partition: scope by site
    'nvlink_logical_partition': {
        'scope_fields': ['site_id'],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Network Security Group
    'network_security_group': {
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # SSH Key Group
    'ssh_key_group': {
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # IP Block
    'ip_block': {
        'scope_fields': ['site_id'],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Tenant Account
    'tenant_account': {
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Site
    'site': {
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Allocation
    'allocation': {
        'scope_fields': ['site_id'],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # VPC
    'vpc': {
        'scope_fields': ['site_id'],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Instance Type
    'instance_type': {
        'scope_fields': ['site_id'],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
    },

    # Operating System: a valid create needs an image definition — the schema
    # cannot mark these required because raw-iPXE and templated-iPXE creates
    # use different fields — so the create example carries one complete
    # image-based variant.
    'operating_system': {
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
        'example_extras': [
            'site_ids:',
            '  - "{{ site_id }}"',
            'image_url: "{{ image_url }}"',
            'image_sha: "{{ image_sha }}"',
            'root_fs_id: "{{ root_fs_id }}"',
        ],
    },

    # TaskRun: status never reaches Ready/Error — use actual terminal states
    'task_run': {
        'ready_statuses': ['Completed', 'Cancelled', 'CompletedWithFailures'],
        'error_statuses': ['Failed'],
    },
    'task_run_info': {
        'ready_statuses': ['Completed', 'Cancelled', 'CompletedWithFailures'],
        'error_statuses': ['Failed'],
    },

    # POST-only action modules: no item path to poll, disable wait
    'task_cancel': {'no_wait': True},
    'task_run_pause': {'no_wait': True},
    # Resume applies only to operator-paused Task Runs; a run paused at a
    # phase gate must use the advance endpoint instead.
    'task_run_resume': {
        'no_wait': True,
        'example_create_name': 'Resume an operator-paused Task Run',
    },
    'task_run_advance': {'no_wait': True},
    'task_run_cancel': {'no_wait': True},
    'machine_decommission': {'no_wait': True},

    # Machine Spectrum-X multiplane: the update requires exactly one rail
    # selector and one plane selector — a conditional the schema cannot mark
    # required — so the update example selects all rails and all planes.
    'machine_spectrumx_multiplane': {
        'example_extras': [
            'all_rails: true',
            'all_planes: true',
        ],
    },
}

# Tags that map to read-only resources (no POST/PATCH/DELETE on main resource)
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
}

# Tags to skip entirely (deprecated endpoints, sub-resource-only, etc.)
SKIP_TAGS = {
    'Deprecations',
}

# Map from spec tag name to module name override (when auto-conversion isn't right)
TAG_TO_MODULE = {
    'SSH Key Group': 'ssh_key_group',
    'SSH Key': 'ssh_key',
    'IP Block': 'ip_block',
    'DPU Extension Service': 'dpu_extension_service',
    'InfiniBand Partition': 'infiniband_partition',
    'NVLink Logical Partition': 'nvlink_logical_partition',
    'Instance Type': 'instance_type',
    'Expected Machine': 'expected_machine',
    'Tenant Account': 'tenant_account',
    'Network Security Group': 'network_security_group',
    'Machine Capability': 'machine_capability',
    'Service Account': 'service_account',
    'Infrastructure Provider': 'infrastructure_provider',
    'Operating System': 'operating_system',
    'VPC Prefix': 'vpc_prefix',
}

# Paths to skip (sub-resources, status-history, special endpoints that don't
# map to a standalone module)
SKIP_PATHS = {
    # Standard OIDC/PKI discovery endpoints — read-only, not Ansible-manageable resources
    '/v2/org/{org}/nico/site/{siteID}/.well-known/jwks.json',
    '/v2/org/{org}/nico/site/{siteID}/.well-known/openid-configuration',
    '/v2/org/{org}/nico/site/{siteID}/.well-known/spiffe/jwks.json',
    # Status history endpoints
    '/v2/org/{org}/nico/site/{siteId}/status-history',
    '/v2/org/{org}/nico/instance/{instanceId}/status-history',
    '/v2/org/{org}/nico/machine/{machineId}/status-history',
    # Stats endpoints
    '/v2/org/{org}/nico/infrastructure-provider/current/stats',
    '/v2/org/{org}/nico/tenant/current/stats',
    # VPC virtualization update (special sub-operation)
    '/v2/org/{org}/nico/vpc/{vpcId}/virtualization',
    # Instance type machine association (sub-resource)
    '/v2/org/{org}/nico/instance/type/{instanceTypeId}/machine',
    '/v2/org/{org}/nico/instance/type/{instanceTypeId}/machine/{machineAssociationId}',
    # Instance interfaces (sub-resources)
    '/v2/org/{org}/nico/instance/{instanceId}/interface',
    '/v2/org/{org}/nico/instance/{instanceId}/infiniband-interface',
    # IB interface listing endpoint — tagged "InfiniBand Partition" in the spec but
    # only has GET; skip so infiniband_partition uses /infiniband-partition (POST/GET).
    '/v2/org/{org}/nico/infiniband-interface',
    '/v2/org/{org}/nico/nvlink-interface',
    # IP Block derived
    '/v2/org/{org}/nico/ipblock/{ipBlockId}/derived',
    # DPU Extension Service versioned sub-resource
    '/v2/org/{org}/nico/dpu-extension-service/{dpuExtensionServiceId}/version/{version}',
    # Expected machine batch
    '/v2/org/{org}/nico/expected-machine/batch',
    # Instance batch (handled separately as instance_batch module)
    '/v2/org/{org}/nico/instance/batch',
    # Machine capability - tagged as "Machine" in spec but is a separate read-only resource;
    # handled via PATH_TAG_OVERRIDES
    '/v2/org/{org}/nico/machine-capability',
    # Stats-only endpoints — no Ansible-manageable resources.
    '/v2/org/{org}/nico/machine/gpu/stats',
    '/v2/org/{org}/nico/machine/instance-type/stats/summary',
    '/v2/org/{org}/nico/machine/instance-type/stats',
    '/v2/org/{org}/nico/tenant/instance-type/stats',
    # Rack action sub-paths (generated explicitly via ACTION_MODULES)
    '/v2/org/{org}/nico/rack/validation',
    '/v2/org/{org}/nico/rack/{id}/validation',
    '/v2/org/{org}/nico/rack/power',
    '/v2/org/{org}/nico/rack/{id}/power',
    '/v2/org/{org}/nico/rack/firmware',
    '/v2/org/{org}/nico/rack/{id}/firmware',
    # Tray action sub-paths (generated explicitly via ACTION_MODULES)
    '/v2/org/{org}/nico/tray/validation',
    '/v2/org/{org}/nico/tray/{id}/validation',
    '/v2/org/{org}/nico/tray/power',
    '/v2/org/{org}/nico/tray/{id}/power',
    '/v2/org/{org}/nico/tray/firmware',
    '/v2/org/{org}/nico/tray/{id}/firmware',
}

# Batch endpoints: map from module_name -> batch path info
BATCH_MODULES = {
    'instance_batch': {
        'path': '/v2/org/{org}/nico/instance/batch',
        'tag': 'Instance',
        'schema_ref': '#/components/schemas/BatchInstanceCreateRequest',
    },
}

# Info-only endpoints that are mis-tagged in the spec
# These need to be generated as separate _info modules
INFO_ONLY_MODULES = {
    'machine_capability': {
        'collection_path': '/v2/org/{org}/nico/machine-capability',
        'tag': 'Machine Capability',
        'response_schema_ref': '#/components/schemas/MachineCapability',
    },
}

# Action endpoints: imperative operations (validation, power, firmware)
ACTION_MODULES = {
    'rack_validation': {
        'collection_path': '/v2/org/{org}/nico/rack/validation',
        'item_path': '/v2/org/{org}/nico/rack/{id}/validation',
        'tag': 'Rack',
        'method': 'GET',
        'response_schema_ref': '#/components/schemas/RackValidationResult',
    },
    'rack_power': {
        'collection_path': '/v2/org/{org}/nico/rack/power',
        'item_path': '/v2/org/{org}/nico/rack/{id}/power',
        'tag': 'Rack',
        'method': 'PATCH',
        'collection_request_schema_ref': '#/components/schemas/BatchUpdateRackPowerStateRequest',
        'item_request_schema_ref': '#/components/schemas/UpdatePowerStateRequest',
        'response_schema_ref': '#/components/schemas/UpdatePowerStateResponse',
    },
    'rack_firmware': {
        'collection_path': '/v2/org/{org}/nico/rack/firmware',
        'item_path': '/v2/org/{org}/nico/rack/{id}/firmware',
        'tag': 'Rack',
        'method': 'PATCH',
        'collection_request_schema_ref': '#/components/schemas/BatchRackFirmwareUpdateRequest',
        'item_request_schema_ref': '#/components/schemas/FirmwareUpdateRequest',
        'response_schema_ref': '#/components/schemas/FirmwareUpdateResponse',
    },
    'tray_validation': {
        'collection_path': '/v2/org/{org}/nico/tray/validation',
        'item_path': '/v2/org/{org}/nico/tray/{id}/validation',
        'tag': 'Tray',
        'method': 'GET',
        'response_schema_ref': '#/components/schemas/RackValidationResult',
    },
    'tray_power': {
        'collection_path': '/v2/org/{org}/nico/tray/power',
        'item_path': '/v2/org/{org}/nico/tray/{id}/power',
        'tag': 'Tray',
        'method': 'PATCH',
        'collection_request_schema_ref': '#/components/schemas/BatchUpdateTrayPowerStateRequest',
        'item_request_schema_ref': '#/components/schemas/UpdatePowerStateRequest',
        'response_schema_ref': '#/components/schemas/UpdatePowerStateResponse',
    },
    'tray_firmware': {
        'collection_path': '/v2/org/{org}/nico/tray/firmware',
        'item_path': '/v2/org/{org}/nico/tray/{id}/firmware',
        'tag': 'Tray',
        'method': 'PATCH',
        'collection_request_schema_ref': '#/components/schemas/BatchTrayFirmwareUpdateRequest',
        'item_request_schema_ref': '#/components/schemas/FirmwareUpdateRequest',
        'response_schema_ref': '#/components/schemas/FirmwareUpdateResponse',
    },
}
