// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"cmp"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIGetAllDpuMachineRequest binds query parameters for GET /dpu.
type APIGetAllDpuMachineRequest struct {
	SiteID string `query:"siteId"`
}

// Validate checks the DPU list query shape.
func (r *APIGetAllDpuMachineRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
	)
}

// APIGetDpuMachineRequest binds query parameters for GET /dpu/:id.
type APIGetDpuMachineRequest struct {
	SiteID               string `query:"siteId"`
	IncludeNetworkConfig bool   `query:"includeNetworkConfig"`
}

// Validate checks the DPU retrieval query shape.
func (r *APIGetDpuMachineRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
	)
}

// APIDpuNetworkConfig represents the network configuration fields exposed by the REST API for a DPU.
// Internal-only and sensitive Core fields are omitted; this is not the complete Core configuration.
type APIDpuNetworkConfig struct {
	// Asn is the Autonomous System Number for BGP routing
	Asn uint32 `json:"asn"`
	// DhcpServers is the list of DHCP server IP addresses
	DhcpServers []string `json:"dhcpServers"`
	// VniDevice is the VNI device name
	VniDevice string `json:"vniDevice"`
	// ManagedHostConfig is the network configuration applied to the managed host
	ManagedHostConfig *APIManagedHostNetworkConfig `json:"managedHostConfig"`
	// ManagedHostConfigVersion is the version of the managed host configuration
	ManagedHostConfigVersion string `json:"managedHostConfigVersion"`
	// UseAdminNetwork indicates whether to use the admin network
	UseAdminNetwork bool `json:"useAdminNetwork"`
	// AdminInterface is the admin network interface configuration for the DPU
	AdminInterface *APIFlatInterfaceConfig `json:"adminInterface"`
	// TenantInterfaces is the list of tenant interface configurations
	TenantInterfaces []APIFlatInterfaceConfig `json:"tenantInterfaces"`
	// InstanceNetworkConfigVersion is the version of the instance network configuration
	InstanceNetworkConfigVersion *string `json:"instanceNetworkConfigVersion"`
	// InstanceID is the ID of the associated instance
	InstanceID *string `json:"instanceId"`
	// NetworkVirtualizationType is the type of network virtualization
	NetworkVirtualizationType *string `json:"networkVirtualizationType"`
	// VpcVni is the VPC VNI identifier
	VpcVni *uint32 `json:"vpcVni"`
	// RouteServers is the list of route server IP addresses
	RouteServers []string `json:"routeServers"`
	// RemoteID is the remote identifier for the managed host
	RemoteID string `json:"remoteId"`
	// DeprecatedDenyPrefixes is the deprecated list of denied IP prefixes
	DeprecatedDenyPrefixes []string `json:"deprecatedDenyPrefixes"`
	// DpuNetworkPingerType is the type of network pinger to use
	DpuNetworkPingerType *string `json:"dpuNetworkPingerType"`
	// DenyPrefixes is the list of denied IP prefixes
	DenyPrefixes []string `json:"denyPrefixes"`
	// SiteFabricPrefixes is the list of site fabric IP prefixes
	SiteFabricPrefixes []string `json:"siteFabricPrefixes"`
	// VpcIsolationBehavior is the VPC isolation behavior setting
	VpcIsolationBehavior string `json:"vpcIsolationBehavior"`
	// StatefulAclsEnabled indicates whether stateful ACLs are enabled
	StatefulAclsEnabled bool `json:"statefulAclsEnabled"`
	// EnableDhcp indicates whether DHCP is enabled
	EnableDhcp bool `json:"enableDhcp"`
	// HostInterfaceID is the ID of the host interface
	HostInterfaceID *string `json:"hostInterfaceId"`
	// MinDpuFunctioningLinks is the minimum number of functioning DPU links required
	MinDpuFunctioningLinks *uint32 `json:"minDpuFunctioningLinks"`
	// IsPrimaryDpu indicates whether this is the primary DPU
	IsPrimaryDpu bool `json:"isPrimaryDpu"`
	// InternetL3Vni is the Layer 3 VNI used for internet access
	InternetL3Vni *uint32 `json:"internetL3Vni"`
	// DatacenterAsn is the datacenter Autonomous System Number
	DatacenterAsn uint32 `json:"datacenterAsn"`
	// AnycastSitePrefixes is the list of anycast site IP prefixes
	AnycastSitePrefixes []string `json:"anycastSitePrefixes"`
	// TenantHostAsn is the Autonomous System Number for the tenant host
	TenantHostAsn *uint32 `json:"tenantHostAsn"`
	// SiteGlobalVpcVni is the site-global VPC VNI identifier
	SiteGlobalVpcVni *uint32 `json:"siteGlobalVpcVni"`
}

