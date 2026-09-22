// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// model.go abstracts the raw grpc definitions away.  Don't bother implementing fields you don't think you'll use.

func stringsToMachineIds(machineIds []string) (ret []*corev1.MachineId) {
	if len(machineIds) == 0 {
		return nil
	}
	for _, cur := range machineIds {
		ret = append(ret, &corev1.MachineId{Id: cur})
	}
	return ret
}

// MachineDetail represents detailed machine information from NICo
type MachineDetail struct {
	MachineID               string
	ChassisSerial           *string
	State                   string
	MachineType             string
	BmcIP                   string
	BmcMac                  string
	FirmwareVersion         string
	AssociatedDpuMachineIDs []string
	UpdateComplete          bool
	HealthStatus            string
	LastObservationTime     *time.Time
	FirmwareAutoupdate      *bool
}

// MachinePosition represents machine position information from NICo
type MachinePosition struct {
	MachineID        string
	PhysicalSlotNum  *int32
	ComputeTrayIndex *int32
	TopologyID       *int32
}

// ObservedControllerDevice is the actual-inventory identity shared by Core
// switches and power shelves. The resource-specific runtime ID is matched to a
// Flow component through its normalized BMC controller MAC.
type ObservedControllerDevice struct {
	ID     string
	BmcMac string
}

func machineDetailFromPb(machine *corev1.Machine) MachineDetail {
	config := machine.GetConfig()
	status := machine.GetStatus()
	detail := MachineDetail{
		MachineID:      machine.Id.Id,
		State:          machine.State,
		MachineType:    machine.MachineType.String(),
		UpdateComplete: status.GetUpdateComplete(),
	}
	for _, dpuID := range status.GetAssociatedDpuMachineIds() {
		if id := dpuID.GetId(); id != "" {
			detail.AssociatedDpuMachineIDs = append(detail.AssociatedDpuMachineIDs, id)
		}
	}

	// Chassis serial
	if status.GetDiscoveryInfo() != nil && status.GetDiscoveryInfo().DmiData != nil {
		serial := status.GetDiscoveryInfo().DmiData.ChassisSerial
		detail.ChassisSerial = &serial
	}

	// BMC info
	if machine.BmcInfo != nil {
		if machine.BmcInfo.Ip != nil {
			detail.BmcIP = *machine.BmcInfo.Ip
		}
		if machine.BmcInfo.Mac != nil {
			detail.BmcMac = *machine.BmcInfo.Mac
		}
		if machine.BmcInfo.FirmwareVersion != nil {
			detail.FirmwareVersion = *machine.BmcInfo.FirmwareVersion
		}
	}

	// Health status - derived from alerts
	if status.GetHealth() != nil {
		if len(status.GetHealth().Alerts) > 0 {
			detail.HealthStatus = "unhealthy"
		} else {
			detail.HealthStatus = "healthy"
		}
	}

	// Last observation time
	if status.GetLastObservationTime() != nil {
		t := status.GetLastObservationTime().AsTime()
		detail.LastObservationTime = &t
	}

	if config != nil && config.FirmwareAutoupdate != nil {
		v := config.GetFirmwareAutoupdate()
		detail.FirmwareAutoupdate = &v
	}

	return detail
}

func machinePositionFromPb(pos *corev1.MachinePositionInfo) MachinePosition {
	return MachinePosition{
		MachineID:        pos.MachineId.Id,
		PhysicalSlotNum:  pos.PhysicalSlotNumber,
		ComputeTrayIndex: pos.ComputeTrayIndex,
		TopologyID:       pos.TopologyId,
	}
}

type PowerState int

const (
	PowerStateUnknown PowerState = iota
	PowerStateOff
	PowerStateOn
	PowerStateDisabled
	PowerStateHibernating
	PowerStateSleeping
)

// MachinePowerState is information about current and desired power states
type MachinePowerState struct {
	MachineID  string
	PowerState PowerState
}

func machinePowerStateFromPb(state *corev1.PowerOptions) MachinePowerState {
	return MachinePowerState{MachineID: state.HostId.Id, PowerState: powerStateFromPb(state.ActualState)}
}

func powerStateFromPb(state corev1.PowerState) (ret PowerState) {
	switch state {
	case corev1.PowerState_Off:
		ret = PowerStateOff
	case corev1.PowerState_On:
		ret = PowerStateOn
	case corev1.PowerState_PowerManagerDisabled:
		ret = PowerStateDisabled
	default:
		ret = PowerStateUnknown
	}

	return ret
}

func powerStateToPb(state PowerState) corev1.PowerState {
	switch state {
	case PowerStateOff:
		return corev1.PowerState_Off
	case PowerStateOn:
		return corev1.PowerState_On
	case PowerStateDisabled:
		return corev1.PowerState_PowerManagerDisabled
	default:
		return corev1.PowerState_Off
	}
}

