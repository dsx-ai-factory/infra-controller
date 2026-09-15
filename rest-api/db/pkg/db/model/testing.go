// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// TestSetupSchema creates/resets the schema
func TestSetupSchema(t *testing.T, dbSession *db.Session) {
	// create User table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create TenantAccount table
	err = dbSession.DB.ResetModel(context.Background(), (*TenantAccount)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create TenantSite table
	err = dbSession.DB.ResetModel(context.Background(), (*TenantSite)(nil))
	assert.Nil(t, err)
	// create Network Security Group table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create NVLink Logical Partition table
	err = dbSession.DB.ResetModel(context.Background(), (*NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*Domain)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*Allocation)(nil))
	assert.Nil(t, err)
	// create Allocation Constraint table
	err = dbSession.DB.ResetModel(context.Background(), (*AllocationConstraint)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create VpcPrefix table
	err = dbSession.DB.ResetModel(context.Background(), (*VpcPrefix)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*Subnet)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceType)(nil))
	assert.Nil(t, err)
	// create Operating System table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystem)(nil))
	assert.Nil(t, err)
	// create Operating System Site Association table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystemSiteAssociation)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*Instance)(nil))
	assert.Nil(t, err)
	// create MachineInstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineInstanceType)(nil))
	assert.Nil(t, err)
	// create MachineCapability table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineCapability)(nil))
	assert.Nil(t, err)
	// create MachineInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineInterface)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*Interface)(nil))
	assert.Nil(t, err)
	// create Status Detail table
	err = dbSession.DB.ResetModel(context.Background(), (*StatusDetail)(nil))
	assert.Nil(t, err)
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*InfiniBandPartition)(nil))
	assert.Nil(t, err)
	// create InfiniBandInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*InfiniBandInterface)(nil))
	assert.Nil(t, err)
	// create SpectrumXPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*SpectrumXPartition)(nil))
	assert.Nil(t, err)
	// create SpectrumXAttachment table
	err = dbSession.DB.ResetModel(context.Background(), (*SpectrumXAttachment)(nil))
	assert.Nil(t, err)
	// create ssh key table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKey)(nil))
	assert.Nil(t, err)
	// create ssh key group table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroup)(nil))
	assert.Nil(t, err)
	// create ssh key association table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKeyAssociation)(nil))
	assert.Nil(t, err)
	// create ssh key group site association table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroupSiteAssociation)(nil))
	assert.Nil(t, err)
	// create ssh key group instance association table
	err = dbSession.DB.ResetModel(context.Background(), (*SSHKeyGroupInstanceAssociation)(nil))
	assert.Nil(t, err)
	// create network security group table
	err = dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create sku table
	err = dbSession.DB.ResetModel(context.Background(), (*SKU)(nil))
	assert.Nil(t, err)
	// create expected machine table
	err = dbSession.DB.ResetModel(context.Background(), (*ExpectedMachine)(nil))
	assert.Nil(t, err)
	// create nvlink interface table (must be after Instance and NVLinkLogicalPartition)
	err = dbSession.DB.ResetModel(context.Background(), (*NVLinkInterface)(nil))
	assert.Nil(t, err)
	// create dpu extension service table
	err = dbSession.DB.ResetModel(context.Background(), (*DpuExtensionService)(nil))
	assert.Nil(t, err)
	// create dpu extension service deployment table
	err = dbSession.DB.ResetModel(context.Background(), (*DpuExtensionServiceDeployment)(nil))
	assert.Nil(t, err)
}

// TestBuildUser creates a test User
func TestBuildUser(t *testing.T, dbSession *db.Session, starfleetID string, org string, roles []string) *User {
	uDAO := NewUserDAO(dbSession)

	orgData := OrgData{
		org: Org{
			ID:          123,
			Name:        org,
			DisplayName: org,
			OrgType:     "ENTERPRISE",
			Roles:       roles,
		},
	}

	input := UserCreateInput{
		StarfleetID: &starfleetID,
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData:     orgData,
	}
	u, err := uDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return u
}

// TestBuildInfrastructureProvider creates a test Infrastructure Provider
func TestBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string, org string, user *User) *InfrastructureProvider {
	ipDAO := NewInfrastructureProviderDAO(dbSession)

	ip, err := ipDAO.Create(context.Background(), nil, InfrastructureProviderCreateInput{
		Name:           name,
		DisplayName:    cutil.GetPtr("Test Provider"),
		Org:            org,
		OrgDisplayName: cutil.GetPtr(org),
		CreatedBy:      user.ID,
	})
	assert.Nil(t, err)

	return ip
}

// TestBuildTenant creates a test Tenant
func TestBuildTenant(t *testing.T, dbSession *db.Session, name string, org string, user *User) *Tenant {
	tnDAO := NewTenantDAO(dbSession)

	tn, err := tnDAO.Create(context.Background(), nil, TenantCreateInput{
		Name:           name,
		DisplayName:    cutil.GetPtr("Test Tenant"),
		Org:            org,
		OrgDisplayName: cutil.GetPtr(org),
		CreatedBy:      user.ID,
	})
	assert.Nil(t, err)

	return tn
}