// FromProto populates an APIDpuNetworkConfig from its protobuf form.
func (apnnc *APIDpuNetworkConfig) FromProto(protoConfig *corev1.ManagedHostNetworkConfigResponse) {
	if protoConfig == nil {
		return
	}

	apnnc.Asn = protoConfig.Asn
	apnnc.DhcpServers = append(make([]string, 0, len(protoConfig.DhcpServers)), protoConfig.DhcpServers...)
	apnnc.VniDevice = protoConfig.VniDevice
	apnnc.ManagedHostConfigVersion = protoConfig.ManagedHostConfigVersion
	apnnc.UseAdminNetwork = protoConfig.UseAdminNetwork

	if protoConfig.InstanceId != nil {
		instanceID := protoConfig.InstanceId.GetValue()
		apnnc.InstanceID = &instanceID
		apnnc.InstanceNetworkConfigVersion = &protoConfig.InstanceNetworkConfigVersion
	}

	if protoConfig.NetworkVirtualizationType != nil {
		nvt := protoConfig.GetNetworkVirtualizationType().String()
		apnnc.NetworkVirtualizationType = &nvt
	}
	apnnc.RouteServers = append(make([]string, 0, len(protoConfig.RouteServers)), protoConfig.RouteServers...)
	apnnc.RemoteID = protoConfig.RemoteId
	apnnc.DeprecatedDenyPrefixes = append(make([]string, 0, len(protoConfig.DeprecatedDenyPrefixes)), protoConfig.DeprecatedDenyPrefixes...)
	apnnc.DenyPrefixes = append(make([]string, 0, len(protoConfig.DenyPrefixes)), protoConfig.DenyPrefixes...)
	apnnc.SiteFabricPrefixes = append(make([]string, 0, len(protoConfig.SiteFabricPrefixes)), protoConfig.SiteFabricPrefixes...)
	apnnc.VpcIsolationBehavior = protoConfig.VpcIsolationBehavior.String()
	apnnc.StatefulAclsEnabled = protoConfig.StatefulAclsEnabled
	apnnc.EnableDhcp = protoConfig.EnableDhcp
	apnnc.IsPrimaryDpu = protoConfig.IsPrimaryDpu
	apnnc.DatacenterAsn = protoConfig.DatacenterAsn
	apnnc.AnycastSitePrefixes = append(make([]string, 0, len(protoConfig.AnycastSitePrefixes)), protoConfig.AnycastSitePrefixes...)
	apnnc.TenantHostAsn = protoConfig.TenantHostAsn
	apnnc.SiteGlobalVpcVni = protoConfig.SiteGlobalVpcVni

	if protoConfig.ManagedHostConfig != nil {
		apnnc.ManagedHostConfig = &APIManagedHostNetworkConfig{}
		apnnc.ManagedHostConfig.FromProto(protoConfig.ManagedHostConfig)
	}

	if protoConfig.AdminInterface != nil {
		apnnc.AdminInterface = &APIFlatInterfaceConfig{}
		apnnc.AdminInterface.FromProto(protoConfig.AdminInterface)
	}

	apnnc.TenantInterfaces = make([]APIFlatInterfaceConfig, len(protoConfig.TenantInterfaces))
	for i, protoInterface := range protoConfig.TenantInterfaces {
		if protoInterface != nil {
			apnnc.TenantInterfaces[i] = APIFlatInterfaceConfig{}
			apnnc.TenantInterfaces[i].FromProto(protoInterface)
		}
	}

	apnnc.VpcVni = protoConfig.VpcVni

	if protoConfig.DpuNetworkPingerType != nil {
		apnnc.DpuNetworkPingerType = protoConfig.DpuNetworkPingerType
	}

	if protoConfig.HostInterfaceId != nil {
		apnnc.HostInterfaceID = protoConfig.HostInterfaceId
	}

	apnnc.MinDpuFunctioningLinks = protoConfig.MinDpuFunctioningLinks
	apnnc.InternetL3Vni = protoConfig.InternetL3Vni
}