// SystemPowerControl represents power control actions for AdminPowerControl
type SystemPowerControl int

const (
	// Power On
	PowerControlOn      SystemPowerControl = iota
	PowerControlForceOn                    // maps to On (NICo doesn't distinguish)
	// Power Off
	PowerControlGracefulShutdown
	PowerControlForceOff
	// Restart (OS level)
	PowerControlGracefulRestart
	PowerControlForceRestart
	// Reset (hardware level)
	PowerControlWarmReset // maps to GracefulRestart
	PowerControlColdReset // maps to ACPowercycle
)

func (s SystemPowerControl) String() string {
	switch s {
	case PowerControlOn:
		return "On"
	case PowerControlForceOn:
		return "ForceOn"
	case PowerControlGracefulShutdown:
		return "GracefulShutdown"
	case PowerControlForceOff:
		return "ForceOff"
	case PowerControlGracefulRestart:
		return "GracefulRestart"
	case PowerControlForceRestart:
		return "ForceRestart"
	case PowerControlWarmReset:
		return "WarmReset"
	case PowerControlColdReset:
		return "ColdReset"
	default:
		return "Unknown"
	}
}

func (s SystemPowerControl) toPb() corev1.AdminPowerControlRequest_SystemPowerControl {
	switch s {
	case PowerControlOn, PowerControlForceOn:
		return corev1.AdminPowerControlRequest_On
	case PowerControlGracefulShutdown:
		return corev1.AdminPowerControlRequest_GracefulShutdown
	case PowerControlForceOff:
		return corev1.AdminPowerControlRequest_ForceOff
	case PowerControlGracefulRestart, PowerControlWarmReset:
		return corev1.AdminPowerControlRequest_GracefulRestart
	case PowerControlForceRestart:
		return corev1.AdminPowerControlRequest_ForceRestart
	case PowerControlColdReset:
		return corev1.AdminPowerControlRequest_ACPowercycle
	default:
		return corev1.AdminPowerControlRequest_On
	}
}

// MachineInterface represents a network interface from nico-core-api
type MachineInterface struct {
	MacAddress string
	Addresses  []string // IP addresses assigned to this interface
}

func machineInterfaceFromPb(iface *corev1.MachineInterface) MachineInterface {
	return MachineInterface{
		MacAddress: iface.MacAddress,
		Addresses:  iface.Address,
	}
}

// AddExpectedMachineRequest contains the parameters for registering
// an expected machine with NICo.
type AddExpectedMachineRequest struct {
	BMCMACAddress            string   `json:"bmc_mac_address"`
	BMCUsername              string   `json:"bmc_username"`
	BMCPassword              string   `json:"bmc_password"`
	ChassisSerialNumber      string   `json:"chassis_serial_number,omitempty"`
	FallbackDPUSerialNumbers []string `json:"fallback_dpu_serial_numbers,omitempty"`
	RackID                   string   `json:"rack_id,omitempty"`
	PauseIngestionAndPowerOn *bool    `json:"default_pause_ingestion_and_poweron,omitempty"`
}

// AddExpectedPowerShelfRequest contains the parameters for registering
// an expected power shelf with NICo.
type AddExpectedPowerShelfRequest struct {
	BMCMACAddress     string `json:"bmc_mac_address"`
	BMCUsername       string `json:"bmc_username"`
	BMCPassword       string `json:"bmc_password"`
	ShelfSerialNumber string `json:"shelf_serial_number,omitempty"`
	IPAddress         string `json:"ip_address,omitempty"`
	RackID            string `json:"rack_id,omitempty"`
}

// ExpectedSwitchInfo represents an expected switch retrieved from NICo,
// including metadata labels (e.g., "host_mac_address" for the NVOS MAC).
// Credentials are omitted; NICo configures them in Vault and NSM
// queries Vault by MAC address.
type ExpectedSwitchInfo struct {
	BMCMACAddress      string
	SwitchSerialNumber string
	Metadata           map[string]string
}

func expectedSwitchInfoFromPb(es *corev1.ExpectedSwitch) ExpectedSwitchInfo {
	info := ExpectedSwitchInfo{
		BMCMACAddress:      es.GetBmcMacAddress(),
		SwitchSerialNumber: es.GetSwitchSerialNumber(),
		Metadata:           make(map[string]string),
	}
	if es.GetMetadata() != nil {
		for _, label := range es.GetMetadata().GetLabels() {
			if label.Value != nil {
				info.Metadata[label.GetKey()] = label.GetValue()
			}
		}
	}
	return info
}

