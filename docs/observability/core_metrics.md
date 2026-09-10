# NVIDIA Infra Controller (NICo) Core Metrics

This file contains a list of metrics exported by NVIDIA Infra Controller (NICo). The list is auto-generated from an integration test (`test_integration`). Metrics no test exercises are added with `cargo xtask check-metric-docs --fix`. NVLink partition monitor's metrics are documented in the manual: [NVLink Partitioning](../manuals/nvlink_partitioning.md#metrics).

<table>
<tr><td>Name</td><td>Type</td><td>Description</td></tr>
<tr><td>carbide_active_host_firmware_update_count</td><td>gauge</td><td>Number of host machines in the system currently working on updating their firmware.</td></tr>
<tr><td>carbide_api_admission_admitted_total</td><td>counter</td><td>Number of API requests admitted for execution</td></tr>
<tr><td>carbide_api_admission_handler_execution_duration_seconds</td><td>histogram</td><td>Duration of admitted API request handler execution</td></tr>
<tr><td>carbide_api_admission_pending_requests</td><td>gauge</td><td>Number of API requests currently waiting for an execution slot</td></tr>
<tr><td>carbide_api_admission_pending_wait_duration_seconds</td><td>histogram</td><td>Duration API requests spent waiting for admission</td></tr>
<tr><td>carbide_api_admission_rejected_total</td><td>counter</td><td>Number of API requests rejected before handler execution</td></tr>
<tr><td>carbide_api_admission_work_in_flight</td><td>gauge</td><td>Number of API requests currently holding an execution slot</td></tr>
<tr><td>carbide_api_bmc_proxy_client_reload_failures_total</td><td>counter</td><td>Number of failed rebuilds of the nico-bmc-proxy-facing client from on-disk certificates.</td></tr>
<tr><td>carbide_api_db_queries_total</td><td>counter</td><td>Number of database queries that occurred inside a span</td></tr>
<tr><td>carbide_api_db_span_query_time_milliseconds</td><td>histogram</td><td>Total time the request spent inside a span on database transactions</td></tr>
<tr><td>carbide_api_grpc_server_duration_milliseconds</td><td>histogram</td><td>Processing time for a request on the carbide API server</td></tr>
<tr><td>carbide_api_ready</td><td>gauge</td><td>Whether the NICo API is running</td></tr>
<tr><td>carbide_api_secrets_request_duration_milliseconds</td><td>histogram</td><td>Duration of Postgres secrets operations, in milliseconds.</td></tr>
<tr><td>carbide_api_secrets_requests_failed_total</td><td>counter</td><td>Number of failed Postgres secrets operations.</td></tr>
<tr><td>carbide_api_secrets_requests_succeeded_total</td><td>counter</td><td>Number of successful Postgres secrets operations.</td></tr>
<tr><td>carbide_api_secrets_requests_total</td><td>counter</td><td>Number of Postgres secrets operations attempted.</td></tr>
<tr><td>carbide_api_tls_cert_refreshes_total</td><td>counter</td><td>Number of TLS acceptor refreshes performed by the API listener</td></tr>
<tr><td>carbide_api_tls_connection_attempted_total</td><td>counter</td><td>Number of inbound TLS connection attempts</td></tr>
<tr><td>carbide_api_tls_connection_fail_total</td><td>counter</td><td>Number of failed inbound TLS connection attempts</td></tr>
<tr><td>carbide_api_tls_connection_success_total</td><td>counter</td><td>Number of successful TLS connections</td></tr>
<tr><td>carbide_api_tracing_spans_open</td><td>gauge</td><td>Number of open logging/tracing spans</td></tr>
<tr><td>carbide_api_vault_request_duration_milliseconds</td><td>histogram</td><td>Duration of outbound Vault requests, in milliseconds</td></tr>
<tr><td>carbide_api_vault_requests_attempted_total</td><td>counter</td><td>Number of attempted Vault requests</td></tr>
<tr><td>carbide_api_vault_requests_failed_total</td><td>counter</td><td>Number of failed Vault requests</td></tr>
<tr><td>carbide_api_vault_requests_succeeded_total</td><td>counter</td><td>Number of successful Vault requests</td></tr>
<tr><td>carbide_api_vault_token_time_until_refresh_seconds</td><td>gauge</td><td>The amount of time, in seconds, until the Vault token is required to be refreshed</td></tr>
<tr><td>carbide_api_version</td><td>gauge</td><td>Version (git sha, build date, etc) of this service</td></tr>
<tr><td>carbide_attestation_total</td><td>counter</td><td>Number of device attestations performed, by device type and outcome.</td></tr>
<tr><td>carbide_auth_context_missing_total</td><td>counter</td><td>Number of Forge authorization requests missing authentication context, by authorizer</td></tr>
<tr><td>carbide_auth_denied_total</td><td>counter</td><td>Number of Forge calls denied by the authorizer</td></tr>
<tr><td>carbide_auth_permissive_overrides_total</td><td>counter</td><td>Number of policy denials overridden by authorization permissive mode, by principal class</td></tr>
<tr><td>carbide_authn_client_cert_rejected_total</td><td>counter</td><td>Number of client certificates rejected during authentication</td></tr>
<tr><td>carbide_authn_connection_attributes_missing_total</td><td>counter</td><td>Number of requests authentication could not inspect because connection attributes were missing</td></tr>
<tr><td>carbide_available_ips_count</td><td>gauge</td><td>Number of available IPs per network segment</td></tr>
<tr><td>carbide_bmc_credential_rotation_results_total</td><td>counter</td><td>Number of persisted BMC credential rotation results, by result</td></tr>
<tr><td>carbide_bmc_proxy_authorization_denied_total</td><td>counter</td><td>Number of BMC proxy requests denied by authorization layer and HTTP method</td></tr>
<tr><td>carbide_bmc_proxy_authorization_errors_total</td><td>counter</td><td>Number of BMC proxy authorization errors caused by missing authentication context, by authorization layer and HTTP method</td></tr>
<tr><td>carbide_bmc_proxy_tls_connection_attempted_total</td><td>counter</td><td>Number of inbound TLS connection attempts</td></tr>
<tr><td>carbide_bmc_proxy_tls_connection_fail_total</td><td>counter</td><td>Number of failed inbound connections, by failure reason</td></tr>
<tr><td>carbide_bmc_proxy_tls_connection_success_total</td><td>counter</td><td>Number of successful TLS connections</td></tr>
<tr><td>carbide_bmc_proxy_upstream_auth_retries_total</td><td>counter</td><td>Number of forwarded requests replayed once with freshly resolved BMC credentials after the BMC rejected the proxy&#39;s cached credential, by HTTP method</td></tr>
<tr><td>carbide_bmc_proxy_upstream_request_duration_milliseconds</td><td>histogram</td><td>Duration of requests the proxy forwarded to BMCs, by HTTP method and upstream status class; the _count series, split by status, gives the request and outcome rates.</td></tr>
<tr><td>carbide_bmc_session_cleanup_failures_total</td><td>counter</td><td>Number of BMC session cleanup failures, by operation.</td></tr>
<tr><td>carbide_bmc_session_lockout_breaker_transitions_total</td><td>counter</td><td>Number of BMC session lockout-avoidance breaker transitions.</td></tr>
<tr><td>carbide_bms_dsx_commands_dropped_total</td><td>counter</td><td>Number of BMS DSX Exchange commands dropped because the worker stopped, by command kind.</td></tr>
<tr><td>carbide_certs_renewals_total</td><td>counter</td><td>Number of client certificate renewal attempts, by outcome</td></tr>
<tr><td>carbide_client_tcp_connect_attempts_total</td><td>counter</td><td>Number of outbound TCP connect attempts across all HTTP connectors</td></tr>
<tr><td>carbide_client_tcp_connect_errors_total</td><td>counter</td><td>Number of failed outbound TCP connect attempts across all HTTP connectors</td></tr>
<tr><td>carbide_client_tcp_connect_successes_total</td><td>counter</td><td>Number of successful outbound TCP connects across all HTTP connectors</td></tr>
<tr><td>carbide_concurrent_machine_updates_available</td><td>gauge</td><td>Number of machines in the system that can be updated concurrently.</td></tr>
<tr><td>carbide_database_transaction_rollback_failures_total</td><td>counter</td><td>Number of database transaction rollback failures, by trigger.</td></tr>
<tr><td>carbide_db_pool_idle_conns</td><td>gauge</td><td>Number of idle connections in the carbide database pool</td></tr>
<tr><td>carbide_db_pool_total_conns</td><td>gauge</td><td>Number of (active + idle) connections in the carbide database pool</td></tr>
<tr><td>carbide_dhcp_dropped_requests_total</td><td>counter</td><td>Number of DHCPv4 packets dropped without a reply, by drop reason.</td></tr>
<tr><td>carbide_dhcp_replies_sent_total</td><td>counter</td><td>Number of DHCPv4 replies sent, by reply message type.</td></tr>
<tr><td>carbide_dhcp_requests_total</td><td>counter</td><td>Number of DHCPv4 packets received and decoded, by DHCP message type.</td></tr>
<tr><td>carbide_dhcp_socket_setup_failures_total</td><td>counter</td><td>Number of DHCP socket setup failures, by operation and next action.</td></tr>
<tr><td>carbide_dhcp_timestamp_file_failures_total</td><td>counter</td><td>Number of DHCP timestamp file failures, by operation</td></tr>
<tr><td>carbide_dhcp_v6_listener_unavailable_total</td><td>counter</td><td>Number of DHCPv6 listeners made unavailable by socket setup failure, by reason.</td></tr>
<tr><td>carbide_dhcp_v6_replies_sent_total</td><td>counter</td><td>Number of DHCPv6 replies sent, by response message type.</td></tr>
<tr><td>carbide_dhcp_v6_requests_dropped_total</td><td>counter</td><td>Number of DHCPv6 requests dropped or refused, by reason.</td></tr>
<tr><td>carbide_dhcp_v6_requests_total</td><td>counter</td><td>Number of DHCPv6 requests received, by DHCP message type.</td></tr>
<tr><td>carbide_dns_negative_cache_hit_count_total</td><td>counter</td><td>Number of negative DNS cache hits, by response code</td></tr>
<tr><td>carbide_dns_negative_cache_miss_count_total</td><td>counter</td><td>Number of negative DNS cache misses, by response code</td></tr>
<tr><td>carbide_dns_queries_total</td><td>counter</td><td>Number of DNS queries received, by query type</td></tr>
<tr><td>carbide_dns_request_duration_milliseconds</td><td>histogram</td><td>Time to process a DNS query, by query type and response code</td></tr>
<tr><td>carbide_dns_responses_total</td><td>counter</td><td>Number of DNS responses sent, by response code</td></tr>
<tr><td>carbide_dpa_monitor_iteration_latency_milliseconds</td><td>histogram</td><td>Time consumed for one monitor iteration</td></tr>
<tr><td>carbide_dpa_monitor_operations_latency_milliseconds</td><td>histogram</td><td>Time consumed for one operation</td></tr>
<tr><td>carbide_dpu_agent_report_total</td><td>counter</td><td>Number of DPU-agent report-loop iterations, by loop and outcome</td></tr>
<tr><td>carbide_dpu_agent_service_restart_attempts_total</td><td>counter</td><td>Number of DPU-agent service restart attempts, by service and result.</td></tr>
<tr><td>carbide_dpu_agent_version_count</td><td>gauge</td><td>Number of DPU agents which have reported a certain version.</td></tr>
<tr><td>carbide_dpu_bmc_service_credential_rotation_results_total</td><td>counter</td><td>Number of persisted DPU BMC service credential rotation results, by result</td></tr>
<tr><td>carbide_dpu_firmware_version_count</td><td>gauge</td><td>Number of DPUs which have reported a certain firmware version.</td></tr>
<tr><td>carbide_dpu_remediation_executor_failures_total</td><td>counter</td><td>Number of DPU remediation executor failures, by failure stage.</td></tr>
<tr><td>carbide_dpu_uefi_password_setup_skips_total</td><td>counter</td><td>Number of DPU UEFI password setup operations skipped, by reason.</td></tr>
<tr><td>carbide_dpus_healthy_count</td><td>gauge</td><td>Number of DPUs in the system that have reported healthy in the last report. Healthy does not imply up - the report from the DPU might be outdated.</td></tr>
<tr><td>carbide_dpus_up_count</td><td>gauge</td><td>Number of DPUs in the system that are up. Up means we have received a health report less than 5 minutes ago.</td></tr>
<tr><td>carbide_dsx_event_bus_publish_count_total</td><td>counter</td><td>Number of MQTT publish attempts</td></tr>
<tr><td>carbide_dsx_exchange_consumer_alerts_detected_total</td><td>counter</td><td>Number of leak alerts detected</td></tr>
<tr><td>carbide_dsx_exchange_consumer_dedup_skipped_total</td><td>counter</td><td>Number of messages skipped due to deduplication</td></tr>
<tr><td>carbide_dsx_exchange_consumer_health_report_persist_failures_total</td><td>counter</td><td>Number of rack health report persist failures against the Carbide API</td></tr>
<tr><td>carbide_dsx_exchange_consumer_message_age_seconds</td><td>histogram</td><td>Age of consumed BMS value messages at processing time (consumer lag), in seconds</td></tr>
<tr><td>carbide_dsx_exchange_consumer_messages_dropped_total</td><td>counter</td><td>Number of messages dropped due to queue overflow</td></tr>
<tr><td>carbide_dsx_exchange_consumer_messages_processed_total</td><td>counter</td><td>Number of messages successfully processed</td></tr>
<tr><td>carbide_dsx_exchange_consumer_messages_received_total</td><td>counter</td><td>Number of MQTT messages received</td></tr>
<tr><td>carbide_endpoint_exploration_duration_milliseconds</td><td>histogram</td><td>The time it took to explore an endpoint</td></tr>
<tr><td>carbide_endpoint_exploration_expected_machines_missing_overall_count</td><td>gauge</td><td>Number of machines expected but not identified</td></tr>
<tr><td>carbide_endpoint_exploration_expected_power_shelves_missing_overall_count</td><td>gauge</td><td>Number of power shelves expected but not identified</td></tr>
<tr><td>carbide_endpoint_exploration_identified_managed_hosts_overall_count</td><td>gauge</td><td>Number of managed hosts identified by expectation</td></tr>
<tr><td>carbide_endpoint_exploration_machines_explored_overall_count</td><td>gauge</td><td>Number of machines explored, by expectation and machine type</td></tr>
<tr><td>carbide_endpoint_exploration_step_latency_milliseconds</td><td>histogram</td><td>The time it took to perform one endpoint exploration step</td></tr>
<tr><td>carbide_endpoint_exploration_success_count</td><td>gauge</td><td>Number of successful endpoint explorations</td></tr>
<tr><td>carbide_endpoint_explorations_count</td><td>gauge</td><td>Number of attempted endpoint explorations</td></tr>
<tr><td>carbide_exhausted_reprovision_retry_count</td><td>gauge</td><td>Number of host machines in the system whose host firmware upgrade retry budget is exhausted.</td></tr>
<tr><td>carbide_extension_service_credential_cleanup_failures_total</td><td>counter</td><td>Number of extension-service credential cleanup failures, by operation.</td></tr>
<tr><td>carbide_extension_services_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_extension_services in the system</td></tr>
<tr><td>carbide_extension_services_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_extension_services</td></tr>
<tr><td>carbide_extension_services_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_extension_services</td></tr>
<tr><td>carbide_extension_services_total</td><td>gauge</td><td>Number of carbide_extension_services in the system</td></tr>
<tr><td>carbide_external_call_duration_milliseconds</td><td>histogram</td><td>Duration of outbound calls by backend, operation, and outcome; the _count series, split by outcome, gives the request and error rates.</td></tr>
<tr><td>carbide_firmware_artifact_unavailable_total</td><td>counter</td><td>Number of firmware artifacts unavailable before download, by reason.</td></tr>
<tr><td>carbide_firmware_download_duration_seconds</td><td>histogram</td><td>Duration of background firmware artifact downloads, by outcome; an ok attempt spans fetch, checksum verification, and publish, and the _count series, split by outcome, is the download and failure rate.</td></tr>
<tr><td>carbide_firmware_update_failures_total</td><td>counter</td><td>Number of firmware update failures, by update target and cause</td></tr>
<tr><td>carbide_firmware_updates_total</td><td>counter</td><td>Number of firmware updates started and completed, by update target and phase; only the host target emits both phases</td></tr>
<tr><td>carbide_fmds_config_updates_total</td><td>counter</td><td>Number of FMDS gRPC config-update ingests, by outcome</td></tr>
<tr><td>carbide_fmds_phone_home_total</td><td>counter</td><td>Number of FMDS tenant phone-home operations, by outcome</td></tr>
<tr><td>carbide_gpus_in_use_count</td><td>gauge</td><td>Number of GPUs actively used by tenants in instances in the NICo deployment</td></tr>
<tr><td>carbide_gpus_total_count</td><td>gauge</td><td>Number of GPUs in the NICo deployment</td></tr>
<tr><td>carbide_gpus_usable_count</td><td>gauge</td><td>Number of remaining GPUs in the NICo deployment available for immediate instance creation</td></tr>
<tr><td>carbide_health_otlp_export_failures_total</td><td>counter</td><td>Number of OTLP export batches dropped after a send failure, by signal and gRPC status code.</td></tr>
<tr><td>carbide_health_otlp_queue_depth</td><td>gauge</td><td>Number of entries waiting in an OTLP queue, by target and signal.</td></tr>
<tr><td>carbide_health_otlp_queue_dropped_total</td><td>counter</td><td>Number of OTLP queue entries dropped because a per-target queue reached capacity, by target and signal.</td></tr>
<tr><td>carbide_health_redfish_sse_event_record_resolution_failures_total</td><td>counter</td><td>Number of Redfish SSE event records dropped after a referenced record could not be resolved, by failure reason.</td></tr>
<tr><td>carbide_health_report_submissions_total</td><td>counter</td><td>Number of health report submissions to the NICo API, by report target and outcome.</td></tr>
<tr><td>carbide_host_reprovision_retries_total</td><td>counter</td><td>Number of times a failed host firmware upgrade was retried during host reprovisioning</td></tr>
<tr><td>carbide_hosts_by_sku_count</td><td>gauge</td><td>Number of hosts by SKU and device type (&#39;unknown&#39; for hosts without SKU)</td></tr>
<tr><td>carbide_hosts_health_overrides_count</td><td>gauge</td><td>Number of health overrides configured in the site</td></tr>
<tr><td>carbide_hosts_health_status_count</td><td>gauge</td><td>Number of managed hosts in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_hosts_in_use_count</td><td>gauge</td><td>Number of hosts actively used by tenants as instances in the NICo deployment</td></tr>
<tr><td>carbide_hosts_unhealthy_by_classification_count</td><td>gauge</td><td>Number of objects marked with a certain classification due to being unhealthy</td></tr>
<tr><td>carbide_hosts_unhealthy_by_probe_id_count</td><td>gauge</td><td>Number of objects which reported a certain Health Probe Alert</td></tr>
<tr><td>carbide_hosts_usable_count</td><td>gauge</td><td>Number of remaining hosts in the NICo deployment available for immediate instance creation</td></tr>
<tr><td>carbide_hosts_with_bios_password_set</td><td>gauge</td><td>Number of hosts in the system that have their BIOS password set.</td></tr>
<tr><td>carbide_ib_monitor_fabrics_count</td><td>gauge</td><td>Number of monitored InfiniBand fabrics</td></tr>
<tr><td>carbide_ib_monitor_iteration_latency_milliseconds</td><td>histogram</td><td>The time it took to perform one IB fabric monitor iteration</td></tr>
<tr><td>carbide_ib_monitor_machine_ib_status_updates_count</td><td>gauge</td><td>Number of Machines whose InfiniBand status observation was updated</td></tr>
<tr><td>carbide_ib_monitor_machines_with_missing_pkeys_count</td><td>gauge</td><td>Number of machines where at least one port is not assigned to the expected pkey on UFM</td></tr>
<tr><td>carbide_ib_monitor_machines_with_unexpected_pkeys_count</td><td>gauge</td><td>Number of machines where at least one port is assigned to an unexpected pkey on UFM</td></tr>
<tr><td>carbide_ib_monitor_machines_with_unknown_pkeys_count</td><td>gauge</td><td>Number of machines where at least one port is assigned to a pkey value not associated with any partition ID</td></tr>
<tr><td>carbide_ib_monitor_partial_failures_total</td><td>counter</td><td>Number of IB fabric monitor partial failures, by failure stage.</td></tr>
<tr><td>carbide_ib_monitor_ufm_changes_applied_total</td><td>counter</td><td>Number of changes performed at UFM</td></tr>
<tr><td>carbide_ib_partition_pkey_missing_total</td><td>counter</td><td>Number of IB partitions missing a pkey, by controller state</td></tr>
<tr><td>carbide_ib_partitions_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_ib_partitions in the system</td></tr>
<tr><td>carbide_ib_partitions_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_ib_partitions</td></tr>
<tr><td>carbide_ib_partitions_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_ib_partitions</td></tr>
<tr><td>carbide_ib_partitions_total</td><td>gauge</td><td>Number of carbide_ib_partitions in the system</td></tr>
<tr><td>carbide_ipmi_commands_total</td><td>counter</td><td>Number of IPMI command executions, by command and outcome.</td></tr>
<tr><td>carbide_kms_token_maintenance_failures_total</td><td>counter</td><td>Number of Transit KMS token maintenance failures, by maintenance stage</td></tr>
<tr><td>carbide_log_events_total</td><td>counter</td><td>Number of log events emitted, by level and component. The always-on log-volume and error-rate signal for every binary.</td></tr>
<tr><td>carbide_machine_dpu_info</td><td>gauge</td><td>Association between a host machine and one of its DPUs. Exposed only on the opt-in per-object endpoint.</td></tr>
<tr><td>carbide_machine_identity_sign_proxy_failures_total</td><td>counter</td><td>Number of machine identity sign proxy transport, local response handling, and upstream server failures, by failure stage.</td></tr>
<tr><td>carbide_machine_identity_stored_secret_decryption_failures_total</td><td>counter</td><td>Number of stored machine identity secret decryption failures, by secret kind.</td></tr>
<tr><td>carbide_machine_identity_token_exchange_failures_total</td><td>counter</td><td>Number of machine identity token exchange failures, by failure stage</td></tr>
<tr><td>carbide_machine_instance_info</td><td>gauge</td><td>Association between a host machine and its tenant instance. Exposed only on the opt-in per-object endpoint.</td></tr>
<tr><td>carbide_machine_reboot_duration_seconds</td><td>histogram</td><td>Time taken for machine/host to reboot in seconds</td></tr>
<tr><td>carbide_machine_updates_started_count</td><td>gauge</td><td>Number of machines in the system in the process of updating</td></tr>
<tr><td>carbide_machine_validation_completed</td><td>gauge</td><td>Number of successfully completed machine validation runs</td></tr>
<tr><td>carbide_machine_validation_failed</td><td>gauge</td><td>Number of failed machine validation runs</td></tr>
<tr><td>carbide_machine_validation_heartbeat_failures_total</td><td>counter</td><td>Number of machine validation heartbeat failures, by stage and reason.</td></tr>
<tr><td>carbide_machine_validation_in_progress</td><td>gauge</td><td>Number of machine validation runs in progress</td></tr>
<tr><td>carbide_machine_validation_oldest_active_age_seconds</td><td>gauge</td><td>Age in seconds of the oldest active machine validation run</td></tr>
<tr><td>carbide_machine_validation_outcomes_total</td><td>counter</td><td>Number of machine validation runs that completed as passed or failed, by outcome and failure cause; runs skipped by a disabled validation config are not counted</td></tr>
<tr><td>carbide_machine_validation_result_persistence_attempts_total</td><td>counter</td><td>Number of machine validation result persistence attempts, by outcome.</td></tr>
<tr><td>carbide_machine_validation_stale_runs_count</td><td>gauge</td><td>Number of active machine validation runs considered stale</td></tr>
<tr><td>carbide_machine_validation_tests</td><td>gauge</td><td>The details of machine validation tests</td></tr>
<tr><td>carbide_machines_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_machines in the system</td></tr>
<tr><td>carbide_machines_handler_latency_in_state_milliseconds</td><td>histogram</td><td>The amount of time it took to invoke the state handler for objects of type carbide_machines in a certain state</td></tr>
<tr><td>carbide_machines_in_maintenance_count</td><td>gauge</td><td>Number of machines in the system in maintenance</td></tr>
<tr><td>carbide_machines_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_completed_total</td><td>counter</td><td>Number of object handling tasks completed for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_dispatched_total</td><td>counter</td><td>Number of object handling tasks dequeued and dispatched for processing for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_errored_total</td><td>counter</td><td>Number of object handling tasks completed with an error for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_object_tasks_requeued_total</td><td>counter</td><td>Number of object handling tasks requeued for objects of type carbide_machines</td></tr>
<tr><td>carbide_machines_per_state</td><td>gauge</td><td>Number of carbide_machines in the system with a given state</td></tr>
<tr><td>carbide_machines_per_state_above_sla</td><td>gauge</td><td>Number of carbide_machines currently in a state longer than the SLA allows</td></tr>
<tr><td>carbide_machines_state_entered_total</td><td>counter</td><td>Number of times objects of type carbide_machines have entered a certain state</td></tr>
<tr><td>carbide_machines_state_exited_total</td><td>counter</td><td>Number of times objects of type carbide_machines have exited a certain state</td></tr>
<tr><td>carbide_machines_time_in_state_seconds</td><td>histogram</td><td>The amount of time objects of type carbide_machines have spent in a certain state</td></tr>
<tr><td>carbide_machines_total</td><td>gauge</td><td>Number of carbide_machines in the system</td></tr>
<tr><td>carbide_machines_with_state_handling_errors_per_state</td><td>gauge</td><td>Number of state-handling errors for carbide_machines in a given state</td></tr>
<tr><td>carbide_managed_loop_iterations_total</td><td>counter</td><td>Number of managed loop iterations, by manager and outcome; the measured boot metrics collector&#39;s iterations are counted by its latency histogram instead</td></tr>
<tr><td>carbide_measured_boot_bundles_total</td><td>gauge</td><td>Number of measured boot bundles.</td></tr>
<tr><td>carbide_measured_boot_collector_iteration_latency_milliseconds</td><td>histogram</td><td>Number of milliseconds a full measured boot metrics collector iteration took, by outcome</td></tr>
<tr><td>carbide_measured_boot_machines_per_bundle_state_total</td><td>gauge</td><td>Number of machines per measured boot bundle state.</td></tr>
<tr><td>carbide_measured_boot_machines_per_machine_state_total</td><td>gauge</td><td>Number of machines per measured boot machine state.</td></tr>
<tr><td>carbide_measured_boot_machines_total</td><td>gauge</td><td>Number of machines reporting measurements.</td></tr>
<tr><td>carbide_measured_boot_profiles_total</td><td>gauge</td><td>Number of measured boot profiles.</td></tr>
<tr><td>carbide_measured_boot_verification_failures_total</td><td>counter</td><td>Number of measured boot verification failures, across quote verification and attestation handling, by cause</td></tr>
<tr><td>carbide_metrics_scrape_failures_total</td><td>counter</td><td>Number of /metrics scrape failures, by outcome.</td></tr>
<tr><td>carbide_mqtt_dispatch_dropped_total</td><td>counter</td><td>Number of received MQTT messages dropped because the handler concurrency semaphore could not be acquired</td></tr>
<tr><td>carbide_mqtt_reconnects_total</td><td>counter</td><td>Number of times an MQTT client re-established its broker connection after the initial connect</td></tr>
<tr><td>carbide_network_segments_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_network_segments in the system</td></tr>
<tr><td>carbide_network_segments_handler_latency_in_state_milliseconds</td><td>histogram</td><td>The amount of time it took to invoke the state handler for objects of type carbide_network_segments in a certain state</td></tr>
<tr><td>carbide_network_segments_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_completed_total</td><td>counter</td><td>Number of object handling tasks completed for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_dispatched_total</td><td>counter</td><td>Number of object handling tasks dequeued and dispatched for processing for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_object_tasks_requeued_total</td><td>counter</td><td>Number of object handling tasks requeued for objects of type carbide_network_segments</td></tr>
<tr><td>carbide_network_segments_per_state</td><td>gauge</td><td>Number of carbide_network_segments in the system with a given state</td></tr>
<tr><td>carbide_network_segments_per_state_above_sla</td><td>gauge</td><td>Number of carbide_network_segments currently in a state longer than the SLA allows</td></tr>
<tr><td>carbide_network_segments_state_entered_total</td><td>counter</td><td>Number of times objects of type carbide_network_segments have entered a certain state</td></tr>
<tr><td>carbide_network_segments_state_exited_total</td><td>counter</td><td>Number of times objects of type carbide_network_segments have exited a certain state</td></tr>
<tr><td>carbide_network_segments_time_in_state_seconds</td><td>histogram</td><td>The amount of time objects of type carbide_network_segments have spent in a certain state</td></tr>
<tr><td>carbide_network_segments_total</td><td>gauge</td><td>Number of carbide_network_segments in the system</td></tr>
<tr><td>carbide_network_segments_with_state_handling_errors_per_state</td><td>gauge</td><td>Number of state-handling errors for carbide_network_segments in a given state</td></tr>
<tr><td>carbide_nvlink_partition_monitor_iteration_latency_milliseconds</td><td>histogram</td><td>Time consumed for one monitor iteration</td></tr>
<tr><td>carbide_nvlink_partition_monitor_machine_status_updates_count</td><td>gauge</td><td>Number of machines whose NVLink status observation was updated</td></tr>
<tr><td>carbide_nvlink_partition_monitor_nmxc_changes_applied_total</td><td>counter</td><td>Number of changes requested to NMX-C</td></tr>
<tr><td>carbide_nvlink_partition_monitor_nmxc_op_latency_milliseconds</td><td>histogram</td><td>Time consumed for one NMX-C operation</td></tr>
<tr><td>carbide_nvlink_partition_monitor_num_logical_partitions</td><td>gauge</td><td>Number of monitored logical partitions</td></tr>
<tr><td>carbide_nvlink_partition_monitor_num_physical_partitions</td><td>gauge</td><td>Number of monitored physical partitions</td></tr>
<tr><td>carbide_nvlink_partition_monitor_nvlink_info_mismatches</td><td>gauge</td><td>Number of NVLink GPU partition ID mismatches between DB and NMX-C</td></tr>
<tr><td>carbide_nvlink_partition_monitor_stale_partitions_deleted</td><td>gauge</td><td>Number of stale partitions deleted from DB (not found in NMX-C)</td></tr>
<tr><td>carbide_nvlink_switch_cert_monitor_iteration_latency_milliseconds</td><td>histogram</td><td>Time consumed for one NMX-C switch certificate monitor iteration</td></tr>
<tr><td>carbide_object_info</td><td>gauge</td><td>Stable identifying traits for a state-controller object. Exposed only on the opt-in per-object endpoint.</td></tr>
<tr><td>carbide_object_manual_intervention_required</td><td>gauge</td><td>Indicates that an object currently requires manual operator intervention. Exposed only on the opt-in per-object endpoint.</td></tr>
<tr><td>carbide_object_state_entered_timestamp_seconds</td><td>gauge</td><td>Unix timestamp at which an object entered its current state. Exposed only on the opt-in per-object endpoint.</td></tr>
<tr><td>carbide_object_state_sla_seconds</td><td>gauge</td><td>Resolved SLA in seconds for an objects current state. Exposed only on the opt-in per-object endpoint.</td></tr>
<tr><td>carbide_pending_dpu_nic_firmware_update_count</td><td>gauge</td><td>Number of machines in the system that need a DPU/NIC firmware update</td></tr>
<tr><td>carbide_pending_host_firmware_update_count</td><td>gauge</td><td>Number of host machines in the system that need a firmware update.</td></tr>
<tr><td>carbide_power_shelves_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_power_shelves in the system</td></tr>
<tr><td>carbide_power_shelves_health_overrides_count</td><td>gauge</td><td>Number of health overrides configured in the site</td></tr>
<tr><td>carbide_power_shelves_health_status_count</td><td>gauge</td><td>Number of power shelves in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_power_shelves_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_power_shelves</td></tr>
<tr><td>carbide_power_shelves_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_power_shelves</td></tr>
<tr><td>carbide_power_shelves_total</td><td>gauge</td><td>Number of carbide_power_shelves in the system</td></tr>
<tr><td>carbide_preingestion_bfb_copy_duration_seconds</td><td>histogram</td><td>Duration of preingestion BFB copies to a DPU rshim, by outcome; the _count series, split by outcome, is the copy and failure rate.</td></tr>
<tr><td>carbide_preingestion_firmware_upgrade_tasks_total</td><td>counter</td><td>Number of preingestion firmware upgrade Redfish tasks reaching a terminal state, by firmware component, final task state, and outcome.</td></tr>
<tr><td>carbide_preingestion_firmware_upload_total</td><td>counter</td><td>Number of preingestion firmware uploads to a BMC, by upload method and outcome.</td></tr>
<tr><td>carbide_preingestion_power_control_total</td><td>counter</td><td>Number of preingestion Redfish power operations (host power control, BMC and chassis resets), by operation and outcome.</td></tr>
<tr><td>carbide_preingestion_total</td><td>gauge</td><td>Number of known machines currently being evaluated prior to ingestion</td></tr>
<tr><td>carbide_preingestion_waiting_download</td><td>gauge</td><td>Number of machines that are waiting for firmware downloads on other machines to complete before doing their own</td></tr>
<tr><td>carbide_preingestion_waiting_installation</td><td>gauge</td><td>Number of machines which have had firmware uploaded to them and are currently in the process of installing that firmware</td></tr>
<tr><td>carbide_pxe_boot_outcomes_total</td><td>counter</td><td>Number of PXE boot-path outcomes served, by endpoint and reason.</td></tr>
<tr><td>carbide_rack_maintenance_access_token_cleanup_failures_total</td><td>counter</td><td>Number of rack maintenance access token cleanup failures</td></tr>
<tr><td>carbide_racks_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_racks in the system</td></tr>
<tr><td>carbide_racks_health_overrides_count</td><td>gauge</td><td>Number of health overrides configured in the site</td></tr>
<tr><td>carbide_racks_health_status_count</td><td>gauge</td><td>Number of racks in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_racks_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_racks</td></tr>
<tr><td>carbide_racks_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_racks</td></tr>
<tr><td>carbide_racks_total</td><td>gauge</td><td>Number of carbide_racks in the system</td></tr>
<tr><td>carbide_reboot_attempts_in_booting_with_discovery_image</td><td>histogram</td><td>Reboot attempts per machine in BootingWithDiscoveryImage, recorded when a machine is rebooted again after no response from the host.</td></tr>
<tr><td>carbide_redfish_action_result_persistence_failures_total</td><td>counter</td><td>Number of detached Redfish action results that failed to persist</td></tr>
<tr><td>carbide_reserved_ips_count</td><td>gauge</td><td>Number of reserved IPs per network segment</td></tr>
<tr><td>carbide_resource_pool_lifecycle_failures_total</td><td>counter</td><td>Number of resource pool lifecycle failures, by operation, failure, failure policy, allocation mode, and value type.</td></tr>
<tr><td>carbide_resourcepool_free_count</td><td>gauge</td><td>Number of values in the resource pool currently available for allocation</td></tr>
<tr><td>carbide_resourcepool_used_count</td><td>gauge</td><td>Number of currently allocated values in the resource pool</td></tr>
<tr><td>carbide_rms_switch_certificate_unrecognized_job_states_total</td><td>counter</td><td>Number of unrecognized RMS switch certificate job states.</td></tr>
<tr><td>carbide_running_dpu_updates_count</td><td>gauge</td><td>Number of machines in the system that are currently running a DPU/NIC firmware update</td></tr>
<tr><td>carbide_scout_actions_total</td><td>counter</td><td>Number of scout control-loop actions handled, by action and outcome.</td></tr>
<tr><td>carbide_scout_firmware_download_attempt_failures_total</td><td>counter</td><td>Number of failed Scout firmware download attempts, by download kind and next action.</td></tr>
<tr><td>carbide_scout_mlx_failures_total</td><td>counter</td><td>Number of Scout MLX observation, read, mutation, and recovery failures, by operation and failure stage.</td></tr>
<tr><td>carbide_scout_pci_evaluations_total</td><td>counter</td><td>Number of comparisons between scout PCI address ordering and stored boot interface selections, by result.</td></tr>
<tr><td>carbide_scout_storage_device_cleanup_duration_seconds</td><td>histogram</td><td>Duration of per-device scout storage cleanup operations, by device type and outcome.</td></tr>
<tr><td>carbide_scout_stream_connections_total</td><td>counter</td><td>Number of scout stream connection attempts, by outcome.</td></tr>
<tr><td>carbide_scout_stream_reconnects_total</td><td>counter</td><td>Number of scout stream reconnect cycles after a stream closed or errored.</td></tr>
<tr><td>carbide_scout_stream_responses_dropped_total</td><td>counter</td><td>Number of scout stream responses dropped after the outbound request stream closed.</td></tr>
<tr><td>carbide_site_exploration_expected_machines_sku_count</td><td>gauge</td><td>Number of expected machines by SKU ID and device type</td></tr>
<tr><td>carbide_site_exploration_identified_managed_hosts_count</td><td>gauge</td><td>Number of Host+DPU pairs identified in the last SiteExplorer run</td></tr>
<tr><td>carbide_site_explorer_bmc_password_rotations_total</td><td>counter</td><td>Number of BMC root password rotations onto the site-wide credential, by outcome</td></tr>
<tr><td>carbide_site_explorer_bmc_reset_attempts_total</td><td>counter</td><td>Number of Site Explorer BMC reset attempts, by method and status.</td></tr>
<tr><td>carbide_site_explorer_bmc_reset_count</td><td>gauge</td><td>Number of successful BMC resets in the last SiteExplorer run</td></tr>
<tr><td>carbide_site_explorer_bmc_reset_timestamp_persistence_failures_total</td><td>counter</td><td>Number of Site Explorer BMC reset timestamp persistence failures, by method.</td></tr>
<tr><td>carbide_site_explorer_boot_interface_selections_total</td><td>counter</td><td>Number of successful Site Explorer boot interface selections for hosts with more than one explored DPU and effective DPU policy Manage, by selection mechanism.</td></tr>
<tr><td>carbide_site_explorer_create_machines</td><td>gauge</td><td>Whether site-explorer machine creation is enabled (1) or disabled (0)</td></tr>
<tr><td>carbide_site_explorer_create_machines_latency_milliseconds</td><td>histogram</td><td>The time it took to perform create_machines inside site-explorer</td></tr>
<tr><td>carbide_site_explorer_created_machines_count</td><td>gauge</td><td>Number of machine pairs created by Site Explorer after identification</td></tr>
<tr><td>carbide_site_explorer_created_power_shelves_count</td><td>gauge</td><td>Number of power shelves created by Site Explorer after identification</td></tr>
<tr><td>carbide_site_explorer_dpu_migration_signals_count</td><td>gauge</td><td>Number of DPU NIC-mode migration signals by signal type -- mode-mismatch found, set_nic_mode issued, reset requested, and zero-DPU registered for a host whose DPU policy is nic.</td></tr>
<tr><td>carbide_site_explorer_enabled</td><td>gauge</td><td>Whether site-explorer is enabled (1) or paused (0)</td></tr>
<tr><td>carbide_site_explorer_iteration_latency_milliseconds</td><td>histogram</td><td>The time it took to perform one site explorer iteration</td></tr>
<tr><td>carbide_site_explorer_last_run_status</td><td>gauge</td><td>The status of the latest Site Explorer run</td></tr>
<tr><td>carbide_site_explorer_machine_slot_tray_enrichment_failures_total</td><td>counter</td><td>Number of Site Explorer machine slot and tray enrichment failures, by failure stage.</td></tr>
<tr><td>carbide_site_explorer_phase_latency_milliseconds</td><td>histogram</td><td>The time it took to perform one site explorer iteration phase</td></tr>
<tr><td>carbide_site_explorer_update_explored_endpoints_count</td><td>gauge</td><td>Counts from the last update_explored_endpoints phase by kind</td></tr>
<tr><td>carbide_site_prefix_admission_total</td><td>counter</td><td>Number of completed tenant SitePrefix admission attempts, by result.</td></tr>
<tr><td>carbide_site_prefix_retirements_total</td><td>counter</td><td>Number of tenant SitePrefix retirements, by previous lifecycle state.</td></tr>
<tr><td>carbide_spdm_evidence_collection_unexpected_task_states_total</td><td>counter</td><td>Number of unexpected SPDM evidence collection task states, by task state and next action.</td></tr>
<tr><td>carbide_state_handler_wakeup_failures_total</td><td>counter</td><td>Number of times a machine&#39;s state handler could not be woken after an observed or desired state change</td></tr>
<tr><td>carbide_static_address_assignments_total</td><td>counter</td><td>Number of static address assignment attempts, by outcome.</td></tr>
<tr><td>carbide_static_address_preallocations_total</td><td>counter</td><td>Number of static address preallocation outcomes recorded, by outcome; successful outcomes are counted only after commit.</td></tr>
<tr><td>carbide_static_address_removals_total</td><td>counter</td><td>Number of static address removal attempts, by outcome.</td></tr>
<tr><td>carbide_static_credential_watcher_failures_total</td><td>counter</td><td>Number of static credential watcher failures, by operation.</td></tr>
<tr><td>carbide_switch_slot_tray_enrichment_failures_total</td><td>counter</td><td>Number of switch slot and tray enrichment failures, by failure stage.</td></tr>
<tr><td>carbide_switches_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_switches in the system</td></tr>
<tr><td>carbide_switches_health_overrides_count</td><td>gauge</td><td>Number of health overrides configured in the site</td></tr>
<tr><td>carbide_switches_health_status_count</td><td>gauge</td><td>Number of switches in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts</td></tr>
<tr><td>carbide_switches_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_switches</td></tr>
<tr><td>carbide_switches_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_switches</td></tr>
<tr><td>carbide_switches_total</td><td>gauge</td><td>Number of carbide_switches in the system</td></tr>
<tr><td>carbide_total_ips_count</td><td>gauge</td><td>Number of IPs per network segment</td></tr>
<tr><td>carbide_unavailable_dpu_nic_firmware_update_count</td><td>gauge</td><td>Number of machines in the system that need a DPU/NIC firmware update but are unavailable for update</td></tr>
<tr><td>carbide_vpc_prefixes_enqueuer_iteration_latency_milliseconds</td><td>histogram</td><td>The overall time it took to enqueue state handling tasks for all carbide_vpc_prefixes in the system</td></tr>
<tr><td>carbide_vpc_prefixes_handler_latency_in_state_milliseconds</td><td>histogram</td><td>The amount of time it took to invoke the state handler for objects of type carbide_vpc_prefixes in a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_iteration_latency_milliseconds</td><td>histogram</td><td>The elapsed time in the last state processor iteration to handle objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_completed_total</td><td>counter</td><td>Number of object handling tasks completed for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_dispatched_total</td><td>counter</td><td>Number of object handling tasks dequeued and dispatched for processing for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_enqueued_total</td><td>counter</td><td>Number of object handling tasks freshly enqueued for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_object_tasks_requeued_total</td><td>counter</td><td>Number of object handling tasks requeued for objects of type carbide_vpc_prefixes</td></tr>
<tr><td>carbide_vpc_prefixes_per_state</td><td>gauge</td><td>Number of carbide_vpc_prefixes in the system with a given state</td></tr>
<tr><td>carbide_vpc_prefixes_per_state_above_sla</td><td>gauge</td><td>Number of carbide_vpc_prefixes currently in a state longer than the SLA allows</td></tr>
<tr><td>carbide_vpc_prefixes_state_entered_total</td><td>counter</td><td>Number of times objects of type carbide_vpc_prefixes have entered a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_state_exited_total</td><td>counter</td><td>Number of times objects of type carbide_vpc_prefixes have exited a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_time_in_state_seconds</td><td>histogram</td><td>The amount of time objects of type carbide_vpc_prefixes have spent in a certain state</td></tr>
<tr><td>carbide_vpc_prefixes_total</td><td>gauge</td><td>Number of carbide_vpc_prefixes in the system</td></tr>
<tr><td>carbide_vpc_prefixes_with_state_handling_errors_per_state</td><td>gauge</td><td>Number of state-handling errors for carbide_vpc_prefixes in a given state</td></tr>
<tr><td>carbide_work_lock_failures_total</td><td>counter</td><td>Number of work-lock lifecycle failures, by operation and failure kind.</td></tr>
<tr><td>site_explorer_create_power_shelves_latency_seconds</td><td>histogram</td><td>Duration of power shelf creation</td></tr>
<tr><td>site_explorer_create_switches_latency_seconds</td><td>histogram</td><td>Duration of switch creation</td></tr>
</table>