// APIManagedHostQuarantineState represents quarantine state
type APIManagedHostQuarantineState struct {
	// Mode is the quarantine mode
	Mode string `json:"mode"`
	// Reason is the reason for quarantine
	Reason *string `json:"reason,omitempty"`
}

// FromProto populates an APIManagedHostQuarantineState from its protobuf form.
func (amhq *APIManagedHostQuarantineState) FromProto(protoQuarantineState *corev1.ManagedHostQuarantineState) {
	if protoQuarantineState == nil {
		return
	}
	amhq.Mode = protoQuarantineState.Mode.String()
	amhq.Reason = protoQuarantineState.Reason
}

// APIManagedHostNetworkConfig represents the managed host network configuration
type APIManagedHostNetworkConfig struct {
	// LoopbackIP is the loopback IP address
	LoopbackIP string `json:"loopbackIp"`
	// QuarantineState is the quarantine state for the managed host
	QuarantineState *APIManagedHostQuarantineState `json:"quarantineState,omitempty"`
}

// FromProto populates an APIManagedHostNetworkConfig from its protobuf form.
func (amnc *APIManagedHostNetworkConfig) FromProto(protoConfig *corev1.ManagedHostNetworkConfig) {
	if protoConfig == nil {
		return
	}
	amnc.LoopbackIP = protoConfig.LoopbackIp

	quarantineState := protoConfig.QuarantineState
	if quarantineState != nil {
		amnc.QuarantineState = &APIManagedHostQuarantineState{}
		amnc.QuarantineState.FromProto(quarantineState)
	}
}

// APIFlatInterfaceConfig represents a flat interface configuration
type APIFlatInterfaceConfig struct {
	// FunctionType is the function type (e.g. PHYSICAL_FUNCTION, VIRTUAL_FUNCTION)
	FunctionType string `json:"functionType"`
	// VlanID is the VLAN ID
	VlanID uint32 `json:"vlanId"`
	// Vni is the VXLAN Network Identifier
	Vni uint32 `json:"vni"`
	// Gateway is the gateway IP address
	Gateway string `json:"gateway"`
	// IP is the interface IP address
	IP string `json:"ip"`
	// InterfacePrefix is the interface name prefix
	InterfacePrefix string `json:"interfacePrefix"`
	// VirtualFunctionID is the virtual function ID if applicable
	VirtualFunctionID *uint32 `json:"virtualFunctionId"`
	// VpcPrefixes is the list of VPC IP prefixes
	VpcPrefixes []string `json:"vpcPrefixes"`
	// Prefix is the IP prefix for the interface
	Prefix string `json:"prefix"`
	// Fqdn is the fully qualified domain name
	Fqdn string `json:"fqdn"`
	// BootURL is the boot URL for PXE/iPXE boot
	BootURL *string `json:"bootUrl"`
	// VpcVni is the VPC VXLAN Network Identifier
	VpcVni uint32 `json:"vpcVni"`
	// SviIP is the switch virtual interface (SVI) IP address
	SviIP *string `json:"sviIp"`
	// TenantVrfLoopbackIP is the tenant VRF loopback IP address
	TenantVrfLoopbackIP *string `json:"tenantVrfLoopbackIp"`
	// IsL2Segment indicates whether the interface is an L2 segment
	IsL2Segment bool `json:"isL2Segment"`
	// VpcPeerPrefixes is the list of peered VPC IP prefixes
	VpcPeerPrefixes []string `json:"vpcPeerPrefixes"`
	// VpcPeerVnis is the list of peered VPC VNIs
	VpcPeerVnis []uint32 `json:"vpcPeerVnis"`
	// Mtu is the maximum transmission unit (MTU) for the interface
	Mtu *uint32 `json:"mtu"`
	// NetworkSecurityGroup is the network security group configuration resolved on the interface
	NetworkSecurityGroup *APIFlatInterfaceNetworkSecurityGroupConfig `json:"networkSecurityGroup"`
}