// LinkedExpectedSwitch represents an expected switch linked to its
// explored endpoint and live Switch resource in Core.
type LinkedExpectedSwitch struct {
	BMCMACAddress      string
	SwitchID           string // Core's live Switch ID; empty if not yet created
	ExpectedSwitchID   string
	SwitchSerialNumber string
}

func linkedExpectedSwitchFromPb(les *corev1.LinkedExpectedSwitch) LinkedExpectedSwitch {
	info := LinkedExpectedSwitch{
		BMCMACAddress:      les.GetBmcMacAddress(),
		SwitchSerialNumber: les.GetSwitchSerialNumber(),
	}
	if les.GetSwitchId() != nil {
		info.SwitchID = les.GetSwitchId().GetId()
	}
	if les.GetExpectedSwitchId() != nil {
		info.ExpectedSwitchID = les.GetExpectedSwitchId().GetValue()
	}
	return info
}

// LinkedExpectedPowerShelf represents an expected power shelf linked to its
// explored endpoint and live PowerShelf resource in Core.
type LinkedExpectedPowerShelf struct {
	BMCMACAddress        string
	PowerShelfID         string // Core's live PowerShelf ID; empty if not yet created
	ExpectedPowerShelfID string
	ShelfSerialNumber    string
}

func linkedExpectedPowerShelfFromPb(leps *corev1.LinkedExpectedPowerShelf) LinkedExpectedPowerShelf {
	info := LinkedExpectedPowerShelf{
		BMCMACAddress:     leps.GetBmcMacAddress(),
		ShelfSerialNumber: leps.GetShelfSerialNumber(),
	}
	if leps.GetPowerShelfId() != nil {
		info.PowerShelfID = leps.GetPowerShelfId().GetId()
	}
	if leps.GetExpectedPowerShelfId() != nil {
		info.ExpectedPowerShelfID = leps.GetExpectedPowerShelfId().GetValue()
	}
	return info
}

// AddExpectedSwitchRequest contains the parameters for registering
// an expected switch with NICo.
type AddExpectedSwitchRequest struct {
	BMCMACAddress      string `json:"bmc_mac_address"`
	BMCUsername        string `json:"bmc_username"`
	BMCPassword        string `json:"bmc_password"`
	SwitchSerialNumber string `json:"switch_serial_number,omitempty"`
	RackID             string `json:"rack_id,omitempty"`
	NVOSUsername       string `json:"nvos_username,omitempty"`
	NVOSPassword       string `json:"nvos_password,omitempty"`
}

// BringUpState represents the bring-up state of a machine in
// relation to NICo's power-on gate.
type BringUpState int

const (
	BringUpStateNotDiscovered BringUpState = iota
	BringUpStateWaitingForIngestion
	BringUpStateMachineNotCreated
	BringUpStateMachineCreated
)

func (s BringUpState) String() string {
	switch s {
	case BringUpStateNotDiscovered:
		return "NotDiscovered"
	case BringUpStateWaitingForIngestion:
		return "WaitingForIngestion"
	case BringUpStateMachineNotCreated:
		return "IngestionMachineNotCreated"
	case BringUpStateMachineCreated:
		return "IngestionMachineCreated"
	default:
		return "Unknown"
	}
}

func bringUpStateFromPb(
	s corev1.MachineIngestionState,
) BringUpState {
	switch s {
	case corev1.MachineIngestionState_WaitingForIngestion:
		return BringUpStateWaitingForIngestion
	case corev1.MachineIngestionState_IngestionMachineNotCreated:
		return BringUpStateMachineNotCreated
	case corev1.MachineIngestionState_IngestionMachineCreated:
		return BringUpStateMachineCreated
	default:
		return BringUpStateNotDiscovered
	}
}

// ExpectedRackDetail is the canonical expected-rack representation returned by
// GetAllExpectedRacks. RackID is the operator-supplied stable identifier (the
// same string referenced by ExpectedMachine.RackID, ExpectedSwitchDetail.RackID,
// and ExpectedPowerShelfDetail.RackID). Labels carries well-known keys such as
// chassis.manufacturer / chassis.serial-number / chassis.model and location.*.
type ExpectedRackDetail struct {
	RackID        string
	RackProfileID string
	Name          string
	Description   string
	Labels        map[string]string
}

// NVLinkDomainMembership is one valid rack-scoped NVLink domain observation
// from Core's actual switch inventory. Core stores the last valid observation
// on every active switch in a rack, so a snapshot may contain duplicate rows
// for the same domain and rack.
type NVLinkDomainMembership struct {
	DomainID string
	RackID   string
}

