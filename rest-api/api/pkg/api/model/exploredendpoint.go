// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIExploredEndpointGetAllRequest binds query parameters for GET /site-explorer/endpoint.
// Pagination is bound separately via pagination.PageRequest.
type APIExploredEndpointGetAllRequest struct {
	SiteID string `query:"siteId"`
}

// Validate checks the list query shape.
func (r *APIExploredEndpointGetAllRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
	)
}

// APIExploredEndpoint is a Site Explorer explored endpoint.
type APIExploredEndpoint struct {
	Address               string                        `json:"address"`
	Report                *APIEndpointExplorationReport `json:"report"`
	ReportVersion         string                        `json:"reportVersion"`
	ExplorationRequested  bool                          `json:"explorationRequested"`
	PreingestionState     string                        `json:"preingestionState"`
	LastRedfishBmcReset   string                        `json:"lastRedfishBmcReset"`
	LastIpmitoolBmcReset  string                        `json:"lastIpmitoolBmcReset"`
	LastRedfishReboot     string                        `json:"lastRedfishReboot"`
	LastRedfishPowercycle string                        `json:"lastRedfishPowercycle"`
	PauseRemediation      bool                          `json:"pauseRemediation"`
	Warnings              []string                      `json:"warnings"`
}

// APIEndpointExplorationReport is data gathered about an endpoint during site exploration.
type APIEndpointExplorationReport struct {
	EndpointType               string                      `json:"endpointType"`
	LastExplorationError       *string                     `json:"lastExplorationError"`
	MachineID                  *string                     `json:"machineId"`
	LastExplorationLatency     *string                     `json:"lastExplorationLatency"`
	Vendor                     *string                     `json:"vendor"`
	Managers                   []APIExploredManager        `json:"managers"`
	Systems                    []APIExploredComputerSystem `json:"systems"`
	Chassis                    []APIExploredChassis        `json:"chassis"`
	Service                    []APIExploredService        `json:"service"`
	MachineSetupStatus         *APIMachineSetupStatus      `json:"machineSetupStatus"`
	SecureBootStatus           *APISecureBootStatus        `json:"secureBootStatus"`
	LockdownStatus             *APILockdownStatus          `json:"lockdownStatus"`
	FirmwareVersions           map[string]string           `json:"firmwareVersions"`
	LastExplorationErrorSchema *APIOperatorErrorSchema     `json:"lastExplorationErrorSchema"`
}

// APIOperatorErrorSchema is a structured exploration error for operator tooling.
type APIOperatorErrorSchema struct {
	ErrorCode  string  `json:"errorCode"`
	Mitigation *string `json:"mitigation"`
	Text       string  `json:"text"`
}

// APIExploredManager is a Redfish Manager from an exploration report.
type APIExploredManager struct {
	ID                 string                         `json:"id"`
	EthernetInterfaces []APIExploredEthernetInterface `json:"ethernetInterfaces"`
}

// APIExploredComputerSystem is a Redfish ComputerSystem from an exploration report.
type APIExploredComputerSystem struct {
	ID                 string                              `json:"id"`
	Manufacturer       *string                             `json:"manufacturer"`
	Model              *string                             `json:"model"`
	SerialNumber       *string                             `json:"serialNumber"`
	Attributes         *APIComputerSystemAttributes        `json:"attributes"`
	EthernetInterfaces []APIExploredEthernetInterface      `json:"ethernetInterfaces"`
	PCIeDevices        []APIExploredPCIeDevice             `json:"pcieDevices"`
	PowerState         APIExploredComputerSystemPowerState `json:"powerState"`
	BootOrder          *APIBootOrder                       `json:"bootOrder"`
}

// APIComputerSystemAttributes holds ComputerSystem attributes from Redfish.
type APIComputerSystemAttributes struct {
	NicMode *APIExploredNicMode `json:"nicMode"`
}

// APIExploredNicMode is the normalized REST representation of Core's NIC mode.
type APIExploredNicMode string

// APIExploredComputerSystemPowerState is the normalized REST representation of Core's power state.
type APIExploredComputerSystemPowerState string

// APIExploredInternalLockdownStatus is the normalized REST representation of Core's lockdown status.
type APIExploredInternalLockdownStatus string

// APIExploredEthernetInterface is a Redfish EthernetInterface from an exploration report.
type APIExploredEthernetInterface struct {
	ID               *string `json:"id"`
	Description      *string `json:"description"`
	InterfaceEnabled *bool   `json:"interfaceEnabled"`
	MacAddress       *string `json:"macAddress"`
	LinkStatus       *string `json:"linkStatus"`
}

