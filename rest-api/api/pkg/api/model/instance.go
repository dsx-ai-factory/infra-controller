// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	goset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"gopkg.in/yaml.v3"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ValidatePowerProfile validates and normalizes set operations when direct DPS
// integration is enabled. Omitted values and explicit clears do not require
// policy discovery.
func ValidatePowerProfile(ctx context.Context, dpsEnabled bool, provider dpsclient.PolicyProvider, powerProfile *string) *cutil.APIError {
	if !dpsEnabled || powerProfile == nil || *powerProfile == "" {
		return nil
	}
	if provider == nil {
		return cutil.NewAPIError(http.StatusServiceUnavailable, "DPS power-profile validation is unavailable", nil)
	}

	normalized, err := dpsclient.ValidatePowerProfile(ctx, provider, *powerProfile)
	if err == nil {
		*powerProfile = normalized
		return nil
	}
	if errors.Is(err, dpsclient.ErrPowerProfileRequired) {
		return cutil.NewAPIError(http.StatusBadRequest, "Power profile must not be empty", nil)
	}
	if errors.Is(err, dpsclient.ErrPowerProfileNotFound) {
		return cutil.NewAPIError(http.StatusBadRequest, "Power profile does not exist in DPS", nil)
	}

	return cutil.NewAPIError(http.StatusServiceUnavailable, "Failed to validate power profile with DPS", nil)
}

const (
	// MaxInterfaceCount is the maximum number of Interfaces allowed per Instance
	MaxInterfaceCount = 16
	// MaxSpectrumXAttachmentCount is the maximum number of SpectrumX Attachments allowed per Instance.
	// Core caps the usable count at the Machine's SpectrumX PFs, and multiplane splits each port into
	// one PF per plane, so an 8-SuperNIC HGX node on the 4-plane Astra profile exposes 32. This only
	// keeps an unbounded list from reaching the Site, so it sits above that rather than near it.
	MaxSpectrumXAttachmentCount = 64
	// MachineIssueCategoryHardware is the category for hardware issues
	MachineIssueCategoryHardware = "Hardware"
	// MachineIssueCategoryNetwork is the category for network issues
	MachineIssueCategoryNetwork = "Network"
	// MachineIssueCategoryPerformance is the category for performance issues
	MachineIssueCategoryPerformance = "Performance"
	// MachineIssueCategoryOther is the category for other issues
	MachineIssueCategoryOther = "Other"
)

// validationErrorUserDataLength derives from util.MaxUserDataBytes so the
// message and the enforced limit cannot drift apart.
var validationErrorUserDataLength = fmt.Sprintf("`userData` must not exceed %d KiB", util.MaxUserDataBytes/1024)

var (
	// SitePhoneHomeCloudInit default cloudinit with phone home config
	SitePhoneHomeCloudInit = `#cloud-config
     phone_home:
        url: %s
        post: all`

	// MachineIssueCategoriesFromAPIToProtobuf is the map of instance issue categories to their corresponding values
	MachineIssueCategoriesFromAPIToProtobuf = map[string]int32{
		MachineIssueCategoryHardware:    int32(corev1.IssueCategory_HARDWARE),
		MachineIssueCategoryNetwork:     int32(corev1.IssueCategory_NETWORK),
		MachineIssueCategoryPerformance: int32(corev1.IssueCategory_PERFORMANCE),
		MachineIssueCategoryOther:       int32(corev1.IssueCategory_OTHER),
	}
)

// ValidateMultiEthernetDeviceInterfaces validates the Multi-Ethernet Device Interfaces for the Instance
// Check Instance Type's Machine Capabilities to ensure:
// Example: mapping of device, deviceInstance, isPhysical/virtualFunctionID
// device, deviceInstance, isPhysical/virtualFunctionID
// device, 0, 0 - Physical
// device, 0, 1 - Virtual
// device, 0, 2 - Virtual
// device, 0, 16 - Virtual
// device, 1, 0 - Physical
// device, 1, 1 - Virtual
// device, 1, 2 - Virtual
// device, 1, 16 - Virtual
// In above example, since deviceInstance is 0 and 1, it has 2 DPUs, make sure that `MachineCapabilityDeviceTypeDPU`
// is present in the Instance Type's Machine Capabilities with minimum count of 2
func ValidateMultiEthernetDeviceInterfaces(itNetworkCaps []cdbm.MachineCapability, dbifcs []cdbm.Interface) error {
	// Get the total count of device instances for the Instance Type's Machine Capabilities
	itDeviceInstanceCountMap := map[string]int{}
	for _, itNetworkCap := range itNetworkCaps {
		if itNetworkCap.Count != nil {
			itDeviceInstanceCountMap[itNetworkCap.Name] = *itNetworkCap.Count
		}
	}

	deviceInstanceMap := map[string]bool{}
	for _, dbifc := range dbifcs {
		if dbifc.Device != nil && dbifc.DeviceInstance != nil {
			deviceInstanceId := fmt.Sprintf("%s-%d", *dbifc.Device, *dbifc.DeviceInstance)
			if dbifc.IsPhysical {
				deviceInstanceId = fmt.Sprintf("%s-physical", deviceInstanceId)
			} else {
				deviceInstanceId = fmt.Sprintf("%s-virtual-%d", deviceInstanceId, *dbifc.VirtualFunctionID)
			}

			// Check if the device name is present in the Instance Type's network capabilities
			_, exists := itDeviceInstanceCountMap[*dbifc.Device]
			if !exists {
				return validation.Errors{
					"device": fmt.Errorf("Device %v is not present in the Instance Type's network capabilities", *dbifc.Device),
				}
			}

			// Check if duplicate device name and device instance is provided in the request
			_, exists = deviceInstanceMap[deviceInstanceId]
			if exists {
				return validation.Errors{
					"device": fmt.Errorf("Duplicate Interface configuration specified for Device %v, Device Instance: %v", *dbifc.Device, *dbifc.DeviceInstance),
				}
			}

			// Check if the requested device instance count is greater than the number of device instances in the Instance Type's Machine Capabilities
			if *dbifc.DeviceInstance >= itDeviceInstanceCountMap[*dbifc.Device] {
				return validation.Errors{
					"deviceInstance": fmt.Errorf("Device Instance: %v for Device %v exceeds Instance Type's network capability count", *dbifc.DeviceInstance, *dbifc.Device),
				}
			}
			// Add device instance to map to check for duplicates
			deviceInstanceMap[deviceInstanceId] = true
		}
	}

	return nil
}

// ValidateInterfaces validates the Interfaces for the Instance
func ValidateInterfaces(ifcs *[]APIInterfaceCreateOrUpdateRequest) error {
	// Validate Interfaces
	vpcInterfaceCount := 0
	subnetInterfaceCount := 0
	hasVpcSelection := false

	multiEthernetInterfaceCount := 0
	singleEthernetInterfaceCount := 0

	physicalInterfaceCount := 0

	for index := range *ifcs {
		ifcr := &(*ifcs)[index]
		err := ifcr.Validate()

		if err != nil {
			return err
		}

		if ifcr.VpcPrefixID != nil || ifcr.VpcID != nil {
			vpcInterfaceCount++
			hasVpcSelection = hasVpcSelection || ifcr.VpcID != nil
		} else {
			subnetInterfaceCount++
		}

		if ifcr.IsMultiEthernetInterface() {
			multiEthernetInterfaceCount++
		} else {
			singleEthernetInterfaceCount++
		}

		if ifcr.IsPhysical {
			physicalInterfaceCount++
		}
	}

	if vpcInterfaceCount > 0 && subnetInterfaceCount > 0 {
		if hasVpcSelection {
			return validation.Errors{
				validationCommonErrorField: errors.New("either all interfaces must be VPC based or all of them must be Subnet based"),
			}
		}
		return validation.Errors{
			validationCommonErrorField: errors.New("either all interfaces must be VPC Prefix based or all of them must be Subnet based"),
		}
	}

	if multiEthernetInterfaceCount > 0 && singleEthernetInterfaceCount > 0 {
		return validation.Errors{
			validationCommonErrorField: errors.New("either all interfaces must specify device/deviceInstance or none of them should specify those fields"),
		}
	}

	if singleEthernetInterfaceCount > 0 {
		if physicalInterfaceCount > 1 {
			return validation.Errors{
				"interface": errors.New("only one interface can be marked as physical for single-Ethernet interfaces"),
			}
		} else if physicalInterfaceCount == 0 {
			// Set first interface as physical if none of the interface is marked as physical
			(*ifcs)[0].IsPhysical = true
		}
	}

	return nil
}

// InfiniBandInterfaceRequestMatchResult captures whether a machine can satisfy an InfiniBand interface request.
type InfiniBandInterfaceRequestMatchResult struct {
	Satisfied                 bool
	CountSatisfiable          bool
	SuggestedByDevice         map[string][]int
	UnsatisfiedRequestIndices []int
}

// ValidateInfiniBandRequestForMachineCapability checks whether machine InfiniBand capabilities
// can satisfy the requested interfaces.
func (req *APIInstanceCreateRequest) ValidateInfiniBandRequestForMachineCapability(machineIbCaps []cdbm.MachineCapability) InfiniBandInterfaceRequestMatchResult {
	capByDevice := make(map[string]cdbm.MachineCapability, len(machineIbCaps))
	for _, cap := range machineIbCaps {
		capByDevice[cap.Name] = cap
	}

	result := InfiniBandInterfaceRequestMatchResult{
		Satisfied:         true,
		CountSatisfiable:  true,
		SuggestedByDevice: make(map[string][]int, len(capByDevice)),
	}

	// Build the available by device map from the capabilities
	for device, cap := range capByDevice {
		// If the count is nil, skip the device
		if cap.Count == nil {
			continue
		}

		inactive := make(map[int]bool, len(cap.InactiveDevices))
		for _, deviceInstance := range cap.InactiveDevices {
			inactive[deviceInstance] = true
		}

		active := make([]int, 0, *cap.Count)
		for deviceInstance := 0; deviceInstance < *cap.Count; deviceInstance++ {
			if !inactive[deviceInstance] {
				active = append(active, deviceInstance)
			}
		}
		// Add the active device instances to the available by device map
		result.SuggestedByDevice[device] = active
	}

	requestedByDevice := make(map[string]int)
	for idx, ibifc := range req.InfiniBandInterfaces {
		cap, found := capByDevice[ibifc.Device]
		if !found {
			result.Satisfied = false
			result.CountSatisfiable = false
			result.UnsatisfiedRequestIndices = append(result.UnsatisfiedRequestIndices, idx)
			continue
		}

		if ibifc.Vendor != nil && cap.Vendor != nil && *ibifc.Vendor != *cap.Vendor {
			result.Satisfied = false
			result.UnsatisfiedRequestIndices = append(result.UnsatisfiedRequestIndices, idx)
		}

		activeDeviceInstance := make(map[int]bool, len(result.SuggestedByDevice[ibifc.Device]))
		for _, deviceInstance := range result.SuggestedByDevice[ibifc.Device] {
			activeDeviceInstance[deviceInstance] = true
		}
		if !activeDeviceInstance[ibifc.DeviceInstance] {
			result.Satisfied = false
			result.UnsatisfiedRequestIndices = append(result.UnsatisfiedRequestIndices, idx)
		}

		requestedByDevice[ibifc.Device]++
	}

	for device, requestedCount := range requestedByDevice {
		if len(result.SuggestedByDevice[device]) < requestedCount {
			result.CountSatisfiable = false
		}
	}

	if len(req.InfiniBandInterfaces) == 0 {
		result.Satisfied = true
		result.CountSatisfiable = true
	}

	return result
}

