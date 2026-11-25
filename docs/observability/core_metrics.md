# NVIDIA Infra Controller (NICo) Core Metrics

This file contains a list of metrics exported by NVIDIA Infra Controller (NICo). The list is auto-generated from an integration test (`test_integration`). Metrics for workflows which are not exercised by the test are missing. NVLink partition monitor's metrics are documented in the manual: [NVLink Partitioning](../manuals/nvlink_partitioning.md#metrics).

<table>
<tr><td>Name</td><td>Type</td><td>Description</td></tr>
<tr><td>carbide_active_host_firmware_update_count</td><td>gauge</td><td>The number of host machines in the system currently working on updating their firmware.</td></tr>
<tr><td>carbide_api_db_queries_total</td><td>counter</td><td>The amount of database queries that occurred inside a span</td></tr>
<tr><td>carbide_api_db_span_query_time_milliseconds</td><td>histogram</td><td>Total time the request spent inside a span on database transactions</td></tr>
<tr><td>carbide_api_grpc_server_duration_milliseconds</td><td>histogram</td><td>Processing time for a request on the carbide API server</td></tr>
<tr><td>carbide_api_ready</td><td>gauge</td><td>Whether the NICo API is running</td></tr>
<tr><td>carbide_api_tls_connection_attempted_total</td><td>counter</td><td>The amount of tls connections that were attempted</td></tr>
<tr><td>carbide_api_tls_connection_success_total</td><td>counter</td><td>The amount of tls connections that were successful</td></tr>
<tr><td>carbide_api_tracing_spans_open</td><td>gauge</td><td>Number of open logging/tracing spans</td></tr>
<tr><td>carbide_api_vault_request_duration_milliseconds</td><td>histogram</td><td>the duration of outbound vault requests, in milliseconds</td></tr>
<tr><td>carbide_api_vault_requests_attempted_total</td><td>counter</td><td>The amount of tls connections that were attempted</td></tr>
<tr><td>carbide_api_vault_requests_failed_total</td><td>counter</td><td>The amount of tcp connections that were failures</td></tr>
<tr><td>carbide_api_vault_requests_succeeded_total</td><td>counter</td><td>The amount of tls connections that were successful</td></tr>
<tr><td>carbide_api_vault_token_time_until_refresh_seconds</td><td>gauge</td><td>The amount of time, in seconds, until the vault token is required to be refreshed</td></tr>
<tr><td>carbide_api_version</td><td>gauge</td><td>Version (git sha, build date, etc) of this service</td></tr>
<tr><td>carbide_available_ips_count</td><td>gauge</td><td>The total number of available ips in the site</td></tr>
<tr><td>carbide_concurrent_machine_updates_available</td><td>gauge</td><td>The number of machines in the system that we will update concurrently.</td></tr>
<tr><td>carbide_db_pool_idle_conns</td><td>gauge</td><td>The amount of idle connections in the carbide database pool</td></tr>
<tr><td>carbide_db_pool_total_conns</td><td>gauge</td><td>The amount of total (active + idle) connections in the carbide database pool</td></tr>
<tr><td>carbide_dpu_agent_version_count</td><td>gauge</td><td>The amount of DPU agents which have reported a certain version.</td></tr>
<tr><td>carbide_dpu_firmware_version_count</td><td>gauge</td><td>The amount of DPUs which have reported a certain firmware version.</td></tr>
<tr><td>carbide_dpus_healthy_count</td><td>gauge</td><td>The total number of DPUs in the system that have reported healthy in the last report. Healthy does not imply up - the report from the DPU might be outdated.</td></tr>
<tr><td>carbide_dpus_up_count</td><td>gauge</td><td>The total number of DPUs in the system that are up. Up means we have received a health report less than 5 minutes ago.</td></tr>
<tr><td>carbide_endpoint_exploration_duration_milliseconds</td><td>histogram</td><td>The time it took to explore an endpoint</td></tr>
<tr><td>carbide_endpoint_exploration_expected_machines_missing_overall_count</td><td>gauge</td><td>The total number of machines that were expected but not identified</td></tr>
<tr><td>carbide_endpoint_exploration_expected_power_shelves_missing_overall_count</td><td>gauge</td><td>The total number of power shelves that were expected but not identified</td></tr>
<tr><td>carbide_endpoint_exploration_identified_managed_hosts_overall_count</td><td>gauge</td><td>The total number of managed hosts identified by expectation</td></tr>
<tr><td>carbide_endpoint_exploration_machines_explored_overall_count</td><td>gauge</td><td>The total number of machines explored by machine type</td></tr>
<tr><td>carbide_endpoint_exploration_step_latency_milliseconds</td><td>histogram</td><td>The time it took to perform one endpoint exploration step</td></tr>
<tr><td>carbide_endpoint_exploration_success_count</td><td>gauge</td><td>The amount of endpoint explorations that have been successful</td></tr>
<tr><td>carbide_endpoint_explorations_count</td><td>gauge</td><td>The amount of endpoint explorations that have been attempted</td></tr>
<tr><td>carbide_gpus_in_use_count</td><td>gauge</td><td>The total number of GPUs that are actively used by tenants in instances in the NICo deployment</td></tr>
<tr><td>carbide_gpus_total_count</td><td>gauge</td><td>The total number of GPUs available in the NICo deployment</td></tr>
<tr><td>carbide_gpus_usable_count</td><td>gauge</td><td>The remaining number of GPUs in the NICo deployment which are available for immediate instance creation</td></tr>
<tr><td>carbide_hosts_by_sku_count</td><td>gauge</td><td>The amount of hosts by SKU and device type (&#39;unknown&#39; for hosts without SKU)</td></tr>
<tr><td>carbide_hosts_health_overrides_count</td><td>gauge</td><td>The amount of health overrides that are configured in the site</td></tr>
<tr><td>carbide_hosts_health_status_count</td><td>gauge</td><td>The total number of Managed Hosts in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_hosts_in_use_count</td><td>gauge</td><td>The total number of hosts that are actively used by tenants as instances in the NICo deployment</td></tr>
<tr><td>carbide_hosts_unhealthy_by_classification_count</td><td>gauge</td><td>The amount of objects which are marked with a certain classification due to being unhealthy</td></tr>
<tr><td>carbide_hosts_unhealthy_by_probe_id_count</td><td>gauge</td><td>The amount of objects which reported a certain Health Probe Alert</td></tr>
<tr><td>carbide_hosts_usable_count</td><td>gauge</td><td>The remaining number of hosts in the NICo deployment which are available for immediate instance creation</td></tr>
<tr><td>carbide_hosts_with_bios_password_set</td><td>gauge</td><td>The total number of Hosts in the system that have their BIOS password set.</td></tr>
<tr><td>carbide_ib_monitor_fabrics_count</td><td>gauge</td><td>The amount of InfiniBand fabrics that are monitored</td></tr>
<tr><td>carbide_ib_monitor_machine_ib_status_updates_count</td><td>gauge</td><td>The amount of Machines where the infiniband_status_observation got updated</td></tr>
<tr><td>carbide_ib_monitor_machines_with_missing_pkeys_count</td><td>gauge</td><td>The amount of machines where at least one port is not assigned to the expected pkey on UFM</td></tr>
<tr><td>carbide_ib_monitor_machines_with_unexpected_pkeys_count</td><td>gauge</td><td>The amount of machines where at least one port is assigned to an unexpected pkey on UFM</td></tr>
<tr><td>carbide_ib_monitor_machines_with_unknown_pkeys_count</td><td>gauge</td><td>The amount of machines where at least one port is assigned to a pkey value that is not associated with any partition ID</td></tr>
<tr><td>carbide_ib_partitions_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_ib_partitions in the system</td></tr>
<tr><td>carbide_ib_partitions_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_ib_partitions</td></tr>
<tr><td>carbide_ib_partitions_object_tasks_enqueued_total</td><td>counter</td><td>The amount of types that object handling tasks that have been freshly enqueued for objects of type carbide_ib_partitions</td></tr>
<tr><td>carbide_ib_partitions_total</td><td>gauge</td><td>The total number of carbide_ib_partitions in the system</td></tr>
<tr><td>carbide_machine_reboot_duration_seconds</td><td>histogram</td><td>Time taken for machine/host to reboot in seconds</td></tr>
<tr><td>carbide_machine_updates_started_count</td><td>gauge</td><td>The number of machines in the system that are in the process of updating.</td></tr>
<tr><td>carbide_machine_validation_completed</td><td>gauge</td><td>Count of machine validation that have completed successfully</td></tr>
<tr><td>carbide_machine_validation_failed</td><td>gauge</td><td>Count of machine validation that have failed</td></tr>
<tr><td>carbide_machine_validation_in_progress</td><td>gauge</td><td>Count of machine validation that are in progress</td></tr>
<tr><td>carbide_machine_validation_oldest_active_age_seconds</td><td>gauge</td><td>Age in seconds of the oldest active machine validation run</td></tr>
<tr><td>carbide_machine_validation_stale_runs_count</td><td>gauge</td><td>Count of active machine validation runs considered stale</td></tr>
<tr><td>carbide_machine_validation_tests</td><td>gauge</td><td>The details of machine validation tests</td></tr>
<tr><td>carbide_machines_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_machines in the system</td></tr>
<tr><td>carbide_machines_handler_latency_in_state_milliseconds</td><td>histogram</td><td>The amount of time it took to invoke the state handler for objects of type carbide_machines in a certain state</td></tr>
<tr><td>carbide_machines_in_maintenance_count</td><td>gauge</td><td>The total number of machines in the system that are in maintenance.</td></tr>
<tr><td>carbide_machines_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_completed_total</td><td>counter</td><td>The amount of object handling tasks that have been completed for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_dispatched_total</td><td>counter</td><td>The amount of types that object handling tasks that have been dequeued and dispatched for processing for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_enqueued_total</td><td>counter</td><td>The amount of types that object handling tasks that have been freshly enqueued for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_requeued_total</td><td>counter</td><td>The amount of object handling tasks that have been requeued for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_per_state</td><td>gauge</td><td>The number of carbide_machines in the system with a given state</td></tr>
<tr><td>carbide_machines_per_state_above_sla</td><td>gauge</td><td>The number of carbide_machines in the system which had been longer in a state than allowed per SLA</td></tr>
<tr><td>carbide_machines_state_entered_total</td><td>counter</td><td>The amount of types that objects of type carbide_machines have entered a certain state</td></tr>
<tr><td>carbide_machines_state_exited_total</td><td>counter</td><td>The amount of types that objects of type carbide_machines have exited a certain state</td></tr>
<tr><td>carbide_machines_time_in_state_seconds</td><td>histogram</td><td>The amount of time objects of type carbide_machines have spent in a certain state</td></tr>
<tr><td>carbide_machines_total</td><td>gauge</td><td>The total number of carbide_machines in the system</td></tr>
<tr><td>carbide_machines_with_state_handling_errors_per_state</td><td>gauge</td><td>The number of carbide_machines in the system with a given state that failed state handling</td></tr>
<tr><td>carbide_measured_boot_bundles_total</td><td>gauge</td><td>The total number of measured boot bundles.</td></tr>
<tr><td>carbide_measured_boot_machines_per_bundle_state_total</td><td>gauge</td><td>The total number of machines per a given measured boot bundle state.</td></tr>
<tr><td>carbide_measured_boot_machines_per_machine_state_total</td><td>gauge</td><td>The total number of machines per a given measured boot machine state.</td></tr>
<tr><td>carbide_measured_boot_machines_total</td><td>gauge</td><td>The total number of machines reporting measurements.</td></tr>
<tr><td>carbide_measured_boot_profiles_total</td><td>gauge</td><td>The total number of measured boot profiles.</td></tr>
<tr><td>carbide_network_segments_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_network_segments in the system</td></tr>
<tr><td>carbide_network_segments_handler_latency_in_state_milliseconds</td><td>histogram</td><td>The amount of time it took to invoke the state handler for objects of type carbide_network_segments in a certain state</td></tr>
<tr><td>carbide_network_segments_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_completed_total</td><td>counter</td><td>The amount of object handling tasks that have been completed for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_dispatched_total</td><td>counter</td><td>The amount of types that object handling tasks that have been dequeued and dispatched for processing for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_enqueued_total</td><td>counter</td><td>The amount of types that object handling tasks that have been freshly enqueued for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_requeued_total</td><td>counter</td><td>The amount of object handling tasks that have been requeued for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_per_state</td><td>gauge</td><td>The number of carbide_network_segments in the system with a given state</td></tr>
<tr><td>carbide_network_segments_per_state_above_sla</td><td>gauge</td><td>The number of carbide_network_segments in the system which had been longer in a state than allowed per SLA</td></tr>
<tr><td>carbide_network_segments_state_entered_total</td><td>counter</td><td>The amount of types that objects of type carbide_network_segments have entered a certain state</td></tr>
<tr><td>carbide_network_segments_state_exited_total</td><td>counter</td><td>The amount of types that objects of type carbide_network_segments have exited a certain state</td></tr>
<tr><td>carbide_network_segments_time_in_state_seconds</td><td>histogram</td><td>The amount of time objects of type carbide_network_segments have spent in a certain state</td></tr>
<tr><td>carbide_network_segments_total</td><td>gauge</td><td>The total number of carbide_network_segments in the system</td></tr>
<tr><td>carbide_network_segments_with_state_handling_errors_per_state</td><td>gauge</td><td>The number of carbide_network_segments in the system with a given state that failed state handling</td></tr>
<tr><td>carbide_nvlink_partition_monitor_machine_status_updates_count</td><td>gauge</td><td>Number of machines nvlink_status_observation got updated</td></tr>
<tr><td>carbide_nvlink_partition_monitor_nmxc_changes_applied_total</td><td>counter</td><td>Number of changes requested to NMX-C</td></tr>
<tr><td>carbide_nvlink_partition_monitor_num_logical_partitions</td><td>gauge</td><td>Number of logical partitions that were monitored</td></tr>
<tr><td>carbide_nvlink_partition_monitor_num_physical_partitions</td><td>gauge</td><td>Number of physical partitions that were monitored</td></tr>
<tr><td>carbide_nvlink_partition_monitor_nvlink_info_mismatches</td><td>gauge</td><td>Number of NVLink GPU partition ID mismatches between DB and NMX-C</td></tr>
<tr><td>carbide_nvlink_partition_monitor_stale_partitions_deleted</td><td>gauge</td><td>Number of stale partitions deleted from DB (not found in NMX-C)</td></tr>
<tr><td>carbide_nvlink_partition_monitor_unhealthy_alert_write_failures_total</td><td>counter</td><td>Number of NvlinkUnhealthy health-report writes that failed and were deferred to the next pass</td></tr>
<tr><td>carbide_pending_dpu_nic_firmware_update_count</td><td>gauge</td><td>The number of machines in the system that need a firmware update.</td></tr>
<tr><td>carbide_pending_host_firmware_update_count</td><td>gauge</td><td>The number of host machines in the system that need a firmware update.</td></tr>
<tr><td>carbide_power_shelves_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_power_shelves in the system</td></tr>
<tr><td>carbide_power_shelves_health_overrides_count</td><td>gauge</td><td>The amount of health overrides that are configured in the site</td></tr>
<tr><td>carbide_power_shelves_health_status_count</td><td>gauge</td><td>The total number of Power Shelves in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_power_shelves_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_power_shelves</td></tr>
<tr><td>carbide_power_shelves_object_tasks_enqueued_total</td><td>counter</td><td>The amount of types that object handling tasks that have been freshly enqueued for objects of type carbide_power_shelves</td></tr>
<tr><td>carbide_power_shelves_total</td><td>gauge</td><td>The total number of carbide_power_shelves in the system</td></tr>
<tr><td>carbide_preingestion_total</td><td>gauge</td><td>The amount of known machines currently being evaluated prior to ingestion</td></tr>
<tr><td>carbide_preingestion_waiting_download</td><td>gauge</td><td>The amount of machines that are waiting for firmware downloads on other machines to complete before doing their own</td></tr>
<tr><td>carbide_preingestion_waiting_installation</td><td>gauge</td><td>The amount of machines which have had firmware uploaded to them and are currently in the process of installing that firmware</td></tr>
<tr><td>carbide_racks_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_racks in the system</td></tr>
<tr><td>carbide_racks_health_overrides_count</td><td>gauge</td><td>The amount of health overrides that are configured in the site</td></tr>
<tr><td>carbide_racks_health_status_count</td><td>gauge</td><td>The total number of Racks in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_racks_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_racks</td></tr>
<tr><td>carbide_racks_object_tasks_enqueued_total</td><td>counter</td><td>The amount of types that object handling tasks that have been freshly enqueued for objects of type carbide_racks</td></tr>
<tr><td>carbide_racks_total</td><td>gauge</td><td>The total number of carbide_racks in the system</td></tr>
<tr><td>carbide_reboot_attempts_in_booting_with_discovery_image</td><td>histogram</td><td>The amount of machines rebooted again in BootingWithDiscoveryImage since there is no response after a certain time from host.</td></tr>
<tr><td>carbide_reserved_ips_count</td><td>gauge</td><td>The total number of reserved ips in the site</td></tr>
<tr><td>carbide_resourcepool_free_count</td><td>gauge</td><td>Count of values in the pool currently available for allocation</td></tr>
<tr><td>carbide_resourcepool_used_count</td><td>gauge</td><td>Count of values in the pool currently allocated</td></tr>
<tr><td>carbide_running_dpu_updates_count</td><td>gauge</td><td>The number of machines in the system that are running a firmware update.</td></tr>
<tr><td>carbide_site_exploration_expected_machines_sku_count</td><td>gauge</td><td>The total count of expected machines by SKU ID and device type</td></tr>
<tr><td>carbide_site_exploration_identified_managed_hosts_count</td><td>gauge</td><td>The amount of Host+DPU pairs that has been identified in the last SiteExplorer run</td></tr>
<tr><td>carbide_site_explorer_bmc_reset_count</td><td>gauge</td><td>The amount of BMC resets initiated in the last SiteExplorer run</td></tr>
<tr><td>carbide_site_explorer_create_machines</td><td>gauge</td><td>Whether site-explorer machine creation is enabled (1) or disabled (0)</td></tr>
<tr><td>carbide_site_explorer_create_machines_latency_milliseconds</td><td>histogram</td><td>The time it took to perform create_machines inside site-explorer</td></tr>
<tr><td>carbide_site_explorer_created_machines_count</td><td>gauge</td><td>The amount of Machine pairs that had been created by Site Explorer after being identified</td></tr>
<tr><td>carbide_site_explorer_created_power_shelves_count</td><td>gauge</td><td>The amount of Power Shelves that had been created by Site Explorer after being identified</td></tr>
<tr><td>carbide_site_explorer_dpu_migration_signals_count</td><td>gauge</td><td>Count of DPU NIC-mode migration signals by kind -- mode-mismatch found, set_nic_mode issued, reset requested, and zero-DPU registered for a NicMode host.</td></tr>
<tr><td>carbide_site_explorer_enabled</td><td>gauge</td><td>Whether site-explorer is enabled (1) or paused (0)</td></tr>
<tr><td>carbide_site_explorer_iteration_latency_milliseconds</td><td>histogram</td><td>The time it took to perform one site explorer iteration</td></tr>
<tr><td>carbide_site_explorer_last_run_status</td><td>gauge</td><td>The status of the latest Site Explorer run</td></tr>
<tr><td>carbide_site_explorer_phase_latency_milliseconds</td><td>histogram</td><td>The time it took to perform one site explorer iteration phase</td></tr>
<tr><td>carbide_site_explorer_update_explored_endpoints_count</td><td>gauge</td><td>Counts from the last update_explored_endpoints phase by kind</td></tr>
<tr><td>carbide_switches_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_switches in the system</td></tr>
<tr><td>carbide_switches_health_overrides_count</td><td>gauge</td><td>The amount of health overrides that are configured in the site</td></tr>
<tr><td>carbide_switches_health_status_count</td><td>gauge</td><td>The total number of Switches in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_switches_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_switches</td></tr>
<tr><td>carbide_switches_object_tasks_enqueued_total</td><td>counter</td><td>The amount of types that object handling tasks that have been freshly enqueued for objects of type carbide_switches</td></tr>
<tr><td>carbide_switches_total</td><td>gauge</td><td>The total number of carbide_switches in the system</td></tr>
<tr><td>carbide_total_ips_count</td><td>gauge</td><td>The total number of ips in the site</td></tr>
<tr><td>carbide_unavailable_dpu_nic_firmware_update_count</td><td>gauge</td><td>The number of machines in the system that need a firmware update but are unavailable for update.</td></tr>
<tr><td>carbide_vpc_prefixes_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_vpc_prefixes in the system</td></tr>
<tr><td>carbide_vpc_prefixes_handler_latency_in_state_milliseconds</td><td>histogram</td><td>The amount of time it took to invoke the state handler for objects of type carbide_vpc_prefixes in a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_completed_total</td><td>counter</td><td>The amount of object handling tasks that have been completed for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_dispatched_total</td><td>counter</td><td>The amount of types that object handling tasks that have been dequeued and dispatched for processing for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_enqueued_total</td><td>counter</td><td>The amount of types that object handling tasks that have been freshly enqueued for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_requeued_total</td><td>counter</td><td>The amount of object handling tasks that have been requeued for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_per_state</td><td>gauge</td><td>The number of carbide_vpc_prefixes in the system with a given state</td></tr>
<tr><td>carbide_vpc_prefixes_per_state_above_sla</td><td>gauge</td><td>The number of carbide_vpc_prefixes in the system which had been longer in a state than allowed per SLA</td></tr>
<tr><td>carbide_vpc_prefixes_state_entered_total</td><td>counter</td><td>The amount of types that objects of type carbide_vpc_prefixes have entered a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_state_exited_total</td><td>counter</td><td>The amount of types that objects of type carbide_vpc_prefixes have exited a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_time_in_state_seconds</td><td>histogram</td><td>The amount of time objects of type carbide_vpc_prefixes have spent in a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_total</td><td>gauge</td><td>The total number of carbide_vpc_prefixes in the system</td></tr>
<tr><td>carbide_vpc_prefixes_with_state_handling_errors_per_state</td><td>gauge</td><td>The number of carbide_vpc_prefixes in the system with a given state that failed state handling</td></tr>
</table>
