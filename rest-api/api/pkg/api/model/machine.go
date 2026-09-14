// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	camu "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	validation "github.com/go-ozzo/ozzo-validation/v4"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// MachineMaxLabelCount is the maximum number of Labels allowed per Machine
	MachineMaxLabelCount = 10
	// InstanceLabelOnlineRepairAllowAutoDeletion records repairPolicy.allowAutoInstanceDeletionOnFailure from the last enter-online-repair request ("true" / "false").
	InstanceLabelOnlineRepairAllowAutoDeletion = "onlineRepair.allowAutoInstanceDeletionOnFailure"
	// MachineHealthOverrideSourceOnlineRepair is the merges-path source NICo Core uses for in-pool
	// online repair (repair_merge_active); must align with RequestOnlineRepair / admin CLI.
	MachineHealthOverrideSourceOnlineRepair = "request-online-repair"
	// MachineHealthAlertIDOnlineRepair is the ID of the online repair health alert.
	MachineHealthAlertIDOnlineRepair = "OnLineRepair"
	// TenantReportedIssueAlertID is the ID of the tenant-reported issue health alert.
	MachineTenantReportedIssueAlertID = "tenant-reported"
	// MachineHealthIssueSummaryMaxLength is the maximum length of the summary of the MachineHealthIssue.
	MachineHealthIssueSummaryMaxLength = 512
	// MachineHealthIssueDetailsMaxLength is the maximum length of the details of the MachineHealthIssue.
	MachineHealthIssueDetailsMaxLength = 8192
)

const (
	MachineAlertClassificationPreventAllocations       = "PreventAllocations"       // Prevents new allocations from being created on the machine.
	MachineAlertClassificationPreventInstanceDeletion  = "PreventInstanceDeletion"  // Prevents the Instance from being deleted.
	MachineAlertClassificationSuppressExternalAlerting = "SuppressExternalAlerting" // Suppresses external alerting for the alert.
)

const (
	HealthIssueHardware    = "Hardware"
	HealthIssueNetwork     = "Network"
	HealthIssuePerformance = "Performance"
	HealthIssueStorage     = "Storage"
	HealthIssueSoftware    = "Software"
	HealthIssueOther       = "Other"
)

// ValidHealthIssueCategories lists accepted HealthIssue.category values for online repair.
var ValidHealthIssueCategoriesMap = map[string]string{
	HealthIssueHardware:    "HARDWARE",
	HealthIssueNetwork:     "NETWORK",
	HealthIssuePerformance: "PERFORMANCE",
	HealthIssueStorage:     "STORAGE",
	HealthIssueSoftware:    "SOFTWARE",
	HealthIssueOther:       "OTHER",
}

// APIMachineHealthIssue describes the tenant-reported issue when requesting online repair.
type APIMachineHealthIssue struct {
	// Category is the type of the issue
	Category string `json:"category"`
	// Summary is the summary of the issue
	Summary *string `json:"summary"`
	// Details is the message of the issue
	Details *string `json:"details"`
}

// APIMachineOnlineRepairPolicy carries escalation policy for online repair.
type APIMachineOnlineRepairPolicy struct {
	AllowAutoInstanceDeletionOnFailure *bool `json:"allowAutoInstanceDeletionOnFailure"`
}

// APIMachineOnlineRepairAcknowledgments are required confirmations to enter online repair.
type APIMachineOnlineRepairAcknowledgments struct {
	AcceptDataCorruptionRisk   *bool `json:"acceptDataCorruptionRisk"`
	AcceptRepairTeamAccess     *bool `json:"acceptRepairTeamAccess"`
	AcceptInstanceDeletionRisk *bool `json:"acceptInstanceDeletionRisk"`
}

type APIMachineOnlineRepair struct {
	// Enabled when true enters in-pool online repair; when false exits online repair.
	Enabled         *bool                                  `json:"enabled"`
	Policy          *APIMachineOnlineRepairPolicy          `json:"policy,omitempty"`
	Acknowledgments *APIMachineOnlineRepairAcknowledgments `json:"acknowledgments,omitempty"`
}

