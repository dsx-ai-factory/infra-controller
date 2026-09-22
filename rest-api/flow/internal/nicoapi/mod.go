// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package nicoapi abstracts the GRPC interface used to communicate with nico-core-api.  New connection pools can be created with
// NewClient to create a real client or NewMockClient which fakes everything for unit tests.

package nicoapi

import (
	"context"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Client allow us to have both a real implemenation and a mock implementation for unit tests which can be switched transparently
type Client interface {
	Version(ctx context.Context) (string, error)
	GetMachines(ctx context.Context) ([]MachineDetail, error)
	GetLeakingMachineIds(ctx context.Context) ([]string, error)
	GetLeakingSwitchIds(ctx context.Context) ([]string, error)
	GetPowerStates(ctx context.Context, machineIds []string) (ret []MachinePowerState, err error)
	SetFirmwareUpdateTimeWindow(ctx context.Context, machineIds []string, startTime, endTime time.Time) error
	// FindInterfaces returns all machine interfaces known by nico-core-api, keyed by MAC address
	FindInterfaces(ctx context.Context) (map[string]MachineInterface, error)

	// AdminPowerControl performs power control operations on a machine
	AdminPowerControl(ctx context.Context, machineID string, action SystemPowerControl) error

	// UpdatePowerOption sets the desired power state for a machine in NICo's power manager.
	// This controls NICo's power-on gate: setting desired state to On allows
	// the machine to power on, while Off or Disabled prevents it.
	UpdatePowerOption(ctx context.Context, machineID string, desiredState PowerState) error

	// FindMachinesByIds returns detailed machine information for the given machine IDs
	FindMachinesByIds(ctx context.Context, machineIds []string) ([]MachineDetail, error)

	// FindHostMachineIdsByRack returns the IDs of host (non-DPU) machines that
	// belong to the given rack. Empty rackID is rejected. Returns nil when the
	// rack has no host machines.
	FindHostMachineIdsByRack(ctx context.Context, rackID string) ([]string, error)

	// FindSwitchRackIDs returns the mapping from switch ID to rack ID for the
	// given switches. A switch without a rack assignment is omitted from the
	// result rather than reported as an empty string.
	FindSwitchRackIDs(ctx context.Context, switchIds []string) (map[string]string, error)

	// FindPowerShelfRackIDs returns the mapping from power-shelf ID to rack ID
	// for the given shelves. A shelf without a rack assignment is omitted from
	// the result rather than reported as an empty string.
	FindPowerShelfRackIDs(ctx context.Context, shelfIds []string) (map[string]string, error)

	// FindSwitchControllerStates returns the raw controller_state string Core
	// reports for each switch. The value is the JSON-tagged form emitted by
	// core (e.g. `{"state":"ready"}`); decoding is the caller's job. Switches
	// for which Core returns no controller_state are omitted from the result.
	FindSwitchControllerStates(ctx context.Context, switchIds []string) (map[string]string, error)

	// FindSwitchNvosIPs returns the resolved NVOS host IP for each switch,
	// keyed by Core SwitchId. Core populates nvos_info only once both the NVOS
	// MAC and its assigned address resolve, so switches without a resolved NVOS
	// endpoint are omitted from the result.
	FindSwitchNvosIPs(ctx context.Context, switchIds []string) (map[string]string, error)

	// GetObservedNVLinkDomainMemberships returns valid rack/domain observations
	// from a complete snapshot of Core's active switch inventory. Switches without
	// a valid observation are omitted, so callers may clear omitted rack
	// memberships after a successful read.
	GetObservedNVLinkDomainMemberships(ctx context.Context) ([]NVLinkDomainMembership, error)

	// FindPowerShelfControllerStates is the power-shelf equivalent of
	// FindSwitchControllerStates.
	FindPowerShelfControllerStates(ctx context.Context, shelfIds []string) (map[string]string, error)

	// GetSwitches returns a complete snapshot of active Core switches with the
	// runtime ID and BMC MAC needed for actual-inventory reconciliation.
	GetSwitches(ctx context.Context) ([]ObservedControllerDevice, error)

	// GetPowerShelves is the power-shelf equivalent of GetSwitches.
	GetPowerShelves(ctx context.Context) ([]ObservedControllerDevice, error)

	// GetMachinePositionInfo returns position information for the given machine IDs
	GetMachinePositionInfo(ctx context.Context, machineIds []string) ([]MachinePosition, error)

	// AllowIngestionAndPowerOn opens NICo's power-on gate for a BMC endpoint,
	// allowing the machine to be ingested and powered on.
	AllowIngestionAndPowerOn(ctx context.Context, bmcIP string, bmcMAC string) error

	// DetermineMachineIngestionState returns the bring-up state of a machine
	// relative to NICo's power-on gate.
	DetermineMachineIngestionState(ctx context.Context, bmcIP string, bmcMAC string) (BringUpState, error) //nolint

	// AddExpectedMachine registers an expected machine with NICo for ingestion.
	AddExpectedMachine(ctx context.Context, req AddExpectedMachineRequest) error

	// GetAllExpectedSwitches returns all expected switches registered with NICo,
	// keyed by BMC MAC address, including metadata (e.g., "host_mac_address" for the NVOS MAC).
	GetAllExpectedSwitches(ctx context.Context) (map[string]ExpectedSwitchInfo, error)

	// AddExpectedSwitch registers an expected switch with NICo for ingestion.
	AddExpectedSwitch(ctx context.Context, req AddExpectedSwitchRequest) error

	// AddExpectedPowerShelf registers an expected power shelf with NICo for ingestion.
	AddExpectedPowerShelf(ctx context.Context, req AddExpectedPowerShelfRequest) error

	// InsertHealthReportOverride inserts a health-report override for a machine
	// (replaces the deprecated SetMaintenance RPC).
	InsertHealthReportOverride(ctx context.Context, machineID string, source string) error

	// RemoveHealthReportOverride removes a previously inserted health-report override.
	RemoveHealthReportOverride(ctx context.Context, machineID string, source string) error

	// InsertHostUpdateInProgressHealthOverride inserts the specific
	// "HostUpdateInProgress" health alert with the "PreventAllocations"
	// classification that Core's `TriggerDpuReprovisioning` requires as a
	// precondition (see crates/api-core/src/handlers/dpu.rs::trigger_dpu_reprovisioning).
	//
	// The plain InsertHealthReportOverride above writes a different alert
	// id ("Maintenance" + "SuppressExternalAlerting") used by power-cycle
	// flows; that alert does NOT satisfy the DPU reprov precondition, so
	// callers initiating DPU reprovisioning MUST use this method instead.
	//
	// `message` is recorded as the alert's human-readable message and is
	// the only caller-supplied free-form field; everything else is fixed
	// to the canonical (id, source, classifications) tuple Core expects.
	InsertHostUpdateInProgressHealthOverride(ctx context.Context, machineID string, message string) error

	// RemoveHostUpdateInProgressHealthOverride removes the override
	// inserted by InsertHostUpdateInProgressHealthOverride. Idempotent:
	// removing an override that does not exist is treated as success so
	// callers can run this from `defer` cleanup blocks without special-
	// casing the "we never managed to insert it" branch.
	RemoveHostUpdateInProgressHealthOverride(ctx context.Context, machineID string) error

	// TriggerDpuReprovisioning sets the reprovisioning request flag on
	// either a single DPU machine (when machineID is a DPU machine id) or
	// every DPU attached to a host (when machineID is a host machine id).
	// Initiator is fixed to AdminCli and Mode is fixed to Set; the only
	// caller-controlled knob is whether the reprovisioning should also
	// roll the DPU NIC firmware to the site-configured target version.
	//
	// The host MUST already carry the HostUpdateInProgress /
	// PreventAllocations health alert (see
	// InsertHostUpdateInProgressHealthOverride) or Core rejects the call
	// with InvalidArgument. This method does NOT power-cycle the host;
	// the caller is responsible for triggering the actual reboot via
	// InvokeInstancePower (when a tenant instance owns the host) or via
	// AdminPowerControl (when the host is unassigned).
	TriggerDpuReprovisioning(ctx context.Context, machineID string, updateFirmware bool) error

	// IsDpuReprovisioningPendingForHost returns true if any DPU attached
	// to the given host machine is currently listed by
	// ListDpuWaitingForReprovisioning. This is the polling hook Flow uses
	// to wait for completion: a DPU disappears from the list once Core
	// finishes reprovisioning it (success or failure resets the flag).
	IsDpuReprovisioningPendingForHost(ctx context.Context, hostMachineID string) (bool, error)

	// FindAssociatedDpuMachineIds returns the DPU machine ids attached to
	// a given host machine, taken from the host Machine's
	// `associated_dpu_machine_ids` field. Returns an empty slice (not an
	// error) when the host has no DPUs; callers can use this for an
	// early "host has no DPUs to reprov" guard before trying to mutate
	// state.
	FindAssociatedDpuMachineIds(ctx context.Context, hostMachineID string) ([]string, error)

	// FindInstanceIdByMachineId returns the tenant instance id currently
	// attached to the given host machine, or "" when the host is in a
	// non-Assigned state (no instance attached). The host's instance id
	// is the argument required by InvokeInstancePower.
	FindInstanceIdByMachineId(ctx context.Context, machineID string) (string, error)

	// InvokeInstancePower issues a POWER_RESET against a tenant instance.
	// When applyUpdates is true, Core sets `user_approval_received` so
	// any pending reprovisioning / firmware update is allowed to proceed
	// during the reboot — this is the gate the DPU reprovisioning state
	// machine waits on for hosts in Assigned/* lifecycle.
	InvokeInstancePower(ctx context.Context, instanceID string, applyUpdates bool) error

	// ComponentPowerControl performs power control on component targets (switches, power shelves).
	ComponentPowerControl(ctx context.Context, req *corev1.ComponentPowerControlRequest) (*corev1.ComponentPowerControlResponse, error)

	// UpdateComponentFirmware queues firmware updates for component targets.
	UpdateComponentFirmware(ctx context.Context, req *corev1.UpdateComponentFirmwareRequest) (*corev1.UpdateComponentFirmwareResponse, error)

	// GetComponentFirmwareStatus returns firmware update status for component targets.
	GetComponentFirmwareStatus(ctx context.Context, req *corev1.GetComponentFirmwareStatusRequest) (*corev1.GetComponentFirmwareStatusResponse, error)

	// ListComponentFirmwareVersions lists available firmware versions for component targets.
	ListComponentFirmwareVersions(ctx context.Context, req *corev1.ListComponentFirmwareVersionsRequest) (*corev1.ListComponentFirmwareVersionsResponse, error)

	// GetComponentInventory retrieves inventory (including site exploration reports) for component targets.
	GetComponentInventory(ctx context.Context, req *corev1.GetComponentInventoryRequest) (*corev1.GetComponentInventoryResponse, error)

	// GetAllExpectedSwitchesLinked returns expected switches linked to their
	// explored endpoints and live Switch resources. Each entry includes the
	// BMC MAC, Core's SwitchId (if the switch has been created), and the
	// expected switch UUID.
	GetAllExpectedSwitchesLinked(ctx context.Context) ([]LinkedExpectedSwitch, error)

	// GetAllExpectedPowerShelvesLinked returns expected power shelves linked
	// to their explored endpoints and live PowerShelf resources. Each entry
	// includes the BMC/PMC MAC, Core's PowerShelfId (if the shelf has been
	// created), and the expected power shelf UUID.
	GetAllExpectedPowerShelvesLinked(ctx context.Context) ([]LinkedExpectedPowerShelf, error)

	// GetAllExpectedRackDetails returns every expected rack registered with
	// Core. The result is the canonical view: rack_id (operator-supplied
	// stable identifier), rack_profile_id, and Metadata (name, description,
	// labels including chassis.* / location.*).
	GetAllExpectedRackDetails(ctx context.Context) ([]ExpectedRackDetail, error)

	// GetAllExpectedMachineDetails returns every expected machine registered
	// with Core, including bmc_mac, chassis_serial_number, rack_id, the
	// expected_machine UUID and the full Metadata block. This is the source
	// of truth for Flow's expected-inventory sync; do not confuse with
	// GetMachines, which returns runtime (discovered) machine state.
	GetAllExpectedMachineDetails(ctx context.Context) ([]ExpectedMachineDetail, error)

	// GetAllExpectedSwitchDetails returns every expected switch registered
	// with Core as a flat slice carrying the full proto contents. The
	// existing GetAllExpectedSwitches (keyed by BMC MAC, thin info) is kept
	// for its current callers and intentionally not replaced here.
	GetAllExpectedSwitchDetails(ctx context.Context) ([]ExpectedSwitchDetail, error)

	// GetAllExpectedPowerShelfDetails returns every expected power shelf
	// registered with Core as a flat slice with the full proto contents.
	GetAllExpectedPowerShelfDetails(ctx context.Context) ([]ExpectedPowerShelfDetail, error)

	// GetDesiredFirmwareVersions returns a slice of desired firmware version
	// entries configured in Core. Each entry carries vendor and model fields;
	// iterate the slice to find matching entries.
	GetDesiredFirmwareVersions(ctx context.Context) ([]*corev1.DesiredFirmwareVersionEntry, error)

	// FindExploredEndpointsByIds returns explored endpoint data (including
	// firmware_versions) for the given BMC IP addresses.
	FindExploredEndpointsByIds(ctx context.Context, bmcIPs []string) ([]*corev1.ExploredEndpoint, error)

	// SetMachineAutoUpdate enables or disables firmware auto-update for a machine.
	SetMachineAutoUpdate(ctx context.Context, machineID string, enable bool) error

	// FindMachineControllerStates returns the raw state string Core reports for
	// each machine. Machines for which Core returns no state are omitted.
	FindMachineControllerStates(ctx context.Context, machineIDs []string) (map[string]string, error)

	// DecommissionMachine initiates decommissioning of the given machine via Core.
	DecommissionMachine(ctx context.Context, machineID string) error

	// DecommissionSwitch initiates decommissioning of the given switch via Core.
	// TODO: Core Decommission Switch RPC pending.
	DecommissionSwitch(ctx context.Context, switchID string) error

	// DecommissionPowerShelf initiates decommissioning of the given power shelf via Core.
	DecommissionPowerShelf(ctx context.Context, shelfID string) error

	// The following are only valid in the mock environment and should only be called by unit tests
	AddMachine(MachineDetail)
	AddPowerState(machineID string, state PowerState)
	SetFirmwareUpdateTimeWindowError(err error)
	SetAdminPowerControlError(err error)
	AddMachineInterface(iface MachineInterface)
	AddExpectedSwitchInfo(info ExpectedSwitchInfo)
	SetLeakingMachineIds(ids []string)
	SetLeakingSwitchIds([]string)
	SetSwitchRackID(switchID, rackID string)
	SetPowerShelfRackID(shelfID, rackID string)
	SetSwitchControllerState(switchID, state string)
	SetSwitchNvosIP(switchID, ip string)
	SetObservedNVLinkDomainMemberships(memberships []NVLinkDomainMembership)
	SetPowerShelfControllerState(shelfID, state string)
	SetObservedSwitches(devices []ObservedControllerDevice)
	SetObservedPowerShelves(devices []ObservedControllerDevice)
	SetRackHostMachineIDs(rackID string, machineIDs []string)
	AddExpectedRackDetail(detail ExpectedRackDetail)
	AddExpectedMachineDetail(detail ExpectedMachineDetail)
	AddExpectedSwitchDetail(detail ExpectedSwitchDetail)
	AddExpectedPowerShelfDetail(detail ExpectedPowerShelfDetail)

	// Decommission mock fixtures.
	SetMachineControllerState(machineID, state string)
	SetDecommissionMachineError(err error)
	SetDecommissionSwitchError(err error)
	SetDecommissionPowerShelfError(err error)

	// DPU reprovisioning mock fixtures + recorders.
	SetHostDpuMachineIds(hostMachineID string, dpuIDs []string)
	SetHostInstanceID(hostMachineID string, instanceID string)
	SetDpuReprovisioningPending(hostMachineID string, pending bool)
	SetInsertHostUpdateOverrideError(err error)
	SetTriggerDpuReprovisioningError(err error)
	SetInvokeInstancePowerError(err error)
	DpuReprovisioningTriggers() []DpuReprovisioningCall
	InstancePowerCalls() []InstancePowerCall
	HostUpdateOverridesActive() map[string]string
}