// ValidateInfiniBandInterfaces validates the InfiniBand Interfaces for Instance create/update request
func ValidateInfiniBandInterfaces(itIbCaps []cdbm.MachineCapability, ibifcs []APIInfiniBandInterfaceCreateOrUpdateRequest) error {
	// Get the total count of device instances for the InfiniBand Instance Type's Machine Capabilities
	deviceInstanceCountMap := map[string]int{}
	deviceInactiveInstanceMap := map[string]map[int]bool{}
	deviceVendorMap := map[string]bool{}

	for _, itIbCap := range itIbCaps {
		if itIbCap.Count != nil {
			deviceInstanceCountMap[itIbCap.Name] = *itIbCap.Count
		}
		if itIbCap.InactiveDevices != nil {
			deviceInactiveInstanceMap[itIbCap.Name] = make(map[int]bool)
			for _, inactiveDevice := range itIbCap.InactiveDevices {
				deviceInactiveInstanceMap[itIbCap.Name][inactiveDevice] = true
			}
		}
		if itIbCap.Vendor != nil {
			deviceVendorMap[*itIbCap.Vendor] = true
		}
	}

	deviceInstanceMap := map[string]bool{}

	for _, ibifc := range ibifcs {
		if ibifc.Device != "" && ibifc.DeviceInstance >= 0 {
			deviceInstanceId := fmt.Sprintf("%s-%d", ibifc.Device, ibifc.DeviceInstance)
			if ibifc.IsPhysical {
				deviceInstanceId = fmt.Sprintf("%s-physical", deviceInstanceId)
			}

			// Check if the duplicate infiniband device instance is already present in the request
			_, exists := deviceInstanceMap[deviceInstanceId]
			if exists {
				return validation.Errors{
					"device": fmt.Errorf("Duplicate InfiniBand interface configuration specified for Device %v, Device Instance: %v", ibifc.Device, ibifc.DeviceInstance),
				}
			}

			// Check if the infiniband device name is present in the  Instance Type's InfiniBand Capabilities
			_, exists = deviceInstanceCountMap[ibifc.Device]
			if !exists {
				return validation.Errors{
					"device": fmt.Errorf("Device %v is not present in Instance Type's InfiniBand Capabilities", ibifc.Device),
				}
			}

			// Check if the infiniband device vendor is present in the  Instance Type's InfiniBand Capabilities
			if ibifc.Vendor != nil && !deviceVendorMap[*ibifc.Vendor] {
				return validation.Errors{
					"vendor": fmt.Errorf("Vendor %v is not present in Instance Type's InfiniBand Capabilities", *ibifc.Vendor),
				}
			}

			if ibifc.DeviceInstance >= deviceInstanceCountMap[ibifc.Device] {
				return validation.Errors{
					"deviceInstance": fmt.Errorf("Device Instance: %v for Device %v exceeds Instance Type's InfiniBand Capabilities count", ibifc.DeviceInstance, ibifc.Device),
				}
			}

			// Check if the specified Instance Type's InfiniBand device instance is inactive
			_, exists = deviceInactiveInstanceMap[ibifc.Device]
			if exists {
				_, exists = deviceInactiveInstanceMap[ibifc.Device][ibifc.DeviceInstance]
				if exists {
					return validation.Errors{
						"deviceInstance": fmt.Errorf("Device Instance: %v for Device %v is inactive", ibifc.DeviceInstance, ibifc.Device),
					}
				}
			}

			// Add device instance to map to check for duplicates
			deviceInstanceMap[deviceInstanceId] = true
		}
	}

	return nil
}

// ValidateDpuExtensionServiceDeployments validates the DpuExtensionServiceDeployments for the Instance create/update request
func ValidateDpuExtensionServiceDeployments(desdrs []APIDpuExtensionServiceDeploymentRequest) error {
	for _, desdr := range desdrs {
		err := desdr.Validate()
		if err != nil {
			return err
		}
	}

	desVersionMap := map[string]bool{}
	for _, desdr := range desdrs {
		desvID := fmt.Sprintf("%s:%s", desdr.DpuExtensionServiceID, desdr.Version)
		_, exists := desVersionMap[desvID]
		if exists {
			return validation.Errors{
				"dpuExtensionServiceDeployments": fmt.Errorf("duplicate deployment requests found for DPU Extension Service ID and version: %s", desvID),
			}
		}
		desVersionMap[desvID] = true
	}

	return nil
}

// ValidateSpectrumXAttachments validates the SpectrumX Attachments for the Instance create/update request.
// Each attachment consumes one device instance in Core's allocate_spx_port_mac, which rejects a repeated
// device and device instance pair irrespective of virtualFunctionId. Reject it here so the caller gets a
// 400 instead of a Site failure. Core bounds the real count by the Machine's SpectrumX interfaces, so
// MaxSpectrumXAttachmentCount only keeps an unbounded list from reaching it.
func ValidateSpectrumXAttachments(sacs []APISpectrumXAttachmentCreateOrUpdateRequest) error {
	if len(sacs) > MaxSpectrumXAttachmentCount {
		return validation.Errors{
			"spectrumXAttachments": fmt.Errorf("at most %v SpectrumX Attachments can be specified", MaxSpectrumXAttachmentCount),
		}
	}

	deviceInstanceMap := map[string]bool{}

	for _, sac := range sacs {
		err := sac.Validate()
		if err != nil {
			return err
		}

		deviceInstanceID := fmt.Sprintf("%s-%d", sac.Device, *sac.DeviceInstance)

		_, exists := deviceInstanceMap[deviceInstanceID]
		if exists {
			return validation.Errors{
				"spectrumXAttachments": fmt.Errorf("duplicate SpectrumX Attachment specified for Device %v, Device Instance: %v", sac.Device, *sac.DeviceInstance),
			}
		}

		deviceInstanceMap[deviceInstanceID] = true
	}

	return nil
}

// ValidateNVLinkInterfaces validates the NVLink interfaces for Instance create/update request.
// A subset of GPUs may be specified; specifying more interfaces than GPUs is not allowed.
// Each DeviceInstance (GPU index) must be unique and within the valid range for the machine.
func ValidateNVLinkInterfaces(itNvlCaps []cdbm.MachineCapability, nvlifcs []APINVLinkInterfaceCreateOrUpdateRequest) error {
	for _, itNvlCap := range itNvlCaps {
		if itNvlCap.Count == nil {
			continue
		}
		gpuCount := *itNvlCap.Count

		if len(nvlifcs) > gpuCount {
			return validation.Errors{
				"nvLinkInterfaces": fmt.Errorf("number of NVLink Interfaces (%d) exceeds the number of available GPU indexes (%d)", len(nvlifcs), gpuCount),
			}
		}

		// Validate each DeviceInstance is within range and unique
		seen := make(map[int]bool, len(nvlifcs))
		for _, nvlifc := range nvlifcs {
			if nvlifc.DeviceInstance < 0 || nvlifc.DeviceInstance >= gpuCount {
				return validation.Errors{
					"nvLinkInterfaces": fmt.Errorf("deviceInstance: %d is out of available range [0, %d]", nvlifc.DeviceInstance, gpuCount),
				}
			}
			if seen[nvlifc.DeviceInstance] {
				return validation.Errors{
					"nvLinkInterfaces": fmt.Errorf("duplicate deviceInstance: %d specified in NVLink Interfaces", nvlifc.DeviceInstance),
				}
			}
			seen[nvlifc.DeviceInstance] = true
		}
	}
	return nil
}

// APIInstanceCreateRequest is the data structure to capture request to create a new Instance
type APIInstanceCreateRequest struct {
	// Name is the name of the Instance
	Name string `json:"name"`
	// Description is the description of the Instance
	Description *string `json:"description"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// InstanceTypeID is the ID of the Instance Type. Only InstanceTypeID or Machineid can be present
	InstanceTypeID *string `json:"instanceTypeId"`
	// VpcID is the ID of the VPC containing the Instance
	VpcID string `json:"vpcId"`
	// SecondaryVpcIDs lists additional VPC UUIDs for non-primary interfaces on
	// the Instance that select a prefix. Validate() rejects this field unless
	// every entry in Interfaces uses vpcPrefixId or vpcId, and the create handler then
	// verifies that the supplied UUIDs exactly match the VPCs resolved from those
	// interfaces.
	SecondaryVpcIDs []string `json:"secondaryVpcIds"`
	// OperatingSystemID is the ID of the Operating System
	OperatingSystemID *string `json:"operatingSystemId"`
	// IpxeScript is the iPXE script for the Operating System
	IpxeScript *string `json:"ipxeScript"`
	// AlwaysBootWithCustomIpxe is the flag to allow always boot with ipxe
	AlwaysBootWithCustomIpxe *bool `json:"alwaysBootWithCustomIpxe"`
	// PhoneHomeEnabled is the flag to allow enable phone home for the instance
	PhoneHomeEnabled *bool `json:"phoneHomeEnabled"`
	// UserData is the ID of the Operating System
	UserData *string `json:"userData"`
	// Interfaces is the list of Interfaces to create for the Instance.
	// Mutually exclusive with `AutoNetwork`: when `AutoNetwork` is true this MUST be empty.
	Interfaces []APIInterfaceCreateOrUpdateRequest `json:"interfaces"`
	// AutoNetwork, when true, asks NICo to auto-resolve the Instance's network
	// interfaces from the host's HostInband network segments. Intended for
	// instances on zero-DPU hosts (or hosts with their DPU in NIC mode).
	// When true, `Interfaces` MUST be empty. The resolved per-interface
	// details surface in the Instance's status.
	AutoNetwork bool `json:"autoNetwork"`
	// InfiniBandInterfaces is the list of InfiniBandInterface to create for the Instance
	InfiniBandInterfaces []APIInfiniBandInterfaceCreateOrUpdateRequest `json:"infinibandInterfaces"`
	// SpectrumXAttachments is the list of SpectrumX Partition attachments to create for the Instance
	SpectrumXAttachments []APISpectrumXAttachmentCreateOrUpdateRequest `json:"spectrumXAttachments"`
	// DpuExtensionServiceDeployments is the list of DpuExtensionServiceDeployments to create for the Instance
	DpuExtensionServiceDeployments []APIDpuExtensionServiceDeploymentRequest `json:"dpuExtensionServiceDeployments"`
	// NVLinkInterfaces is the list of NVLinkInterface to create for the Instance
	NVLinkInterfaces []APINVLinkInterfaceCreateOrUpdateRequest `json:"nvLinkInterfaces"`
	// SSHKeyGroupIDs is a list of SSHKeyID objects
	SSHKeyGroupIDs []string `json:"sshKeyGroupIds"`
	// Labels is a key value objects
	Labels map[string]string `json:"labels"`
	// NetworkSecurityGroupID is the ID if a desired
	// NSG to attach to the instance
	NetworkSecurityGroupID *string `json:"networkSecurityGroupId"`
	// MachineID is the ID of the Machine. Only MachineID or InstanceTypeID can be present
	MachineID *string `json:"machineId"`
	// MachineLabelSelector restricts automatic Machine selection, or validates a
	// specifically requested Machine, by exact Machine label key/value matches.
	// All entries must match.
	MachineLabelSelector map[string]string `json:"machineLabelSelector"`
	// AllowUnhealthyMachine is a flag that can be used to target Machines are in maintenance or have health alerts preventing regular provision flow.
	AllowUnhealthyMachine *bool `json:"allowUnhealthyMachine"`
	// PowerProfile is the external power provisioning profile for the Instance.
	PowerProfile *string `json:"powerProfile"`
}

// APIBatchInstanceCreateRequest is the data structure to capture request to create multiple instances in a single request
// with rack-aware allocation logic to place instances on the same rack when possible
type APIBatchInstanceCreateRequest struct {
	// NamePrefix is the prefix for instance names (e.g., "worker" will create "worker-1", "worker-2",
	// etc.)
	NamePrefix string `json:"namePrefix"`
	// Count is the number of instances to create
	Count int `json:"count"`
	// Description is the description for all instances
	Description *string `json:"description"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// InstanceTypeID is the ID of the Instance Type
	InstanceTypeID string `json:"instanceTypeId"`
	// MachineLabelSelector restricts Machine selection by exact Machine label
	// key/value matches. All entries must match.
	MachineLabelSelector map[string]string `json:"machineLabelSelector"`
	// VpcID is the ID of the VPC containing the Instances
	VpcID string `json:"vpcId"`
	// SecondaryVpcIDs lists additional VPC UUIDs for non-primary interfaces on
	// each Instance in the batch that select a prefix. Validate() rejects this
	// field unless every entry in Interfaces uses vpcPrefixId or vpcId, and batch create
	// processing expects these UUIDs to align with the VPCs implied by those
	// interfaces.
	SecondaryVpcIDs []string `json:"secondaryVpcIds"`
	// OperatingSystemID is the ID of the Operating System
	OperatingSystemID *string `json:"operatingSystemId"`
	// IpxeScript is the iPXE script for the Operating System
	IpxeScript *string `json:"ipxeScript"`
	// AlwaysBootWithCustomIpxe is the flag to allow always boot with ipxe
	AlwaysBootWithCustomIpxe *bool `json:"alwaysBootWithCustomIpxe"`
	// PhoneHomeEnabled is the flag to allow enable phone home for the instance
	PhoneHomeEnabled *bool `json:"phoneHomeEnabled"`
	// UserData is the user data for the instances
	UserData *string `json:"userData"`
	// PowerProfile is the external power provisioning profile for every Instance in the batch.
	PowerProfile *string `json:"powerProfile"`
	// Interfaces is the list of Interfaces to create for each instance (shared across all instances).
	// Mutually exclusive with `AutoNetwork`: when `AutoNetwork` is true this MUST be empty.
	Interfaces []APIInterfaceCreateOrUpdateRequest `json:"interfaces"`
	// AutoNetwork, when true, asks NICo to auto-resolve each Instance's network
	// interfaces from the host's HostInband network segments. Intended for
	// instances on zero-DPU hosts (or hosts with their DPU in NIC mode).
	// When true, `Interfaces` MUST be empty.
	AutoNetwork bool `json:"autoNetwork"`
	// InfiniBandInterfaces is the list of InfiniBandInterface to create for each instance (shared across all instances)
	InfiniBandInterfaces []APIInfiniBandInterfaceCreateOrUpdateRequest `json:"infinibandInterfaces"`
	// SpectrumXAttachments is the list of SpectrumX Partition attachments to create for each instance (shared across all instances)
	SpectrumXAttachments []APISpectrumXAttachmentCreateOrUpdateRequest `json:"spectrumXAttachments"`
	// NVLinkInterfaces is the list of NVLinkInterface to create for each instance (shared across all instances)
	NVLinkInterfaces []APINVLinkInterfaceCreateOrUpdateRequest `json:"nvLinkInterfaces"`
	// DpuExtensionServiceDeployments is the list of DpuExtensionServiceDeployments to create for each Instance (shared across all instances)
	DpuExtensionServiceDeployments []APIDpuExtensionServiceDeploymentRequest `json:"dpuExtensionServiceDeployments"`
	// SSHKeyGroupIDs is a list of SSHKeyGroup IDs (shared across all instances)
	SSHKeyGroupIDs []string `json:"sshKeyGroupIds"`
	// Labels is a key value objects to be applied to all instances (shared across all instances)
	Labels map[string]string `json:"labels"`
	// NetworkSecurityGroupID is the ID of a desired NSG to attach to all instances (shared across all instances)
	NetworkSecurityGroupID *string `json:"networkSecurityGroupId"`
	// TopologyOptimized indicates whether to enforce rack-aware placement
	// If true, all instances must be allocated on machines within the same rack or the request will fail
	TopologyOptimized *bool `json:"topologyOptimized"`
}