// APIMachineUpdateRequest is the data structure to capture request to update a Machine
type APIMachineUpdateRequest struct {
	// InstanceTypeID is the ID of the InstanceType to set for the Machine
	InstanceTypeID *string `json:"instanceTypeId"`
	// ClearInstanceType indicates that the InstanceType should be cleared
	ClearInstanceType *bool `json:"clearInstanceType"`
	// SetMaintenanceMode enables or disables maintenance mode
	SetMaintenanceMode *bool `json:"setMaintenanceMode"`
	// MaintenanceMessage is the message to display during maintenance mode
	MaintenanceMessage *string `json:"maintenanceMessage"`
	// Labels allows setting a key value pair of arbitrary string metadata for the Machine
	Labels map[string]string `json:"labels"`
	// OnlineRepair is the request to enter/exit online repair
	OnlineRepair *APIMachineOnlineRepair `json:"onlineRepair"`
	// HealthIssue is required when onlineRepair.enabled is true.
	HealthIssue *APIMachineHealthIssue `json:"healthIssue"`
}

// IsOnlineRepair reports whether this request is for in-pool online repair (enter or exit).
func (mur *APIMachineUpdateRequest) IsOnlineRepair() bool {
	return mur.OnlineRepair != nil
}

// OnlineRepairEnabled is true when the request enters online repair (enabled == true). Caller must ensure Validate() passed or check for nil Enabled.
func (mur *APIMachineUpdateRequest) OnlineRepairEnabled() bool {
	if mur.OnlineRepair == nil || mur.OnlineRepair.Enabled == nil {
		return false
	}
	return *mur.OnlineRepair.Enabled
}

// Validate ensure the values passed in request are acceptable
func (mur APIMachineUpdateRequest) Validate() error {
	err := validation.ValidateStruct(&mur,
		validation.Field(&mur.InstanceTypeID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&mur.MaintenanceMessage,
			validation.When(mur.SetMaintenanceMode == nil || (mur.SetMaintenanceMode != nil && !*mur.SetMaintenanceMode), validation.Nil.Error("MaintenanceMessage cannot be specified unless SetMaintenanceMode is true")),
			validation.When(mur.SetMaintenanceMode != nil && *mur.SetMaintenanceMode && mur.MaintenanceMessage == nil, validation.Required.Error("MaintenanceMessage is required when SetMaintenanceMode is true")),
			validation.When(mur.SetMaintenanceMode != nil && *mur.SetMaintenanceMode && mur.MaintenanceMessage != nil, validation.Required.Error("MaintenanceMessage cannot be empty")),
			validation.When(mur.SetMaintenanceMode != nil && *mur.SetMaintenanceMode && mur.MaintenanceMessage != nil, validation.Match(camu.NotAllWhitespaceRegexp).Error("field consists only of whitespace")),
			validation.When(mur.SetMaintenanceMode != nil && *mur.SetMaintenanceMode && mur.MaintenanceMessage != nil, validation.Length(5, 256).Error(validationErrorMachineMaintenanceStringLength)),
		),
	)

	exclusiveOptionsCount := 0
	if mur.InstanceTypeID != nil {
		exclusiveOptionsCount++
	}

	if mur.ClearInstanceType != nil {
		exclusiveOptionsCount++
	}

	if mur.SetMaintenanceMode != nil {
		exclusiveOptionsCount++
	}

	if mur.Labels != nil {
		exclusiveOptionsCount++
	}

	if mur.OnlineRepair != nil {
		exclusiveOptionsCount++
	}

	if err == nil && exclusiveOptionsCount > 1 {
		err = validation.Errors{
			validationCommonErrorField: errors.New("only one of setMaintenanceMode, instanceTypeId, clearInstanceType, labels, or onlineRepair can be set at a time"),
		}
	}

	if err == nil && exclusiveOptionsCount == 0 {
		err = validation.Errors{
			validationCommonErrorField: errors.New("no updates specified. At least one of setMaintenanceMode, instanceTypeId, clearInstanceType, labels, or onlineRepair must be specified"),
		}
	}

	if err == nil && mur.ClearInstanceType != nil && !*mur.ClearInstanceType {
		err = validation.Errors{
			"clearInstanceType": errors.New("must be set to true to clear the Instance Type"),
		}
	}

	if err == nil && mur.HealthIssue != nil && mur.OnlineRepair == nil {
		err = validation.Errors{
			"healthIssue": errors.New("healthIssue must only be set together with onlineRepair"),
		}
	}

	if err == nil && mur.OnlineRepair != nil {
		orr := mur.OnlineRepair
		if orr.Enabled == nil {
			err = validation.Errors{
				"onlineRepair.enabled": errors.New("enabled is required when onlineRepair is set"),
			}
		} else if *orr.Enabled {
			verr := validation.Errors{}
			if mur.HealthIssue == nil {
				verr["healthIssue"] = errors.New("healthIssue is required when onlineRepair.enabled is true")
			} else {
				mhi := mur.HealthIssue
				if _, ok := ValidHealthIssueCategoriesMap[mhi.Category]; !ok || mhi.Category == "" {
					allowed := slices.Collect(maps.Keys(ValidHealthIssueCategoriesMap))
					verr["healthIssue.category"] = errors.New("must be one of " + strings.Join(allowed, ", "))
				}
				if mhi.Summary == nil || *mhi.Summary == "" {
					verr["healthIssue.summary"] = errors.New("summary is required")
				} else if utf8.RuneCountInString(*mhi.Summary) > MachineHealthIssueSummaryMaxLength {
					verr["healthIssue.summary"] = errors.New("summary must be at most " + strconv.Itoa(MachineHealthIssueSummaryMaxLength) + " characters")
				}
				if mhi.Details == nil || *mhi.Details == "" {
					verr["healthIssue.details"] = errors.New("details is required")
				} else if utf8.RuneCountInString(*mhi.Details) > MachineHealthIssueDetailsMaxLength {
					verr["healthIssue.details"] = errors.New("details must be at most " + strconv.Itoa(MachineHealthIssueDetailsMaxLength) + " characters")
				}
			}
			if orr.Policy == nil || orr.Policy.AllowAutoInstanceDeletionOnFailure == nil {
				verr["onlineRepair.policy"] = errors.New("policy.allowAutoInstanceDeletionOnFailure is required when entering online repair")
			}
			if orr.Acknowledgments == nil {
				verr["onlineRepair.acknowledgments"] = errors.New("acknowledgments is required when entering online repair")
			} else {
				a := orr.Acknowledgments
				if a.AcceptDataCorruptionRisk == nil || !*a.AcceptDataCorruptionRisk ||
					a.AcceptRepairTeamAccess == nil || !*a.AcceptRepairTeamAccess ||
					a.AcceptInstanceDeletionRisk == nil || !*a.AcceptInstanceDeletionRisk {
					verr["onlineRepair.acknowledgments"] = errors.New("all acknowledgment flags must be true to enter online repair")
				}
			}
			if len(verr) > 0 {
				err = verr
			}
		} else {
			if mur.HealthIssue != nil || orr.Policy != nil || orr.Acknowledgments != nil {
				err = validation.Errors{
					validationCommonErrorField: errors.New("healthIssue, onlineRepair.policy, and onlineRepair.acknowledgments must not be set when exiting online repair"),
				}
			}
		}
	}

	if err := camu.ValidateLabels(mur.Labels); err != nil {
		return err
	}

	return err
}