// FromProto populates an APIFlatInterfaceConfig from its protobuf form.
func (afic *APIFlatInterfaceConfig) FromProto(protoConfig *corev1.FlatInterfaceConfig) {
	if protoConfig == nil {
		return
	}

	afic.FunctionType = protoConfig.FunctionType.String()
	afic.VlanID = protoConfig.VlanId
	afic.Vni = protoConfig.Vni
	afic.Gateway = protoConfig.GetGateway()                 //nolint:staticcheck // Preserve the scalar REST compatibility field.
	afic.IP = protoConfig.GetIp()                           //nolint:staticcheck // Preserve the scalar REST compatibility field.
	afic.InterfacePrefix = protoConfig.GetInterfacePrefix() //nolint:staticcheck // Preserve the scalar REST compatibility field.

	afic.VirtualFunctionID = protoConfig.VirtualFunctionId

	afic.VpcPrefixes = protoConfig.VpcPrefixes
	afic.Prefix = protoConfig.GetPrefix() //nolint:staticcheck // Preserve the scalar REST compatibility field.
	afic.Fqdn = protoConfig.Fqdn

	afic.BootURL = protoConfig.Booturl
	afic.VpcVni = protoConfig.VpcVni
	afic.SviIP = protoConfig.SviIp                             //nolint:staticcheck // Preserve the scalar REST compatibility field.
	afic.TenantVrfLoopbackIP = protoConfig.TenantVrfLoopbackIp //nolint:staticcheck // Preserve optional REST compatibility presence.
	afic.IsL2Segment = protoConfig.IsL2Segment
	afic.VpcPeerPrefixes = protoConfig.VpcPeerPrefixes

	afic.VpcPeerVnis = protoConfig.VpcPeerVnis

	afic.Mtu = protoConfig.Mtu

	if protoConfig.NetworkSecurityGroup != nil {
		afic.NetworkSecurityGroup = &APIFlatInterfaceNetworkSecurityGroupConfig{}
		afic.NetworkSecurityGroup.FromProto(protoConfig.NetworkSecurityGroup)
	}
}

// APIFlatInterfaceNetworkSecurityGroupConfig represents network security group configuration
type APIFlatInterfaceNetworkSecurityGroupConfig struct {
	// ID is the ID of the Network Security Group
	ID string `json:"id"`
	// Version is the version of the Network Security Group
	Version string `json:"version"`
	// Source is the source of the Network Security Group configuration
	Source string `json:"source"`
	// Rules are the resolved rules for the Network Security Group
	Rules []APIResolvedNetworkSecurityGroupRule `json:"rules"`
}