// APIExploredChassis is a Redfish Chassis from an exploration report.
type APIExploredChassis struct {
	ID              string                      `json:"id"`
	NetworkAdapters []APIExploredNetworkAdapter `json:"networkAdapters"`
	Manufacturer    *string                     `json:"manufacturer"`
	Model           *string                     `json:"model"`
	PartNumber      *string                     `json:"partNumber"`
	SerialNumber    *string                     `json:"serialNumber"`
}

// APIExploredNetworkAdapter is a Redfish NetworkAdapter from an exploration report.
type APIExploredNetworkAdapter struct {
	ID           string  `json:"id"`
	Manufacturer *string `json:"manufacturer"`
	Model        *string `json:"model"`
	PartNumber   *string `json:"partNumber"`
	SerialNumber *string `json:"serialNumber"`
}

// APIExploredService is a Redfish UpdateService from an exploration report.
type APIExploredService struct {
	ID          string                 `json:"id"`
	Inventories []APIExploredInventory `json:"inventories"`
}

// APIExploredInventory is firmware inventory from a Redfish UpdateService.
type APIExploredInventory struct {
	ID          string  `json:"id"`
	Description *string `json:"description"`
	Version     *string `json:"version"`
	ReleaseDate *string `json:"releaseDate"`
}

// APIMachineSetupStatus is the result of a Redfish machine-setup check.
type APIMachineSetupStatus struct {
	IsDone                 bool                           `json:"isDone"`
	Diffs                  []APIMachineSetupDiff          `json:"diffs"`
	EvaluatedBootInterface *APIMachineBootInterfaceTarget `json:"evaluatedBootInterface"`
}

// APIMachineBootInterfaceTarget is the boot-interface target assessed during exploration.
type APIMachineBootInterfaceTarget struct {
	Pair    *APIMachineBootInterfacePair `json:"pair"`
	MacOnly *string                      `json:"macOnly"`
}

// APIMachineBootInterfacePair is a MAC and Redfish EthernetInterface.Id pair.
type APIMachineBootInterfacePair struct {
	MacAddress  string `json:"macAddress"`
	InterfaceID string `json:"interfaceId"`
}

// APIMachineSetupDiff is one expected-vs-actual machine-setup difference.
type APIMachineSetupDiff struct {
	Key      string `json:"key"`
	Expected string `json:"expected"`
	Actual   string `json:"actual"`
}

// APIExploredPCIeDevice is a Redfish PCIe device from an exploration report.
type APIExploredPCIeDevice struct {
	Description     *string          `json:"description"`
	FirmwareVersion *string          `json:"firmwareVersion"`
	GpuVendor       *string          `json:"gpuVendor"`
	ID              *string          `json:"id"`
	Manufacturer    *string          `json:"manufacturer"`
	Name            *string          `json:"name"`
	PartNumber      *string          `json:"partNumber"`
	SerialNumber    *string          `json:"serialNumber"`
	Status          *APISystemStatus `json:"status"`
}

// APISystemStatus is Redfish health/state status.
type APISystemStatus struct {
	Health       *string `json:"health"`
	HealthRollup *string `json:"healthRollup"`
	State        string  `json:"state"`
}

// APIBootOrder is a Redfish boot order list.
type APIBootOrder struct {
	BootOrder []APIBootOption `json:"bootOrder"`
}

// APIBootOption is one Redfish boot option.
type APIBootOption struct {
	DisplayName       string  `json:"displayName"`
	ID                string  `json:"id"`
	BootOptionEnabled *bool   `json:"bootOptionEnabled"`
	UefiDevicePath    *string `json:"uefiDevicePath"`
}

// APISecureBootStatus is Redfish secure-boot status.
type APISecureBootStatus struct {
	IsEnabled bool `json:"isEnabled"`
}

// APILockdownStatus is Redfish lockdown status.
type APILockdownStatus struct {
	Status  APIExploredInternalLockdownStatus `json:"status"`
	Message string                            `json:"message"`
}

// NewAPIExploredEndpoint creates an API model from a Core ExploredEndpoint.
func NewAPIExploredEndpoint(ep *corev1.ExploredEndpoint) *APIExploredEndpoint {
	if ep == nil {
		return nil
	}
	resp := &APIExploredEndpoint{}
	resp.FromProto(ep)
	return resp
}