// TestBuildSite creates a test Site
func TestBuildSite(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string, user *User) *Site {
	stDAO := NewSiteDAO(dbSession)

	st, err := stDAO.Create(context.Background(), nil, SiteCreateInput{
		Name:                          name,
		DisplayName:                   cutil.GetPtr("Test Site"),
		Description:                   cutil.GetPtr("Test Site Description"),
		Org:                           ip.Org,
		InfrastructureProviderID:      ip.ID,
		SiteControllerVersion:         cutil.GetPtr("1.0.0"),
		SiteAgentVersion:              cutil.GetPtr("1.0.0"),
		RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration:   cutil.GetPtr(db.GetCurTime()),
		IsInfinityEnabled:             true,
		SerialConsoleHostname:         cutil.GetPtr("TestSshHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(30),
		SerialConsoleMaxSessionLength: cutil.GetPtr(60),
		Status:                        SiteStatusPending,
		CreatedBy:                     user.ID,
	})
	assert.Nil(t, err)

	return st
}

// TestBuildTenantSite creates a test Tenant/Site relationship
func TestBuildTenantSite(t *testing.T, dbSession *db.Session, tn *Tenant, st *Site, config *TenantSiteConfig, user *User) *TenantSite {
	tsDAO := NewTenantSiteDAO(dbSession)

	ts, err := tsDAO.Create(context.Background(), nil, TenantSiteCreateInput{
		TenantID:  tn.ID,
		TenantOrg: tn.Org,
		SiteID:    st.ID,
		Config:    config,
		CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	return ts
}

// TestBuildInstanceType creates a test Instance Type
func TestBuildInstanceType(t *testing.T, dbSession *db.Session, name string, ip *InfrastructureProvider, site *Site, user *User) *InstanceType {
	instanceType := &InstanceType{
		ID:                       uuid.New(),
		Name:                     name,
		DisplayName:              &name,
		Description:              cutil.GetPtr("Instance Type Description"),
		ControllerMachineType:    nil,
		InfrastructureProviderID: ip.ID,
		SiteID:                   &site.ID,
		Status:                   InstanceTypeStatusPending,
		CreatedBy:                user.ID,
	}

	_, err := dbSession.DB.NewInsert().Model(instanceType).Exec(context.Background())
	assert.Nil(t, err)

	return instanceType
}

// TestBuildIPBlock creates a test IP Block
func TestBuildIPBlock(t *testing.T, dbSession *db.Session, name string, site *Site, tenant *Tenant, routingType, prefix string, blockSize int, protocolVersion string) *IPBlock {
	ipbDAO := NewIPBlockDAO(dbSession)

	ipb, err := ipbDAO.Create(
		context.Background(),
		nil,
		IPBlockCreateInput{
			Name:                     name,
			SiteID:                   site.ID,
			InfrastructureProviderID: site.InfrastructureProviderID,
			TenantID:                 &tenant.ID,
			RoutingType:              routingType,
			Prefix:                   prefix,
			PrefixLength:             blockSize,
			ProtocolVersion:          protocolVersion,
			Status:                   IPBlockStatusPending,
			CreatedBy:                cutil.GetPtr(uuid.New()),
		},
	)
	assert.Nil(t, err)

	return ipb
}

// TestBuildAllocation creates a test Allocation
func TestBuildAllocation(t *testing.T, dbSession *db.Session, name string, st *Site, tn *Tenant, user *User) *Allocation {
	alDAO := NewAllocationDAO(dbSession)

	al, err := alDAO.Create(context.Background(), nil, AllocationCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("Test Allocation Description"),
		InfrastructureProviderID: st.InfrastructureProviderID,
		TenantID:                 tn.ID,
		SiteID:                   st.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)

	return al
}

// TestBuildAllocationConstraint creates a test Allocation Constraint of Instance Type
func TestBuildAllocationConstraint(t *testing.T, dbSession *db.Session, al *Allocation, it *InstanceType, ipb *IPBlock, constraintValue int, user *User) *AllocationConstraint {
	var resourceID uuid.UUID
	resourceType := AllocationResourceTypeInstanceType
	if it != nil {
		resourceID = it.ID
	} else if ipb != nil {
		resourceID = ipb.ID
		resourceType = AllocationResourceTypeIPBlock
	}

	acDAO := NewAllocationConstraintDAO(dbSession)
	ac, err := acDAO.Create(context.Background(), nil, AllocationConstraintCreateInput{
		AllocationID: al.ID, ResourceType: resourceType,
		ResourceTypeID: resourceID, ConstraintType: AllocationConstraintTypeReserved,
		ConstraintValue: constraintValue, CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	return ac
}

// TestBuildVPC creates a test VPC
func TestBuildVPC(t *testing.T, dbSession *db.Session, name string, ip *InfrastructureProvider, tn *Tenant, st *Site, networkVirtualizationType *string, controllerID *uuid.UUID, labels map[string]string, status string, user *User, nsgID *string) *Vpc {
	vDAO := NewVpcDAO(dbSession)

	input := VpcCreateInput{
		Name:                      name,
		Description:               cutil.GetPtr("Test Vpc"),
		Org:                       tn.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: networkVirtualizationType,
		NetworkSecurityGroupID:    nsgID,
		ControllerVpcID:           controllerID,
		Labels:                    labels,
		Status:                    status,
		CreatedBy:                 *user,
	}

	vpc, err := vDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

// TestBuildSubnet creates a test subnet
func TestBuildSubnet(t *testing.T, dbSession *db.Session, name string, tn *Tenant, vpc *Vpc, controllerID *uuid.UUID, ipv4Block *IPBlock, status string, user *User) *Subnet {
	subnetDAO := NewSubnetDAO(dbSession)

	subnet, err := subnetDAO.Create(context.Background(), nil, SubnetCreateInput{
		Name:                       name,
		Description:                cutil.GetPtr("Test Subnet"),
		Org:                        tn.Org,
		SiteID:                     vpc.SiteID,
		VpcID:                      vpc.ID,
		TenantID:                   tn.ID,
		ControllerNetworkSegmentID: controllerID,
		RoutingType:                &ipv4Block.RoutingType,
		IPv4Prefix:                 &ipv4Block.Prefix,
		IPv4BlockID:                &ipv4Block.ID,
		PrefixLength:               ipv4Block.PrefixLength,
		Status:                     status,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)

	return subnet
}

// TestBuildMachine creates a test Machine
func TestBuildMachine(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, site *Site, instanceType *InstanceType, controllerMachineType *string) *Machine {
	defMacAddr := "00:1B:44:11:3A:B7"
	machine := &Machine{
		ID:                       uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		ControllerMachineType:    controllerMachineType,
		Metadata:                 nil,
		DefaultMacAddress:        &defMacAddr,
		Status:                   MachineStatusInitializing,
	}

	if instanceType != nil {
		machine.InstanceTypeID = &instanceType.ID
	}

	_, err := dbSession.DB.NewInsert().Model(machine).Exec(context.Background())
	assert.Nil(t, err)

	return machine
}

// TestBuildMachineInstanceType creates a test Machine/Instance Type association
func TestBuildMachineInstanceType(t *testing.T, dbSession *db.Session, machine *Machine, instanceType *InstanceType) *MachineInstanceType {
	mit := &MachineInstanceType{
		ID:             uuid.New(),
		MachineID:      machine.ID,
		InstanceTypeID: instanceType.ID,
	}

	_, err := dbSession.DB.NewInsert().Model(mit).Exec(context.Background())
	assert.Nil(t, err)

	return mit
}

// TestBuildOperatingSystem creates a test os
func TestBuildOperatingSystem(t *testing.T, dbSession *db.Session, name string, tn *Tenant, status string, user *User) *OperatingSystem {
	operatingSystem := &OperatingSystem{
		ID:        uuid.New(),
		Name:      name,
		TenantID:  cutil.GetPtr(tn.ID),
		Status:    status,
		CreatedBy: user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

// TestBuildInstance creates a test instance.
// It returns a persisted instance without any allocation linkage.
func TestBuildInstance(t *testing.T, dbSession *db.Session, name string, tn *Tenant, ip *InfrastructureProvider, st *Site, it *InstanceType, vpc *Vpc, m *Machine, os *OperatingSystem) *Instance {
	ins := &Instance{
		ID:                       uuid.New(),
		Name:                     name,
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   st.ID,
		InstanceTypeID:           &it.ID,
		VpcID:                    vpc.ID,
		MachineID:                &m.ID,
		OperatingSystemID:        &os.ID,
		Hostname:                 nil,
		UserData:                 nil,
		IpxeScript:               nil,
		Created:                  db.GetCurTime(),
		Updated:                  db.GetCurTime(),
		Status:                   InstanceStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(ins).Exec(context.Background())
	assert.Nil(t, err)
	return ins
}

// TestBuildInterface creates a test interface
func TestBuildInterface(t *testing.T, dbSession *db.Session, ins *Instance, sbID *uuid.UUID, vpID *uuid.UUID, isPhysical bool, status string) *Interface {
	ifc := &Interface{
		ID:          uuid.New(),
		InstanceID:  ins.ID,
		SubnetID:    sbID,
		VpcPrefixID: vpID,
		IsPhysical:  isPhysical,
		Status:      status,
	}
	_, err := dbSession.DB.NewInsert().Model(ifc).Exec(context.Background())
	assert.Nil(t, err)
	return ifc
}

// TestBuildNetworkSecurityGroup creates a test NSG
func TestBuildNetworkSecurityGroup(t *testing.T, dbSession *db.Session, name string, tn *Tenant, st *Site) *NetworkSecurityGroup {
	nsg := &NetworkSecurityGroup{
		ID:        uuid.NewString(),
		Name:      name,
		SiteID:    st.ID,
		TenantOrg: tn.Org,
		TenantID:  tn.ID,
		Status:    NetworkSecurityGroupStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(nsg).Exec(context.Background())
	assert.Nil(t, err)
	return nsg
}