// FromProto populates an APIFlatInterfaceNetworkSecurityGroupConfig from its protobuf form.
func (aficsg *APIFlatInterfaceNetworkSecurityGroupConfig) FromProto(protoConfig *corev1.FlatInterfaceNetworkSecurityGroupConfig) {
	if protoConfig == nil {
		return
	}

	aficsg.ID = protoConfig.Id
	aficsg.Version = protoConfig.Version
	aficsg.Source = protoConfig.Source.String()
	aficsg.Rules = make([]APIResolvedNetworkSecurityGroupRule, len(protoConfig.Rules))
	for i, protoRule := range protoConfig.Rules {
		if protoRule != nil {
			aficsg.Rules[i] = APIResolvedNetworkSecurityGroupRule{}
			aficsg.Rules[i].FromProto(protoRule)
		}
	}
}

// APIResolvedNetworkSecurityGroupRule represents a resolved network security group rule
type APIResolvedNetworkSecurityGroupRule struct {
	// Rule is the underlying Network Security Group rule
	Rule *APINetworkSecurityGroupRule `json:"rule"`
	// SrcPrefixes are the resolved source IP prefixes for the rule
	SrcPrefixes []string `json:"srcPrefixes"`
	// DstPrefixes are the resolved destination IP prefixes for the rule
	DstPrefixes []string `json:"dstPrefixes"`
}

// FromProto populates an APIResolvedNetworkSecurityGroupRule from its protobuf form.
func (arnsr *APIResolvedNetworkSecurityGroupRule) FromProto(protoRule *corev1.ResolvedNetworkSecurityGroupRule) {
	if protoRule == nil {
		return
	}

	arnsr.Rule = NewAPINetworkSecurityGroupRule(protoRule.Rule)

	arnsr.SrcPrefixes = protoRule.SrcPrefixes
	arnsr.DstPrefixes = protoRule.DstPrefixes
}

// APIDpuMachineSoftwareComponent represents a DPU Machine software component
type APIDpuMachineSoftwareComponent struct {
	// Name is the name of the software component
	Name string `json:"name"`
	// Version is the version of the software component
	Version string `json:"version"`
	// URL is where the software component can be obtained
	URL string `json:"url"`
}

// FromProto populates an APIDpuMachineSoftwareComponent from its protobuf form.
func (apmsc *APIDpuMachineSoftwareComponent) FromProto(protoComponent *corev1.MachineInventorySoftwareComponent) {
	if protoComponent == nil {
		return
	}
	apmsc.Name = protoComponent.Name
	apmsc.Version = protoComponent.Version
	apmsc.URL = protoComponent.Url
}

// APIDpuMachineInterface represents a DPU Machine interface
type APIDpuMachineInterface struct {
	// ID is the interface ID
	ID string `json:"id"`
	// MachineID is the ID of the Machine the interface belongs to
	MachineID string `json:"machineId"`
	// SegmentID is the network segment ID
	SegmentID string `json:"segmentId"`
	// Hostname is the hostname of the interface
	Hostname string `json:"hostname"`
	// PrimaryInterface indicates whether this is the primary interface
	PrimaryInterface bool `json:"primaryInterface"`
	// MacAddress is the MAC address of the interface
	MacAddress string `json:"macAddress"`
	// Address are the IP addresses assigned to the interface
	Address []string `json:"address"`
	// Vendor is the vendor of the interface
	Vendor *string `json:"vendor"`
	// Created is the timestamp when the interface was first observed
	Created *time.Time `json:"created"`
	// LastDhcp is the timestamp of the last DHCP lease for the interface
	LastDhcp *time.Time `json:"lastDhcp"`
	// IsBmc indicates whether this interface is a BMC interface
	IsBmc bool `json:"isBmc"`
}