// FromProto converts one Core ExploredEndpoint.
func (r *APIExploredEndpoint) FromProto(ep *corev1.ExploredEndpoint) {
	if ep == nil {
		return
	}
	*r = APIExploredEndpoint{
		Address:               ep.GetAddress(),
		ReportVersion:         ep.GetReportVersion(),
		ExplorationRequested:  ep.GetExplorationRequested(),
		PreingestionState:     ep.GetPreingestionState(),
		LastRedfishBmcReset:   ep.GetLastRedfishBmcReset(),
		LastIpmitoolBmcReset:  ep.GetLastIpmitoolBmcReset(),
		LastRedfishReboot:     ep.GetLastRedfishReboot(),
		LastRedfishPowercycle: ep.GetLastRedfishPowercycle(),
		PauseRemediation:      ep.GetPauseRemediation(),
		Warnings:              append([]string{}, ep.GetWarnings()...),
	}
	if report := ep.GetReport(); report != nil {
		r.Report = newAPIEndpointExplorationReport(report)
	}
}

func newAPIEndpointExplorationReport(report *corev1.EndpointExplorationReport) *APIEndpointExplorationReport {
	if report == nil {
		return nil
	}
	out := &APIEndpointExplorationReport{
		EndpointType: report.GetEndpointType(),
	}
	if report.LastExplorationError != nil {
		out.LastExplorationError = report.LastExplorationError
	}
	if report.MachineId != nil {
		out.MachineID = report.MachineId
	}
	if latency := report.GetLastExplorationLatency(); latency != nil {
		s := latency.AsDuration().String()
		out.LastExplorationLatency = &s
	}
	if report.Vendor != nil {
		out.Vendor = report.Vendor
	}
	if len(report.GetManagers()) > 0 {
		out.Managers = make([]APIExploredManager, 0, len(report.GetManagers()))
		for _, m := range report.GetManagers() {
			out.Managers = append(out.Managers, newAPIExploredManager(m))
		}
	}
	if len(report.GetSystems()) > 0 {
		out.Systems = make([]APIExploredComputerSystem, 0, len(report.GetSystems()))
		for _, s := range report.GetSystems() {
			out.Systems = append(out.Systems, newAPIExploredComputerSystem(s))
		}
	}
	if len(report.GetChassis()) > 0 {
		out.Chassis = make([]APIExploredChassis, 0, len(report.GetChassis()))
		for _, c := range report.GetChassis() {
			out.Chassis = append(out.Chassis, newAPIExploredChassis(c))
		}
	}
	if len(report.GetService()) > 0 {
		out.Service = make([]APIExploredService, 0, len(report.GetService()))
		for _, s := range report.GetService() {
			out.Service = append(out.Service, newAPIExploredService(s))
		}
	}
	if mss := report.GetMachineSetupStatus(); mss != nil {
		out.MachineSetupStatus = newAPIMachineSetupStatus(mss)
	}
	if sbs := report.GetSecureBootStatus(); sbs != nil {
		out.SecureBootStatus = &APISecureBootStatus{IsEnabled: sbs.GetIsEnabled()}
	}
	if ls := report.GetLockdownStatus(); ls != nil {
		out.LockdownStatus = &APILockdownStatus{Message: ls.GetMessage()}
		out.LockdownStatus.Status.FromProto(ls.GetStatus())
	}
	if len(report.GetFirmwareVersions()) > 0 {
		out.FirmwareVersions = make(map[string]string, len(report.GetFirmwareVersions()))
		for k, v := range report.GetFirmwareVersions() {
			out.FirmwareVersions[k] = v
		}
	}
	if schema := report.GetLastExplorationErrorSchema(); schema != nil {
		out.LastExplorationErrorSchema = &APIOperatorErrorSchema{
			ErrorCode: schema.GetErrorCode(),
			Text:      schema.GetText(),
		}
		if schema.Mitigation != nil {
			out.LastExplorationErrorSchema.Mitigation = schema.Mitigation
		}
	}
	return out
}

func newAPIExploredManager(m *corev1.Manager) APIExploredManager {
	out := APIExploredManager{ID: m.GetId()}
	if len(m.GetEthernetInterfaces()) > 0 {
		out.EthernetInterfaces = make([]APIExploredEthernetInterface, 0, len(m.GetEthernetInterfaces()))
		for _, iface := range m.GetEthernetInterfaces() {
			out.EthernetInterfaces = append(out.EthernetInterfaces, newAPIExploredEthernetInterface(iface))
		}
	}
	return out
}