func (mur APIMachineUpdateRequest) ToInsertHealthReportRequestProto(machineID string) (*corev1.InsertMachineHealthReportRequest, error) {
	mhi := mur.HealthIssue

	m, err := json.Marshal(struct {
		Details       string `json:"details"`
		IssueCategory string `json:"issue_category"`
		Summary       string `json:"summary"`
	}{
		Details:       *mhi.Details,
		IssueCategory: ValidHealthIssueCategoriesMap[mhi.Category],
		Summary:       *mhi.Summary,
	})
	if err != nil {
		return nil, err
	}
	msg := string(m)

	alert := &corev1.HealthProbeAlert{
		Id:            MachineHealthAlertIDOnlineRepair,
		Target:        cutil.GetPtr(MachineTenantReportedIssueAlertID),
		Message:       msg,
		TenantMessage: cutil.GetPtr(fmt.Sprintf("TenantReportedIssue: %s", *mhi.Summary)),
		Classifications: []string{
			MachineAlertClassificationPreventAllocations,
			MachineAlertClassificationPreventInstanceDeletion,
			MachineAlertClassificationSuppressExternalAlerting,
		},
	}
	hr := &corev1.HealthReport{
		Source: MachineHealthOverrideSourceOnlineRepair,
		Alerts: []*corev1.HealthProbeAlert{alert},
	}
	return &corev1.InsertMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		HealthReportEntry: &corev1.HealthReportEntry{
			Report: hr,
			Mode:   corev1.HealthReportApplyMode_Merge,
		},
	}, nil
}

func (mur APIMachineUpdateRequest) ToRemoveHealthReportRequestProto(machineID string) (*corev1.RemoveMachineHealthReportRequest, error) {
	return &corev1.RemoveMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		Source:    MachineHealthOverrideSourceOnlineRepair,
	}, nil
}