// FromProto populates an APIDpuMachineInterface from its protobuf form.
func (admif *APIDpuMachineInterface) FromProto(protoInterface *corev1.MachineInterface) {
	if protoInterface == nil {
		return
	}
	admif.ID = protoInterface.GetId().GetValue()
	admif.MachineID = protoInterface.GetMachineId().GetId()
	admif.SegmentID = protoInterface.GetSegmentId().GetValue()
	admif.Hostname = protoInterface.Hostname
	admif.PrimaryInterface = protoInterface.PrimaryInterface
	admif.MacAddress = protoInterface.MacAddress
	admif.Address = protoInterface.Address
	admif.Vendor = protoInterface.Vendor

	if protoInterface.Created != nil {
		created := protoInterface.Created.AsTime()
		admif.Created = &created
	}
	if protoInterface.LastDhcp != nil {
		lastDhcp := protoInterface.LastDhcp.AsTime()
		admif.LastDhcp = &lastDhcp
	}

	if protoInterface.InterfaceType != nil {
		admif.IsBmc = protoInterface.GetInterfaceType() == corev1.InterfaceType_INTERFACE_TYPE_BMC
	} else {
		admif.IsBmc = protoInterface.GetIsBmc() //nolint:staticcheck // Preserve compatibility with Core responses that predate interface_type.
	}
}

// APIDpuMachine represents a DPU Machine with its complete configuration
type APIDpuMachine struct {
	// ID is the ID of the DPU Machine as reported by NICo Core
	ID string `json:"id"`
	// InfrastructureProviderID is the ID of the Infrastructure Provider that owns the host Machine
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// SiteID is the ID of the Site that the host Machine belongs to
	SiteID string `json:"siteId"`
	// HostMachineID is the ID of the host Machine that this DPU is attached to
	HostMachineID string `json:"hostMachineId"`
	// DpuAgentVersion is the version of the DPU agent running on the DPU
	DpuAgentVersion string `json:"dpuAgentVersion"`
	// BMCInfo is the BMC (Baseboard Management Controller) information for the DPU
	BMCInfo *APIBMCInfo `json:"bmcInfo"`
	// DMIData is the DMI (Desktop Management Interface) data reported for the DPU
	DMIData *APIDMIData `json:"dmiData"`
	// Interfaces are the interfaces reported on the DPU
	Interfaces []APIDpuMachineInterface `json:"interfaces"`
	// SoftwareComponents are the software components reported on the DPU
	SoftwareComponents []APIDpuMachineSoftwareComponent `json:"softwareComponents"`
	// Health is the health information for the DPU
	Health *APIMachineHealth `json:"health"`
	// Labels are the labels associated with the DPU
	Labels APILabels `json:"labels"`
	// State is the lifecycle state of the DPU as reported by NICo Core
	State string `json:"state"`
	// DpuNetworkConfig contains the network configuration fields exposed by the REST API for the DPU.
	// Internal-only and sensitive Core fields are omitted; the REST response retains its public JSON shape.
	DpuNetworkConfig *APIDpuNetworkConfig `json:"dpuNetworkConfig"`
	// LastRebooted is the last reboot timestamp reported by NICo Core
	LastRebooted *time.Time `json:"lastRebooted"`
	// PlacementInRack is the physical placement of the DPU Machine within its Rack
	PlacementInRack *APIPlacementInRack `json:"placementInRack"`
}

// APIDpuMachineProtoContext carries the host Machine ID, Site ID and
// Infrastructure Provider ID that the DpuMachine proto does not include and
// that the handler supplies from the host Machine's record.
type APIDpuMachineProtoContext struct {
	// HostMachineID is the ID of the host Machine the DPU is attached to, not the DPU's own ID.
	HostMachineID string
	// SiteID is the ID of the Site that the host Machine belongs to.
	SiteID uuid.UUID
	// InfrastructureProviderID is the ID of the Infrastructure Provider that owns the host Machine.
	InfrastructureProviderID uuid.UUID
}