// ExpectedMachineDetail is the canonical expected-machine representation
// returned by GetAllExpectedMachines. ExpectedMachineID is the Core-side UUID
// for the expected_machines row (distinct from the runtime machine_id assigned
// after discovery). Labels carries the component metadata cloud REST writes
// alongside the typed fields: manufacturer, model, firmware_version, slot_id,
// tray_idx, host_id.
type ExpectedMachineDetail struct {
	ExpectedMachineID   string
	BMCMACAddress       string
	BMCIPAddress        string
	ChassisSerialNumber string
	RackID              string
	Name                string
	Description         string
	Labels              map[string]string
}

// ExpectedSwitchDetail is the canonical expected-switch representation returned
// by GetAllExpectedMachines's switch peer (GetAllExpectedSwitches has a
// different shape keyed by BMC MAC). ExpectedSwitchID is the Core-side UUID for
// the expected_switches row. Labels carries the same component metadata keys as
// ExpectedMachineDetail.
type ExpectedSwitchDetail struct {
	ExpectedSwitchID   string
	BMCMACAddress      string
	BMCIPAddress       string
	SwitchSerialNumber string
	RackID             string
	Name               string
	Description        string
	Labels             map[string]string
}

// ExpectedPowerShelfDetail is the canonical expected-power-shelf representation
// returned by GetAllExpectedPowerShelves. ExpectedPowerShelfID is the Core-side
// UUID for the expected_power_shelves row.
type ExpectedPowerShelfDetail struct {
	ExpectedPowerShelfID string
	BMCMACAddress        string
	BMCIPAddress         string
	ShelfSerialNumber    string
	RackID               string
	Name                 string
	Description          string
	Labels               map[string]string
}

// metadataToGo extracts name, description and labels from a proto Metadata
// message into plain Go values. A nil metadata yields zero values and a nil
// labels map.
func metadataToGo(md *corev1.Metadata) (name, description string, labels map[string]string) {
	if md == nil {
		return "", "", nil
	}
	name = md.GetName()
	description = md.GetDescription()
	if pbLabels := md.GetLabels(); len(pbLabels) > 0 {
		labels = make(map[string]string, len(pbLabels))
		for _, l := range pbLabels {
			if l == nil {
				continue
			}
			if l.Value != nil {
				labels[l.GetKey()] = l.GetValue()
			}
		}
	}
	return name, description, labels
}

func expectedRackDetailFromPb(er *corev1.ExpectedRack) ExpectedRackDetail {
	d := ExpectedRackDetail{
		RackProfileID: er.GetRackProfileId().GetId(),
	}
	if er.GetRackId() != nil {
		d.RackID = er.GetRackId().GetId()
	}
	d.Name, d.Description, d.Labels = metadataToGo(er.GetMetadata())
	return d
}

func expectedMachineDetailFromPb(em *corev1.ExpectedMachine) ExpectedMachineDetail {
	d := ExpectedMachineDetail{
		BMCMACAddress:       em.GetBmcMacAddress(),
		ChassisSerialNumber: em.GetChassisSerialNumber(),
	}
	if em.Id != nil {
		d.ExpectedMachineID = em.GetId().GetValue()
	}
	if em.BmcIpAddress != nil {
		d.BMCIPAddress = em.GetBmcIpAddress()
	}
	if em.RackId != nil {
		d.RackID = em.GetRackId().GetId()
	}
	d.Name, d.Description, d.Labels = metadataToGo(em.GetMetadata())
	return d
}

func expectedSwitchDetailFromPb(es *corev1.ExpectedSwitch) ExpectedSwitchDetail {
	d := ExpectedSwitchDetail{
		BMCMACAddress:      es.GetBmcMacAddress(),
		BMCIPAddress:       es.GetBmcIpAddress(),
		SwitchSerialNumber: es.GetSwitchSerialNumber(),
	}
	if es.ExpectedSwitchId != nil {
		d.ExpectedSwitchID = es.GetExpectedSwitchId().GetValue()
	}
	if es.RackId != nil {
		d.RackID = es.GetRackId().GetId()
	}
	d.Name, d.Description, d.Labels = metadataToGo(es.GetMetadata())
	return d
}

func expectedPowerShelfDetailFromPb(eps *corev1.ExpectedPowerShelf) ExpectedPowerShelfDetail {
	d := ExpectedPowerShelfDetail{
		BMCMACAddress:     eps.GetBmcMacAddress(),
		BMCIPAddress:      eps.GetBmcIpAddress(),
		ShelfSerialNumber: eps.GetShelfSerialNumber(),
	}
	if eps.ExpectedPowerShelfId != nil {
		d.ExpectedPowerShelfID = eps.GetExpectedPowerShelfId().GetValue()
	}
	if eps.RackId != nil {
		d.RackID = eps.GetRackId().GetId()
	}
	d.Name, d.Description, d.Labels = metadataToGo(eps.GetMetadata())
	return d
}