func newAPIExploredComputerSystem(s *corev1.ComputerSystem) APIExploredComputerSystem {
	out := APIExploredComputerSystem{
		ID: s.GetId(),
	}
	out.PowerState.FromProto(s.GetPowerState())
	if s.Manufacturer != nil {
		out.Manufacturer = s.Manufacturer
	}
	if s.Model != nil {
		out.Model = s.Model
	}
	if s.SerialNumber != nil {
		out.SerialNumber = s.SerialNumber
	}
	if attrs := s.GetAttributes(); attrs != nil {
		out.Attributes = &APIComputerSystemAttributes{}
		if attrs.NicMode != nil {
			mode := APIExploredNicMode("")
			mode.FromProto(*attrs.NicMode)
			out.Attributes.NicMode = &mode
		}
	}
	if len(s.GetEthernetInterfaces()) > 0 {
		out.EthernetInterfaces = make([]APIExploredEthernetInterface, 0, len(s.GetEthernetInterfaces()))
		for _, iface := range s.GetEthernetInterfaces() {
			out.EthernetInterfaces = append(out.EthernetInterfaces, newAPIExploredEthernetInterface(iface))
		}
	}
	if len(s.GetPcieDevices()) > 0 {
		out.PCIeDevices = make([]APIExploredPCIeDevice, 0, len(s.GetPcieDevices()))
		for _, d := range s.GetPcieDevices() {
			out.PCIeDevices = append(out.PCIeDevices, newAPIExploredPCIeDevice(d))
		}
	}
	if bo := s.GetBootOrder(); bo != nil {
		out.BootOrder = newAPIBootOrder(bo)
	}
	return out
}

func newAPIExploredEthernetInterface(iface *corev1.EthernetInterface) APIExploredEthernetInterface {
	out := APIExploredEthernetInterface{}
	if iface.Id != nil {
		out.ID = iface.Id
	}
	if iface.Description != nil {
		out.Description = iface.Description
	}
	if iface.InterfaceEnabled != nil {
		out.InterfaceEnabled = iface.InterfaceEnabled
	}
	if iface.MacAddress != nil {
		out.MacAddress = iface.MacAddress
	}
	if iface.LinkStatus != nil {
		out.LinkStatus = iface.LinkStatus
	}
	return out
}

func newAPIExploredChassis(c *corev1.Chassis) APIExploredChassis {
	out := APIExploredChassis{ID: c.GetId()}
	if c.Manufacturer != nil {
		out.Manufacturer = c.Manufacturer
	}
	if c.Model != nil {
		out.Model = c.Model
	}
	if c.PartNumber != nil {
		out.PartNumber = c.PartNumber
	}
	if c.SerialNumber != nil {
		out.SerialNumber = c.SerialNumber
	}
	if len(c.GetNetworkAdapters()) > 0 {
		out.NetworkAdapters = make([]APIExploredNetworkAdapter, 0, len(c.GetNetworkAdapters()))
		for _, a := range c.GetNetworkAdapters() {
			out.NetworkAdapters = append(out.NetworkAdapters, newAPIExploredNetworkAdapter(a))
		}
	}
	return out
}

func newAPIExploredNetworkAdapter(a *corev1.NetworkAdapter) APIExploredNetworkAdapter {
	out := APIExploredNetworkAdapter{ID: a.GetId()}
	if a.Manufacturer != nil {
		out.Manufacturer = a.Manufacturer
	}
	if a.Model != nil {
		out.Model = a.Model
	}
	if a.PartNumber != nil {
		out.PartNumber = a.PartNumber
	}
	if a.SerialNumber != nil {
		out.SerialNumber = a.SerialNumber
	}
	return out
}

func newAPIExploredService(s *corev1.Service) APIExploredService {
	out := APIExploredService{ID: s.GetId()}
	if len(s.GetInventories()) > 0 {
		out.Inventories = make([]APIExploredInventory, 0, len(s.GetInventories()))
		for _, inv := range s.GetInventories() {
			out.Inventories = append(out.Inventories, newAPIExploredInventory(inv))
		}
	}
	return out
}

func newAPIExploredInventory(inv *corev1.Inventory) APIExploredInventory {
	out := APIExploredInventory{ID: inv.GetId()}
	if inv.Description != nil {
		out.Description = inv.Description
	}
	if inv.Version != nil {
		out.Version = inv.Version
	}
	if inv.ReleaseDate != nil {
		out.ReleaseDate = inv.ReleaseDate
	}
	return out
}

func newAPIMachineSetupStatus(mss *corev1.MachineSetupStatus) *APIMachineSetupStatus {
	out := &APIMachineSetupStatus{IsDone: mss.GetIsDone()}
	if len(mss.GetDiffs()) > 0 {
		out.Diffs = make([]APIMachineSetupDiff, 0, len(mss.GetDiffs()))
		for _, d := range mss.GetDiffs() {
			out.Diffs = append(out.Diffs, APIMachineSetupDiff{
				Key:      d.GetKey(),
				Expected: d.GetExpected(),
				Actual:   d.GetActual(),
			})
		}
	}
	if target := mss.GetEvaluatedBootInterface(); target != nil {
		out.EvaluatedBootInterface = &APIMachineBootInterfaceTarget{}
		out.EvaluatedBootInterface.FromProto(target)
	}
	return out
}