// Validate ensure the values passed in request are acceptable
func (icr APIInstanceCreateRequest) Validate() error {
	err := validation.ValidateStruct(&icr,
		validation.Field(&icr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&icr.Description,
			validation.When(icr.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&icr.TenantID,
			validation.When(icr.TenantID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&icr.InstanceTypeID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&icr.VpcID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&icr.OperatingSystemID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&icr.UserData,
			validation.When(icr.UserData != nil,
				validation.Length(0, util.MaxUserDataBytes).Error(validationErrorUserDataLength)),
		),
		validation.Field(&icr.PowerProfile,
			validation.When(icr.PowerProfile != nil, validation.Required.Error("`powerProfile` must not be empty"))),
		validation.Field(&icr.Interfaces,
			// When AutoNetwork is true, the Instance has NICo auto-resolve interfaces
			// from the host's HostInband segments, so the explicit list MUST
			// be empty. Otherwise at least one interface is required.
			validation.When(icr.AutoNetwork,
				validation.Length(0, 0).Error("`interfaces` must be empty when `autoNetwork` is true"),
			).Else(
				validation.Required.Error("at least one Interface must be specified"),
				validation.Length(1, MaxInterfaceCount).Error(fmt.Sprintf("at most %v Interfaces can be specified", MaxInterfaceCount)),
			)),
	)

	if err != nil {
		return err
	}

	if icr.SecondaryVpcIDs != nil {
		if icr.AutoNetwork {
			return validation.Errors{
				"secondaryVpcIds": errors.New("`secondaryVpcIds` is not supported when `autoNetwork` is true"),
			}
		}
		for _, iface := range icr.Interfaces {
			if iface.VpcPrefixID == nil && iface.VpcID == nil {
				return validation.Errors{
					"secondaryVpcIds": errors.New("`secondaryVpcIds` can only be specified when `vpcPrefixId` or `vpcId` is specified within `interfaces`"),
				}
			}
		}
	}

	// ensure we have one and only one of InstanceTypeID or MachineID
	if icr.InstanceTypeID != nil && icr.MachineID != nil {
		return validation.Errors{"machineId": errors.New("only one of `instanceTypeId` or `machineId` can be specified in request, not both")}
	} else if icr.InstanceTypeID == nil && icr.MachineID == nil {
		return validation.Errors{"instanceTypeId": errors.New("either `instanceTypeId` or `machineId` must be specified")}
	}

	// make sure either of them provided
	if icr.OperatingSystemID == nil {
		if icr.IpxeScript == nil {
			return validation.Errors{
				"operatingSystemId": errors.New("either `operatingSystemId` or `ipxeScript` must be specified"),
			}
		} else if *icr.IpxeScript == "" {
			return validation.Errors{
				"ipxeScript": errors.New("cannot be empty when `operatingSystemId` is not specified"),
			}
		}
	}

	// Validate Interfaces
	err = ValidateInterfaces(&icr.Interfaces)
	if err != nil {
		return err
	}

	// Validate InfiniBand Interfaces
	for _, ibic := range icr.InfiniBandInterfaces {
		err = ibic.Validate()
		if err != nil {
			return err
		}
	}

	// Validate SpectrumX Attachments
	err = ValidateSpectrumXAttachments(icr.SpectrumXAttachments)
	if err != nil {
		return err
	}

	// Validate DpuExtensionServiceDeployments
	err = ValidateDpuExtensionServiceDeployments(icr.DpuExtensionServiceDeployments)
	if err != nil {
		return err
	}

	// Validate NVLink interfaces
	// Make sure NVLink Logical Partition ID is valid
	for _, nvlifc := range icr.NVLinkInterfaces {
		err = nvlifc.Validate()
		if err != nil {
			return err
		}
	}

	err = util.ValidateLabels(icr.Labels)
	if err != nil {
		return err
	}
	err = validateMachineLabelSelector(icr.MachineLabelSelector)
	if err != nil {
		return err
	}

	return err
}

// ValidateForVpc validates request fields whose legality depends on the
// resolved VPC the Instance will be created in. It is separate from
// Validate() because the VPC is only known after a DB lookup in the
// handler. Core enforces the same rule; this is defense in depth that
// also avoids round-tripping the Site for an obviously bad request.
func (icr APIInstanceCreateRequest) ValidateForVpc(vpc *cdbm.Vpc) error {
	// `autoNetwork` asks NICo to resolve interfaces from the host's
	// HostInband segments, which only makes sense on a Flat VPC.
	if icr.AutoNetwork && !cdbm.VpcTypeSupportsAutoInterface(vpc.NetworkVirtualizationType) {
		return validation.Errors{
			"autoNetwork": errors.New("`autoNetwork` is only supported when the VPC has `networkVirtualizationType` set to `FLAT`"),
		}
	}
	return nil
}

// Validate the OS against any additional option combinations specified.
func (icr *APIInstanceCreateRequest) ValidateAndSetOperatingSystemData(cfg *config.Config, os *cdbm.OperatingSystem) error {
	// The OS passed in will either be:
	// * The OS that the request is attempting to set for the instance
	// * Nil, if custom/no-OS is being chosen.
	//
	// The error returned will be used to provide context to the API caller/client
	// and should not be allowed to leak any internal information.

	// Default to the request data
	mergedUserData := icr.UserData
	mergedPhoneHomeEnabled := icr.PhoneHomeEnabled
	mergedIpxeScript := icr.IpxeScript
	mergedAlwaysBootWithCustomIpxe := icr.AlwaysBootWithCustomIpxe

	// If the request supplies no user-data of its own, the document being
	// edited is the base OS's blob, whose phone-home block NICo authored
	// whenever the OS was stored with phone-home enabled. That block is
	// removed by key, because the URL frozen into it may predate a change
	// to site.phoneHomeUrl. Caller-supplied user-data stays URL-matched.
	nicoAuthoredPhoneHome := icr.UserData == nil && os != nil && os.PhoneHomeEnabled

	if os == nil {
		// If no OS is being chosen...
		// The expectation is that iPXE, user-data, and phone-home
		// settings must all be explicitly passed in..

		if mergedIpxeScript == nil {
			return validation.Errors{
				"ipxeScript": errors.New("cannot be unset or empty if no Operating System is specified"),
			}
		}

		if mergedPhoneHomeEnabled == nil {
			mergedPhoneHomeEnabled = cutil.GetPtr(false)
			icr.PhoneHomeEnabled = mergedPhoneHomeEnabled
		}

		if mergedAlwaysBootWithCustomIpxe == nil {
			mergedAlwaysBootWithCustomIpxe = cutil.GetPtr(false)
			icr.AlwaysBootWithCustomIpxe = mergedAlwaysBootWithCustomIpxe
		}

	} else {

		// ensure OS is active
		if !os.IsActive {
			return validation.Errors{
				"isActive": errors.New("Operating System specified in request has been deactivated and cannot be used to create an Instance"),
			}
		}

		osUserDataOverrideRequested := icr.UserData != nil

		// Merge things in from OS when not
		// found in request.
		if mergedIpxeScript == nil {
			// If no script was sent in the request, and the
			// request is selecting the operating system,
			// give it precedence.
			// I.e., the caller has said, "Use this base OS,"
			// but has not sent in any override for iPXE script,
			// so the script of the base OS will be used.
			mergedIpxeScript = os.IpxeScript

			// Set it so the DB gets updated.
			icr.IpxeScript = mergedIpxeScript
		}

		if mergedUserData == nil {
			// If no user-data was sent in the request, and the
			// request is changing the operating system,
			// give it precedence.
			// I.e., the caller has said, "Use this base OS,"
			// but has not sent in any override for user-data,
			// so the user-data of the base OS will be used.
			mergedUserData = os.UserData

			// Set it so that the DB gets updated
			icr.UserData = mergedUserData
		}

		if mergedPhoneHomeEnabled == nil {
			// If no phoneHomeEnabled was sent in the request,
			// and the request is selecting a base OS,
			// give it precedence.
			// This means that a user MUST send in an
			// instance-level override (via the API request) if desired.
			mergedPhoneHomeEnabled = &os.PhoneHomeEnabled

			// Set it so that the DB gets updated.
			icr.PhoneHomeEnabled = mergedPhoneHomeEnabled
		}

		if mergedAlwaysBootWithCustomIpxe == nil {
			// If no alwaysBootWithCustomIpxe was sent in the request,
			// default to false.
			// This means that a user MUST send in an
			// instance-level override (via the API request) if desired.
			mergedAlwaysBootWithCustomIpxe = cutil.GetPtr(false)

			// Set it so that the DB gets updated.
			icr.AlwaysBootWithCustomIpxe = mergedAlwaysBootWithCustomIpxe
		}

		// Start the real validation
		if os.Type != cdbm.OperatingSystemTypeIPXE {
			if mergedIpxeScript != nil && *mergedIpxeScript != "" {
				return validation.Errors{
					"ipxeScript": fmt.Errorf("cannot be specified with Operating System of type `%s`", os.Type),
				}
			}

			if *mergedAlwaysBootWithCustomIpxe {
				return validation.Errors{
					"alwaysBootWithCustomIpxe": fmt.Errorf("cannot be set with Operating System of type `%s`", os.Type),
				}
			}
		} else {
			if mergedIpxeScript == nil || *mergedIpxeScript == "" {
				return validation.Errors{
					"ipxeScript": errors.New("cannot be unset or empty with iPXE-based Operating System"),
				}
			}
		}

		// Validate that the OS permits overriding user-data
		if osUserDataOverrideRequested && !os.AllowOverride {
			return validation.Errors{
				"allowOverride": errors.New("Operating System specified in request does not allow overriding `userData`"),
			}
		}
	}

	// If the request is setting PhoneHomeEnabled
	if icr.PhoneHomeEnabled != nil {
		// If there's some existing user-data,
		// we'll need to modify it to either insert phone-home
		// settings or snip them out
		if mergedUserData != nil && *mergedUserData != "" {
			userDataMap := &yaml.Node{}

			var documentRoot *yaml.Node

			isUserDataValidYAML := false
			err := yaml.Unmarshal([]byte(*mergedUserData), userDataMap)

			if err == nil {

				// We have a slightly more restrictive view of what
				// counts as valid YAML.
				if len(userDataMap.Content) > 0 {
					documentRoot = userDataMap.Content[0]

					if documentRoot.Kind == yaml.MappingNode {
						isUserDataValidYAML = true
					}
				}
			}

			if *mergedPhoneHomeEnabled {
				// Phone home can only be enabled if the user-data is valid YAML
				if !isUserDataValidYAML {
					return validation.Errors{
						"userData": errors.New("userData specified in request must be valid CloudInit YAML to enable phone home"),
					}
				}

				if err := util.InsertPhoneHomeIntoUserData(documentRoot, cfg.GetSitePhoneHomeUrl()); err != nil {
					return validation.Errors{
						"userData": errors.New("failed to insert phone-home into userData"),
					}
				}

			} else if isUserDataValidYAML {
				// We have to make sure we don't try to remove from invalid yaml,
				// but the UI will always send false if phone-home is unchecked,
				// so we want to do this check silently and not alert people who
				// are using non-YAML user-data.

				// NICo's own block is removed by key, because the URL frozen
				// into it may predate a change to site.phoneHomeUrl.
				var phoneHomeURLFilter *string
				if !nicoAuthoredPhoneHome {
					phoneHomeURLFilter = cutil.GetPtr(cfg.GetSitePhoneHomeUrl())
				}

				if err := util.RemovePhoneHomeFromUserData(documentRoot, phoneHomeURLFilter); err != nil {
					return validation.Errors{
						"userData": errors.New("failed to disable phone-home in userData after processing phone home config"),
					}
				}

			}

			// If there's still user-data, marshal so that it can be stored in the DB later
			if isUserDataValidYAML && len(documentRoot.Content) > 0 {

				byteUserData, err := util.MarshalUserData(userDataMap)
				if err != nil {
					return validation.Errors{
						"userData": errors.New("failed to re-construct userData after processing phone home config"),
					}
				}
				icr.UserData = cutil.GetPtr(string(byteUserData))
			} else if isUserDataValidYAML && !*mergedPhoneHomeEnabled {
				// This would be a case of valid YAML where the user
				// disabled phone-home.
				// If the only user-data _was_ the phone-home data but phone-home
				// is being disabled, then we'll blank out the field in the DB.
				icr.UserData = cutil.GetPtr("")
			}
			// There's an implied case here of invalid YAML
			// In that case, we do nothing, and icr.UserData will stay untouched.
		} else {
			// If user-data is nil or empty, but phone-home is being enabled,
			// we need to set the default phone-home settings string.
			// (Nothing to do if user-data is nil or empty and phone-home is being disabled.)
			if *mergedPhoneHomeEnabled {
				icr.UserData = cutil.GetPtr(fmt.Sprintf(SitePhoneHomeCloudInit, cfg.GetSitePhoneHomeUrl()))
			}
		}
	}

	return util.ValidateEffectiveUserData(icr.UserData)
}

// ValidateMultiEthernetDeviceInterfaces validates the Multi-Ethernet Device Interfaces for the Instance
func (icr *APIInstanceCreateRequest) ValidateMultiEthernetDeviceInterfaces(itNetworkCaps []cdbm.MachineCapability, dbifcs []cdbm.Interface) error {
	return ValidateMultiEthernetDeviceInterfaces(itNetworkCaps, dbifcs)
}

// ValidateInfiniBandInterfaces validates the InfiniBand Interfaces for the Instance
func (icr *APIInstanceCreateRequest) ValidateInfiniBandInterfaces(itIbCaps []cdbm.MachineCapability) error {
	return ValidateInfiniBandInterfaces(itIbCaps, icr.InfiniBandInterfaces)
}

// ValidateDpuExtensionServiceDeployments validates the DpuExtensionServiceDeployments for the Instance create request
func (icr *APIInstanceCreateRequest) ValidateDpuExtensionServiceDeployments(desdrs []APIDpuExtensionServiceDeploymentRequest) error {
	return ValidateDpuExtensionServiceDeployments(desdrs)
}

// Validate ensure the values passed in batch instance create request are acceptable
func (bicr APIBatchInstanceCreateRequest) Validate() error {
	err := validation.ValidateStruct(&bicr,
		validation.Field(&bicr.NamePrefix,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 240).Error("Name prefix must contain at least 2 characters and a maximum of 240 characters")),
		validation.Field(&bicr.Count,
			validation.Required.Error(validationErrorValueRequired),
			validation.Min(2).Error("Count must be at least 2"),
			// TODO: the number 18 is a temporary limit until we have a better way to handle topology-optimized allocation. 18 is the largest possible GB200 domain size.
			validation.Max(18).Error("Count cannot exceed 18")),
		validation.Field(&bicr.Description,
			validation.When(bicr.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&bicr.TenantID,
			validation.When(bicr.TenantID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&bicr.InstanceTypeID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&bicr.VpcID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&bicr.OperatingSystemID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&bicr.UserData,
			validation.When(bicr.UserData != nil,
				validation.Length(0, util.MaxUserDataBytes).Error(validationErrorUserDataLength)),
		),
		validation.Field(&bicr.PowerProfile,
			validation.When(bicr.PowerProfile != nil, validation.Required.Error("`powerProfile` must not be empty"))),
		validation.Field(&bicr.Interfaces,
			// When AutoNetwork is true, the batch has NICo auto-resolve interfaces
			// from the host's HostInband segments, so the explicit list MUST
			// be empty. Otherwise at least one interface is required.
			validation.When(bicr.AutoNetwork,
				validation.Length(0, 0).Error("`interfaces` must be empty when `autoNetwork` is true"),
			).Else(
				validation.Required.Error("at least one Interface must be specified"),
				validation.Length(1, MaxInterfaceCount).Error(fmt.Sprintf("at most %v Interfaces can be specified", MaxInterfaceCount)),
			)),
	)

	if err != nil {
		return err
	}

	if bicr.SecondaryVpcIDs != nil {
		if bicr.AutoNetwork {
			return validation.Errors{
				"secondaryVpcIds": errors.New("`secondaryVpcIds` is not supported when `autoNetwork` is true"),
			}
		}
		for _, iface := range bicr.Interfaces {
			if iface.VpcPrefixID == nil && iface.VpcID == nil {
				return validation.Errors{
					"secondaryVpcIds": errors.New("`secondaryVpcIds` can only be specified when `vpcPrefixId` or `vpcId` is specified within `interfaces`"),
				}
			}
		}
	}

	// Validate that either OperatingSystemID or IpxeScript is specified
	if bicr.OperatingSystemID == nil {
		if bicr.IpxeScript == nil {
			return validation.Errors{
				"operatingSystemId": errors.New("either `operatingSystemId` or `ipxeScript` must be specified"),
			}
		} else if *bicr.IpxeScript == "" {
			return validation.Errors{
				"ipxeScript": errors.New("cannot be empty when `operatingSystemId` is not specified"),
			}
		}
	}

	// Validate Interfaces
	err = ValidateInterfaces(&bicr.Interfaces)
	if err != nil {
		return err
	}
	for _, ifc := range bicr.Interfaces {
		if ifc.IPAddress != nil {
			return validation.Errors{
				"interfaces": errors.New("batch instance create does not support `ipAddress` on interfaces"),
			}
		}
	}

	// Validate InfiniBand Interfaces
	for _, ibic := range bicr.InfiniBandInterfaces {
		err = ibic.Validate()
		if err != nil {
			return err
		}
	}

	// Validate SpectrumX Attachments
	err = ValidateSpectrumXAttachments(bicr.SpectrumXAttachments)
	if err != nil {
		return err
	}

	// Validate DpuExtensionServiceDeployments
	err = ValidateDpuExtensionServiceDeployments(bicr.DpuExtensionServiceDeployments)
	if err != nil {
		return err
	}

	// Validate NVLink interfaces
	// Make sure NVLink Logical Partition ID is valid
	for _, nvlifc := range bicr.NVLinkInterfaces {
		err = nvlifc.Validate()
		if err != nil {
			return err
		}
	}

	err = util.ValidateLabels(bicr.Labels)
	if err != nil {
		return err
	}
	err = validateMachineLabelSelector(bicr.MachineLabelSelector)
	if err != nil {
		return err
	}

	// err should be nil at this point
	return err
}

// validateMachineLabelSelector applies the shared label-map contract while
// reporting validation errors against the public request field.
func validateMachineLabelSelector(selector map[string]string) error {
	err := util.ValidateLabels(selector)
	if err == nil {
		return nil
	}

	labelErrors, ok := err.(validation.Errors)
	if !ok {
		return validation.Errors{"machineLabelSelector": err}
	}

	labelErr, found := labelErrors["labels"]
	if !found {
		return validation.Errors{"machineLabelSelector": err}
	}
	if errors.Is(labelErr, util.ErrValidationLabelNUL) {
		return validation.Errors{
			"machineLabelSelector": errors.New("machine label selector keys and values must not contain NUL characters"),
		}
	}

	return validation.Errors{"machineLabelSelector": labelErr}
}

// ValidateForVpc validates request fields whose legality depends on the
// resolved VPC the batch's Instances will be created in. See
// APIInstanceCreateRequest.ValidateForVpc for rationale.
func (bicr APIBatchInstanceCreateRequest) ValidateForVpc(vpc *cdbm.Vpc) error {
	if bicr.AutoNetwork && !cdbm.VpcTypeSupportsAutoInterface(vpc.NetworkVirtualizationType) {
		return validation.Errors{
			"autoNetwork": errors.New("`autoNetwork` is only supported when the VPC has `networkVirtualizationType` set to `FLAT`"),
		}
	}
	return nil
}

// Validate the OS against any additional option combinations specified.
func (bicr *APIBatchInstanceCreateRequest) ValidateAndSetOperatingSystemData(cfg *config.Config, os *cdbm.OperatingSystem) error {
	// The OS passed in will either be:
	// * The OS that the request is attempting to set for the instance
	// * Nil, if custom/no-OS is being chosen.
	//
	// The error returned will be used to provide context to the API caller/client
	// and should not be allowed to leak any internal information.

	// Default to the request data
	mergedUserData := bicr.UserData
	mergedPhoneHomeEnabled := bicr.PhoneHomeEnabled
	mergedIpxeScript := bicr.IpxeScript
	mergedAlwaysBootWithCustomIpxe := bicr.AlwaysBootWithCustomIpxe

	// If the request supplies no user-data of its own, the document being
	// edited is the base OS's blob, whose phone-home block NICo authored
	// whenever the OS was stored with phone-home enabled. That block is
	// removed by key, because the URL frozen into it may predate a change
	// to site.phoneHomeUrl. Caller-supplied user-data stays URL-matched.
	nicoAuthoredPhoneHome := bicr.UserData == nil && os != nil && os.PhoneHomeEnabled

	if os == nil {
		// If no OS is being chosen...
		// The expectation is that iPXE, user-data, and phone-home
		// settings must all be explicitly passed in..

		if mergedIpxeScript == nil {
			return validation.Errors{
				"ipxeScript": errors.New("cannot be unset or empty if no Operating System is specified"),
			}
		}

		if mergedPhoneHomeEnabled == nil {
			mergedPhoneHomeEnabled = cutil.GetPtr(false)
			bicr.PhoneHomeEnabled = mergedPhoneHomeEnabled
		}

		if mergedAlwaysBootWithCustomIpxe == nil {
			mergedAlwaysBootWithCustomIpxe = cutil.GetPtr(false)
			bicr.AlwaysBootWithCustomIpxe = mergedAlwaysBootWithCustomIpxe
		}

	} else {

		// ensure OS is active
		if !os.IsActive {
			return validation.Errors{
				"isActive": errors.New("Operating System specified in request has been deactivated and cannot be used to create an Instance"),
			}
		}

		osUserDataOverrideRequested := bicr.UserData != nil

		// Merge things in from OS when not
		// found in request.
		if mergedIpxeScript == nil {
			mergedIpxeScript = os.IpxeScript
			bicr.IpxeScript = mergedIpxeScript
		}

		if mergedUserData == nil {
			mergedUserData = os.UserData
			bicr.UserData = mergedUserData
		}

		if mergedPhoneHomeEnabled == nil {
			mergedPhoneHomeEnabled = &os.PhoneHomeEnabled
			bicr.PhoneHomeEnabled = mergedPhoneHomeEnabled
		}

		if mergedAlwaysBootWithCustomIpxe == nil {
			mergedAlwaysBootWithCustomIpxe = cutil.GetPtr(false)
			bicr.AlwaysBootWithCustomIpxe = mergedAlwaysBootWithCustomIpxe
		}

		// Start the real validation
		if os.Type != cdbm.OperatingSystemTypeIPXE {
			if mergedIpxeScript != nil && *mergedIpxeScript != "" {
				return validation.Errors{
					"ipxeScript": fmt.Errorf("cannot be specified with Operating System of type `%s`", os.Type),
				}
			}

			if *mergedAlwaysBootWithCustomIpxe {
				return validation.Errors{
					"alwaysBootWithCustomIpxe": fmt.Errorf("cannot be set with Operating System of type `%s`", os.Type),
				}
			}
		} else {
			if mergedIpxeScript == nil || *mergedIpxeScript == "" {
				return validation.Errors{
					"ipxeScript": errors.New("cannot be unset or empty with iPXE-based Operating System"),
				}
			}
		}

		// Validate that the OS permits overriding user-data
		if osUserDataOverrideRequested && !os.AllowOverride {
			return validation.Errors{
				"allowOverride": errors.New("Operating System specified in request does not allow overriding `userData`"),
			}
		}
	}

	// If the request is setting PhoneHomeEnabled
	if bicr.PhoneHomeEnabled != nil {
		// If there's some existing user-data,
		// we'll need to modify it to either insert phone-home
		// settings or snip them out
		if mergedUserData != nil && *mergedUserData != "" {
			userDataMap := &yaml.Node{}

			var documentRoot *yaml.Node

			isUserDataValidYAML := false
			err := yaml.Unmarshal([]byte(*mergedUserData), userDataMap)

			if err == nil {

				// We have a slightly more restrictive view of what
				// counts as valid YAML.
				if len(userDataMap.Content) > 0 {
					documentRoot = userDataMap.Content[0]

					if documentRoot.Kind == yaml.MappingNode {
						isUserDataValidYAML = true
					}
				}
			}

			if *mergedPhoneHomeEnabled {
				// Phone home can only be enabled if the user-data is valid YAML
				if !isUserDataValidYAML {
					return validation.Errors{
						"userData": errors.New("userData specified in request must be valid CloudInit YAML to enable phone home"),
					}
				}

				if err := util.InsertPhoneHomeIntoUserData(documentRoot, cfg.GetSitePhoneHomeUrl()); err != nil {
					return validation.Errors{
						"userData": errors.New("failed to insert phone-home into userData"),
					}
				}

			} else if isUserDataValidYAML {
				// NICo's own block is removed by key, because the URL frozen
				// into it may predate a change to site.phoneHomeUrl.
				var phoneHomeURLFilter *string
				if !nicoAuthoredPhoneHome {
					phoneHomeURLFilter = cutil.GetPtr(cfg.GetSitePhoneHomeUrl())
				}

				if err := util.RemovePhoneHomeFromUserData(documentRoot, phoneHomeURLFilter); err != nil {
					return validation.Errors{
						"userData": errors.New("failed to disable phone-home in userData after processing phone home config"),
					}
				}
			}

			// If there's still user-data, marshal so that it can be stored in the DB later
			if isUserDataValidYAML && len(documentRoot.Content) > 0 {

				byteUserData, err := util.MarshalUserData(userDataMap)
				if err != nil {
					return validation.Errors{
						"userData": errors.New("failed to re-construct userData after processing phone home config"),
					}
				}
				bicr.UserData = cutil.GetPtr(string(byteUserData))
			} else if isUserDataValidYAML && !*mergedPhoneHomeEnabled {
				bicr.UserData = cutil.GetPtr("")
			}
		} else {
			// If user-data is nil or empty, but phone-home is being enabled,
			// we need to set the default phone-home settings string.
			if *mergedPhoneHomeEnabled {
				bicr.UserData = cutil.GetPtr(fmt.Sprintf(SitePhoneHomeCloudInit, cfg.GetSitePhoneHomeUrl()))
			}
		}
	}

	return util.ValidateEffectiveUserData(bicr.UserData)
}

// ValidateNVLinkInterfaces validates the NVLink interfaces for the Instance
func (icr *APIInstanceCreateRequest) ValidateNVLinkInterfaces(itNvlCaps []cdbm.MachineCapability) error {
	return ValidateNVLinkInterfaces(itNvlCaps, icr.NVLinkInterfaces)
}

// APIInstanceUpdateRequest is the data structure to capture request to update an Instance
type APIInstanceUpdateRequest struct {
	// Name is the name of the Instance
	Name *string `json:"name"`
	// Description is the description of the Instance
	Description *string `json:"description"`
	// Labels is a key value objects
	Labels map[string]string `json:"labels"`
	// TriggerReboot is the flag to trigger reboot
	TriggerReboot *bool `json:"triggerReboot"`
	// RebootWithCustomIpxe is the flag to allow reboot with ipxe
	RebootWithCustomIpxe *bool `json:"rebootWithCustomIpxe"`
	// ApplyUpdatesOnReboot is the flag to allow update first before reboot
	ApplyUpdatesOnReboot *bool `json:"applyUpdatesOnReboot"`
	// OperatingSystemID is the ID of the Operating System
	OperatingSystemID *string `json:"operatingSystemId"`
	// IpxeScript is the iPXE script for the Operating System
	IpxeScript *string `json:"ipxeScript"`
	// UserData is the user-date to be used when booting; e.g., a cloud-init config
	UserData *string `json:"userData"`
	// PhoneHomeEnabled is an attribute which is specified by user if Instance needs to be enabled for phone home or not
	PhoneHomeEnabled *bool `json:"phoneHomeEnabled"`
	// AlwaysBootWithCustomIpxe is an attribute which is specified by user if instance boot with ipxe or not
	AlwaysBootWithCustomIpxe *bool `json:"alwaysBootWithCustomIpxe"`
	// SecondaryVpcIDs lists additional VPC IDs for non-primary interfaces on the
	// Instance that select a prefix. This field will be rejected unless
	// Interfaces is provided and non-empty and every entry in Interfaces uses
	// vpcPrefixId or vpcId. The update handler then verifies that the supplied UUIDs
	// exactly match the VPCs resolved from those interfaces.
	SecondaryVpcIDs []string `json:"secondaryVpcIds"`
	// Interfaces is the list of Interfaces to update for the Instance.
	// Mutually exclusive with `AutoNetwork`: when `AutoNetwork` is true this MUST be empty.
	Interfaces []APIInterfaceCreateOrUpdateRequest `json:"interfaces"`
	// AutoNetwork, when set, asks NICo to auto-resolve the Instance's network
	// interfaces from the host's HostInband network segments. `nil` leaves
	// the value unchanged; `true` re-resolves; `false` returns to explicit
	// interface configuration. When `true`, `Interfaces` MUST be empty.
	AutoNetwork *bool `json:"autoNetwork"`
	// InfiniBandInterfaces is the list of InfiniBandInterface to update for the Instance
	InfiniBandInterfaces []APIInfiniBandInterfaceCreateOrUpdateRequest `json:"infinibandInterfaces"`
	// SpectrumXAttachments is the list of SpectrumX Partition attachments to update for the Instance. `nil` leaves
	// the Instance's SpectrumX attachments unchanged; a non-nil (possibly empty) list replaces them entirely.
	SpectrumXAttachments []APISpectrumXAttachmentCreateOrUpdateRequest `json:"spectrumXAttachments"`
	// DpuExtensionServiceDeployments is the list of DpuExtensionServiceDeployments to update for the Instance
	DpuExtensionServiceDeployments []APIDpuExtensionServiceDeploymentRequest `json:"dpuExtensionServiceDeployments"`
	// NVLinkInterfaces is the list of NVLinkInterface to update for the Instance
	NVLinkInterfaces []APINVLinkInterfaceCreateOrUpdateRequest `json:"nvLinkInterfaces"`
	// SSHKeyGroupIDs is a list of SSHKeyID objects
	SSHKeyGroupIDs []string `json:"sshKeyGroupIds"`
	// NetworkSecurityGroupID is the ID of Network Security Group to attach to the Instance
	NetworkSecurityGroupID *string `json:"networkSecurityGroupId"`
	// PowerProfile updates the external power provisioning profile. An empty string clears it.
	PowerProfile *string `json:"powerProfile"`
}

// Validate the OS against any additional option combinations specified.
func (iur *APIInstanceUpdateRequest) ValidateAndSetOperatingSystemData(cfg *config.Config, instance *cdbm.Instance, os *cdbm.OperatingSystem) error {

	// The OS passed in will either be:
	// * The OS that the request is attempting to set for the instance
	// * The existing OS of the instance if the request is not attempting
	//   to change base OS
	// * Nil, if the OS is being cleared
	//
	// The error returned will be used to provide context to the API caller/client
	// and should not be allowed to leak any internal information.

	// Default to the request data
	mergedUserData := iur.UserData
	mergedPhoneHomeEnabled := iur.PhoneHomeEnabled
	mergedIpxeScript := iur.IpxeScript
	mergedAlwaysBootWithCustomIpxe := iur.AlwaysBootWithCustomIpxe

	// If the request supplies no user-data of its own, the document being
	// edited is a stored blob — the new base OS's when the request changes
	// the OS, otherwise the instance's — and any phone-home block in it was
	// authored by NICo whenever that blob was stored with phone-home
	// enabled. Such a block is removed by key, because the URL frozen into
	// it may predate a change to site.phoneHomeUrl. Caller-supplied
	// user-data stays URL-matched.
	nicoAuthoredPhoneHome := false
	if iur.UserData == nil {
		if iur.OperatingSystemID != nil {
			nicoAuthoredPhoneHome = os != nil && os.PhoneHomeEnabled
		} else {
			nicoAuthoredPhoneHome = instance.PhoneHomeEnabled
		}
	}

	if os == nil {
		// If the OS is being cleared...
		// The expectation is that iPXE, user-data, and phone-home
		// settings must all be explicitly passed in..
		if iur.OperatingSystemID != nil && *iur.OperatingSystemID == "" {
			if mergedUserData == nil {
				mergedUserData = cutil.GetPtr("")
				iur.UserData = mergedUserData
			}

			if mergedPhoneHomeEnabled == nil {
				mergedPhoneHomeEnabled = cutil.GetPtr(false)
				iur.PhoneHomeEnabled = mergedPhoneHomeEnabled
			}
		} else {
			// Otherwise, this is a case where an update request is coming in
			// for an instance that didn't already have an associated OS,
			// and there is no change being requested for the base OS.
			// I.e., it's being left as an instance with no associated OS.
			// In that case, the instance's OS settings are the only ones
			// and should be respected if not explicitly overridden.
			if mergedIpxeScript == nil {
				mergedIpxeScript = instance.IpxeScript
				iur.IpxeScript = mergedIpxeScript
			}

			if mergedUserData == nil {
				mergedUserData = instance.UserData
				iur.UserData = mergedUserData
			}

			if mergedPhoneHomeEnabled == nil {
				mergedPhoneHomeEnabled = &instance.PhoneHomeEnabled
				iur.PhoneHomeEnabled = mergedPhoneHomeEnabled
			}
		}

		if mergedIpxeScript == nil {
			return validation.Errors{
				"ipxeScript": errors.New("cannot be unset or empty if no OperatingSystem is specified"),
			}
		}
	} else {

		osUserDataOverrideRequested := iur.UserData != nil

		// Merge things in from OS and/or instance when not
		// found in request.
		if mergedIpxeScript == nil {
			if iur.OperatingSystemID != nil {
				// If no script was sent in the request, and the
				// request is changing the operating system,
				// give it precedence.
				// I.e., the caller has said, "Switch the base OS,"
				// but has not sent in any override for iPXE script,
				// so the script of the base OS will be used.
				mergedIpxeScript = os.IpxeScript

				// Set it so the DB gets updated.
				iur.IpxeScript = mergedIpxeScript
			} else {
				// If no script was sent in the request AND no
				// change in base OS is being requested, then
				// use the existing iPXE script of the instance.
				mergedIpxeScript = instance.IpxeScript
			}
		}

		if mergedUserData == nil {
			if iur.OperatingSystemID != nil {
				// If no user-data was sent in the request, and the
				// request is changing the operating system,
				// give it precedence.
				// I.e., the caller has said, "Switch the base OS,"
				// but has not sent in any override for user-data,
				// so the user-data of the base OS will be used.
				mergedUserData = os.UserData

				// Set it so that the DB gets updated
				iur.UserData = mergedUserData
			} else {
				// If no user-data was sent in the request AND no
				// change in base OS is being requested, then
				// use the existing user-data of the instance.
				mergedUserData = instance.UserData
			}
		}

		if mergedPhoneHomeEnabled == nil {
			if iur.OperatingSystemID != nil {
				// If no phoneHomeEnabled was sent in the request,
				// and the request is changing the operating system,
				// give it precedence.
				// This means that a user MUST send in an
				// instance-level override (via the API request) if desired.
				mergedPhoneHomeEnabled = &os.PhoneHomeEnabled

				// Set it so that the DB gets updated.
				iur.PhoneHomeEnabled = mergedPhoneHomeEnabled
			} else {
				mergedPhoneHomeEnabled = &instance.PhoneHomeEnabled
			}
		}

		if mergedAlwaysBootWithCustomIpxe == nil {
			mergedAlwaysBootWithCustomIpxe = &instance.AlwaysBootWithCustomIpxe
		}

		// Start the real validation
		if os.Type != cdbm.OperatingSystemTypeIPXE {
			if mergedIpxeScript != nil && *mergedIpxeScript != "" {
				return validation.Errors{
					"ipxeScript": fmt.Errorf("cannot be specified if Operating System of type `%s`", os.Type),
				}
			}

			if *mergedAlwaysBootWithCustomIpxe {
				return validation.Errors{
					"alwaysBootWithCustomIpxe": fmt.Errorf("cannot be set if Operating System is of type `%s`", os.Type),
				}
			}
		} else {
			if mergedIpxeScript == nil || *mergedIpxeScript == "" {
				return validation.Errors{
					"ipxeScript": errors.New("cannot be unset or empty with iPXE-based Operating System"),
				}
			}
		}

		// Validate that the OS permits overriding user-data
		if osUserDataOverrideRequested && !os.AllowOverride {
			return validation.Errors{
				"allowOverride": errors.New("Operating System specified in request does not allow overriding user data"),
			}
		}
	}

	// If the request is updating PhoneHomeEnabled
	// OR the request is updating user-data,
	// OR the request updated the base OS,
	// which could have updated the user-data,
	// then we'll need to make sure we update user-data accordingly.
	if iur.PhoneHomeEnabled != nil || iur.UserData != nil || iur.OperatingSystemID != nil {
		// If there's some existing user-data,
		// we'll need to modify it to either insert phone-home
		// settings or snip them out
		if mergedUserData != nil && *mergedUserData != "" {
			userDataMap := &yaml.Node{}

			var documentRoot *yaml.Node

			isUserDataValidYAML := false
			err := yaml.Unmarshal([]byte(*mergedUserData), userDataMap)

			if err == nil {

				// We have a slightly more restrictive view of what
				// counts as valid YAML.
				if len(userDataMap.Content) > 0 {
					documentRoot = userDataMap.Content[0]

					if documentRoot.Kind == yaml.MappingNode {
						isUserDataValidYAML = true
					}
				}
			}

			if *mergedPhoneHomeEnabled {
				// Phone home can only be enabled if the user-data is valid YAML
				if !isUserDataValidYAML {
					return validation.Errors{
						"userData": errors.New("must be valid CloudInit YAML to enable phone home"),
					}
				}

				if err := util.InsertPhoneHomeIntoUserData(documentRoot, cfg.GetSitePhoneHomeUrl()); err != nil {
					return validation.Errors{
						"userData": errors.New("failed to insert phone-home into userData"),
					}
				}

			} else if isUserDataValidYAML {
				// We have to make sure we don't try to remove from invalid yaml,
				// but the UI will always send false if phone-home is unchecked,
				// so we want to do this check silently and not alert people who
				// are using non-YAML user-data.

				// NICo's own block is removed by key, because the URL frozen
				// into it may predate a change to site.phoneHomeUrl.
				var phoneHomeURLFilter *string
				if !nicoAuthoredPhoneHome {
					phoneHomeURLFilter = cutil.GetPtr(cfg.GetSitePhoneHomeUrl())
				}

				if err := util.RemovePhoneHomeFromUserData(documentRoot, phoneHomeURLFilter); err != nil {
					return validation.Errors{
						"userData": errors.New("failed to disable phone-home in userData after processing phone home config"),
					}
				}
			}

			// If there's still user-data, marshal so that it can be stored in the DB later
			if isUserDataValidYAML && len(documentRoot.Content) > 0 {

				byteUserData, err := util.MarshalUserData(userDataMap)
				if err != nil {
					return validation.Errors{
						"userData": errors.New("failed to re-construct userData after processing phone home config"),
					}
				}
				iur.UserData = cutil.GetPtr(string(byteUserData))
			} else if isUserDataValidYAML && !*mergedPhoneHomeEnabled {
				// This would be a case of valid YAML where the user
				// disabled phone-home.
				// If the only user-data _was_ the phone-home data but phone-home
				// is being disabled, then we'll blank out the field in the DB.
				iur.UserData = cutil.GetPtr("")
			}
			// There's an implied case here of invalid YAML
			// In that case, we do nothing, and iur.UserData will stay untouche
		} else {
			// If user-data is nil or empty, but phone-home is being enabled,
			// we need to set the default phone-home settings string.
			// (Nothing to do if user-data is nil or empty and phone-home is being disabled.)
			if *mergedPhoneHomeEnabled {
				iur.UserData = cutil.GetPtr(fmt.Sprintf(SitePhoneHomeCloudInit, cfg.GetSitePhoneHomeUrl()))
			}
		}
	}

	// An update that touches none of user-data, the base OS, or phone-home
	// leaves iur.UserData nil, so the value heading to the Site is the stored
	// blob that mergedUserData resolved to. Checking it here rather than
	// assigning it back keeps the field absent from the update, so an
	// unrelated update cannot rewrite a column the caller never named.
	effectiveUserData := iur.UserData
	if effectiveUserData == nil {
		effectiveUserData = mergedUserData
	}

	return util.ValidateEffectiveUserData(effectiveUserData)
}

// ValidateMultiEthernetDeviceInterfaces validates the Multi-Ethernet Device Interfaces for the Instance
func (iur *APIInstanceUpdateRequest) ValidateMultiEthernetDeviceInterfaces(itNetworkCaps []cdbm.MachineCapability, dbifcs []cdbm.Interface) error {
	return ValidateMultiEthernetDeviceInterfaces(itNetworkCaps, dbifcs)
}

// ValidateInfiniBandInterfaces validates the InfiniBand Interfaces for the Instance
func (iur *APIInstanceUpdateRequest) ValidateInfiniBandInterfaces(itIbCaps []cdbm.MachineCapability) error {
	return ValidateInfiniBandInterfaces(itIbCaps, iur.InfiniBandInterfaces)
}

// ValidateDpuExtensionServiceDeployments validates the DpuExtensionServiceDeployments for the Instance update request
func (iur *APIInstanceUpdateRequest) ValidateDpuExtensionServiceDeployments(desdrs []APIDpuExtensionServiceDeploymentRequest) error {
	return ValidateDpuExtensionServiceDeployments(desdrs)
}

// ValidateNVLinkInterfaces validates the NVLink interfaces for the Instance
func (iur *APIInstanceUpdateRequest) ValidateNVLinkInterfaces(itNvlCaps []cdbm.MachineCapability) error {
	return ValidateNVLinkInterfaces(itNvlCaps, iur.NVLinkInterfaces)
}

// IsUpdateRequest checks if the request is an instance config update request
func (iur *APIInstanceUpdateRequest) IsUpdateRequest() bool {
	return iur.Name != nil ||
		iur.Description != nil ||
		iur.Labels != nil ||
		iur.OperatingSystemID != nil ||
		iur.IpxeScript != nil ||
		iur.UserData != nil ||
		iur.PhoneHomeEnabled != nil ||
		iur.AlwaysBootWithCustomIpxe != nil ||
		iur.SecondaryVpcIDs != nil ||
		iur.Interfaces != nil ||
		iur.AutoNetwork != nil ||
		iur.InfiniBandInterfaces != nil ||
		iur.SpectrumXAttachments != nil ||
		iur.NVLinkInterfaces != nil ||
		iur.SSHKeyGroupIDs != nil ||
		iur.NetworkSecurityGroupID != nil ||
		iur.PowerProfile != nil
}

// IsInterfaceUpdateRequest checks if the request is an instance interface update request
func (iur *APIInstanceUpdateRequest) IsInterfaceUpdateRequest() bool {
	return iur.Interfaces != nil || iur.AutoNetwork != nil || iur.InfiniBandInterfaces != nil || iur.NVLinkInterfaces != nil ||
		iur.SpectrumXAttachments != nil
}

// IsRebootRequest checks if the request is an instance reboot request
func (iur *APIInstanceUpdateRequest) IsRebootRequest() bool {
	return iur.TriggerReboot != nil && *iur.TriggerReboot
}

// Validate ensures the values passed in request are acceptable
func (iur APIInstanceUpdateRequest) Validate() error {
	err := validation.ValidateStruct(&iur,
		validation.Field(&iur.Name,
			// length validation rule accepts empty string as valid, hence, required is needed
			validation.When(iur.Name != nil, validation.Required.Error(validationErrorStringLength), validation.By(util.ValidateNameCharacters), validation.Length(2, 256).Error(validationErrorStringLength)),
		),
		validation.Field(&iur.Description,
			validation.When(iur.Description != nil, validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&iur.OperatingSystemID,
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
		validation.Field(&iur.UserData,
			validation.When(iur.UserData != nil,
				validation.Length(0, util.MaxUserDataBytes).Error(validationErrorUserDataLength)),
		),
		validation.Field(&iur.Interfaces,
			validation.When(len(iur.Interfaces) > 0, validation.Length(1, MaxInterfaceCount).Error(fmt.Sprintf("at most %v Interfaces can be specified", MaxInterfaceCount))),
		),
	)

	if err != nil {
		return err
	}

	// AutoNetwork/interfaces exclusivity: if the caller is explicitly switching
	// to auto, an explicit interface list cannot also be supplied.
	if iur.AutoNetwork != nil && *iur.AutoNetwork && len(iur.Interfaces) > 0 {
		return validation.Errors{
			"interfaces": errors.New("`interfaces` must be empty when `autoNetwork` is true"),
		}
	}

	if iur.SecondaryVpcIDs != nil {
		if iur.AutoNetwork != nil && *iur.AutoNetwork {
			return validation.Errors{
				"secondaryVpcIds": errors.New("`secondaryVpcIds` is not supported when `autoNetwork` is true"),
			}
		}
		if len(iur.Interfaces) == 0 {
			return validation.Errors{
				"secondaryVpcIds": errors.New("`secondaryVpcIds` can only be specified when `interfaces` is specified and non-empty"),
			}
		}

		for _, iface := range iur.Interfaces {
			if iface.VpcPrefixID == nil && iface.VpcID == nil {
				return validation.Errors{
					"secondaryVpcIds": errors.New("`secondaryVpcIds` can only be specified when `vpcPrefixId` or `vpcId` is specified within `interfaces`"),
				}
			}
		}
	}

	if iur.IsRebootRequest() && iur.IsUpdateRequest() {
		return validation.Errors{
			"triggerReboot": errors.New("reboot cannot be triggered if Instance attributes are being updated in the same request"),
		}
	}

	// NOTE: Deeper validation takes place in ValidateAndSetOperatingSystemData and in ValidateInfiniBandInterfaces
	if !iur.IsRebootRequest() {
		if iur.RebootWithCustomIpxe != nil && *iur.RebootWithCustomIpxe {
			return validation.Errors{
				"rebootWithCustomIpxe": errors.New("`rebootWithCustomIpxe` can only be specified when `triggerReboot` is specified"),
			}
		}

		if iur.ApplyUpdatesOnReboot != nil && *iur.ApplyUpdatesOnReboot {
			return validation.Errors{
				"applyUpdatesOnReboot": errors.New("`applyUpdatesOnReboot` can only be specified when `triggerReboot` is specified"),
			}
		}
	}

	// Validate Interfaces if provided
	if len(iur.Interfaces) > 0 {
		err = ValidateInterfaces(&iur.Interfaces)
		if err != nil {
			return err
		}
	}

	// Validate InfiniBand Interfaces
	for _, ibifc := range iur.InfiniBandInterfaces {
		err = ibifc.Validate()
		if err != nil {
			return err
		}
	}

	// Validate SpectrumX Attachments
	err = ValidateSpectrumXAttachments(iur.SpectrumXAttachments)
	if err != nil {
		return err
	}

	// Validate DpuExtensionServiceDeployments
	err = ValidateDpuExtensionServiceDeployments(iur.DpuExtensionServiceDeployments)
	if err != nil {
		return err
	}

	// Validate NVLink interfaces
	for _, nvlic := range iur.NVLinkInterfaces {
		err = nvlic.Validate()
		if err != nil {
			return err
		}
	}

	if err := util.ValidateLabels(iur.Labels); err != nil {
		return err
	}

	return err
}

// ValidateForVpc validates network fields whose legality depends on the
// resolved VPC and the Instance's currently-persisted auto state.
// `currentAutoNetwork` is the Instance's persisted AutoNetwork value.
//
//   - Explicitly setting `autoNetwork: true` requires a Flat VPC.
//   - Explicit `interfaces` may not be sent while the *effective*
//     post-update auto state is true (the request value if supplied,
//     otherwise the persisted value). Validate() already rejects
//     `autoNetwork: true` + interfaces in the same payload; this also
//     catches a PATCH that omits `autoNetwork` against an
//     already-auto Instance, which would otherwise persist Interface
//     rows the workflow drops -- leaving DB/Site state diverged.
func (iur APIInstanceUpdateRequest) ValidateForVpc(vpc *cdbm.Vpc, currentAutoNetwork bool) error {
	if iur.AutoNetwork != nil && *iur.AutoNetwork && !cdbm.VpcTypeSupportsAutoInterface(vpc.NetworkVirtualizationType) {
		return validation.Errors{
			"autoNetwork": errors.New("`autoNetwork: true` is only supported when the Instance's VPC has `networkVirtualizationType` set to `FLAT`"),
		}
	}

	effectiveAuto := currentAutoNetwork
	if iur.AutoNetwork != nil {
		effectiveAuto = *iur.AutoNetwork
	}
	if effectiveAuto && len(iur.Interfaces) > 0 {
		return validation.Errors{
			"interfaces": errors.New("`interfaces` cannot be set while `autoNetwork` is true; disable `autoNetwork` first or omit `interfaces`"),
		}
	}
	return nil
}

// APIInstanceDeleteRequest is the data structure to capture request to delete an Instance
type APIInstanceDeleteRequest struct {
	// MachineHealthIssue is the report of a machine health issue
	MachineHealthIssue *APIMachineHealthIssue `json:"machineHealthIssue"`
	IsRepairTenant     *bool                  `json:"isRepairTenant"`
}

// Validate ensures the values passed in request are acceptable
func (idr *APIInstanceDeleteRequest) Validate() error {
	if idr.MachineHealthIssue != nil {
		err := validation.ValidateStruct(idr.MachineHealthIssue,
			validation.Field(&idr.MachineHealthIssue.Category,
				validation.Required,
				validation.In(MachineIssueCategoryHardware, MachineIssueCategoryNetwork, MachineIssueCategoryPerformance, MachineIssueCategoryOther),
			),
			validation.Field(&idr.MachineHealthIssue.Summary,
				validation.Required,
				validation.Length(0, 1024).Error(validationErrorStringLength)),
			// TODO: what are the constrains on Summary and Details? For now limiting to 1024..
			validation.Field(&idr.MachineHealthIssue.Details,
				validation.Length(0, 1024).Error(validationErrorStringLength)),
		)

		if err != nil {
			return err
		}
	}

	return nil
}

// ToProto builds the workflow request that asks a Site to release
// (delete) the given Instance for this API request. `instance` is the
// loaded DB record; its `ToReleaseRequestProto()` is the source of the
// canonical wire ID. Optional request-side fields (`MachineHealthIssue`,
// `IsRepairTenant`) are overlaid on top.
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see. In particular, the `IsRepairTenant` capability gate
// (TargetedInstanceCreation on the Tenant config) is an authorization
// check that stays in the handler before this method runs.
func (idr *APIInstanceDeleteRequest) ToProto(instance *cdbm.Instance, user *cdbm.User) *corev1.InstanceReleaseRequest {
	req := instance.ToReleaseRequestProto()
	if idr.MachineHealthIssue != nil {
		req.Issue = &corev1.Issue{
			Category: corev1.IssueCategory(MachineIssueCategoriesFromAPIToProtobuf[idr.MachineHealthIssue.Category]),
		}
		if idr.MachineHealthIssue.Summary != nil {
			req.Issue.Summary = *idr.MachineHealthIssue.Summary
		}
		if idr.MachineHealthIssue.Details != nil {
			req.Issue.Details = *idr.MachineHealthIssue.Details
		}
	}
	if idr.IsRepairTenant != nil {
		req.IsRepairTenant = idr.IsRepairTenant
	}

	// Build the delete attribution proto
	initiatedBy := &corev1.DeleteInitiatedBy{
		Org:      instance.Tenant.Org,
		UserId:   user.ID.String(),
		TenantId: instance.Tenant.ID.String(),
	}
	if instance.Tenant.OrgDisplayName != nil {
		initiatedBy.OrgDisplayName = *instance.Tenant.OrgDisplayName
	}
	req.DeleteAttribution = &corev1.DeleteAttribution{
		InitiatedBy: initiatedBy,
	}
	return req
}

// SSHKeyGroupsSummaryDeprecated ensures we keep returning empty array until deprecation time even with omitempty
type SSHKeyGroupsSummaryDeprecated struct {
	SSHKeyGroups []APISSHKeyGroupSummary
}

// MarshalJSON provides custom JSON marshaling for SSHKeyGroupsSummaryDeprecated
func (skgsd *SSHKeyGroupsSummaryDeprecated) MarshalJSON() ([]byte, error) {
	return json.Marshal(skgsd.SSHKeyGroups)
}

// UnmarshalJSON provides custom JSON unmarshaling for SSHKeyGroupsSummaryDeprecated
func (skgsd *SSHKeyGroupsSummaryDeprecated) UnmarshalJSON(data []byte) error {
	return json.Unmarshal(data, &skgsd.SSHKeyGroups)
}

// APIInstance is the data structure to capture API representation of an Instance
type APIInstance struct {
	// ID is the unique UUID v4 identifier for the Instance
	ID string `json:"id"`
	// Name of the Instance
	Name string `json:"name"`
	// Description is the description of the Instance
	Description *string `json:"description"`
	// ControllerInstanceID is the ID of the Instance in Site Controller
	ControllerInstanceID string `json:"controllerInstanceId"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// InfrastructureProviderID is the ID of the Infrastructure Provider
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the Infrastructure Provider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// InstanceTypeID is the ID of the InstanceType
	InstanceTypeID *string `json:"instanceTypeId"`
	// InstanceType is the summary of the InstanceType
	InstanceType *APIInstanceTypeSummary `json:"instanceType,omitempty"`
	// VpcID is the ID of the VPC
	VpcID string `json:"vpcId"`
	// Vpc is the summary of the VPC
	Vpc *APIVpcSummary `json:"vpc,omitempty"`
	// SecondaryVpcIDs lists non-primary VPC UUIDs derived from prefix-backed
	// interfaces attached to the Instance. These values are populated from
	// interface intent or relations rather than stored directly on the Instance record.
	SecondaryVpcIDs []string `json:"secondaryVpcIds"`
	// MachineID is the ID of the Machine
	MachineID *string `json:"machineId"`
	// Machine is the summary of the Machine
	Machine *APIMachineSummary `json:"machine,omitempty"`
	// OperatingSystemID is the ID of the OperatingSystem
	OperatingSystemID *string `json:"operatingSystemId"`
	// OperatingSystem is the summary of the OperatingSystem
	OperatingSystem *APIOperatingSystemSummary `json:"operatingSystem,omitempty"`
	// ipxeScript is an attribute which is inherited from Operating System
	IpxeScript *string `json:"ipxeScript"`
	// AlwaysBootWithCustomIpxe is an attribute which is specified by user if instance boot with ipxe or not
	AlwaysBootWithCustomIpxe bool `json:"alwaysBootWithCustomIpxe"`
	// PhoneHomeEnabled is an attribute which is specified by user if instance needs to be enabled for phone home or not
	PhoneHomeEnabled bool `json:"phoneHomeEnabled"`
	// UserData is inherited from Operating System or specified by user if allowed
	UserData *string `json:"userData"`
	// Labels is Instace labels specified by user
	Labels map[string]string `json:"labels"`
	// IsUpdatePending is an attribute suggest if instance update pending or not
	IsUpdatePending bool `json:"isUpdatePending"`
	// SerialConsoleURL is the ssh serial console URL associated with the instance
	SerialConsoleURL *string `json:"serialConsoleUrl"`
	// NetworkSecurityGroupID is the ID of attached NSG, if any
	NetworkSecurityGroupID *string `json:"networkSecurityGroupId"`
	// NetworkSecurityGroup holds the summary for attached NSG, if requested via includeRelation
	NetworkSecurityGroup *APINetworkSecurityGroupSummary `json:"networkSecurityGroup,omitempty"`
	// NetworkSecurityGroupPropagationDetails is the propagation details for the attched NSG, if any
	NetworkSecurityGroupPropagationDetails *APINetworkSecurityGroupPropagationDetails `json:"networkSecurityGroupPropagationDetails"`
	// NetworkSecurityGroupInherited indicates if the Instance is inheriting Network Security Group rules from parent VPC
	NetworkSecurityGroupInherited bool `json:"networkSecurityGroupInherited"`
	// TPM EK Cert
	TpmEkCertificate *string `json:"tpmEkCertificate"`
	// Status is the status of the Instance
	Status string `json:"status"`
	// PowerProfile is the external power provisioning profile associated with the Instance.
	PowerProfile *string `json:"powerProfile"`
	// AutoNetwork is true when this Instance had its network interfaces
	// auto-resolved by NICo from the host's HostInband segments. When
	// true, `Interfaces` reflects the resolved set; the caller's request
	// list was empty.
	AutoNetwork bool `json:"autoNetwork"`
	// Interfaces are list of the subnet associated with the Instance
	Interfaces []APIInterface `json:"interfaces"`
	// InfiniBandInterfaces are list of the InfiniBandInterface associated with the Instance
	InfiniBandInterfaces []APIInfiniBandInterface `json:"infinibandInterfaces"`
	// DpuExtensionServiceDeployments are list of the DpuExtensionServiceDeployments associated with the Instance
	DpuExtensionServiceDeployments []APIDpuExtensionServiceDeployment `json:"dpuExtensionServiceDeployments"`
	// NVLinkInterfaces are list of the NVLinkInterface associated with the Instance
	NVLinkInterfaces []APINVLinkInterface `json:"nvLinkInterfaces"`
	// SSHKeyGroupIDs are list of the ssh key group IDs associated with the Instance
	SSHKeyGroupIDs []string `json:"sshKeyGroupIds"`
	// SSHKeyGroups are list of the ssh key group associated with the Instance
	SSHKeyGroups []APISSHKeyGroupSummary `json:"sshKeyGroups"`
	// StatusHistory is the history of statuses for the Instance
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
	// Deprecations is the list of deprecation messages denoting fields which are being deprecated
	Deprecations []APIDeprecation `json:"deprecations,omitempty"`
}

// APIInstanceStats holds aggregated instance status counts at the API layer.
type APIInstanceStats = cdbm.InstanceCountByStatus

var (
	// instanceQueryParamDeprecationTime is when the deprecated infrastructureProviderId
	// query parameter on the list-Instances endpoint will no longer be accepted.
	instanceQueryParamDeprecationTime = time.Date(2026, time.October, 10, 0, 0, 0, 0, time.UTC)

	// instanceListQueryParamDeprecations are the deprecated query parameters accepted by the
	// list-Instances endpoint. Instances will no longer be filtered by Infrastructure Provider;
	// results are scoped to the org's Tenant.
	instanceListQueryParamDeprecations = []DeprecatedEntity{
		{OldValue: "infrastructureProviderId", Type: DeprecationTypeQueryParam, TakeActionBy: instanceQueryParamDeprecationTime},
	}
)

// InstanceListQueryParamDeprecations returns the deprecation notices for the deprecated query
// parameters accepted by the list-Instances endpoint.
func InstanceListQueryParamDeprecations() []APIDeprecation {
	deprecations := make([]APIDeprecation, 0, len(instanceListQueryParamDeprecations))
	for _, d := range instanceListQueryParamDeprecations {
		deprecations = append(deprecations, NewAPIDeprecation(d))
	}
	return deprecations
}

// NewAPIInstance accepts a DB layer Instance object returns an API layer object.
// SecondaryVpcIDs are derived from Interface.VpcID or the explicit prefix relation, so
// callers must preload Interface.VpcPrefix when explicit-prefix IDs should be populated.
func NewAPIInstance(dbinst *cdbm.Instance, dbSite *cdbm.Site, dbiss []cdbm.Interface, dbibis []cdbm.InfiniBandInterface, dbdesds []cdbm.DpuExtensionServiceDeployment, dbnvlis []cdbm.NVLinkInterface, dbskgs []cdbm.SSHKeyGroup, dbsds []cdbm.StatusDetail) *APIInstance {
	var instanceTypeID *string
	if dbinst.InstanceTypeID != nil {
		instanceTypeID = cutil.GetPtr(dbinst.InstanceTypeID.String())
	}
	apiInstance := APIInstance{
		ID:                                     dbinst.ID.String(),
		Name:                                   dbinst.Name,
		Description:                            dbinst.Description,
		TenantID:                               dbinst.TenantID.String(),
		InfrastructureProviderID:               dbinst.InfrastructureProviderID.String(),
		SiteID:                                 dbinst.SiteID.String(),
		InstanceTypeID:                         instanceTypeID,
		VpcID:                                  dbinst.VpcID.String(),
		MachineID:                              dbinst.MachineID,
		NetworkSecurityGroupID:                 dbinst.NetworkSecurityGroupID,
		NetworkSecurityGroupPropagationDetails: NewAPINetworkSecurityGroupPropagationDetails(dbinst.NetworkSecurityGroupPropagationDetails),
		IpxeScript:                             dbinst.IpxeScript,
		AlwaysBootWithCustomIpxe:               dbinst.AlwaysBootWithCustomIpxe,
		PhoneHomeEnabled:                       dbinst.PhoneHomeEnabled,
		UserData:                               dbinst.UserData,
		AutoNetwork:                            dbinst.AutoNetwork,
		Labels:                                 dbinst.Labels,
		IsUpdatePending:                        dbinst.IsUpdatePending,
		PowerProfile:                           dbinst.PowerProfile,
		Created:                                dbinst.Created,
		Updated:                                dbinst.Updated,
	}

	if dbinst.OperatingSystemID != nil {
		apiInstance.OperatingSystemID = cutil.GetPtr(dbinst.OperatingSystemID.String())
	}

	if dbinst.ControllerInstanceID != nil {
		apiInstance.ControllerInstanceID = dbinst.ControllerInstanceID.String()
	}

	if dbinst.Tenant != nil {
		apiInstance.Tenant = NewAPITenantSummary(dbinst.Tenant)
	}

	if dbinst.InfrastructureProvider != nil {
		apiInstance.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbinst.InfrastructureProvider)
	}

	if dbinst.Site != nil {
		apiInstance.Site = NewAPISiteSummary(dbinst.Site)
	}

	if dbinst.InstanceType != nil {
		apiInstance.InstanceType = NewAPIInstanceTypeSummary(dbinst.InstanceType)
	}

	if dbinst.Vpc != nil {
		apiInstance.Vpc = NewAPIVpcSummary(dbinst.Vpc)
	}

	if dbinst.Vpc != nil {
		apiInstance.Vpc = NewAPIVpcSummary(dbinst.Vpc)
	}

	if dbinst.NetworkSecurityGroup != nil {
		apiInstance.NetworkSecurityGroup = NewAPINetworkSecurityGroupSummary(dbinst.NetworkSecurityGroup)
	}

	if dbinst.Machine != nil {
		apiInstance.Machine = NewAPIMachineSummary(dbinst.Machine)
	}

	if dbinst.OperatingSystem != nil {
		apiInstance.OperatingSystem = NewAPIOperatingSystemSummary(dbinst.OperatingSystem)
	}

	if dbinst.ControllerInstanceID != nil && dbSite != nil && dbSite.SerialConsoleHostname != nil {
		serialConsoleURL := fmt.Sprintf("ssh://%s@%s", dbinst.ControllerInstanceID.String(), *dbSite.SerialConsoleHostname)
		apiInstance.SerialConsoleURL = cutil.GetPtr(serialConsoleURL)
	}

	if dbinst.TpmEkCertificate != nil {
		apiInstance.TpmEkCertificate = dbinst.TpmEkCertificate
	}

	apiInstance.Status = dbinst.GetAggregatedStatus(dbinst.Status, dbinst.PowerStatus)

	secondaryVpcIDs := goset.NewSet[string]()

	apiInstance.Interfaces = []APIInterface{}
	for _, dbis := range dbiss {
		curis := dbis
		apiInstance.Interfaces = append(apiInstance.Interfaces, *NewAPIInterface(&curis))
		if dbis.VpcID != nil && *dbis.VpcID != dbinst.VpcID {
			secondaryVpcIDs.Add(dbis.VpcID.String())
		} else if dbis.VpcID == nil && dbis.VpcPrefix != nil && dbis.VpcPrefix.VpcID != dbinst.VpcID {
			secondaryVpcIDs.Add(dbis.VpcPrefix.VpcID.String())
		}
	}

	apiInstance.SecondaryVpcIDs = secondaryVpcIDs.ToSlice()
	slices.Sort(apiInstance.SecondaryVpcIDs)

	apiInstance.InfiniBandInterfaces = []APIInfiniBandInterface{}
	for _, dbibi := range dbibis {
		curibi := dbibi
		apiInstance.InfiniBandInterfaces = append(apiInstance.InfiniBandInterfaces, *NewAPIInfiniBandInterface(&curibi))
	}

	apiInstance.NVLinkInterfaces = []APINVLinkInterface{}
	for _, dbnvli := range dbnvlis {
		curnvli := dbnvli
		apiInstance.NVLinkInterfaces = append(apiInstance.NVLinkInterfaces, *NewAPINVLinkInterface(&curnvli))
	}

	apiInstance.DpuExtensionServiceDeployments = []APIDpuExtensionServiceDeployment{}
	for _, dbdesd := range dbdesds {
		curdesd := dbdesd
		apiInstance.DpuExtensionServiceDeployments = append(apiInstance.DpuExtensionServiceDeployments, *NewAPIDpuExtensionServiceDeployment(&curdesd))
	}

	apiInstance.SSHKeyGroups = []APISSHKeyGroupSummary{}
	for _, dbskg := range dbskgs {
		skg := dbskg
		apiInstance.SSHKeyGroupIDs = append(apiInstance.SSHKeyGroupIDs, skg.ID.String())
		apiInstance.SSHKeyGroups = append(apiInstance.SSHKeyGroups, *NewAPISSHKeyGroupSummary(&skg))
	}

	apiInstance.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiInstance.StatusHistory = append(apiInstance.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	return &apiInstance
}

// APIInstanceSummary is the data structure to capture API summary of an Instance
type APIInstanceSummary struct {
	// ID of the Instance
	ID string `json:"id"`
	// Name of the Instance, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// InfrastructureProviderID is the ID of the Infrastructure Provider
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// InstanceTypeID is the ID of the InstanceType
	InstanceTypeID string `json:"instanceTypeId"`
	// ControllerInstanceID is the ID of the Instance in Site Controller
	ControllerInstanceID string `json:"controllerInstanceId"`
	// Status is the status of the Instance
	Status string `json:"status"`
}

// NewAPIInstanceSummary accepts a DB layer Instance object returns an API layer object
func NewAPIInstanceSummary(dbist *cdbm.Instance) *APIInstanceSummary {
	ins := APIInstanceSummary{
		ID:                       dbist.ID.String(),
		Name:                     dbist.Name,
		InfrastructureProviderID: dbist.InfrastructureProviderID.String(),
		SiteID:                   dbist.SiteID.String(),
		Status:                   dbist.GetAggregatedStatus(dbist.Status, dbist.PowerStatus),
	}

	// Check if the instance type is set.  If so, use it.  If not set, it's a targeted Instance.
	if dbist.InstanceTypeID != nil {
		ins.InstanceTypeID = dbist.InstanceTypeID.String()
	}

	if dbist.ControllerInstanceID != nil {
		ins.ControllerInstanceID = dbist.ControllerInstanceID.String()
	}

	return &ins
}