// NewAPIDpuMachines builds the API DPU machines from the GetDpuMachines workflow response.
func NewAPIDpuMachines(protoDpuMachines []*corev1.DpuMachine, ctx APIDpuMachineProtoContext) []APIDpuMachine {
	apiDpuMachines := []APIDpuMachine{}
	for _, protoDpuMachine := range protoDpuMachines {
		if protoDpuMachine == nil || protoDpuMachine.GetMachine() == nil {
			continue
		}
		apiDpuMachine := APIDpuMachine{}
		apiDpuMachine.FromProto(protoDpuMachine, ctx)
		apiDpuMachine.DpuNetworkConfig = cmp.Or(apiDpuMachine.DpuNetworkConfig, &APIDpuNetworkConfig{})
		apiDpuMachines = append(apiDpuMachines, apiDpuMachine)
	}
	return apiDpuMachines
}

// FromProto populates an APIDpuMachine from its protobuf form, using ctx for
// the host Machine, Site and Infrastructure Provider IDs not carried on the proto.
func (apd *APIDpuMachine) FromProto(protoDpuMachine *corev1.DpuMachine, ctx APIDpuMachineProtoContext) {
	if protoDpuMachine == nil {
		return
	}

	protoMachine := protoDpuMachine.GetMachine()
	if protoMachine == nil {
		return
	}
	protoMachineStatus := protoMachine.GetStatus()

	apd.ID = protoMachine.GetId().GetId()
	apd.InfrastructureProviderID = ctx.InfrastructureProviderID.String()
	apd.SiteID = ctx.SiteID.String()
	apd.HostMachineID = ctx.HostMachineID

	if protoMachineStatus != nil && protoMachineStatus.DpuAgentVersion != nil {
		apd.DpuAgentVersion = *protoMachineStatus.DpuAgentVersion
	}

	if protoMachine.BmcInfo != nil {
		apd.BMCInfo = &APIBMCInfo{}
		apd.BMCInfo.FromProto(protoMachine.BmcInfo)
	}

	if protoMachineStatus.GetDiscoveryInfo() != nil && protoMachineStatus.GetDiscoveryInfo().DmiData != nil {
		apd.DMIData = &APIDMIData{}
		apd.DMIData.FromProto(protoMachineStatus.GetDiscoveryInfo().DmiData)
	}

	if protoMachineStatus.GetInterfaces() != nil {
		apd.Interfaces = make([]APIDpuMachineInterface, 0, len(protoMachineStatus.GetInterfaces()))
		for _, protoInterface := range protoMachineStatus.GetInterfaces() {
			if protoInterface != nil {
				apdInterface := APIDpuMachineInterface{}
				apdInterface.FromProto(protoInterface)
				apd.Interfaces = append(apd.Interfaces, apdInterface)
			}
		}
	}

	if protoMachine.Inventory != nil {
		apd.SoftwareComponents = []APIDpuMachineSoftwareComponent{}
		for _, protoComponent := range protoMachine.Inventory.Components {
			if protoComponent == nil {
				continue
			}
			apdComponent := APIDpuMachineSoftwareComponent{}
			apdComponent.FromProto(protoComponent)
			apd.SoftwareComponents = append(apd.SoftwareComponents, apdComponent)
		}
	}

	if protoMachineStatus.GetHealth() != nil {
		apd.Health = &APIMachineHealth{}
		apd.Health.FromProto(protoMachineStatus.GetHealth())
	}

	var labels cdbm.Labels
	labels.FromProto(protoMachine.GetMetadata().GetLabels())
	apd.Labels = APILabels(labels)

	apd.State = protoMachine.State

	if protoDpuMachine.DpuNetworkConfig != nil {
		apd.DpuNetworkConfig = &APIDpuNetworkConfig{}
		apd.DpuNetworkConfig.FromProto(protoDpuMachine.DpuNetworkConfig)
	}

	if protoMachineStatus.GetLastRebootTime() != nil {
		lastRebooted := protoMachineStatus.GetLastRebootTime().AsTime()
		apd.LastRebooted = &lastRebooted
	}

	if placement := protoMachine.GetPlacementInRack(); placement != nil {
		apd.PlacementInRack = &APIPlacementInRack{
			SlotNumber: placement.SlotNumber,
			TrayIndex:  placement.TrayIndex,
		}
	}
}