// FromProto converts a Core machine boot-interface target.
func (r *APIMachineBootInterfaceTarget) FromProto(target *corev1.MachineBootInterfaceTarget) {
	if target == nil {
		return
	}
	*r = APIMachineBootInterfaceTarget{}
	if pair := target.GetPair(); pair != nil {
		r.Pair = &APIMachineBootInterfacePair{
			MacAddress:  pair.GetMacAddress(),
			InterfaceID: pair.GetInterfaceId(),
		}
	}
	if _, ok := target.GetTarget().(*corev1.MachineBootInterfaceTarget_MacOnly); ok {
		mac := target.GetMacOnly()
		r.MacOnly = &mac
	}
}

func newAPIExploredPCIeDevice(d *corev1.PCIeDevice) APIExploredPCIeDevice {
	out := APIExploredPCIeDevice{}
	if d.Description != nil {
		out.Description = d.Description
	}
	if d.FirmwareVersion != nil {
		out.FirmwareVersion = d.FirmwareVersion
	}
	if d.GpuVendor != nil {
		out.GpuVendor = d.GpuVendor
	}
	if d.Id != nil {
		out.ID = d.Id
	}
	if d.Manufacturer != nil {
		out.Manufacturer = d.Manufacturer
	}
	if d.Name != nil {
		out.Name = d.Name
	}
	if d.PartNumber != nil {
		out.PartNumber = d.PartNumber
	}
	if d.SerialNumber != nil {
		out.SerialNumber = d.SerialNumber
	}
	if st := d.GetStatus(); st != nil {
		out.Status = &APISystemStatus{State: st.GetState()}
		if st.Health != nil {
			out.Status.Health = st.Health
		}
		if st.HealthRollup != nil {
			out.Status.HealthRollup = st.HealthRollup
		}
	}
	return out
}

func newAPIBootOrder(bo *corev1.BootOrder) *APIBootOrder {
	out := &APIBootOrder{}
	if len(bo.GetBootOrder()) > 0 {
		out.BootOrder = make([]APIBootOption, 0, len(bo.GetBootOrder()))
		for _, opt := range bo.GetBootOrder() {
			item := APIBootOption{
				DisplayName: opt.GetDisplayName(),
				ID:          opt.GetId(),
			}
			if opt.BootOptionEnabled != nil {
				item.BootOptionEnabled = opt.BootOptionEnabled
			}
			if opt.UefiDevicePath != nil {
				item.UefiDevicePath = opt.UefiDevicePath
			}
			out.BootOrder = append(out.BootOrder, item)
		}
	}
	return out
}

// FromProto converts a Core NIC mode, preserving unknown values as Unknown.
func (r *APIExploredNicMode) FromProto(mode corev1.NicMode) {
	switch mode {
	case corev1.NicMode_DPU:
		*r = "Dpu"
	case corev1.NicMode_NIC:
		*r = "Nic"
	default:
		*r = "Unknown"
	}
}

// FromProto converts a Core computer-system power state, preserving unknown values as Unknown.
func (r *APIExploredComputerSystemPowerState) FromProto(state corev1.ComputerSystemPowerState) {
	switch state {
	case corev1.ComputerSystemPowerState_On:
		*r = "On"
	case corev1.ComputerSystemPowerState_Off:
		*r = "Off"
	case corev1.ComputerSystemPowerState_PoweringOff:
		*r = "PoweringOff"
	case corev1.ComputerSystemPowerState_PoweringOn:
		*r = "PoweringOn"
	case corev1.ComputerSystemPowerState_Paused:
		*r = "Paused"
	case corev1.ComputerSystemPowerState_Unknown:
		*r = "Unknown"
	case corev1.ComputerSystemPowerState_Hibernating:
		*r = "Hibernating"
	case corev1.ComputerSystemPowerState_Sleeping:
		*r = "Sleeping"
	default:
		*r = "Unknown"
	}
}

// FromProto converts a Core lockdown status, preserving unknown values as Unknown.
func (r *APIExploredInternalLockdownStatus) FromProto(status corev1.InternalLockdownStatus) {
	switch status {
	case corev1.InternalLockdownStatus_ENABLED:
		*r = "Enabled"
	case corev1.InternalLockdownStatus_PARTIAL:
		*r = "Partial"
	case corev1.InternalLockdownStatus_DISABLED:
		*r = "Disabled"
	default:
		*r = "Unknown"
	}
}