// APIMachine is the data structure to capture API representation of a Machine
type APIMachine struct {
	// ID is the unique UUID v4 identifier for the Machine
	ID string `json:"id"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// InstanceTypeID is the ID of the associated Instance Type
	InstanceTypeID *string `json:"instanceTypeId"`
	// InstanceType is the summary of the associated Instance Type
	InstanceType *APIInstanceTypeSummary `json:"instanceType,omitempty"`
	// InstanceID is the ID of the associated Instance (if any)
	InstanceID *string `json:"instanceId"`
	// Instance is the summary of the associated Instance (if any)
	Instance *APIInstanceSummary `json:"instance,omitempty"`
	// TenantID is the ID of the Tenant that owns the Instance associated (if any)
	TenantID *string `json:"tenantId"`
	// Tenant is the summary of the Tenant that owns the Instance associated (if any)
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// ControllerMachineID is the ID of the controllerMachine
	ControllerMachineID string `json:"controllerMachineId"`
	// ControllerMachineType is the type of the controller machine
	ControllerMachineType *string `json:"controllerMachineType"`
	// HwSkuDeviceType is the sku derived device type of the machine, e.g. cpu, gpu, cache, storage, etc.
	HwSkuDeviceType *string `json:"hwSkuDeviceType"`
	// Vendor is the vendor of the Machine
	Vendor *string `json:"vendor"`
	// ProductName is the product name of the Machine
	ProductName *string `json:"productName"`
	// SerialNumber is the serial number of the Machine
	SerialNumber *string `json:"serialNumber"`
	// Hostname is the hostname of the Machine
	Hostname *string `json:"hostname"`
	// MachineCapabilities is the list of capabilities of the machine
	MachineCapabilities []APIMachineCapability `json:"machineCapabilities"`
	// MachineInterfaces is the list of admin interfaces of the machine
	MachineInterfaces []APIMachineInterface `json:"machineInterfaces"`
	// AssociatedDpuMachineIds are the IDs of the DPU Machines attached to this (host) Machine
	AssociatedDpuMachineIds []string `json:"associatedDpuMachineIds"`
	// PlacementInRack is the physical placement of the Machine within its Rack
	PlacementInRack *APIPlacementInRack `json:"placementInRack"`
	// MaintenanceMessage is the message to display during maintenance mode
	MaintenanceMessage *string `json:"maintenanceMessage"`
	// ScoutVersion is the Scout version reported by the Machine
	ScoutVersion *string `json:"scoutVersion"`
	// Metadata contains additional metadata about the machine
	Metadata *APIMachineMetadata `json:"metadata,omitempty"`
	// Health contains health information about the machine
	Health *APIMachineHealth `json:"health"`
	// Labels is VPC labels specified by user
	Labels map[string]string `json:"labels"`
	// Status represents the status of the machine
	Status string `json:"status"`
	// IsUsableByTenant indicates whether the machine is usable by or currently in use by a tenant.
	IsUsableByTenant bool `json:"isUsableByTenant"`
	// StatusHistory is the history of statuses for the Machine
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
	// Deprecations is the list of deprecation messages denoting fields which are being deprecated
	Deprecations []APIDeprecation `json:"deprecations,omitempty"`
}

// APIPlacementInRack captures the physical placement of a Machine within its Rack
type APIPlacementInRack struct {
	// SlotNumber is the rack slot the Machine occupies
	SlotNumber *int32 `json:"slotNumber"`
	// TrayIndex is the index of the Machine's tray within its slot
	TrayIndex *int32 `json:"trayIndex"`
}

// APIDMIData is the data structure to capture API representation of a Machine's DMIData
type APIDMIData struct {
	// BoardName is the name of the Machine's board
	BoardName *string `json:"boardName"`
	// BoardVersion is the version of the Machine's board
	BoardVersion *string `json:"boardVersion"`
	// BiosDate is the date of the Machine's bios
	BiosDate *string `json:"biosDate"`
	// BiosVersion is the version of the Machine's bios
	BiosVersion *string `json:"biosVersion"`
	// ProductName is the name of the Machine's product
	ProductName *string `json:"productName"`
	// ProductSerial is searial number the Machine
	ProductSerial *string `json:"productSerial"`
	// BoardSerial is the searial number of the Machine's board
	BoardSerial *string `json:"boardSerial"`
	// ChassisSerial is searial number the Machine's Chassis
	ChassisSerial *string `json:"chassisSerial"`
	// SysVendor is the vendor of the Machine's system
	SysVendor *string `json:"sysVendor"`
}

// FromProto populates an APIDMIData from its protobuf form.
func (admi *APIDMIData) FromProto(protoDMIData *corev1.DmiData) {
	if protoDMIData == nil {
		return
	}
	admi.BoardName = &protoDMIData.BoardName
	admi.BoardSerial = &protoDMIData.BoardSerial
	admi.BoardVersion = &protoDMIData.BoardVersion
	admi.BiosDate = &protoDMIData.BiosDate
	admi.BiosVersion = &protoDMIData.BiosVersion
	admi.ProductSerial = &protoDMIData.ProductSerial
	admi.ChassisSerial = &protoDMIData.ChassisSerial
	admi.ProductName = &protoDMIData.ProductName
	admi.SysVendor = &protoDMIData.SysVendor
}

// APIBMCInfo is the data structure to capture API representation of a Machine's BMC Info
type APIBMCInfo struct {
	// IP is the IP Address of the Machine's BMI
	IP *string `json:"ip"`
	// Mac is the Mac Address of the Machine's BMI
	Mac *string `json:"mac"`
	// Version is the version of the Machine's BMI
	Version *string `json:"version"`
	// firmwareRevision is the firmare version revision of the Machine's BMI
	FirmwareRevision *string `json:"firmwareRevision"`
}

// FromProto populates an APIBMCInfo from its protobuf form.
func (abmc *APIBMCInfo) FromProto(protoBmcInfo *corev1.BmcInfo) {
	if protoBmcInfo == nil {
		return
	}
	abmc.IP = protoBmcInfo.Ip
	abmc.Mac = protoBmcInfo.Mac
	abmc.Version = protoBmcInfo.Version
	abmc.FirmwareRevision = protoBmcInfo.FirmwareVersion
}

// APIMachineGPUInfo is the data structure to capture API representation of a Machine's GPU Info
type APIMachineGPUInfo struct {
	// Name of the Machine's GPU
	Name *string `json:"name"`
	// Serial is the serial number of the Machine's GPU
	Serial *string `json:"serial"`
	// DriverVersion is the version of the Machine's GPU driver
	DriverVersion *string `json:"driverVersion"`
	// VbiosVersion is the bios version of the Machine's GPU
	VbiosVersion *string `json:"vbiosVersion"`
	// InforomVersion is the info rom version of the Machine's GPU
	InforomVersion *string `json:"inforomVersion"`
	// TotalMemory is the total memory of the Machine's GPU
	TotalMemory *string `json:"totalMemory"`
	// Frequency is the frequency of the Machine's GPU
	Frequency *string `json:"frequency"`
	// PciBusId is the PCI BusId of the Machine's GPU
	PciBusId *string `json:"pciBusId"`
}

// FromProto populates an APIMachineGPUInfo from its protobuf form.
func (gpu *APIMachineGPUInfo) FromProto(protoGpu *corev1.Gpu) {
	if protoGpu == nil {
		return
	}
	gpu.Name = &protoGpu.Name
	gpu.Serial = &protoGpu.Serial
	gpu.DriverVersion = &protoGpu.DriverVersion
	gpu.VbiosVersion = &protoGpu.VbiosVersion
	gpu.InforomVersion = &protoGpu.InforomVersion
	gpu.TotalMemory = &protoGpu.TotalMemory
	gpu.Frequency = &protoGpu.Frequency
	gpu.PciBusId = &protoGpu.PciBusId
}

// APIMachineNetworkInterface is the data structure to capture API representation of a Machine's Network Interface Info
type APIMachineNetworkInterface struct {
	// Name of the Machine's NetworkInterface
	MacAddress *string `json:"macAddress"`
	// Vendor is the serial number of the Machine's NetworkInterface
	Vendor *string `json:"vendor"`
	// Device is the device number of the Machine's NetworkInterface
	Device *string `json:"device"`
	// Path is the bios path of the Machine's NetworkInterface
	Path *string `json:"path"`
	// NumaNode is the info of numa node Machine's NetworkInterface
	NumaNode *int32 `json:"numaNode"`
	// Description is the description the Machine's NetworkInterface
	Description *string `json:"description"`
	// Slot is the slot number of the Machine's NetworkInterface
	Slot *string `json:"slot"`
}

// FromProto populates an APIMachineNetworkInterface from its protobuf form.
func (ni *APIMachineNetworkInterface) FromProto(protoNI *corev1.NetworkInterface) {
	if protoNI == nil {
		return
	}
	ni.MacAddress = &protoNI.MacAddress
	if protoNI.PciProperties != nil {
		ni.Vendor = &protoNI.PciProperties.Vendor
		ni.Device = &protoNI.PciProperties.Device
		ni.Path = &protoNI.PciProperties.Path
		ni.NumaNode = &protoNI.PciProperties.NumaNode
		ni.Description = protoNI.PciProperties.Description
		ni.Slot = protoNI.PciProperties.Slot
	}
}

// APIMachineInfiniBandInterface is the data structure to capture API representation of a Machine's InfiniBand Interface Info
type APIMachineInfiniBandInterface struct {
	// Guid of the Machine's InfiniBandInterface
	Guid *string `json:"guid"`
	// Vendor is the serial number of the Machine's InfiniBandInterface
	Vendor *string `json:"vendor"`
	// Device is the device number of the Machine's InfiniBandInterface
	Device *string `json:"device"`
	// Path is the bios path of the Machine's InfiniBandInterface
	Path *string `json:"path"`
	// NumaNode is the info of numa node Machine's InfiniBandInterface
	NumaNode *int32 `json:"numaNode"`
	// Description is the description the Machine's InfiniBandInterface
	Description *string `json:"description"`
	// Slot is the slot number of the Machine's InfiniBandInterface
	Slot *string `json:"slot"`
}

// FromProto populates an APIMachineInfiniBandInterface from its protobuf form.
func (ib *APIMachineInfiniBandInterface) FromProto(protoIB *corev1.InfinibandInterface) {
	if protoIB == nil {
		return
	}
	ib.Guid = &protoIB.Guid
	if protoIB.PciProperties != nil {
		ib.Vendor = &protoIB.PciProperties.Vendor
		ib.Device = &protoIB.PciProperties.Device
		ib.Path = &protoIB.PciProperties.Path
		ib.NumaNode = &protoIB.PciProperties.NumaNode
		ib.Description = protoIB.PciProperties.Description
		ib.Slot = protoIB.PciProperties.Slot
	}
}

// APIMachineLifecycleState is the data structure to capture API representation of Machine lifecycle state
type APIMachineLifecycleState struct {
	// Value is the current lifecycle state value
	Value string `json:"value"`
	// IsAboveSLA indicates if the machine has been in the state longer than the defined SLA
	IsAboveSLA bool `json:"isAboveSLA"`
	// Updated indicates when the state was updated to the latest value
	Updated *time.Time `json:"updated"`
	// SLASeconds is the defined SLA for the current state in seconds
	SLASeconds int64 `json:"slaSeconds"`
}

// APIMachineMetadata is the data structure to capture API representation of a Machine's Metadata Info
type APIMachineMetadata struct {
	// DMIData is the DMI data of the machine
	DMIData *APIDMIData `json:"dmiData,omitempty"`
	// BMCInfo is the BMC Info of the machine
	BMCInfo *APIBMCInfo `json:"bmcInfo,omitempty"`
	// GPUInfo is the list of GPUs for the machine
	GPUs []APIMachineGPUInfo `json:"gpus,omitempty"`
	// NetworkInterfaces is the list of Ethernet interfaces of the machine
	NetworkInterfaces []APIMachineNetworkInterface `json:"networkInterfaces,omitempty"`
	// InfiniBandInterfaces is the list of InfiniBand interfaces of the machine
	InfiniBandInterfaces []APIMachineInfiniBandInterface `json:"infinibandInterfaces,omitempty"`
	// LifecycleState is the machine lifecycle state details
	LifecycleState *APIMachineLifecycleState `json:"lifecycleState,omitempty"`
}

// NewAPIMachine accepts a DB layer Machine object and returns an API object
func NewAPIMachine(dbm *cdbm.Machine, dbmcs []cdbm.MachineCapability, dbmis []cdbm.MachineInterface, dbsds []cdbm.StatusDetail, dbins *cdbm.Instance, includeMetadata bool, isProviderOrPrivilegedTenant bool) *APIMachine {
	apim := &APIMachine{
		ID:                       dbm.ID,
		InfrastructureProviderID: dbm.InfrastructureProviderID.String(),
		SiteID:                   dbm.SiteID.String(),
		ControllerMachineID:      dbm.ControllerMachineID,
		ControllerMachineType:    dbm.ControllerMachineType,
		HwSkuDeviceType:          dbm.HwSkuDeviceType,
		Vendor:                   dbm.Vendor,
		ProductName:              dbm.ProductName,
		Hostname:                 dbm.Hostname,
		MaintenanceMessage:       dbm.MaintenanceMessage,
		Labels:                   dbm.Labels,
		Status:                   dbm.Status,
		IsUsableByTenant:         dbm.IsUsableByTenant,
		Created:                  dbm.Created,
		Updated:                  dbm.Updated,
	}

	if dbm.InfrastructureProvider != nil {
		apim.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbm.InfrastructureProvider)
	}

	if dbm.Site != nil {
		apim.Site = NewAPISiteSummary(dbm.Site)
	}

	if dbm.InstanceTypeID != nil {
		apim.InstanceTypeID = cutil.GetPtr(dbm.InstanceTypeID.String())
	}

	if dbm.InstanceType != nil {
		apim.InstanceType = NewAPIInstanceTypeSummary(dbm.InstanceType)
	}

	if dbins != nil {
		apim.InstanceID = cutil.GetPtr(dbins.ID.String())
		apim.Instance = NewAPIInstanceSummary(dbins)
		apim.TenantID = cutil.GetPtr(dbins.TenantID.String())
		if dbins.Tenant != nil {
			apim.Tenant = NewAPITenantSummary(dbins.Tenant)
		}
	}

	apim.MachineCapabilities = []APIMachineCapability{}
	for _, dbmc := range dbmcs {
		cdbmc := dbmc
		apim.MachineCapabilities = append(apim.MachineCapabilities, *NewAPIMachineCapability(&cdbmc))
	}

	if isProviderOrPrivilegedTenant {
		apim.SerialNumber = dbm.SerialNumber
	}

	// The host's attached DPU machine IDs and rack placement are sourced from the
	// controller Machine metadata proto. They are not sensitive, so they are
	// surfaced regardless of the includeMetadata / provider gate below.
	apim.AssociatedDpuMachineIds = []string{}
	if dbm.Metadata != nil {
		if machine := dbm.Metadata.Machine; machine != nil {
			if status := machine.GetStatus(); status != nil && status.LastScoutObservedVersion != nil {
				apim.ScoutVersion = status.LastScoutObservedVersion
			} else {
				apim.ScoutVersion = machine.LastScoutObservedVersion
			}
		}
		for _, dpuID := range dbm.Metadata.GetStatus().GetAssociatedDpuMachineIds() {
			if id := dpuID.GetId(); id != "" {
				apim.AssociatedDpuMachineIds = append(apim.AssociatedDpuMachineIds, id)
			}
		}
		if placement := dbm.Metadata.GetPlacementInRack(); placement != nil {
			apim.PlacementInRack = &APIPlacementInRack{
				SlotNumber: placement.SlotNumber,
				TrayIndex:  placement.TrayIndex,
			}
		}
	}

	// Only Provider Admin can see the metadata
	if dbm.Metadata != nil && includeMetadata && isProviderOrPrivilegedTenant {
		apim.Metadata = &APIMachineMetadata{}

		// Get the Machine json body
		machine := dbm.Metadata
		discoveryInfo := machine.GetStatus().GetDiscoveryInfo()
		// BMCInfo
		if machine.BmcInfo != nil {
			apim.Metadata.BMCInfo = &APIBMCInfo{}
			apim.Metadata.BMCInfo.FromProto(machine.BmcInfo)
		}

		if discoveryInfo != nil {
			// DMIData
			if discoveryInfo.DmiData != nil {
				apim.Metadata.DMIData = &APIDMIData{}
				apim.Metadata.DMIData.FromProto(discoveryInfo.DmiData)
			}

			// GPUInfo
			if len(discoveryInfo.Gpus) > 0 {
				apim.Metadata.GPUs = []APIMachineGPUInfo{}
				for _, gpuInfo := range discoveryInfo.Gpus {
					lgpuInfo := APIMachineGPUInfo{}
					lgpuInfo.FromProto(gpuInfo)
					apim.Metadata.GPUs = append(apim.Metadata.GPUs, lgpuInfo)
				}
			}

			// Machine Network Interface Info
			if len(discoveryInfo.NetworkInterfaces) > 0 {
				apim.Metadata.NetworkInterfaces = []APIMachineNetworkInterface{}
				for _, nwiInfo := range discoveryInfo.NetworkInterfaces {
					lnwiInfo := APIMachineNetworkInterface{}
					lnwiInfo.FromProto(nwiInfo)
					apim.Metadata.NetworkInterfaces = append(apim.Metadata.NetworkInterfaces, lnwiInfo)
				}
			}

			// Machine InfiniBand Interface Info
			if len(discoveryInfo.InfinibandInterfaces) > 0 {
				apim.Metadata.InfiniBandInterfaces = []APIMachineInfiniBandInterface{}
				for _, ibiInfo := range discoveryInfo.InfinibandInterfaces {
					libiInfo := APIMachineInfiniBandInterface{}
					libiInfo.FromProto(ibiInfo)
					apim.Metadata.InfiniBandInterfaces = append(apim.Metadata.InfiniBandInterfaces, libiInfo)
				}
			}
		}

		// machine lifecycle state
		apim.Metadata.LifecycleState = &APIMachineLifecycleState{
			Value:      machine.State,
			IsAboveSLA: machine.GetStateSla().GetTimeInStateAboveSla(),
			SLASeconds: machine.GetStateSla().GetSla().GetSeconds(),
		}
		if events := machine.GetEvents(); len(events) > 0 {
			for _, event := range events {
				if event.GetVersion() == machine.GetStateVersion() {
					if ts := event.GetTime(); ts != nil {
						apim.Metadata.LifecycleState.Updated = new(ts.AsTime())
					}
					break
				}
			}
		}
	}

	apim.Health = nil

	// Report health info only provider requested
	if dbm.Health != nil && isProviderOrPrivilegedTenant {
		// Get the Machine Health json body
		machineHealth, err := dbm.GetHealth()
		if err == nil && machineHealth != nil {
			apim.Health = &APIMachineHealth{}
			apim.Health.FromDBModel(machineHealth)
		}
	}
	apim.MachineInterfaces = []APIMachineInterface{}
	for _, dbmi := range dbmis {
		cdbmi := dbmi
		apim.MachineInterfaces = append(apim.MachineInterfaces, *NewAPIMachineInterface(&cdbmi, isProviderOrPrivilegedTenant))
	}
	apim.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apim.StatusHistory = append(apim.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	return apim
}

// APIMachineSummary is the data structure to provide a sumamry of a Machine
type APIMachineSummary struct {
	// ID of the Machine
	ID string `json:"id"`
	// ControllerMachineID is the ID of the controllerMachine
	ControllerMachineID string `json:"controllerMachineId"`
	// ControllerMachineType is the type of the controller machine
	ControllerMachineType *string `json:"controllerMachineType"`
	// HwSkuDeviceType is the sku derived device type of the machine, e.g. cpu, gpu, cache, storage, etc.
	HwSkuDeviceType *string `json:"hwSkuDeviceType"`
	// Vendor is the vendor of the Machine
	Vendor *string `json:"vendor"`
	// ProductName is the product name of the Machine
	ProductName *string `json:"productName"`
	// MaintenanceMessage is the message to display during maintenance mode
	MaintenanceMessage *string `json:"maintenanceMessage"`
	// Status represents the status of the machine
	Status string `json:"status"`
}

// NewAPIMachineSummary accepts a DB layer Machine object and returns an API object
func NewAPIMachineSummary(dbm *cdbm.Machine) *APIMachineSummary {
	return &APIMachineSummary{
		ID:                    dbm.ID,
		ControllerMachineID:   dbm.ControllerMachineID,
		ControllerMachineType: dbm.ControllerMachineType,
		HwSkuDeviceType:       dbm.HwSkuDeviceType,
		Vendor:                dbm.Vendor,
		ProductName:           dbm.ProductName,
		MaintenanceMessage:    dbm.MaintenanceMessage,
		Status:                dbm.Status,
	}
}

// APIMachineStats is a data structure to capture information about machine stats at the API layer
type APIMachineStats struct {
	// Total is the total number of the machine object in NICo Cloud
	Total int `json:"total"`
	// Initializing is the total number of initializing machine object in NICo Cloud
	Initializing int `json:"initializing"`
	// Reset is the total number of reset machine object in NICo Cloud
	Reset int `json:"reset"`
	// Ready is the total number of ready machine object in NICo Cloud
	Ready int `json:"ready"`
	// InUse is the total number of Machines in use by Tenant Instances
	InUse int `json:"inUse"`
	// Error is the total number of error machine object in NICo Cloud
	Error int `json:"error"`
	// Decommissioned is the total number of decommissioned machine objects in NICo Cloud
	Decommissioned int `json:"decommissioned"`
	// Decommissioning is the total number of machines currently being decommissioned
	Decommissioning int `json:"decommissioning"`
	// Maintenance is the total number of machines in Maintenance
	Maintenance int `json:"maintenance"`
	// Unknown is the total number of unknown machine object in NICo Cloud
	Unknown int `json:"unknown"`
}
