# Changelog

This file is the historical record for independent **NVIDIA Infra Controller REST**
releases through v1.6.0. Future release information is maintained only in the
[unified NICo changelog](https://docs.nvidia.com/infra-controller/changelog).

Each release lists pull requests grouped by category, with the most recent version
first. Entries within each category preserve their published order.

---

## [v1.6.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.6.0)

<Note>
This release is compatible with Core **v0.10.x**.
</Note>

<Warning>
This is the last independent release of NICo REST. Future releases are documented
in the [unified NICo changelog](https://docs.nvidia.com/infra-controller/changelog).
</Warning>

### Features

- **Add support for online repair of Machines, allowing repair without Instance deletion** ([#415](https://github.com/NVIDIA/infra-controller/rest-api/pull/415))
  Introduces Machine online repair, enabling operators to perform maintenance on machines without first deleting the associated Instance. The repair workflow transitions the machine through a maintenance state and back, preserving the instance assignment throughout. See [Machine](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/machine) endpoints.

- **Support IP Usage stats in VPC Prefix and Subnet** ([#480](https://github.com/NVIDIA/infra-controller/rest-api/pull/480))
  VPC Prefix and Subnet responses now include IP usage statistics showing total, used, and available IP addresses, giving operators visibility into address pool utilization without external IPAM queries. See the updated response fields on [VPC Prefix](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/vpc-prefix) and [Subnet](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/subnet) endpoints.

- **Add Flow task-list endpoints and root /task endpoints** ([#543](https://github.com/NVIDIA/infra-controller/rest-api/pull/543))
  Exposes new REST API endpoints for listing and querying Flow (formerly RLA) tasks, including a root `/task` endpoint that provides a unified view of all task types. See the [Task](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/task) section in the API schema.

- **Add tray-level firmware update target selection in Tray FW Update API** ([#541](https://github.com/NVIDIA/infra-controller/rest-api/pull/541))
  The Tray firmware update endpoint now supports selecting specific firmware update targets at the tray level, allowing operators to target individual components within a tray rather than updating all at once. See the updated [Tray](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/tray) firmware update endpoint.

- **Support filter by location info in Flow Tray API** ([#545](https://github.com/NVIDIA/infra-controller/rest-api/pull/545))
  Adds location-based filtering to the Flow Tray API, enabling queries by physical rack position and other location metadata. See the updated query parameters on the [Tray](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/tray) endpoints.

- **Support filter ListTasks by component_id in Flow** ([#542](https://github.com/NVIDIA/infra-controller/rest-api/pull/542))
  Adds a `component_id` filter to the Flow task list endpoint, enabling operators to retrieve all tasks associated with a specific hardware component.

- **Add switch leak detection and powering off switches on detection** ([#566](https://github.com/NVIDIA/infra-controller/rest-api/pull/566))
  Extends the leak detection subsystem to NVLink Switches. When a coolant leak is detected on a switch, the system automatically triggers a power-off operation through the Flow task manager.

- **Add component manager capability aware task dispatch** ([#554](https://github.com/NVIDIA/infra-controller/rest-api/pull/554))
  Flow task dispatch now checks component manager capabilities before scheduling operations, ensuring tasks are only dispatched to managers that support the requested operation type.

- **Add component manager capability metadata** ([#547](https://github.com/NVIDIA/infra-controller/rest-api/pull/547))
  Introduces structured capability metadata for component managers, enabling the system to discover and advertise what operations each manager supports.

- **Allow Flow AddComponent without a rack assignment** ([#514](https://github.com/NVIDIA/infra-controller/rest-api/pull/514))
  Components can now be added to the Flow inventory without requiring an immediate rack assignment, supporting scenarios where hardware is staged before being placed into a rack.

- **Add rack and tray lifecycle and task commands to TUI** ([#535](https://github.com/NVIDIA/infra-controller/rest-api/pull/535))
  Adds interactive TUI commands for rack and tray operations including power control, firmware update, bringup, validation, and task listing, bringing rack-level administration into the interactive CLI experience.

- **Add TUI parity commands and surface machine blocking alerts** ([#534](https://github.com/NVIDIA/infra-controller/rest-api/pull/534))
  Extends the TUI with additional commands for feature parity with the auto-generated CLI, and surfaces machine blocking alerts (health issues preventing provisioning) directly in machine list output.

- **Improve TUI forms for instance create, instance update, and ssh-key-group create** ([#517](https://github.com/NVIDIA/infra-controller/rest-api/pull/517))
  Enhances the interactive instance and SSH key group creation flows with better form layouts, field validation, and guided selection for complex fields.

- **Package nicocli in nico-rest-api image** ([#522](https://github.com/NVIDIA/infra-controller/rest-api/pull/522))
  The `nicocli` binary is now included in the `nico-rest-api` Docker image, enabling operators to run CLI commands directly from the API container for debugging and administration.

- **Add --api-name flag and OIDC username/password env vars** ([#576](https://github.com/NVIDIA/infra-controller/rest-api/pull/576))
  Adds a `--api-name` CLI flag for overriding the API path segment and supports `NICO_OIDC_USERNAME`/`NICO_OIDC_PASSWORD` environment variables for non-interactive OIDC authentication.

- **Support env var overrides for every CLI config field** ([#575](https://github.com/NVIDIA/infra-controller/rest-api/pull/575))
  Every field in the CLI configuration file can now be overridden via environment variables using a `NICO_` prefix convention, enabling containerized and CI/CD-friendly deployments without config file management.

### Bug Fixes

- **Create status detail for Instance when online repair is enabled/disabled for Machine** ([#573](https://github.com/NVIDIA/infra-controller/rest-api/pull/573))
  Adds Instance status detail entries when a Machine enters or exits online repair, providing clear audit trail visibility into repair-driven state changes. Also updates Instance status checks to verify health override alerts before removal and aligns mode/source info with Core conventions.

- **Delete InfiniBand Interfaces based on config sync flag instead of Instance status** ([#567](https://github.com/NVIDIA/infra-controller/rest-api/pull/567))
  Fixes InfiniBand Interface cleanup to use the Site's configuration sync flag rather than Instance status, ensuring interfaces are properly deleted when the Site reports a completed configuration sync regardless of instance state.

- **Add create verb for forge.nvidia.io/sites in site-manager RBAC** ([#548](https://github.com/NVIDIA/infra-controller/rest-api/pull/548))
  Adds the missing `create` verb to the site-manager's RBAC ClusterRole for the `forge.nvidia.io/sites` resource, fixing permission errors when the site-manager attempts to register a new site.

- **Run nico-rest DB migrations as Helm hooks** ([#546](https://github.com/NVIDIA/infra-controller/rest-api/pull/546))
  Converts database migration jobs from regular Kubernetes resources to Helm pre-install/pre-upgrade hooks, ensuring migrations run before the API and workflow services start and preventing race conditions during upgrades.

- **Ensure DPU Extension Service deployments are deleted on Instance deletion** ([#536](https://github.com/NVIDIA/infra-controller/rest-api/pull/536))
  Fixes Instance deletion to properly cascade and remove all associated DPU Extension Service deployments, preventing orphaned deployment records.

- **Fix debug flag for nicocli** ([#532](https://github.com/NVIDIA/infra-controller/rest-api/pull/532))
  Corrects the `--debug` flag handling in nicocli so that debug-level logging is properly enabled when the flag is set.

### Refactoring

- **Complete WithTx migration for remaining API handlers** ([#475](https://github.com/NVIDIA/infra-controller/rest-api/pull/475), [#479](https://github.com/NVIDIA/infra-controller/rest-api/pull/479), [#496](https://github.com/NVIDIA/infra-controller/rest-api/pull/496), [#499](https://github.com/NVIDIA/infra-controller/rest-api/pull/499), [#557](https://github.com/NVIDIA/infra-controller/rest-api/pull/557), [#558](https://github.com/NVIDIA/infra-controller/rest-api/pull/558), [#559](https://github.com/NVIDIA/infra-controller/rest-api/pull/559), [#560](https://github.com/NVIDIA/infra-controller/rest-api/pull/560), [#562](https://github.com/NVIDIA/infra-controller/rest-api/pull/562), [#563](https://github.com/NVIDIA/infra-controller/rest-api/pull/563), [#564](https://github.com/NVIDIA/infra-controller/rest-api/pull/564), [#565](https://github.com/NVIDIA/infra-controller/rest-api/pull/565))
  Completes the `WithTx` transaction helper migration for all remaining API handlers: Machine, Allocation, Instance Type, SSH Key Group, ExpectedMachine, Machine InstanceType, Tenant, Tenant Account, Site, Subnet, Machine online-repair, and VPC Peering. All API write operations now use consistent closure-based transaction scoping with automatic rollback on error.

- **Split component manager operation interfaces and registry wiring** ([#571](https://github.com/NVIDIA/infra-controller/rest-api/pull/571), [#530](https://github.com/NVIDIA/infra-controller/rest-api/pull/530))
  Decomposes the monolithic component manager interface into granular per-operation interfaces and separates registry wiring from business logic, improving testability and enabling capability-aware dispatch.

- **Improve readability for terminology and NVSwitch naming in Flow** ([#544](https://github.com/NVIDIA/infra-controller/rest-api/pull/544))
  Standardizes variable naming and terminology across the Flow service, aligning NVSwitch references with the official product naming convention. See the updated naming in the [API reference](https://docs.nvidia.com/infra-controller/rest-api-reference/getting-started).

- **Apply layered ToProto convention to MachineInstanceType and VPC handlers** ([#533](https://github.com/NVIDIA/infra-controller/rest-api/pull/533), [#505](https://github.com/NVIDIA/infra-controller/rest-api/pull/505))
  Extends the layered proto conversion pattern to MachineInstanceType and VPC handlers, moving protobuf request construction onto DB models for consistent separation between API and data layers.

### Documentation

- **Add Core compatibility matrix, image rename and repo migration notice** ([#572](https://github.com/NVIDIA/infra-controller/rest-api/pull/572))
  Documents the compatibility matrix between NICo REST and Core versions, announces the Docker image rename from `carbide-rest-*` to `nico-rest-*`, and adds a notice regarding the REST repository migration from [https://github.com/NVIDIA/infra-controller/rest-api](https://github.com/NVIDIA/infra-controller/rest-api) to [https://github.com/NVIDIA/infra-controller/tree/main/rest-api](https://github.com/NVIDIA/infra-controller/tree/main/rest-api).

- **Add DB transaction handling guidance to AGENTS.md** ([#551](https://github.com/NVIDIA/infra-controller/rest-api/pull/551))
  Adds comprehensive guidance for AI coding agents on the `WithTx` transaction helper pattern, including when to use each variant and best practices for closure scoping.

### Chores

- **Allow privileged Tenants to retrieve Machines without specifying Site ID** ([#568](https://github.com/NVIDIA/infra-controller/rest-api/pull/568))
  Privileged Tenants (those with targeted Instance creation capability) can now list Machines across all accessible sites without providing a Site ID filter, simplifying cross-site capacity queries.

- **Maintain Forge gRPC service name until Core proto is updated** ([#528](https://github.com/NVIDIA/infra-controller/rest-api/pull/528), [#552](https://github.com/NVIDIA/infra-controller/rest-api/pull/552))
  Preserves the legacy Forge gRPC service name in Site Agent and Flow to maintain compatibility with Core until the corresponding rename is completed on the Core side. *(#528 also in v1.5.1)*

- **Rename default Flow config path rlaconfig.yaml to flowconfig.yaml** ([#527](https://github.com/NVIDIA/infra-controller/rest-api/pull/527))
  Completes the RLA-to-Flow config file path rename. *(Also in v1.5.1)*

- **Finish RLA to Flow rename in site-agent default and handler tests** ([#531](https://github.com/NVIDIA/infra-controller/rest-api/pull/531))
  Updates remaining test files in the site-agent module to use Flow naming.

- **Add logic to retry Core/Flow gRPC connection** ([#549](https://github.com/NVIDIA/infra-controller/rest-api/pull/549))
  Adds retry logic with backoff for establishing gRPC connections to Core and Flow, improving resilience during startup when dependent services may not yet be available.

- **Revise Core and Flow gRPC client and config structures** ([#539](https://github.com/NVIDIA/infra-controller/rest-api/pull/539))
  Restructures the gRPC client configuration for Core and Flow services, separating connection and authentication concerns for clearer configuration management.

- **Apply the ToProto modeling convention to InstanceType** ([#540](https://github.com/NVIDIA/infra-controller/rest-api/pull/540))
  Adds `ToProto` receiver methods to the InstanceType DB model, following the established convention for proto conversion.

- **Remove Git LFS Tracking** ([#550](https://github.com/NVIDIA/infra-controller/rest-api/pull/550))
  Removes Git LFS tracking configuration from the repository, simplifying the clone and build process.

---

## [v1.5.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.5.0)

<Note>
This release is compatible with Core **v0.9.x**.
</Note>

<Warning>
In our effort to unify the product name, starting from `v1.5.0` the image names
are now prefixed with `nico-` instead of `carbide-`. Docker images produced by
the make commands will now have the `nico-` prefix.
</Warning>

### Features
- **Rename carbide/forge to NVIDIA Infrastructure Controller (NICo)** ([#432](https://github.com/NVIDIA/infra-controller/rest-api/pull/432))
  Comprehensive rebranding of the project from Carbide/Forge to NVIDIA Infrastructure Controller (NICo). All API path segments, CLI binary names, Helm chart names, configuration keys, and documentation are updated. The OpenAPI schema now uses the NICo naming throughout — see the updated [API reference](https://docs.nvidia.com/infra-controller/rest-api-reference/getting-started).

- **Add API model and CRUD endpoints for ExpectedRack** ([#444](https://github.com/NVIDIA/infra-controller/rest-api/pull/444))
  Introduces full CRUD REST API endpoints for managing Expected Rack inventory, enabling operators to define expected rack configurations before physical deployment. See [Expected Rack](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/expected-rack) endpoints in the API schema.

- **Allow updating Site capabilities using Site update API endpoint** ([#470](https://github.com/NVIDIA/infra-controller/rest-api/pull/470))
  Site capabilities (native networking, network security groups, NVLink partitioning, rack-level administration) can now be modified via the existing Site update endpoint, removing the need to delete and recreate sites for configuration changes. See the updated `config` field on [Update Site](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/site/update-site).

- **Add a REST endpoint to cancel a task in Flow (formerly RLA)** ([#489](https://github.com/NVIDIA/infra-controller/rest-api/pull/489))
  Exposes a new endpoint for cancelling in-progress rack-level tasks (firmware upgrades, power operations, etc.), giving operators the ability to abort long-running operations. See the [Task](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/task) endpoints.

- **Support firmware updates to unregistered devices in NSM and PSM** ([#442](https://github.com/NVIDIA/infra-controller/rest-api/pull/442))
  NVSwitch Manager and PowerShelf Manager can now perform firmware updates on devices that have not been formally registered, streamlining initial rack provisioning when firmware must be updated before registration.

- **Add auth script support to CLI** ([#464](https://github.com/NVIDIA/infra-controller/rest-api/pull/464))
  The CLI now supports an `authScript` configuration option that executes a user-defined script to obtain authentication tokens, enabling integration with custom identity providers and credential management systems.

### Bug Fixes

- **Preserve newline before apiVersion in nico-rest-api configmap** ([#509](https://github.com/NVIDIA/infra-controller/rest-api/pull/509))
  Fixes a Helm chart rendering issue where a missing newline before `apiVersion` in the ConfigMap caused the API server to fail parsing its configuration on startup.

- **Clean up related components on Site deletion** ([#490](https://github.com/NVIDIA/infra-controller/rest-api/pull/490))
  Site deletion now properly cascades to clean up all related components (Expected Machines, Expected Switches, Expected PowerShelves, Expected Racks, and other site-scoped resources), preventing orphaned records.

- **Resolve nicocli command alias collisions deterministically** ([#519](https://github.com/NVIDIA/infra-controller/rest-api/pull/519))
  Fixes non-deterministic CLI command alias resolution that could cause different commands to be selected on different runs when multiple commands shared the same alias prefix.

- **Avoid unsafe quoting in rack operation report JSON fallback** ([#521](https://github.com/NVIDIA/infra-controller/rest-api/pull/521))
  Fixes potential JSON injection in the rack operation report fallback path by using proper marshaling instead of string interpolation.

- **Require at least one filter for ExpectedRack DeleteAll** ([#515](https://github.com/NVIDIA/infra-controller/rest-api/pull/515))
  Prevents accidental mass deletion of all Expected Racks by requiring at least one filter parameter on the bulk delete endpoint.

- **Verify if Interfaces exist for deletion of NVLink and InfiniBand Partition** ([#503](https://github.com/NVIDIA/infra-controller/rest-api/pull/503))
  Blocks partition deletion when active Instance Interfaces are still connected, preventing orphaned interface references. *(Also in v1.4.3)*

- **VPC Peering create: check authorization before duplicate** ([#506](https://github.com/NVIDIA/infra-controller/rest-api/pull/506))
  Moves authorization checks before duplicate detection in VPC Peering creation, preventing information leakage where unauthorized tenants could discover the existence of peerings via 409 responses. *(Also in v1.4.3)*

- **Add API routes for Expected Machine batch handlers** ([#465](https://github.com/NVIDIA/infra-controller/rest-api/pull/465))
  Restores missing API route registrations for Expected Machine batch create and batch update endpoints that were dropped during the NICo rebrand.

- **Reject unknown --output values instead of silently using JSON** ([#518](https://github.com/NVIDIA/infra-controller/rest-api/pull/518))
  The CLI now validates the `--output` flag and returns an error for unrecognized format values instead of silently falling back to JSON, preventing unexpected behavior in scripted workflows.

- **Clear sticky error in DynTLSCfg.refresh() after success** ([#502](https://github.com/NVIDIA/infra-controller/rest-api/pull/502))
  Fixes a bug where the dynamic TLS certificate loader would retain a previous error state even after a successful certificate refresh, causing services to report unhealthy TLS status.

- **No-op Instance update when requested NVLink Interfaces match current state** ([#438](https://github.com/NVIDIA/infra-controller/rest-api/pull/438))
  Prevents unnecessary Instance updates to the Site Controller when the requested NVLink Interface configuration is identical to the current state, avoiding spurious status transitions. See [Update Instance](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/instance/update-instance).

- **Mark nullable fields correctly in OpenAPI spec** ([#440](https://github.com/NVIDIA/infra-controller/rest-api/pull/440))
  Adds `nullable: true` annotations to multiple fields across the OpenAPI schema that can legitimately be null in API responses, fixing strict SDK client deserialization failures. See the updated field definitions across the [API reference](https://docs.nvidia.com/infra-controller/rest-api-reference/getting-started).

- **Verify if Site has FNN enabled when updating VPC to FNN** ([#466](https://github.com/NVIDIA/infra-controller/rest-api/pull/466))
  Validates that the target Site supports Fabric Native Networking before allowing a VPC virtualization type update to FNN. *(Also in v1.4.3)*

- **Create VPC in Provisioning state** ([#430](https://github.com/NVIDIA/infra-controller/rest-api/pull/430))
  Fixes VPC creation to start in `Provisioning` status instead of immediately reporting as `Ready`, accurately reflecting that the VPC configuration must be synced to the Site before it is operational.

- **Normalize blank search queries across API and DAO layers** ([#429](https://github.com/NVIDIA/infra-controller/rest-api/pull/429))
  Trims and normalizes whitespace-only search query parameters across all endpoints, preventing empty queries from reaching PostgreSQL's `to_tsquery` and causing 500 errors.

- **Introduce a GetRLAClient() helper** ([#461](https://github.com/NVIDIA/infra-controller/rest-api/pull/461))
  Adds a nil-safe helper for obtaining the RLA/Flow gRPC client, preventing panics when the client is not configured.

- **Harden component manager lookup in RLA** ([#435](https://github.com/NVIDIA/infra-controller/rest-api/pull/435))
  Adds defensive checks in the component manager lookup path to prevent nil pointer panics when component types are not registered.

- **Check VPC Prefix existence before allowing Allocation Constraint update** ([#454](https://github.com/NVIDIA/infra-controller/rest-api/pull/454))
  Blocks Allocation Constraint modifications when VPC Prefixes derived from the allocation already exist. *(Also in v1.4.3)*

- **Check for nil on carbideClient in expected PowerShelf/Switch site activities** ([#458](https://github.com/NVIDIA/infra-controller/rest-api/pull/458))
  Adds nil checks for the Carbide client in Expected PowerShelf and Expected Switch site activities, preventing panics when the client is not initialized.

- **Remove additional VPC Prefix validation logic in Site Workflow** ([#439](https://github.com/NVIDIA/infra-controller/rest-api/pull/439))
  Consolidates VPC Prefix validation into the API and gRPC handlers, removing the redundant workflow-layer validation. *(Also in v1.4.2)*

- **Add Apache source headers** ([#457](https://github.com/NVIDIA/infra-controller/rest-api/pull/457))
  Adds missing Apache 2.0 license headers to source files that were lacking them.

- **Pin grpc-go to v1.79.3 for CVE-2026-33186 authz bypass** ([#485](https://github.com/NVIDIA/infra-controller/rest-api/pull/485))
  Pins grpc-go to v1.79.3 to address CVE-2026-33186, a high-severity authorization bypass vulnerability in the gRPC Go framework.

- **Bump pgx to v5.9.0 and mongo-driver to v1.17.7 for high-severity CVEs** ([#484](https://github.com/NVIDIA/infra-controller/rest-api/pull/484))
  Updates pgx and mongo-driver to patched versions addressing high-severity CVEs in database driver dependencies.

- **Drop docker/docker indirect via testcontainers-go v0.42.0 bump** ([#486](https://github.com/NVIDIA/infra-controller/rest-api/pull/486))
  Bumps testcontainers-go to v0.42.0 to eliminate the docker/docker indirect dependency and its associated vulnerability surface.

### Refactoring

- **Migrate API handlers to WithTx transaction helper** ([#462](https://github.com/NVIDIA/infra-controller/rest-api/pull/462), [#472](https://github.com/NVIDIA/infra-controller/rest-api/pull/472), [#473](https://github.com/NVIDIA/infra-controller/rest-api/pull/473), [#474](https://github.com/NVIDIA/infra-controller/rest-api/pull/474), [#476](https://github.com/NVIDIA/infra-controller/rest-api/pull/476), [#478](https://github.com/NVIDIA/infra-controller/rest-api/pull/478), [#471](https://github.com/NVIDIA/infra-controller/rest-api/pull/471), [#494](https://github.com/NVIDIA/infra-controller/rest-api/pull/494), [#497](https://github.com/NVIDIA/infra-controller/rest-api/pull/497), [#498](https://github.com/NVIDIA/infra-controller/rest-api/pull/498))
  Systematically migrates all API handlers to use the new `WithTx` database transaction helper, replacing manual `Begin`/`Commit`/`Rollback` patterns. Covered handlers: Expected Machine/PowerShelf/Switch, VPC, SSH Key/Group, IP Block, Instance, Network Security Group, Operating System, NVLink Logical Partition, InfiniBand Partition, and VPC Prefix. This ensures consistent transaction scoping and automatic rollback on error across the entire API surface.

- **Add component manager descriptor catalog** ([#523](https://github.com/NVIDIA/infra-controller/rest-api/pull/523))
  Introduces a centralized descriptor catalog for component managers in the Flow service, enabling type-safe component registration and discovery.

- **Streamline component manager provider bootstrap** ([#510](https://github.com/NVIDIA/infra-controller/rest-api/pull/510))
  Simplifies the component manager provider initialization by extracting a common bootstrap pattern, reducing boilerplate when adding new provider implementations.

- **Improve provider config pluggability in RLA** ([#441](https://github.com/NVIDIA/infra-controller/rest-api/pull/441))
  Refactors the Flow (formerly RLA) provider configuration to support pluggable backends, making it easier to add new component manager providers without modifying core initialization code.

- **Add defensive nil check in GetForgeClient** ([#463](https://github.com/NVIDIA/infra-controller/rest-api/pull/463))
  Adds a nil-safety check when retrieving the Forge/Core gRPC client, preventing panics in configurations where the client is not available. Renamed to `GetNICoClient` in [rebranding PR](https://github.com/NVIDIA/infra-controller/rest-api/pull/432).

- **Introduce a CarbideAtomicClient.Forge() helper** ([#460](https://github.com/NVIDIA/infra-controller/rest-api/pull/460))
  Adds a convenience accessor for the Forge/Core client on the atomic client wrapper, reducing indirection in handler code. Renamed to `NICoAtomicClient.NICo()` in [rebranding PR](https://github.com/NVIDIA/infra-controller/rest-api/pull/432).

### Documentation

- **Improve Allocation guidance, fix Machine ID format in OpenAPI spec** ([#488](https://github.com/NVIDIA/infra-controller/rest-api/pull/488))
  Adds guidance on Allocation lifecycle and constraint management to the OpenAPI schema, and corrects the Machine ID format from integer to UUID. See the updated [Allocation](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/allocation) documentation.

- **Fix nicocli help text and clean up CLI README** ([#495](https://github.com/NVIDIA/infra-controller/rest-api/pull/495))
  Updates CLI help text and README to reflect the NICo rebranding, corrects command examples, and removes outdated references.

- **Update stale references for repo name/URL** ([#447](https://github.com/NVIDIA/infra-controller/rest-api/pull/447))
  Updates documentation links and repository references across the codebase to reflect the current repository name and URL.

### CI/CD

- **Remove unused promotion PAT secret** ([#492](https://github.com/NVIDIA/infra-controller/rest-api/pull/492))
  Removes an unused Personal Access Token secret reference from the CI promotion workflow.

### Chores

- **Rebrand service name: RLA to Flow** ([#508](https://github.com/NVIDIA/infra-controller/rest-api/pull/508), [#520](https://github.com/NVIDIA/infra-controller/rest-api/pull/520), [#525](https://github.com/NVIDIA/infra-controller/rest-api/pull/525))
  Renames the Rack Level Administration (RLA) service to "Flow" across the entire codebase — directory structure, package names, configuration keys, Helm chart values, and documentation. The rebrand is split across three PRs for incremental review: core flow directory (#508), inner flow directory (#520), and external references (#525).

- **Update Go module path to remove ncx and match GitHub repo** ([#482](https://github.com/NVIDIA/infra-controller/rest-api/pull/482))
  Updates the Go module path from `github.com/NVIDIA/infra-controller/rest-api` to match the current GitHub repository URL, fixing import resolution for downstream consumers. See the updated module paths in the [API reference](https://docs.nvidia.com/infra-controller/rest-api-reference/getting-started).

- **Expose BMC IP Address Option for Expected Components** ([#445](https://github.com/NVIDIA/infra-controller/rest-api/pull/445))
  Adds an optional `bmcIpAddress` field to Expected Machine, Expected PowerShelf, and Expected Switch create/update requests, allowing operators to pre-configure BMC IP addresses during inventory planning. See the updated fields on [Expected Machine](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/expected-machine), [Expected Power Shelf](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/expected-power-shelf), and [Expected Switch](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/expected-switch) endpoints.

- **Add unique BMC + site constraint to expected_power_shelf and expected_switch** ([#450](https://github.com/NVIDIA/infra-controller/rest-api/pull/450))
  Adds database-level unique constraints on BMC MAC address per site for Expected PowerShelf and Expected Switch tables, matching the existing Expected Machine constraint and preventing duplicate registrations.

- **Add FromProto receivers for ExpectedMachine, PowerShelf, Switch, Rack** ([#500](https://github.com/NVIDIA/infra-controller/rest-api/pull/500))
  Adds `FromProto` receiver methods on Expected component DB models, standardizing proto-to-model conversion and reducing duplication in handler code.

- **Move VPC update/delete proto building onto the DB model** ([#501](https://github.com/NVIDIA/infra-controller/rest-api/pull/501))
  Relocates VPC proto request construction from API handlers to DB model methods, improving separation of concerns.

- **Move Tenant proto conversion onto the db model** ([#468](https://github.com/NVIDIA/infra-controller/rest-api/pull/468))
  Relocates Tenant proto conversion logic from API handlers to a DB model method for consistency.

- **Add ExpectedPowerShelf/ExpectedSwitch ToProto conversions onto db models** ([#467](https://github.com/NVIDIA/infra-controller/rest-api/pull/467))
  Adds `ToProto` methods on Expected PowerShelf and Expected Switch DB models, matching the pattern established for Expected Machine.

- **Apply expectedMachineToProto helper to batch handlers** ([#456](https://github.com/NVIDIA/infra-controller/rest-api/pull/456))
  Extends the proto conversion helper to the batch create and batch update handlers for Expected Machine.

- **Define proto-builder helpers for ExpectedMachine/Switch/PowerShelf API handlers** ([#451](https://github.com/NVIDIA/infra-controller/rest-api/pull/451))
  Extracts reusable proto builder helpers used across Expected component API handlers, reducing code duplication.

- **Use existing protobuf API label-conversion helper across handlers** ([#452](https://github.com/NVIDIA/infra-controller/rest-api/pull/452))
  Replaces inline label-to-proto conversion code with the existing shared helper function across all handlers.

- **Reuse pre-fetched Site in Expected Component GET handlers** ([#449](https://github.com/NVIDIA/infra-controller/rest-api/pull/449))
  Eliminates redundant Site lookups in Expected Component GET handlers by reusing the Site object already fetched during authorization.

- **Drop unused tclient.Client from expected API handlers** ([#448](https://github.com/NVIDIA/infra-controller/rest-api/pull/448))
  Removes the unused Temporal client dependency from Expected component API handlers, simplifying handler construction.

- **Configure Bun to discard unknown columns for DB queries** ([#437](https://github.com/NVIDIA/infra-controller/rest-api/pull/437))
  Enables Bun ORM's `DiscardUnknownColumns` setting so retired database columns are silently ignored during SQL scans, easing the transition window between struct removal and column migration. *(Also in v1.4.1)*

---

## [v1.4.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.4.0)

<Note>
This release is compatible with Core **v0.8.x**.
</Note>

### Features

- **Add support for filtering VPCs by NVLink Logical Partition** ([#380](https://github.com/NVIDIA/infra-controller/rest-api/pull/380))
  VPCs can now be filtered by their associated default NVLink Logical Partition ID. Other VPC filters have also been enhanced to accept multiple values, and network security group filter validation has been improved. See the updated query parameters on [Retrieve all VPCs](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/vpc/get-all-vpc).

- **Allow setting routing profile when creating VPCs** ([#350](https://github.com/NVIDIA/infra-controller/rest-api/pull/350))
  Callers can now include `routingProfile` when creating a VPC with FNN network virtualization. The field is only accepted for FNN-type VPCs and is reflected in the API response accordingly. See the `routingProfile` field on [Create VPC](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/vpc/create-vpc).

- **Allow power control to NSM and PSM without registration** ([#368](https://github.com/NVIDIA/infra-controller/rest-api/pull/368))
  NVSwitch Manager and PowerShelf Manager can now receive power control commands (on/off/cycle) without requiring prior component registration, simplifying initial rack bring-up workflows.

- **Update Site Agent Helm chart to adopt Core prereqs for installation** ([#416](https://github.com/NVIDIA/infra-controller/rest-api/pull/416))
  Updates the README and Helm chart to reference the `helm-prereqs` chart, now located in this repository's `helm-prereqs/` directory, as the recommended installation path for bare-metal cluster setup. Also adds username keys to the common DB credentials secret template.

- **Hint at label filter syntax after TUI list output** ([#406](https://github.com/NVIDIA/infra-controller/rest-api/pull/406))
  After running a list command in the TUI, a context-sensitive hint now displays available label keys and the syntax for `--label`, `--sort-label`, and `scope label` filtering. The hint is suppressed once any label filter is active.

- **Add user-defined task schedules** ([#392](https://github.com/NVIDIA/infra-controller/rest-api/pull/392))
  Extends the RLA scheduler to support user-defined task schedules, allowing operators to configure custom recurring jobs beyond the built-in inventory sync and leak detection schedules.

- **Add support for Delta PMC vendor in Powershelf Manager** ([#331](https://github.com/NVIDIA/infra-controller/rest-api/pull/331))
  PSM now supports Delta as a power shelf vendor alongside the existing Liteon support, broadening hardware compatibility for power management.

- **Support explicit rule ID override in RLA sequence requests** ([#404](https://github.com/NVIDIA/infra-controller/rest-api/pull/404))
  Callers can now specify an operation rule by ID when submitting rack operations, bypassing the normal priority chain (rack association, default, hardcoded) and using the requested rule directly.

### Bug Fixes

- **Reject DB connection when no encryption** ([#428](https://github.com/NVIDIA/infra-controller/rest-api/pull/428))
  Restores the PostgreSQL SSL mode from `disable` (introduced accidentally in v1.1.0 during a DSN builder refactor) back to `prefer`, so that the client attempts TLS first and falls back gracefully. This fixes connection failures against Postgres servers that only accept encrypted connections via `hostssl` rules.

- **Add Site Agent manager for VPC Peering and fix workflows** ([#424](https://github.com/NVIDIA/infra-controller/rest-api/pull/424))
  Adds the missing VPC Peering manager to the Site Agent and fixes VPC Peering workflows to correctly require the VPC Peering ID during creation on Site.

- **Mark ipBlockId as required in VPC Prefix create request** ([#414](https://github.com/NVIDIA/infra-controller/rest-api/pull/414))
  Corrects the OpenAPI schema and CLI/TUI to reflect that `ipBlockId` is a required field when creating a VPC Prefix, matching the server-side validation that already enforced this. See [Create VPC Prefix](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/vpc-prefix/create-vpc-prefix).

- **Prompt for allocation constraints in TUI allocation create** ([#413](https://github.com/NVIDIA/infra-controller/rest-api/pull/413))
  Fixes the TUI allocation creation flow to properly prompt for the required constraint (resource type, IP block selection, constraint type, and value), which was previously missing.

- **Send protocolVersion and routingType on IP block create** ([#412](https://github.com/NVIDIA/infra-controller/rest-api/pull/412))
  Fixes IP Block creation in the CLI/TUI by including the `protocolVersion` and `routingType` parameters that were previously omitted from the request, causing creation failures.

- **Fix Expected Machine OpenAPI misnamed fields for BMC default credentials** ([#421](https://github.com/NVIDIA/infra-controller/rest-api/pull/421))
  Corrects field names for BMC default username and password in the Expected Machine OpenAPI schema, resolving mismatches between the spec and the actual API behavior. See [Expected Machine](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/expected-machine) endpoints.

- **Resolve RLA inventory component manager ID sync issue** ([#409](https://github.com/NVIDIA/infra-controller/rest-api/pull/409))
  Ensures machine IDs are synced on every inventory loop iteration, removing a conditional skip that could leave `external_id` stale and cause leak detection to fail. Also updates default operation timeouts and fixes misleading error messages in component target resolution.

- **Update SSH Key Group status after successful sync to Site** ([#411](https://github.com/NVIDIA/infra-controller/rest-api/pull/411))
  Fixes a bug where the overall SSH Key Group status was not updated after a successful sync to a Site — only the per-site association status was being set, leaving the parent resource in a stale state.

- **Prevent duplicate --data flag panic on carbidecli create commands** ([#401](https://github.com/NVIDIA/infra-controller/rest-api/pull/401))
  Fixes a panic in `carbidecli dpu-extension-service create` (and similar commands) caused by a flag name collision when a request body property is named `data`. Colliding properties are now registered under a `body-` prefix.

- **Resolve required query param for tenant-account list in TUI** ([#408](https://github.com/NVIDIA/infra-controller/rest-api/pull/408))
  Fixes the TUI tenant-account list command that was failing due to a missing required query parameter.

- **Error when flags are placed after a positional argument in carbidecli** ([#400](https://github.com/NVIDIA/infra-controller/rest-api/pull/400))
  Adds detection of misordered flags placed after positional arguments in `carbidecli`, providing a clear error message instead of sending a malformed HTTP request.

- **Remove deprecated Instance/Allocation relationships** ([#371](https://github.com/NVIDIA/infra-controller/rest-api/pull/371))
  Completes the transition from per-instance allocation linkage to aggregate allocation enforcement. Instance creation now validates against total reserved capacity for a tenant's instance type at a site, and allocation constraint updates check against total instance usage. See the updated [Allocation](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/allocation) schema.

- **Move machine ID lock acquisition before record pull** ([#405](https://github.com/NVIDIA/infra-controller/rest-api/pull/405))
  Fixes a race condition in instance creation where the machine record could become stale between initial read and lock acquisition. The lock is now acquired before pulling the record, ensuring all subsequent checks operate on current data.

- **Fix OpenAPI URL for IP Block, SSH Key/Group and Tenant Account in TUI/CLI** ([#397](https://github.com/NVIDIA/infra-controller/rest-api/pull/397))
  Corrects 20 URL path segments in the TUI that were using hyphenated display names instead of the actual API paths (e.g., `ip-block` instead of `ipblock`), fixing silent 404s on list, get, create, update, and delete operations for these four resource types.

- **Revise Allocation status enum and attribute descriptions in OpenAPI spec** ([#395](https://github.com/NVIDIA/infra-controller/rest-api/pull/395))
  Aligns the Allocation status enum values in the OpenAPI schema with database constants (e.g., `Registered` was missing), fixing deserialization errors when listing Allocations. Adds comprehensive attribute descriptions across Allocation models. See the updated [Allocation](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/allocation) schema.

- **Infer Provider/Tenant from org for Site update and Fabric retrieval endpoints** ([#372](https://github.com/NVIDIA/infra-controller/rest-api/pull/372))
  Extends org-based identity inference to Site update and Fabric retrieval endpoints, removing the need to pass `infrastructureProviderId` or `tenantId` query parameters. See updated parameters on [Update Site](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/site/update-site) and [Retrieve all Sites](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference/site/get-all-site).

- **Support reflashing the same firmware version in PSM** ([#393](https://github.com/NVIDIA/infra-controller/rest-api/pull/393))
  Allows PowerShelf Manager to re-apply the same firmware version that is already installed, enabling recovery scenarios where a re-flash is needed without a version change.

- **Include IP block flag in VPC prefix create log** ([#426](https://github.com/NVIDIA/infra-controller/rest-api/pull/426))
  Updates the CLI hint text for VPC Prefix creation to include the required IP Block flag.

### Refactoring

- **Create generic Execute interface and workflow/activity registries in RLA** ([#419](https://github.com/NVIDIA/infra-controller/rest-api/pull/419))
  Introduces a generic `Execute` interface and type-safe workflow/activity registries in the RLA task executor, replacing ad-hoc registration with a structured pattern. Consolidates shared execution logic, adds comprehensive registry tests, and improves discoverability of available actions.

### Documentation

- **Add Getting Started section in OpenAPI schema** ([#402](https://github.com/NVIDIA/infra-controller/rest-api/pull/402))
  Adds a [Getting Started](https://docs.nvidia.com/infra-controller/rest-api-reference/getting-started) section to the API documentation, providing a clear onboarding path for new users. HTTP 200 and 201 responses are now auto-expanded for better discoverability of response schemas.

### CI/CD

- **Reduce tests duration by up to 36%** ([#431](https://github.com/NVIDIA/infra-controller/rest-api/pull/431))
  Optimizes PostgreSQL test container configuration by trading durability for speed (disabling fsync, full page writes, and synchronous commit), reducing local test runs from ~10:38 to ~6:46.

- **Add Grype container vulnerability scan to build-push-service** ([#418](https://github.com/NVIDIA/infra-controller/rest-api/pull/418))
  Integrates Grype as an additional container vulnerability scanner in the Docker build-and-push workflow, complementing existing security scanning.

- **Add GitHub workflow to ensure Core protobuf is up to date** ([#391](https://github.com/NVIDIA/infra-controller/rest-api/pull/391))
  Adds a CI check that verifies generated protobuf code matches the current proto files, preventing drift between proto definitions and generated Go code.

### Chores

- **Clean up deprecated workflows/activities in Site Agent** ([#375](https://github.com/NVIDIA/infra-controller/rest-api/pull/375))
  Removes legacy workflows and activities from Site Agent that have been superseded by the Site Workflow module. Deletes custom proto objects that had drifted from Core. Retained workflows for Temporal CLI based deletion now have a `ByID` suffix for clarity.

- **Switch from deprecated attributes for VPC Prefix create/update request to Site** ([#423](https://github.com/NVIDIA/infra-controller/rest-api/pull/423))
  Migrates VPC Prefix create and update API handlers from deprecated Core proto attributes to their current replacements.

- **Add back standard SDK module after repo rename** ([#265](https://github.com/NVIDIA/infra-controller/rest-api/pull/265))
  Re-adds the standard SDK Go module that was removed during the repository rename, fixing import paths. SDK consumers should update imports to `github.com/NVIDIA/infra-controller/rest-api/sdk/standard`.

- **Configure NSM and PSM to run in-memory mode by default** ([#410](https://github.com/NVIDIA/infra-controller/rest-api/pull/410))
  NVSwitch Manager and PowerShelf Manager now default to in-memory firmware storage mode, eliminating the PostgreSQL dependency for these services in standard deployments.

- **Update Instance creation API test, validate unhealthy Machine flag sent to Core** ([#407](https://github.com/NVIDIA/infra-controller/rest-api/pull/407))
  Strengthens Instance creation tests to verify the `allowUnhealthyMachine` flag is correctly forwarded to Core.

- **Fix idempotency issue for warning comments in Core proto format script** ([#389](https://github.com/NVIDIA/infra-controller/rest-api/pull/389))
  Makes `make core-proto-fmt` fully idempotent by preventing duplicate warning comment and block insertion on repeated runs.

- **Re-generate Core protobuf to align with latest proto files** ([#390](https://github.com/NVIDIA/infra-controller/rest-api/pull/390))
  Runs `make core-protogen` to sync generated Go code with the current `*_carbide.proto` definitions on the main branch.

---

## [v1.3.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.3.0)

<Note>
This release is compatible with Core **v0.7.x**.
</Note>

### Features

- **Add system job scheduler in RLA with trigger and overlap policies** ([#352](https://github.com/NVIDIA/infra-controller/rest-api/pull/352))
  Replaces ad-hoc inventory sync and leak detection go-routines with a structured scheduling framework. Each job is defined with a configurable trigger (timer, cron, trigger-once, or event-driven), an overlap policy, and a worker, providing graceful and forceful shutdown support.

- **Add support for updating InfiniBand Partition data on Site** ([#334](https://github.com/NVIDIA/infra-controller/rest-api/pull/334))
  Implements end-to-end InfiniBand Partition update propagation to the Site Controller. The API handler now starts a site workflow after a successful update to REST DB cache, wiring through proto definitions, Temporal workflows, and activities consistent with the existing create/delete patterns.

- **Add net.HardwareAddr wrapper for BMC MAC JSON marshaling** ([#369](https://github.com/NVIDIA/infra-controller/rest-api/pull/369))
  Introduces a `net.HardwareAddr` wrapper type that provides proper JSON marshaling and unmarshaling for BMC MAC addresses, replacing raw byte-slice serialization with human-readable colon-separated format.

### Bug Fixes

- **Include name in update request of NVLink Partition Update** ([#373](https://github.com/NVIDIA/infra-controller/rest-api/pull/373))
  Ensures new or existing partition name is included in NVLink Logical Partition update requests to Site, since Site expects the update request to reflect the full data.

- **Require TLS certs by default for RLA/PSM/NVSM and IPAM server** ([#333](https://github.com/NVIDIA/infra-controller/rest-api/pull/333))
  RLA, PSM, and NSM now refuse to start without TLS certificates unless `ALLOW_INSECURE_GRPC=true` is explicitly set, hardening the default security posture. Also IPAM gRPC server now supports/requires TLS specification.

- **Update default firmware update sequence for NSM to only include BMC and BIOS updates** ([#376](https://github.com/NVIDIA/infra-controller/rest-api/pull/376))
  Narrows the default NSM firmware update sequence to BMC and BIOS components only, excluding unnecessary sub-component updates that could cause longer maintenance windows.

- **Prepare for Machine/InstanceType Association ID deprecation** ([#367](https://github.com/NVIDIA/infra-controller/rest-api/pull/367))
  Adds Machine ID as a replacement for Instance Type/Machine Association ID for removal of assignment, introduces a dated deprecation window for association IDs and enabling clients to migrate smoothly.

- **Include NVLink and InfiniBand Interfaces while cleaning up Instance resources** ([#366](https://github.com/NVIDIA/infra-controller/rest-api/pull/366))
  Fixes instance termination cleanup to also delete associated NVLink and InfiniBand interfaces, preventing orphaned network interface records. Includes a DB migration to remove previously orphaned interfaces.

- **Harden scheduler dispatcher correctness and unit tests of RLA** ([#364](https://github.com/NVIDIA/infra-controller/rest-api/pull/364))
  Eliminates shared-state race conditions in the scheduler dispatcher by using `forceCtx` as the parent for all job contexts, fixes event-draining on queue exhaustion, and replaces timing-sensitive tests with deterministic assertions.

- **Added status field in NVLink Interface summary API model** ([#363](https://github.com/NVIDIA/infra-controller/rest-api/pull/363))
  Adds the missing `status` field to the NVLink Interface summary API model, allowing consumers to view interface status when listing NVLink Interfaces within an NVLink Partition.

- **Fix bringup sequence, NSM stale records, and unify tray/rack type enums** ([#377](https://github.com/NVIDIA/infra-controller/rest-api/pull/377))
  Addresses several issues found during rack bring-up and firmware update testing: replaces the default BringUp rule's ingestion-based power-on with standard `PowerControl` to avoid BMC MAC lookup failures, restructures firmware upgrade sequencing from parallel to staged execution (compute then NVLSwitch then power recycle), fixes Temporal serialization loss of `FirmwareControlTaskInfo` across child workflow boundaries, filters stale firmware update records in NSM's `GetUpdatesForSwitch` to prevent old failures from masking current successes, and unifies component type enum naming across Tray and Rack API endpoints to PascalCase (`Compute`, `NVLSwitch`, `PowerShelf`, etc.).

- **Add dev mode for RLA service** ([#360](https://github.com/NVIDIA/infra-controller/rest-api/pull/360))
  Introduces an `RLA_ENV` environment variable that gates development-only features: gRPC reflection is enabled only in dev mode, and the log level defaults to debug in dev mode versus info in production, preventing accidental exposure of diagnostic interfaces in deployed environments.

- **Skip config filter in DB if no config query params are set when retrieving all Sites** ([#379](https://github.com/NVIDIA/infra-controller/rest-api/pull/379))
  Fixes a bug where the Site list handler unconditionally applied an empty JSONB config filter, causing sites with a NULL config column to be silently excluded from results. Site listing now only applies config filtering when at least one config query parameter is explicitly provided.

- **Maintain association record when Instance Type is updated in Machine inventory** ([#383](https://github.com/NVIDIA/infra-controller/rest-api/pull/383))
  When a Machine's Instance Type changes during inventory sync, the Machine/InstanceType association record is now updated alongside the Machine attribute itself, keeping both representations consistent until the association ID is fully deprecated.

- **Have PSM read firmware files at startup time rather than using an embedded filesystem** ([#385](https://github.com/NVIDIA/infra-controller/rest-api/pull/385))
  Switches PowerShelf Manager from compile-time embedded firmware binaries to runtime file loading at startup, allowing firmware images to be updated by replacing files on disk without recompiling the service.

### Refactoring

- **Require Ready status for targeted machine instance creation** ([#357](https://github.com/NVIDIA/infra-controller/rest-api/pull/357))
  Targeted instance creation now enforces that the specified machine must be in `Ready` status or in `Error` (health alerts) or `Maintenance` status with the Core state being `Ready` (when `allowUnhealthyMachine` flag is set).

### Chores

- **Replace hardcoded API name in path in TUI using helper** ([#356](https://github.com/NVIDIA/infra-controller/rest-api/pull/356))
  Replaces all 86 hardcoded `/v2/org/{org}/nico/...` path strings in the TUI with calls to a new `apiPath` helper, making path construction consistent with the SDK's configurable API name support.

- **Rename Site Agent and mock Core/RLA server binary** ([#365](https://github.com/NVIDIA/infra-controller/rest-api/pull/365))
  Renames Site Agent and mock server binaries as part of the Site Agent v2 preparation, and removes residual database references from the stateless agent.

- **Update Core proto and improve firmware update sequencing in RLA** ([#361](https://github.com/NVIDIA/infra-controller/rest-api/pull/361))
  Aligns RLA snapshot of Core proto, improves firmware version matching between input requests and observed state, and enables RLA to update the `firmware_autoupdate` flag for machines.

- **Update Core proto snapshot for REST components** ([#251](https://github.com/NVIDIA/infra-controller/rest-api/pull/251))
  Introduces an idempotent `make core-proto` script that automates Core proto file snapshotting with handling for backwards-incompatible changes and REST-specific additions. Also removes deprecated non-paginated object retrieval fallback methods from Site Agent.

- **Add changelog with detailed history of released tags up to v1.2.1** ([#359](https://github.com/NVIDIA/infra-controller/rest-api/pull/359))
  Adds a comprehensive CHANGELOG.md with professional descriptions for every pull request across all 12 released versions.

---

## [v1.2.1](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.2.1)

<Note>
This release is compatible with Core **v0.6.x**.
</Note>

### Features

- **Allow Instance Interfaces to span multiple VPCs** ([#300](https://github.com/NVIDIA/infra-controller/rest-api/pull/300))
  Instances can now attach interfaces to VPC prefixes from different VPCs, enabling multi-VPC networking per instance. The primary interface must still belong to the instance's primary VPC, and NSG propagation status now reflects the aggregate state across all attached VPCs.

- **Infer Tenant/Provider context from org when retrieving Allocation/Instance Type** ([#217](https://github.com/NVIDIA/infra-controller/rest-api/pull/217))
  Removes the requirement for callers to pass explicit `infrastructureProviderId` and `tenantId` query parameters on Allocation and Instance Type endpoints by inferring identity from org membership. Dual-role users now receive a merged view from both provider and tenant perspectives in a single call.

- **Modify NSM and PSM Vault credential format to match NICo's standard pattern** ([#341](https://github.com/NVIDIA/infra-controller/rest-api/pull/341))
  Aligns the credential format that NVSwitch Manager and PowerShelf Manager expect in Vault with the standard credential pattern used by NICo, ensuring consistency across component managers.

- **Revise PatchComponent, add DeleteRack, PurgeRack, PurgeComponent gRPC APIs in RLA** ([#320](https://github.com/NVIDIA/infra-controller/rest-api/pull/320))
  Extends RLA gRPC APIs with soft-delete and permanent purge operations for racks and components. PatchComponent now supports BMC updates, improving rack lifecycle management capabilities.

- **Add Tenant Account search query and fix tsquery parsing** ([#315](https://github.com/NVIDIA/infra-controller/rest-api/pull/315))
  Implements the `query` parameter for tenant account listing, enabling search by account number, tenant org, or display name. Also fixes a bug where multiple consecutive spaces in search queries would generate invalid PostgreSQL `to_tsquery` syntax, causing 500 errors.

### Bug Fixes

- **Aggregate NVSwitch sub-component firmware statuses in NICo path in RLA** ([#355](https://github.com/NVIDIA/infra-controller/rest-api/pull/355))
  Fixes a map-overwrite bug where only the last sub-component firmware status survived per switch when Core returned multiple statuses (BMC, CPLD, BIOS, NVOS). An aggregation function now correctly reports failure if any sub-component fails, and missing switches are reported as Unknown.

- **Prevent UpdateTaskStatus from overwriting started_at with NULL in RLA task executor** ([#354](https://github.com/NVIDIA/infra-controller/rest-api/pull/354))
  The `started_at` column was unconditionally included in UPDATE statements, causing finished-status writes to overwrite the stored timestamp with NULL. The column list is now built dynamically so `started_at` is only set during the Running transition.

- **Align PMC Vault path with Core/NSM** ([#353](https://github.com/NVIDIA/infra-controller/rest-api/pull/353))
  Corrects the Vault path used for storing PMC credentials in PowerShelf Manager to match the convention used by Core and NVSwitch Manager, resolving credential lookup failures.

- **Correct child workflow timeout/error and improve compute firmware update lifecycle in RLA** ([#348](https://github.com/NVIDIA/infra-controller/rest-api/pull/348))
  Fixes timeout budget miscalculation where child workflows shared the same timeout as individual activities, leaving no room for retries. Also adds fail-fast firmware version validation against Core, idempotent scheduling for already-complete machines, and proper error attribution when steps are skipped.

- **Make PSM registration credentials-optional and idempotent** ([#347](https://github.com/NVIDIA/infra-controller/rest-api/pull/347))
  PowerShelf Manager registration no longer requires credentials upfront, and re-registration of the same shelf is now safely idempotent rather than returning an error.

- **Only report inherited VPC propagation when interfaces are attached** ([#342](https://github.com/NVIDIA/infra-controller/rest-api/pull/342))
  NSG inheritance from parent VPCs is now only reported when interfaces are actually attached to instances, preventing misleading propagation status on instances with no active interfaces.

- **Prevent modification of virtualization type for VPCs with Subnets or Instances** ([#343](https://github.com/NVIDIA/infra-controller/rest-api/pull/343))
  The API now rejects virtualization type changes for tenant-owned VPCs that already have attached subnets or instances, preventing disruptive configuration changes on in-use VPCs.

- **Normalize MAC address case in NSM/PSM vault credential lookups** ([#345](https://github.com/NVIDIA/infra-controller/rest-api/pull/345))
  Fixes a case mismatch between Go's lowercase MAC addresses and Core's uppercase Vault paths that caused credential lookups to silently fail in NVSwitch Manager and PowerShelf Manager.

- **Auto-migrate NSM database schema on startup** ([#340](https://github.com/NVIDIA/infra-controller/rest-api/pull/340))
  NVSwitch Manager now applies database migrations automatically on startup, matching the existing behavior of PowerShelf Manager and eliminating manual migration steps.

- **Add health-report override and idempotent power-option handling in RLA** ([#335](https://github.com/NVIDIA/infra-controller/rest-api/pull/335))
  RLA power control now properly marks machines with health-report overrides before operations and cleans them up afterwards. Power state transitions that are already in the desired state are treated as no-ops instead of failing the entire operation.

- **Add 10 MiB request body size limit to prevent OOM** ([#330](https://github.com/NVIDIA/infra-controller/rest-api/pull/330))
  Adds a global 10 MiB request body size limit using Echo's BodyLimit middleware, preventing the audit body middleware from buffering arbitrarily large payloads and eliminating an OOM crash vector for authenticated mutating endpoints.

### Chores

- **Infer Provider and Tenant ID from org association when creating Operating Systems** ([#344](https://github.com/NVIDIA/infra-controller/rest-api/pull/344))
  Removes unnecessary `TenantID` and `InfrastructureProviderID` parameters from OS creation, since both are already derivable from the caller's org association.

- **Add HTTP read/write/idle timeout in Echo config** ([#339](https://github.com/NVIDIA/infra-controller/rest-api/pull/339))
  Configures well-defined read, write, and idle timeouts for the HTTP server, mitigating potential Slowloris-style denial-of-service attacks.

- **Tune RLA power operation timer configuration** (no PR)
  Adjusts timer settings for RLA power operations to better accommodate real-world operation durations.

- **Add detailed review config for CodeRabbit** ([#337](https://github.com/NVIDIA/infra-controller/rest-api/pull/337))
  Updates CodeRabbit configuration to provide more contextual and well-informed automated code reviews.

- **Add codeowners file to repository** ([#295](https://github.com/NVIDIA/infra-controller/rest-api/pull/295))
  Introduces a CODEOWNERS file so reviewers are automatically assigned to pull requests based on file ownership.

- **Optimize NVLink Logical Partition lookup in Instance update API handler** ([#332](https://github.com/NVIDIA/infra-controller/rest-api/pull/332))
  Replaces inefficient one-at-a-time DB lookups of NVLink Logical Partitions with a batch query, and adds proper distinction between internal DB errors and non-existent partitions.

---

## [v1.2.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.2.0)

<Note>
This release is compatible with Core **v0.6.x**.
</Note>

### Features

- **Add full CRUD parity for TUI interactive mode** ([#305](https://github.com/NVIDIA/infra-controller/rest-api/pull/305))
  Extends the CLI's interactive TUI with create, update, and delete commands for all major resource types including sites, VPCs, subnets, instances, allocations, and more. Instance creation scopes machine selection to the VPC's site for a more intuitive workflow.

- **Add label display, filtering, and sorting to TUI** ([#306](https://github.com/NVIDIA/infra-controller/rest-api/pull/306))
  Adds a LABELS column to all label-bearing resources in the TUI, introduces persistent label scope filtering via `scope label key=value`, per-command `--label` filtering with AND logic, and `--sort-label` sorting. Also adds `instance-type list` and `instance-type get` commands.

- **Add interactive instance create/delete and keybinding help** ([#294](https://github.com/NVIDIA/infra-controller/rest-api/pull/294))
  Adds guided interactive flows for creating and deleting instances in the TUI, with VPC and machine selection, name prompts, and optional OS selection. Includes escape-to-cancel support and updated keybinding documentation.

- **Add configurable API name support to SDK REST clients** ([#322](https://github.com/NVIDIA/infra-controller/rest-api/pull/322))
  Introduces an `APIName` configuration hook in both the standard and simple SDK clients, allowing callers to target deployments that use a non-default API path segment without modifying generated code.

- **Update RLA inventory sync to use Core for switch and powershelf management** ([#321](https://github.com/NVIDIA/infra-controller/rest-api/pull/321))
  RLA inventory sync now routes through Core when the component manager for switches and powershelves is configured accordingly, enabling unified inventory management.

- **Add DPU extension observability config options** ([#291](https://github.com/NVIDIA/infra-controller/rest-api/pull/291))
  Adds support for Prometheus and logging observability configuration when creating or updating DPU extension services, enabling operators to instrument DPU workloads at deployment time.

- **Dual-write Expected Inventory REST APIs into Core and RLA** ([#303](https://github.com/NVIDIA/infra-controller/rest-api/pull/303))
  Expected Inventory API calls now write to both Core and RLA, ensuring that the Launch Layer's inventory data reaches both the cloud orchestration layer and the on-site rack-level administration system.

- **Add pagination fallback in RLA gRPC** ([#318](https://github.com/NVIDIA/infra-controller/rest-api/pull/318))
  RLA now falls back to default pagination parameters when upstream callers omit them, preventing panics from nil pagination structs. Also removes obsolete code.

- **Trigger power-off via task manager on leak detection** ([#308](https://github.com/NVIDIA/infra-controller/rest-api/pull/308))
  When a coolant leak is detected in a tray, the system now automatically triggers a power-off operation through the RLA task manager, providing a safety response to prevent hardware damage.

- **Add NVLink Switch inventory sync with NSM registration and drift detection** ([#298](https://github.com/NVIDIA/infra-controller/rest-api/pull/298))
  Implements automatic NVLink Switch inventory synchronization with NVSwitch Manager registration and configuration drift detection, ensuring switch state is kept current.

- **Add REST API models and endpoints for VPC peering** ([#257](https://github.com/NVIDIA/infra-controller/rest-api/pull/257))
  Introduces API models and CRUD endpoints for VPC peering, enabling cross-VPC network connectivity between tenants or within a provider's infrastructure.

- **Initial implementation of tray leak detection** ([#297](https://github.com/NVIDIA/infra-controller/rest-api/pull/297))
  Adds the foundational leak detection subsystem for monitoring coolant leaks in rack trays, providing the sensor data pipeline for automated safety responses.

- **Add mTLS support to RLA CLI and refactor cert packages** ([#299](https://github.com/NVIDIA/infra-controller/rest-api/pull/299))
  Enables mutual TLS authentication for RLA CLI commands and consolidates certificate handling into a shared package, aligning with the security posture of other NICo services.

- **Add AGENTS.md info file for repo** ([#292](https://github.com/NVIDIA/infra-controller/rest-api/pull/292))
  Adds an AGENTS.md file providing comprehensive guidance for AI coding agents working in the repository, covering project structure, build commands, coding conventions, and CI/CD workflows.

- **Support task conflict detection on component-level** ([#278](https://github.com/NVIDIA/infra-controller/rest-api/pull/278))
  Extends the RLA task framework to detect conflicts at the individual component level, preventing overlapping operations on the same hardware component.

- **Add API model and endpoint for retrieving RLA Tasks** ([#252](https://github.com/NVIDIA/infra-controller/rest-api/pull/252))
  Exposes RLA task status and history through the REST API, allowing users to track the progress of rack-level operations such as firmware upgrades and power control.

- **Support firmware upgrades in-memory mode for PSM** ([#277](https://github.com/NVIDIA/infra-controller/rest-api/pull/277))
  Adds support for in-memory firmware upgrade mode in PowerShelf Manager, providing a faster firmware update path for supported hardware.

- **Implement NICo provider for NVLSwitch and PowerShelf component managers** ([#256](https://github.com/NVIDIA/infra-controller/rest-api/pull/256))
  Introduces a NICo-backed provider for managing NVLink Switches and PowerShelves, enabling these component types to be managed through the standard NICo Core API path.

- **Refactor task workflow and component manager in RLA** ([#269](https://github.com/NVIDIA/infra-controller/rest-api/pull/269))
  Restructures the RLA task workflow execution and component manager architecture for better maintainability and extensibility.

- **Add Site-Agent bootstrap hook support in helm chart** ([#263](https://github.com/NVIDIA/infra-controller/rest-api/pull/263))
  Adds support for custom bootstrap hooks in the Site-Agent Helm chart, enabling site-specific initialization logic during deployment.

- **Allow explicit IP selection when creating/updating instances** ([#271](https://github.com/NVIDIA/infra-controller/rest-api/pull/271))
  Enables callers to specify explicit IP addresses when creating or updating instances, rather than relying solely on automatic allocation from VPC prefixes.

### Bug Fixes

- **Populate APIError source attribute from API name** ([#287](https://github.com/NVIDIA/infra-controller/rest-api/pull/287))
  Replaces the hardcoded `nico` source in structured API errors with the configured API name, ensuring error responses accurately identify the originating service. This is a breaking change for clients that matched on `source == "nico"`.

- **Strip leading comma from Docker image tags in CI** ([#323](https://github.com/NVIDIA/infra-controller/rest-api/pull/323))
  Fixes the CI Docker tag generation that produced malformed tags like `,nvcr.io/.../image:tag` due to empty string initialization with comma-prefixed appends.

- **Load RLA Temporal client certificates using cert module** ([#319](https://github.com/NVIDIA/infra-controller/rest-api/pull/319))
  Generates certificate and key file paths using the Kubernetes workload standard convention, fixing TLS certificate loading for RLA's Temporal client connections.

- **Support clearing NVLink Logical Partition ID in VPC update** ([#284](https://github.com/NVIDIA/infra-controller/rest-api/pull/284))
  Allows passing an empty string for NVLink Logical Partition ID in VPC update requests to explicitly clear the default partition assignment.

- **Fixed legacy RLA and PSM makefiles VERSIONS usage and removed obsolete dockerfiles** ([#286](https://github.com/NVIDIA/infra-controller/rest-api/pull/286))
  Corrects how legacy RLA and PSM makefiles reference VERSION files and removes obsolete Dockerfiles that were no longer in use.

- **Reset IsUsableByTenant when machine goes missing on Site** ([#317](https://github.com/NVIDIA/infra-controller/rest-api/pull/317))
  When a machine stops being reported by the Site Controller, `IsUsableByTenant` is now correctly reset to `false` alongside the status change, preventing stale tenant usability flags in the API response.

- **Return all trays when rackId/rackName not specified in GET /tray** ([#312](https://github.com/NVIDIA/infra-controller/rest-api/pull/312))
  Fixes a 500 Internal Server Error when querying trays without specifying `rackId` or `rackName`; the API now correctly returns all trays across all racks in the site.

- **Defaulted native_networking and network_security_group to true** ([#309](https://github.com/NVIDIA/infra-controller/rest-api/pull/309))
  Updates default site configuration to enable native networking and network security group support for newly created sites, as all current deployments require these features.

- **Delete NVLink interfaces when Site reports config as synced** ([#267](https://github.com/NVIDIA/infra-controller/rest-api/pull/267))
  NVLink interfaces are now properly cleaned up in the database when the Site Controller reports configuration as fully synced, resolving stale interface records.

- **Prevent VPC Prefix deletion when Instance Interfaces are present** ([#285](https://github.com/NVIDIA/infra-controller/rest-api/pull/285))
  Blocks deletion of VPC prefixes that have active Instance Interfaces using them, preventing orphaned network references.

- **Enable NVLink deviceType specification in Instance Type update** ([#283](https://github.com/NVIDIA/infra-controller/rest-api/pull/283))
  Allows specifying NVLink `deviceType` when updating Instance Types, and permits NVLink deviceType on GPU capabilities, fixing validation gaps in the Instance Type configuration.

- **Permit NVLink deviceType on GPU capabilities for instance type** ([#280](https://github.com/NVIDIA/infra-controller/rest-api/pull/280))
  Removes an incorrect validation that rejected NVLink deviceType when specified alongside GPU capabilities on Instance Types.

- **Empty array must be passed to underlying SDK when caller specifies one** ([#275](https://github.com/NVIDIA/infra-controller/rest-api/pull/275))
  Fixes a bug where explicitly passing an empty array in API requests was silently ignored instead of being forwarded to the SDK, preventing callers from clearing list fields.

### Refactoring

- **Exclude ingestion from default bringup sequence rule** ([#311](https://github.com/NVIDIA/infra-controller/rest-api/pull/311))
  Removes ingestion from the default bringup sequence in RLA rules, allowing ingestion to be handled separately from the standard rack bringup workflow.

### Documentation

- **Fix routing type enum in IP Block create request schema** ([#313](https://github.com/NVIDIA/infra-controller/rest-api/pull/313))
  Corrects an extraneous space in the routing type enum value within the OpenAPI schema for IP Block creation requests.

### CI/CD

- **Remove trivy scan job** ([#307](https://github.com/NVIDIA/infra-controller/rest-api/pull/307))
  Removes the Trivy container vulnerability scanning job from the CI pipeline.

### Chores

- **Remove email from OpenAPI specs and auto-generated files** ([#302](https://github.com/NVIDIA/infra-controller/rest-api/pull/302))
  Strips personal email addresses from the OpenAPI specification and all associated auto-generated files.

- **Move common and cert-manager to subchart** ([#290](https://github.com/NVIDIA/infra-controller/rest-api/pull/290))
  Restructures the Helm chart to package common utilities and cert-manager as subcharts, improving modularity and deployment flexibility.

- **Update issue templates to standardize and remove duplicates** ([#288](https://github.com/NVIDIA/infra-controller/rest-api/pull/288))
  Consolidates and standardizes GitHub issue templates, removing duplicate templates and ensuring consistent contributor experience.

---

## [v1.1.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.1.0)

### Features

- **Allow different Logical Partitions for NVLink Interfaces on Instance creation/update** ([#225](https://github.com/NVIDIA/infra-controller/rest-api/pull/225))
  NVLink Interfaces no longer need to share the same Logical Partition; each interface can now reference a different partition. Validation has been tightened to require unique GPU indices within machine bounds, and duplicate detection now uses partition + deviceInstance composite keys.

- **Add optional DB secret volume mount in Helm Chart** ([#260](https://github.com/NVIDIA/infra-controller/rest-api/pull/260))
  Adds a `secrets.dbCreds` option to the Helm chart that allows reading the database password from a mounted Kubernetes Secret instead of plaintext in ConfigMap, enabling secure production deployments.

- **Add task conflict detection and queue framework** ([#233](https://github.com/NVIDIA/infra-controller/rest-api/pull/233))
  Introduces a task conflict detection and queuing framework in RLA that prevents overlapping operations on the same resources, ensuring safe concurrent task execution.

- **Add total assigned Machines to Instance Type allocation stats** ([#245](https://github.com/NVIDIA/infra-controller/rest-api/pull/245))
  Includes the total number of assigned machines in Instance Type allocation statistics, giving providers better visibility into resource utilization per instance type.

- **Standardize API enum formatting and extend Tray response fields** ([#241](https://github.com/NVIDIA/infra-controller/rest-api/pull/241))
  Normalizes enum value formatting across API responses and adds additional fields to Tray endpoint responses for richer component metadata.

- **Allow filtering Machines by whether missing on site** ([#243](https://github.com/NVIDIA/infra-controller/rest-api/pull/243))
  Adds a query parameter to filter machines by their `isMissingOnSite` status, enabling operators to quickly identify machines that have stopped reporting from the site.

- **Add REST endpoints for ExpectedPowerShelf and ExpectedSwitch** ([#220](https://github.com/NVIDIA/infra-controller/rest-api/pull/220))
  Introduces CRUD REST API endpoints for managing expected power shelf and switch inventory, maintaining parity with the existing ExpectedMachine endpoints.

- **Add API endpoints for RLA rack bring up** ([#206](https://github.com/NVIDIA/infra-controller/rest-api/pull/206))
  Exposes REST API endpoints for initiating rack bringup operations through RLA, enabling orchestrated rack-level provisioning workflows.

- **Port NV-Switch manager into the repository** ([#192](https://github.com/NVIDIA/infra-controller/rest-api/pull/192))
  Integrates the NVSwitch Manager service directly into the nico-rest repository, consolidating switch management alongside other component managers.

- **Add stale issue or PR check workflow** ([#221](https://github.com/NVIDIA/infra-controller/rest-api/pull/221))
  Introduces an automated GitHub workflow that identifies and labels stale issues and pull requests, helping maintain repository hygiene.

- **Add simple Go SDK focused on easier Instance creation** ([#177](https://github.com/NVIDIA/infra-controller/rest-api/pull/177))
  Provides a streamlined Go SDK at `sdk/simple/` with a simplified API surface focused on common Instance creation workflows, complementing the full-featured generated SDK.

### Bug Fixes

- **Added API endpoints for listing InfiniBand and NVLink Interfaces across Instances** ([#218](https://github.com/NVIDIA/infra-controller/rest-api/pull/218))
  Adds cross-instance listing endpoints for InfiniBand and NVLink Interfaces with filtering by Instance ID and partition ID, resolving gaps in network interface discoverability.

- **Invalidate all scope-filtered resource types on scope change in CLI** ([#249](https://github.com/NVIDIA/infra-controller/rest-api/pull/249))
  Fixes stale data in the TUI by invalidating all cached scope-filtered resources when the user changes their site or VPC scope.

- **Include scope args in CLI interactive mode command printout** ([#248](https://github.com/NVIDIA/infra-controller/rest-api/pull/248))
  Scope arguments are now included in the command printout during interactive mode, making it clear which site/VPC context is active.

- **Add mock methods for ExpectedPowerShelf and ExpectedSwitch to NICoTest** ([#239](https://github.com/NVIDIA/infra-controller/rest-api/pull/239))
  Adds the missing mock implementations needed for testing ExpectedPowerShelf and ExpectedSwitch handlers.

- **Unwrap full Temporal error chain in UnwrapWorkflowError** ([#240](https://github.com/NVIDIA/infra-controller/rest-api/pull/240))
  Ensures the complete Temporal error chain is unwrapped in workflow error handling, providing accurate error messages to API callers instead of generic wrapper errors.

- **Use proto field getters in Site workflow logs to prevent nil panics** ([#238](https://github.com/NVIDIA/infra-controller/rest-api/pull/238))
  Replaces direct proto field access with getter methods in Site workflow logging, preventing nil pointer panics when optional fields are absent.

- **Omit terminating DPU Extension Service deployments in update request to Site** ([#219](https://github.com/NVIDIA/infra-controller/rest-api/pull/219))
  Filters out DPU Extension Services that are in a terminating state from update requests sent to the Site Controller, preventing conflicts with in-progress deletions.

- **Handle nil gRPC client in Site Agent workflows** ([#235](https://github.com/NVIDIA/infra-controller/rest-api/pull/235))
  Adds nil checks for the RLA gRPC client in Site Agent workflows, preventing panics when the RLA integration is disabled.

- **Correct NSM binary path in Dockerfile for CI extraction** ([#234](https://github.com/NVIDIA/infra-controller/rest-api/pull/234))
  Fixes the binary path in the NVSwitch Manager Dockerfile to allow CI to correctly extract the built binary for artifact upload.

### Refactoring

- **Rename Bare Metal Manager module/references to NCX Infra Controller** ([#262](https://github.com/NVIDIA/infra-controller/rest-api/pull/262))
  Updates the Go module path and all documentation references from "Bare Metal Manager" to "NCX Infra Controller", reflecting the project's official naming.

- **Update NICo proto in RLA and refactor RLA inventory loop sync** ([#253](https://github.com/NVIDIA/infra-controller/rest-api/pull/253))
  Refreshes proto definitions from bare-metal-manager-core and refactors the inventory sync loop to eliminate a redundant `FindMachinesByIds` call. Firmware version syncing is extracted into a dedicated function.

- **Rename ExpectedPowerShelf and ExpectedSwitch IDs** ([#258](https://github.com/NVIDIA/infra-controller/rest-api/pull/258))
  Renames the `id` fields on ExpectedPowerShelf and ExpectedSwitch to more descriptive names aligned with NICo Core, and adds missing handler tests.

- **Pass a JSON-safe struct to executor instead of domain Rack** ([#236](https://github.com/NVIDIA/infra-controller/rest-api/pull/236))
  Replaces the domain Rack object with a serialization-safe struct when passing data to task executors, preventing JSON marshaling issues in Temporal workflows.

- **Adopt the common.SetupHandler for more handlers** ([#231](https://github.com/NVIDIA/infra-controller/rest-api/pull/231))
  Migrates additional API handlers to use the standardized `common.SetupHandler` pattern, reducing boilerplate and improving consistency across the codebase.

- **Consolidate duplicated database access and Temporal config code** ([#193](https://github.com/NVIDIA/infra-controller/rest-api/pull/193))
  Extracts shared database connection and Temporal configuration code into common packages, eliminating duplication across services.

- **Create wrapped errors instead of formatted ones** ([#229](https://github.com/NVIDIA/infra-controller/rest-api/pull/229))
  Replaces `fmt.Errorf` with `errors.Wrap` for proper error chaining, improving debuggability through preserved error cause chains.

- **Clean up duplicate package imports** ([#227](https://github.com/NVIDIA/infra-controller/rest-api/pull/227))
  Removes redundant package import aliases and consolidates inconsistent import styles across the codebase.

- **Rename legacy cloud-api references to nico-rest-api** ([#226](https://github.com/NVIDIA/infra-controller/rest-api/pull/226))
  Updates remaining references from the legacy "cloud-api" naming to "nico-rest-api" for consistency with the current project identity.

### Documentation

- **Add deployment guide based on latest kustomization logic** ([#228](https://github.com/NVIDIA/infra-controller/rest-api/pull/228))
  Adds a comprehensive deployment installation guide reflecting the current Kustomize-based deployment structure and component dependencies.

- **Add site machineStats to schema** ([#244](https://github.com/NVIDIA/infra-controller/rest-api/pull/244))
  Documents the `machineStats` field in the site response schema, making the machine statistics structure discoverable in the OpenAPI spec.

- **Update auth documentation to match latest deployment manifests** ([#224](https://github.com/NVIDIA/infra-controller/rest-api/pull/224))
  Revises authentication documentation to accurately reflect the current Keycloak deployment configuration and auth flow.

### CI/CD

- **Update CodeRabbit config to prevent PR description update, reduce noise** ([#255](https://github.com/NVIDIA/infra-controller/rest-api/pull/255))
  Disables CodeRabbit's PR description updates that were causing the PR Title Checker workflow to stall on description change events.

- **Enable CodeRabbit PR review** ([#250](https://github.com/NVIDIA/infra-controller/rest-api/pull/250))
  Enables CodeRabbit.ai for automated code reviews on submitted pull requests, replacing intermittent Copilot functionality.

- **Upgrade Trivy action version** ([#222](https://github.com/NVIDIA/infra-controller/rest-api/pull/222))
  Updates the Trivy security scanning action to the latest version for improved vulnerability detection.

- **Fix Trivy update comment logic** ([#210](https://github.com/NVIDIA/infra-controller/rest-api/pull/210))
  Corrects the logic for updating Trivy scan results in PR comments to prevent duplicate comment creation.

### Chores

- **Support helm deploy on KinD in makefile** ([#232](https://github.com/NVIDIA/infra-controller/rest-api/pull/232))
  Adds Makefile targets for deploying the Helm chart to a local KinD cluster, streamlining the local development workflow.

- **Improved handling of a few swallowed errors** ([#230](https://github.com/NVIDIA/infra-controller/rest-api/pull/230))
  Surfaces several previously swallowed errors in handler and workflow code, improving observability of failure conditions.

- **Rename site-agent chart deployment to statefulset** ([#208](https://github.com/NVIDIA/infra-controller/rest-api/pull/208))
  Changes the Site Agent Helm chart from a Deployment to a StatefulSet, better reflecting the agent's stateful operational requirements.

---

## [v1.0.6](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.6)

### Features

- **Disable Image-based OS for Instance creation/update** ([#176](https://github.com/NVIDIA/infra-controller/rest-api/pull/176))
  Temporarily disables image-based OS selection for instance provisioning due to unresolved issues with SOL-based SSH access and URL accessibility from remote sites.

- **Add rla rack create CLI command and examples directory** ([#191](https://github.com/NVIDIA/infra-controller/rest-api/pull/191))
  Introduces a `rla rack create` CLI command that accepts JSON file or raw JSON data input, along with an examples directory containing sample rack configurations for GB200 NVL72 racks.

- **Add RLA IngestRack API for injecting expected components** ([#189](https://github.com/NVIDIA/infra-controller/rest-api/pull/189))
  Implements the rack ingestion feature that reads component data from RLA's database and routes it to the appropriate component manager (NICo for compute, PSM for power shelves), following the existing task framework pattern.

- **Add NVSwitch Manager plugin for RLA** ([#172](https://github.com/NVIDIA/infra-controller/rest-api/pull/172))
  Adds NVSwitch Manager as a new backend for managing NVLink Switch components within the Rack Level Administration system.

- **Add Go sub-module for SDK at sdk/standard/** ([#187](https://github.com/NVIDIA/infra-controller/rest-api/pull/187))
  Creates a standalone Go sub-module for the generated SDK, allowing downstream providers to import the API client without pulling in the full module's heavy dependencies (Postgres, Redis, Temporal, etc.).

- **Added support for explicit VPC ID and VNI** ([#166](https://github.com/NVIDIA/infra-controller/rest-api/pull/166))
  Enables API consumers to specify explicit VPC IDs and VNIs during VPC creation instead of relying solely on auto-allocation. Separates requested VNI from active VNI in the API, database, and workflow layers.

- **Add bmmcli TUI interactive mode with config selector** ([#165](https://github.com/NVIDIA/infra-controller/rest-api/pull/165))
  Introduces an interactive REPL mode for the CLI with multi-environment config switching, inline autocomplete, scope filtering, org switching, and command history. Supports all 20+ resource types with zero external TUI dependencies.

### Bug Fixes

- **UpdateInstance API: Preserve DPU extension fields if unset** ([#214](https://github.com/NVIDIA/infra-controller/rest-api/pull/214))
  Fixes a bug where omitting DPU extension service fields in an Instance update request would remove existing extensions, since the REST API's partial-update semantics weren't properly converting to Core's whole-replace gRPC API. Omitted fields are now preserved, and explicitly empty arrays trigger removal.

- **Set NVLinkPartition status based on response from Site** ([#134](https://github.com/NVIDIA/infra-controller/rest-api/pull/134))
  NVLink Logical Partition creation now correctly persists the status returned by the Site Controller (e.g., "ready") instead of relying on the initial creation state.

- **Updated stats pagination limit, maxAllocatable formula, and decommissioned machine handling** ([#195](https://github.com/NVIDIA/infra-controller/rest-api/pull/195))
  Fixes several issues in instance type statistics: resolves a pagination bug that capped results at 20 items, corrects the maxAllocatable calculation, and excludes decommissioned machines from allocatable counts.

- **Allow longer prefix length for VPC prefixes** ([#179](https://github.com/NVIDIA/infra-controller/rest-api/pull/179))
  Extends the maximum VPC prefix length to allow /31 prefixes, matching the per-instance /31 allocation size used in VPC virtualization.

- **Added missing entries for RLA and PSM in build-push-docker.yaml** ([#181](https://github.com/NVIDIA/infra-controller/rest-api/pull/181))
  Adds the missing Docker build and push entries for RLA and PowerShelf Manager in the CI workflow configuration.

### Refactoring

- **Refactor Firmware and BringUp workflows to rule-based execution** ([#190](https://github.com/NVIDIA/infra-controller/rest-api/pull/190))
  Migrates FirmwareControl and BringUp workflows from hardcoded sequential logic to the same rule-based execution pattern used by PowerControl, extracting shared stage iteration logic into a common helper.

### Documentation

- **Correct schemas for Rack/Tray endpoint response examples in OpenAPI spec** ([#196](https://github.com/NVIDIA/infra-controller/rest-api/pull/196))
  Fixes Location schema references and adds BMC data in component response examples. Also relaxes the CI check to not require SDK regeneration for example-only changes.

### CI/CD

- **Update style check workflow to error on `go fmt` failure** ([#194](https://github.com/NVIDIA/infra-controller/rest-api/pull/194))
  Changes the formatting check from a silent pass to a proper error, ensuring CI fails when code doesn't conform to `go fmt` standards.

### Chores

- **Move dynamic TLS handler to common package** ([#207](https://github.com/NVIDIA/infra-controller/rest-api/pull/207))
  Relocates the dynamic TLS certificate loader from Cert Manager to the common package, making it reusable by the API, Workflow service, and the next-generation Site Agent.

- **Add pull request template** ([#205](https://github.com/NVIDIA/infra-controller/rest-api/pull/205))
  Introduces a standardized pull request template to ensure consistent information is provided by contributors when opening PRs.

- **Remove DB references from Site Agent** ([#203](https://github.com/NVIDIA/infra-controller/rest-api/pull/203))
  Removes residual database references from the Site Agent, which operates as a stateless service with no direct database access.

- **rename bmmcli to cli** ([#188](https://github.com/NVIDIA/infra-controller/rest-api/pull/188))
  Renames the CLI binary from `bmmcli` to `cli`, updating the package name, Makefile target, shell completion scripts, and documentation.

- **Add bare metal manager rest helm chart** ([#186](https://github.com/NVIDIA/infra-controller/rest-api/pull/186))
  Introduces the initial Helm chart for deploying the full NICo REST stack to Kubernetes, including API, workflow, cert-manager, site-agent, and mock-core components.

- **Update publish chart jobs** ([#199](https://github.com/NVIDIA/infra-controller/rest-api/pull/199))
  Updates CI jobs for Helm chart publishing with revised configurations.

- **Update helm jobs to workflow** ([#198](https://github.com/NVIDIA/infra-controller/rest-api/pull/198))
  Migrates Helm deployment jobs to reusable GitHub Actions workflows for better maintainability.

- **Add back kustomization per-components** ([#148](https://github.com/NVIDIA/infra-controller/rest-api/pull/148))
  Restores per-component Kustomize structure and kind cluster setup scripts to closely resemble production deployments, with proper installation ordering and mTLS certificate fetching for Site Agent.

---

## [v1.0.5](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.5) — 2026-03-02

### Features

- **Allow filtering Machines by the presence of associated Instance** ([#180](https://github.com/NVIDIA/infra-controller/rest-api/pull/180))
  Providers and privileged Tenants can now filter the machine list to show only machines with or without associated instances, aiding capacity management.

- **Handle duplicate key error on SSH Key Group creation by switching to sync workflow** ([#113](https://github.com/NVIDIA/infra-controller/rest-api/pull/113))
  Switches SSH Key Group creation from asynchronous to synchronous workflow execution, enabling proper error handling for duplicate key conflicts and intelligent retry logic.

- **Allow providers to filter Machines by Tenant IDs** ([#168](https://github.com/NVIDIA/infra-controller/rest-api/pull/168))
  Adds a Tenant ID filter to the Machine list endpoint, allowing Providers to quickly determine which machines are currently allocated to a specific Tenant.

- **Add RLA rule engine logic** ([#167](https://github.com/NVIDIA/infra-controller/rest-api/pull/167))
  Introduces the rule engine for RLA task execution, enabling configurable operation sequences for rack-level operations with support for custom step ordering and conditional execution.

- **Add Powershelf Manager service as a component** ([#161](https://github.com/NVIDIA/infra-controller/rest-api/pull/161))
  Integrates the PowerShelf Manager (PSM) service into the repository as a first-class component manager for power shelf hardware.

- **Add Rack Level Administration site config flag to gate RLA endpoints** ([#162](https://github.com/NVIDIA/infra-controller/rest-api/pull/162))
  Adds a `RackLevelAdministration` boolean flag to site configuration that controls access to all rack and tray API endpoints, returning 412 Precondition Failed when not enabled.

- **Add API endpoints for RLA power control and firmware update** ([#157](https://github.com/NVIDIA/infra-controller/rest-api/pull/157))
  Introduces eight new REST API endpoints for power control and firmware upgrade operations on racks and trays, supporting both single-resource and batch operations with configurable power states.

- **Add site-level stats endpoints for GPU, instance type, and tenant allocation** ([#137](https://github.com/NVIDIA/infra-controller/rest-api/pull/137))
  Exposes aggregate statistics at the site level for GPU utilization, instance type allocation, and per-tenant machine assignment.

- **Add bmmcli — OpenAPI-driven CLI for Bare Metal Manager REST API** ([#160](https://github.com/NVIDIA/infra-controller/rest-api/pull/160))
  Introduces a fully OpenAPI-driven CLI that dynamically builds commands from the embedded API spec at startup, covering all 124 operations. Features include OIDC login, NGC API key exchange, auto-pagination, and multiple output formats.

- **Add Tray validation REST API endpoints** ([#156](https://github.com/NVIDIA/infra-controller/rest-api/pull/156))
  Adds tray validation endpoints that compare expected versus actual component state through RLA, following the same pattern as existing rack validation.

- **Add unknown query parameter validation for handlers** ([#149](https://github.com/NVIDIA/infra-controller/rest-api/pull/149))
  Rejects requests containing unrecognized query parameters with a 400 Bad Request, preventing typos from being silently ignored and improving workflow deduplication accuracy.

- **Add API models and endpoints for Tray management** ([#128](https://github.com/NVIDIA/infra-controller/rest-api/pull/128))
  Introduces GET endpoints for retrieving individual trays and listing trays with rich filtering support (by rack, type, component ID, and task ID), bridging the REST API with RLA's tray data.

- **Add rack validate REST API endpoints** ([#122](https://github.com/NVIDIA/infra-controller/rest-api/pull/122))
  Adds rack validation endpoints that invoke RLA's ValidateRackComponents workflow to verify expected versus actual component state.

- **Add labels support in InstanceType** ([#102](https://github.com/NVIDIA/infra-controller/rest-api/pull/102))
  Adds label (key-value metadata) support to Instance Types in both the API and database models, enabling richer categorization and filtering.

- **Relocate RLA code to bare-metal-manager-rest** ([#119](https://github.com/NVIDIA/infra-controller/rest-api/pull/119))
  Transfers the Rack Level Administration codebase from its internal repository into the main nico-rest repo, consolidating all management services in one place.

- **Add generated Golang client for OpenAPI spec** ([#129](https://github.com/NVIDIA/infra-controller/rest-api/pull/129))
  Generates and checks in a Go API client from the OpenAPI specification, providing the foundation for the CLI tool implementation.

### Bug Fixes

- **Aligned InfiniBand Partition proto snapshot with Core** ([#164](https://github.com/NVIDIA/infra-controller/rest-api/pull/164))
  Synchronizes the InfiniBand Partition protobuf definitions with NICo Core, fixing an invalid pkey value error caused by proto misalignment.

- **Unify machine status breakdown into single reusable type** ([#171](https://github.com/NVIDIA/infra-controller/rest-api/pull/171))
  Consolidates multiple machine status breakdown representations into a single reusable type, eliminating inconsistencies across different API endpoints.

- **Replace nico with NICo in Expected Machine/Audit API route prefix** ([#158](https://github.com/NVIDIA/infra-controller/rest-api/pull/158))
  Updates the legacy `/nico/` route prefix to `/nico/` for Expected Machine and Audit endpoints across the OpenAPI spec, SDK, and handler code.

- **Correct binary_name and binary_path for nico-rla build** ([#153](https://github.com/NVIDIA/infra-controller/rest-api/pull/153))
  Fixes a copy-paste error where the RLA build job used the cert-manager binary path, causing CI to fail when extracting the built binary.

- **Verify if VPC Prefixes are present before allowing network Allocation deletion** ([#132](https://github.com/NVIDIA/infra-controller/rest-api/pull/132))
  Adds a safety check to prevent deletion of network Allocations that still have associated VPC Prefixes, avoiding orphaned network resources.

- **Resolve data race in RLA workerpool metrics** ([#145](https://github.com/NVIDIA/infra-controller/rest-api/pull/145))
  Fixes a data race condition in RLA's workerpool metrics by using consistent atomic synchronization, resolving intermittent test failures.

### Documentation

- **Fix link to Core repo in README** ([#146](https://github.com/NVIDIA/infra-controller/rest-api/pull/146))
  Corrects a broken documentation link to the BMM Core repository in the README.

- **Fix invalid NVLink Interface order, DPU Extension Service delete code in schema** ([#150](https://github.com/NVIDIA/infra-controller/rest-api/pull/150))
  Corrects ordering and response code errors in the OpenAPI schema for NVLink Interface and DPU Extension Service endpoints.

- **Fix NVLink Interfaces endpoint parameters** ([#147](https://github.com/NVIDIA/infra-controller/rest-api/pull/147))
  Corrects parameter definitions for NVLink Interface endpoints in the OpenAPI schema.

- **Fix schema tag for NVLink Interfaces endpoint** ([#141](https://github.com/NVIDIA/infra-controller/rest-api/pull/141))
  Fixes incorrect schema tags on the NVLink Interfaces endpoint in the OpenAPI specification.

### CI/CD

- **Add check to ensure SDK and docs are regenerated when OpenAPI spec changes** ([#174](https://github.com/NVIDIA/infra-controller/rest-api/pull/174))
  Adds a CI check that validates generated SDK files and documentation stay in sync with OpenAPI spec changes, with clear guidance when regeneration is needed.

- **Remove duplicate Slack notification trigger in new PR workflow** ([#159](https://github.com/NVIDIA/infra-controller/rest-api/pull/159))
  Removes the redundant `pull_request` trigger that was duplicating Slack notifications already handled by `pull_request_target`.

- **Enable Slack notification for all PRs** ([#152](https://github.com/NVIDIA/infra-controller/rest-api/pull/152))
  Switches to `pull_request_target` to safely support forked PR notifications, updates the Slack message format, and adds automatic cancellation of superseded CI pipelines.

### Chores

- **Allow privileged Tenants to retrieve all Sites** ([#144](https://github.com/NVIDIA/infra-controller/rest-api/pull/144))
  Enables Tenants with targeted Instance creation capability to retrieve all Sites owned by Providers they have Tenant Accounts with.

- **Consolidate kustomize objects into nico-rest namespace** ([#140](https://github.com/NVIDIA/infra-controller/rest-api/pull/140))
  Consolidates all Kustomize objects into a single `nico-rest` namespace for cleaner deployment organization.

- **Update RLA Dockerfile ldflags to use bare-metal-manager-rest module path** ([#136](https://github.com/NVIDIA/infra-controller/rest-api/pull/136))
  Updates Go linker flags in the RLA Dockerfile to use the correct module path after the repository migration.

- **Release SDK for version 1.0.4** ([#142](https://github.com/NVIDIA/infra-controller/rest-api/pull/142))
  Publishes the generated SDK matching the v1.0.4 API surface.

- **Fix role name and grammar in OpenAPI spec** ([#139](https://github.com/NVIDIA/infra-controller/rest-api/pull/139))
  Corrects the Provider viewer role name and fixes grammatical errors in the OpenAPI specification.

- **Rename local deployment elements** ([#138](https://github.com/NVIDIA/infra-controller/rest-api/pull/138))
  Renames internal deployment components (elektraserver to mock-core, cluster name to nico-rest-local) for clearer naming.

- **Remove vault references in setup** ([#135](https://github.com/NVIDIA/infra-controller/rest-api/pull/135))
  Cleans up remaining Vault references from the local setup scripts after the migration to native Go PKI.

---

## [v1.0.4-rc1](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.4-rc1) — 2026-03-05

### Bug Fixes

- **Aligned InfiniBand Partition proto snapshot with Core** ([#164](https://github.com/NVIDIA/infra-controller/rest-api/pull/164))
  Backport of the InfiniBand Partition proto alignment fix from v1.0.5, resolving pkey value parsing errors caused by proto definition mismatches with NICo Core.

---

## [v1.0.4](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.4) — 2026-02-17

### CI/CD

- **Fix version extraction from VERSION file with copyright notice** ([#127](https://github.com/NVIDIA/infra-controller/rest-api/pull/127))
  Corrects the CI version extraction logic that broke after a copyright notice was added to the VERSION file, ensuring the build pipeline correctly parses the semantic version.

---

## [v1.0.3](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.3) — 2026-02-13

### Features

- **Add make command to publish rendered OpenAPI schema to docs/pages** ([#116](https://github.com/NVIDIA/infra-controller/rest-api/pull/116))
  Adds a `make publish-openapi` command that renders the OpenAPI schema to GitHub Pages, providing a publicly accessible API reference.

- **Add support to filter sites by NVLink Partition** ([#101](https://github.com/NVIDIA/infra-controller/rest-api/pull/101))
  Enables filtering the Sites list by NVLink Partition, helping users locate sites with specific NVLink configurations.

- **Add API models and endpoints for Rack management** ([#79](https://github.com/NVIDIA/infra-controller/rest-api/pull/79))
  Introduces REST API endpoints for reading rack inventory via RLA, including single-rack retrieval and list operations with Temporal-based site routing.

- **Use NVLink domain ID instead of rack for batch instance topology optimization** ([#90](https://github.com/NVIDIA/infra-controller/rest-api/pull/90))
  Switches batch instance creation topology optimization from rack-based placement to NVLink domain ID-based placement, improving GPU interconnect locality.

- **Replace embedded Vault in Cert Manager with native Go PKI** ([#59](https://github.com/NVIDIA/infra-controller/rest-api/pull/59))
  Eliminates the embedded Vault sidecar in the cert-manager pod by implementing native Go PKI using `crypto/x509`. Reduces the pod from 4 containers to 1, saving ~150MB memory and removing Vault initialization overhead while maintaining the same security model.

- **Check NVLink, multi-DPU or InfiniBand capability of Machine in Instance create/update** ([#84](https://github.com/NVIDIA/infra-controller/rest-api/pull/84))
  Validates that the selected machine's hardware capabilities match the requested NVLink, multi-DPU, or InfiniBand interfaces, falling back to machine-level capability checks when the Instance Type alone doesn't satisfy the requirements.

- **InfiniBand partition metadata support** ([#69](https://github.com/NVIDIA/infra-controller/rest-api/pull/69))
  Adds labels/metadata support to InfiniBand Partition resources in the API, database, and workflow schema, enabling custom key-value annotation of IB partitions.

- **Allow filtering NVLink Interface API endpoint by Instance ID/Logical Partition ID** ([#89](https://github.com/NVIDIA/infra-controller/rest-api/pull/89))
  Extends the NVLink Interface list endpoint with filters for Instance ID, Logical Partition ID, and Domain ID, including instance and partition summaries in responses.

- **Add RLA gRPC client in Site Agent and Site Workflow** ([#67](https://github.com/NVIDIA/infra-controller/rest-api/pull/67))
  Introduces the RLA gRPC client in the Site Agent and Site Workflow services with TLS and certificate hot-reload support. RLA is disabled by default via `RLA_ENABLED=false` for safe incremental rollout.

- **Remove ElasticSearch Temporal dependency from local kind deployment** ([#95](https://github.com/NVIDIA/infra-controller/rest-api/pull/95))
  Removes the ElasticSearch dependency from the local KinD Temporal deployment, reducing resource requirements for local development environments.

### Bug Fixes

- **Prevent Instance update when missing on Site** ([#124](https://github.com/NVIDIA/infra-controller/rest-api/pull/124))
  Blocks Instance update requests when the instance is marked as missing on Site, since the Site Controller cannot apply updates to unreachable instances. Deletion is still permitted.

- **Switch license for Rack management files from NVIDIA to Apache 2.0** ([#118](https://github.com/NVIDIA/infra-controller/rest-api/pull/118))
  Corrects the license headers on rack management files from NVIDIA proprietary to Apache 2.0, aligning with the repository's open-source license.

- **Limit name uniqueness to per Site, per Tenant when creating InfiniBand Partition** ([#107](https://github.com/NVIDIA/infra-controller/rest-api/pull/107))
  Relaxes the global uniqueness constraint on InfiniBand Partition names to be scoped per Site per Tenant, allowing different tenants to use the same partition name on the same or different sites.

- **Revise DPU Extension Service deletion and version deletion logic** ([#110](https://github.com/NVIDIA/infra-controller/rest-api/pull/110))
  Fixes multiple issues in DPU Extension Service lifecycle: deletion now immediately updates the cache instead of waiting for inventory sync, and deleting the latest version correctly falls back to the most recent older version.

- **Give execution permission to the setup-local.sh script** ([#114](https://github.com/NVIDIA/infra-controller/rest-api/pull/114))
  Adds execute permission to the local setup script, fixing `make kind-reset` failures.

- **Increase Keycloak memory limit** ([#112](https://github.com/NVIDIA/infra-controller/rest-api/pull/112))
  Increases the Keycloak container memory limit from 1 GiB to 3 GiB to prevent OOM kills during startup on systems with certain memory configurations.

- **Use 127.0.0.1 in the output of make preview-openapi** ([#111](https://github.com/NVIDIA/infra-controller/rest-api/pull/111))
  Replaces `localhost` with `127.0.0.1` in the preview URL output to avoid connection failures on systems that resolve localhost to IPv6 `::1`.

- **Fixed OpenAPI schema issues, added validation scripts** ([#105](https://github.com/NVIDIA/infra-controller/rest-api/pull/105))
  Fixes numerous issues in the OpenAPI schema and adds `make lint-openapi` and `make preview-openapi` commands for ongoing schema validation.

- **Send description along with create and update NVLink partition request** ([#87](https://github.com/NVIDIA/infra-controller/rest-api/pull/87))
  Includes the description field in NVLink Logical Partition create and update requests to the Site Controller, fixing an "organization ID should not be updated" error.

- **Return Machine board serial, product name and vendor in DMI data** ([#97](https://github.com/NVIDIA/infra-controller/rest-api/pull/97))
  Exposes board serial number, product name, and vendor from DMI data in Machine responses, with backward-compatible partial deserialization for older machine records.

- **Check if NVLink Logical Partition is already connected on Instance NVLink Interface update** ([#83](https://github.com/NVIDIA/infra-controller/rest-api/pull/83))
  Adds validation to verify whether a provided NVLink Logical Partition ID matches an already-configured partition before attempting an update, preventing conflicting partition assignments.

### CI/CD

- **Enforce PR title format** ([#100](https://github.com/NVIDIA/infra-controller/rest-api/pull/100))
  Adds a CI check that validates PR titles follow the conventional commit format with a lowercase category prefix and a capitalized description of at least 20 characters.

### Chores

- **Update API name config, README for OpenAPI schema development** ([#117](https://github.com/NVIDIA/infra-controller/rest-api/pull/117))
  Adds the missing API name configuration and comprehensive OpenAPI schema development instructions.

- **Update core proto generation command in Makefile** ([#109](https://github.com/NVIDIA/infra-controller/rest-api/pull/109))
  Updates the Makefile command for regenerating Core protobuf definitions.

- **Update Github Go module path to bare-metal-manager-rest** ([#108](https://github.com/NVIDIA/infra-controller/rest-api/pull/108))
  Migrates the Go module path to the new GitHub repository location.

- **Start renaming NICo to NVIDIA Metal Manager** ([#99](https://github.com/NVIDIA/infra-controller/rest-api/pull/99))
  Begins the rebranding effort from NICo to NVIDIA Metal Manager across documentation and configuration.

- **Regenerate third party license file** ([#103](https://github.com/NVIDIA/infra-controller/rest-api/pull/103))
  Regenerates the third-party license file to reflect current dependency state.

- **Update license file to Apache 2.0, remove NVIDIA license file headers** ([#93](https://github.com/NVIDIA/infra-controller/rest-api/pull/93))
  Converts the project license to Apache 2.0 and removes proprietary NVIDIA license headers from source files.

- **Update Temporal and Postgres URLs to use local DNS** ([#98](https://github.com/NVIDIA/infra-controller/rest-api/pull/98))
  Updates service URLs to use Kubernetes local DNS names for improved reliability in local development environments.

- **Add GitHub interaction templates and code of conduct** ([#96](https://github.com/NVIDIA/infra-controller/rest-api/pull/96))
  Adds GitHub issue templates, discussion templates, and a code of conduct to standardize community interactions.

- **Upgrade otelecho to v0.65.0 to fix security vulnerability** ([#91](https://github.com/NVIDIA/infra-controller/rest-api/pull/91))
  Updates the OpenTelemetry Echo middleware to address a known security vulnerability.

- **Remove Kubernetes files** ([#94](https://github.com/NVIDIA/infra-controller/rest-api/pull/94))
  Removes legacy Kubernetes manifest files that have been superseded by Helm charts and Kustomize.

---

## [v1.0.2](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.2) — 2026-01-29

### CI/CD

- **Use revised matching regex for release tags** ([#81](https://github.com/NVIDIA/infra-controller/rest-api/pull/81))
  Corrects the regex pattern used for matching release version tags in CI workflows, ensuring proper triggering of release pipelines.

---

## [v1.0.1](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.1) — 2026-01-28

### Features

- **Add proto and protobuf for RLA gRPC API** ([#66](https://github.com/NVIDIA/infra-controller/rest-api/pull/66))
  Introduces the protobuf definitions and generated code for the RLA gRPC API, establishing a clear separation between RLA and NICo Core proto structures.

- **Support custom claims for JWT issuers** ([#41](https://github.com/NVIDIA/infra-controller/rest-api/pull/41))
  Adds comprehensive custom JWT issuer support with multiple claim mapping strategies: static org with static roles, service accounts, dynamic roles from token attributes, and fully dynamic org/role extraction. Includes parallel JWKS fetching, configurable timeouts, and stricter validation rules.

- **Add schema for Instance batch API** ([#61](https://github.com/NVIDIA/infra-controller/rest-api/pull/61))
  Adds OpenAPI schema documentation for the instance batch creation API endpoint.

- **Enable license scanning** ([#64](https://github.com/NVIDIA/infra-controller/rest-api/pull/64))
  Adds automated license scanning to the CI pipeline for dependency compliance verification.

### Bug Fixes

- **Revise NVLink Logical Partition inventory metadata update logic** ([#74](https://github.com/NVIDIA/infra-controller/rest-api/pull/74))
  Fixes a nil pointer exception caused by incorrect placement of the nil evaluation in NVLink Logical Partition metadata update code.

- **Missing body close on HTTP responses** ([#71](https://github.com/NVIDIA/infra-controller/rest-api/pull/71))
  Adds deferred `Body.Close()` calls on HTTP responses in Slack notification code, preventing resource leaks.

- **Verify provided version in DPU Extension Service active version list for deletion** ([#63](https://github.com/NVIDIA/infra-controller/rest-api/pull/63))
  Validates that the specified DPU Extension Service version exists in the active version list before allowing deletion, preventing attempts to delete non-existent versions.

### CI/CD

- **Fix docker image artifact path in upload workflow** ([#78](https://github.com/NVIDIA/infra-controller/rest-api/pull/78))
  Aligns the Docker image artifact path in the upload workflow with the actual location of built images.

- **Fix docker image tag for artifact extraction/upload in workflows** ([#77](https://github.com/NVIDIA/infra-controller/rest-api/pull/77))
  Corrects Docker image tags used during artifact extraction to match the updated image naming convention.

- **Add pre-commit for secrets scanning** ([#68](https://github.com/NVIDIA/infra-controller/rest-api/pull/68))
  Integrates TruffleHog as a pre-commit hook for automated secrets scanning, preventing accidental credential commits.

### Chores

- **Update scanner actions with improved PR comment handling** ([#75](https://github.com/NVIDIA/infra-controller/rest-api/pull/75))
  Security scanners (TruffleHog, Trivy, CodeQL) now update existing PR comments instead of creating duplicates, and only post when scan status changes.

- **Refactor and optimize Instance API handlers** ([#60](https://github.com/NVIDIA/infra-controller/rest-api/pull/60))
  Converts N+1 individual `GetByID` calls to efficient batch `GetAll` queries for subnets, VPC prefixes, partitions, and DPU extensions in the instance update handler. Adds bulk NVLink Interface status update support.

- **Use non-deprecated UserData field for OS create/update** (no PR)
  Migrates site-agent from the deprecated UserData field to the canonical one that applies across all OS types.

- **Revise docker image push policy** ([#62](https://github.com/NVIDIA/infra-controller/rest-api/pull/62))
  Implements a structured Docker tagging policy: `VERSION-SHA` for main branch, `latest` for most recent main build, version tags for releases, and branch-name tags for feature branches with opt-in `push-container` keyword.

---

## [v1.0.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v1.0.0) — 2026-01-20

### Features

- **Add support for bulk creation/update of Expected Machines** ([#35](https://github.com/NVIDIA/infra-controller/rest-api/pull/35))
  Introduces batch API endpoints for creating and updating Expected Machine entries, enabling efficient onboarding of large hardware inventories in a single request.

- **Add unique DB index for Expected Machine BMC MAC address per Site** ([#36](https://github.com/NVIDIA/infra-controller/rest-api/pull/36))
  Adds a unique database index on BMC MAC addresses scoped per Site, preventing duplicate machine registration and ensuring data integrity during bulk imports.

### Bug Fixes

- **Embed Vault in cert-manager pod** ([#50](https://github.com/NVIDIA/infra-controller/rest-api/pull/50))
  Moves Vault from a standalone deployment to a sidecar container in the cert-manager pod, and deploys cert-manager.io with a Vault ClusterIssuer. Site-manager TLS certificates are now issued through cert-manager.io instead of init containers.

- **Retry local site creation during setup** ([#45](https://github.com/NVIDIA/infra-controller/rest-api/pull/45))
  Adds retry logic for local site creation during `make kind-reset`, handling the race condition where site-manager may not be fully ready when the setup script runs.

- **Add name as orderBy field in DpuExtensionService** ([#53](https://github.com/NVIDIA/infra-controller/rest-api/pull/53))
  Adds `name` as a valid ordering field for DPU Extension Service list queries.

- **Update OpenAPI spec with NVLink schema and DPU Extension Service modifications** ([#52](https://github.com/NVIDIA/infra-controller/rest-api/pull/52))
  Updates the OpenAPI schema to include NVLink-related schemas and corrects DPU Extension Service endpoint definitions.

- **Make sure NVLink interface is marked as Pending when sending update request** ([#40](https://github.com/NVIDIA/infra-controller/rest-api/pull/40))
  Ensures newly added NVLink interfaces are set to Pending status when included in an update request, correctly reflecting their provisioning state.

- **Fix inconsistent test data for MachineCapability GetAll tests** ([#34](https://github.com/NVIDIA/infra-controller/rest-api/pull/34))
  Corrects test fixtures that were producing inconsistent results in MachineCapability GetAll test cases.

- **Wait for mock NICo server to be up before running site-agent tests** ([#37](https://github.com/NVIDIA/infra-controller/rest-api/pull/37))
  Fixes flaky site-agent tests by ensuring the mock NICo gRPC server is fully started before test execution begins, resolving ~50% test failure rates on some machines.

- **Only run promotion workflow on main branch** ([#42](https://github.com/NVIDIA/infra-controller/rest-api/pull/42))
  Restricts the release promotion job to the main branch, preventing unmerged PR branches from showing a perpetually pending "Promote to Release Candidate" check.

- **Updated Go version to 1.25.11 and fixed unit tests** ([#31](https://github.com/NVIDIA/infra-controller/rest-api/pull/31))
  Upgrades the Go toolchain to 1.25.11 across all modules and fixes unit tests affected by the version change.

- **Add golangci-lint and revive config files** ([#30](https://github.com/NVIDIA/infra-controller/rest-api/pull/30))
  Adds the previously missing golangci-lint and revive configuration files required for consistent linting across the project.

- **Revise Slack notification payload for new PR** ([#57](https://github.com/NVIDIA/infra-controller/rest-api/pull/57))
  Fixes the Slack notification message format for new pull request events.

### Chores

- **Update API version in schema, add local preview script** ([#44](https://github.com/NVIDIA/infra-controller/rest-api/pull/44))
  Updates the API version in the OpenAPI schema and adds a local Redoc preview script for browsing the rendered API documentation.

- **Enable binary/docker build for all branches, add security scans** ([#54](https://github.com/NVIDIA/infra-controller/rest-api/pull/54))
  Extends CI to build binaries and Docker images on all branches (push-only for main/release/tags) and adds TruffleHog, Trivy, and CodeQL security scanning.

- **Do not accept all whitespace strings in Expected Machine attributes and labels** ([#49](https://github.com/NVIDIA/infra-controller/rest-api/pull/49))
  Rejects all-whitespace strings for Expected Machine attributes and label keys, preventing accidental entry of blank values.

- **Improve SiteID validation for APIExpectedMachineUpdateRequest** ([#46](https://github.com/NVIDIA/infra-controller/rest-api/pull/46))
  Strengthens SiteID validation in Expected Machine update requests and updates the OpenAPI schema with batch operation documentation.

- **Set up binaries build action and build condition** ([#43](https://github.com/NVIDIA/infra-controller/rest-api/pull/43))
  Adds a dedicated GitHub Actions workflow for cross-platform Go binary builds (linux/amd64, linux/arm64, darwin/arm64) with selective triggering on main, release branches, and tags.

---

## [v0.1.0](https://github.com/NVIDIA/infra-controller/rest-api/releases/tag/v0.1.0) — 2026-01-09

### Features

- **Initial NICo REST release for GitHub** (no PR)
  The foundational release of NCX Infra Controller REST, establishing the multi-tenant REST API for bare-metal lifecycle management. Includes the core API server, Temporal workflow engine integration, site management, certificate management, authentication via Keycloak/JWT, database layer with PostgreSQL, and the initial OpenAPI specification.
