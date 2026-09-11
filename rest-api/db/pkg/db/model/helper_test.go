// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"math/rand"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"

	"go.opentelemetry.io/otel/trace"
)

var letterRunes = []rune("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")

func testGenerateRandomString(n int) string {
	var letterRunes = []rune("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_1234567890")

	b := make([]rune, n)
	for i := range b {
		b[i] = letterRunes[rand.Intn(len(letterRunes))]
	}
	return string(b)
}

func testInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))

	rand.Seed(time.Now().UnixNano())

	return dbSession
}

func testBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, id *uuid.UUID, name, org string, createdBy uuid.UUID) *InfrastructureProvider {
	ipid := uuid.New()
	if id != nil {
		ipid = *id
	}

	provider := &InfrastructureProvider{
		ID:        ipid,
		Name:      name,
		Org:       org,
		CreatedBy: createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(provider).Exec(context.Background())
	require.NoError(t, err)

	return provider
}

func testBuildTenant(t *testing.T, dbSession *db.Session, id *uuid.UUID, name, org string, createdBy uuid.UUID) *Tenant {
	tid := uuid.New()
	if id != nil {
		tid = *id
	}

	tenant := &Tenant{
		ID:        tid,
		Name:      name,
		Org:       org,
		CreatedBy: createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	require.NoError(t, err)

	return tenant
}

func testBuildAllocation(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, tenant *Tenant, site *Site, name string) *Allocation {
	allocation := &Allocation{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(allocation).Exec(context.Background())
	assert.Nil(t, err)

	return allocation
}

func testBuildAllocationConstraint(t *testing.T, dbSession *db.Session, allocation *Allocation, resourceType string, resourceTypeID uuid.UUID, constraintType string, constraintValue int, createdBy uuid.UUID) *AllocationConstraint {
	constraint := &AllocationConstraint{
		ID:              uuid.New(),
		AllocationID:    allocation.ID,
		ResourceType:    resourceType,
		ResourceTypeID:  resourceTypeID,
		ConstraintType:  constraintType,
		ConstraintValue: constraintValue,
		CreatedBy:       createdBy,
	}

	_, err := dbSession.DB.NewInsert().Model(constraint).Exec(context.Background())
	assert.Nil(t, err)

	return constraint
}

func testBuildSite(t *testing.T, dbSession *db.Session, id *uuid.UUID, ipID uuid.UUID, name, displayName, org string, createdBy uuid.UUID) *Site {
	sid := uuid.New()
	if id != nil {
		sid = *id
	}

	site := &Site{
		ID:                       sid,
		InfrastructureProviderID: ipID,
		Name:                     name,
		DisplayName:              &displayName,
		Org:                      org,
		CreatedBy:                uuid.New(),
	}

	_, err := dbSession.DB.NewInsert().Model(site).Exec(context.Background())
	require.NoError(t, err)

	return site
}

func testBuildVpc(t *testing.T, dbSession *db.Session, id *uuid.UUID, name string, description *string, org string, infrastructureProviderID uuid.UUID, tenantID uuid.UUID, siteID uuid.UUID, nvlinkLogicalPartitionID *uuid.UUID, networkVirtualizationType *string, controllerVpcID *uuid.UUID, labels map[string]string, status *string, createdBy uuid.UUID, networkSecurityGroupId *string) *Vpc {
	vid := uuid.New()
	if id != nil {
		vid = *id
	}

	vstatus := VpcStatusPending
	if status != nil {
		vstatus = *status
	}

	vpc := &Vpc{
		ID:                        vid,
		Name:                      name,
		Description:               description,
		Org:                       org,
		InfrastructureProviderID:  infrastructureProviderID,
		TenantID:                  tenantID,
		SiteID:                    siteID,
		NVLinkLogicalPartitionID:  nvlinkLogicalPartitionID,
		NetworkVirtualizationType: networkVirtualizationType,
		ControllerVpcID:           controllerVpcID,
		NetworkSecurityGroupID:    networkSecurityGroupId,
		Labels:                    labels,
		Status:                    vstatus,
		CreatedBy:                 createdBy,
	}

	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	require.NoError(t, err)

	return vpc
}

func testUpdateVpc(t *testing.T, dbSession *db.Session, v *Vpc) {

	_, err := dbSession.DB.NewUpdate().Where("id = ?", v.ID).Model(v).Exec(context.Background())
	require.NoError(t, err)
}

func testBuildUser(t *testing.T, dbSession *db.Session, id *uuid.UUID, starfleetID string, email, firstName, lastName *string) *User {
	uid := uuid.New()
	if id != nil {
		uid = *id
	}

	user := &User{
		ID:          uid,
		StarfleetID: cutil.GetPtr(starfleetID),
		Email:       email,
		FirstName:   firstName,
		LastName:    lastName,
	}

	_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	require.NoError(t, err)

	return user
}

func testBuildSSHKeyGroup(t *testing.T, dbSession *db.Session, name string, description *string, org string, tenantID uuid.UUID, version *string, status string, createdBy uuid.UUID) *SSHKeyGroup {
	sshKeyGroup := &SSHKeyGroup{
		ID:          uuid.New(),
		Name:        name,
		Description: description,
		Org:         org,
		TenantID:    tenantID,
		Version:     version,
		Status:      status,
		CreatedBy:   createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshKeyGroup).Exec(context.Background())
	assert.Nil(t, err)
	return sshKeyGroup
}

func testBuildSSHKeyGroupSiteAssociation(t *testing.T, dbSession *db.Session, sshKeyGroupID uuid.UUID, siteID uuid.UUID, version *string, status string, createdBy uuid.UUID) *SSHKeyGroupSiteAssociation {
	SSHKeyGroupSiteAssociation := &SSHKeyGroupSiteAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: sshKeyGroupID,
		SiteID:        siteID,
		Version:       version,
		Status:        status,
		CreatedBy:     createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(SSHKeyGroupSiteAssociation).Exec(context.Background())
	assert.Nil(t, err)
	return SSHKeyGroupSiteAssociation
}

func testBuildSSHKey(t *testing.T, dbSession *db.Session, name, org string, tenantID uuid.UUID, publicKey string, fingerprint *string, expires *time.Time, createdBy uuid.UUID) *SSHKey {
	sshkey := &SSHKey{
		ID:          uuid.New(),
		Name:        name,
		Org:         org,
		TenantID:    tenantID,
		PublicKey:   publicKey,
		Fingerprint: fingerprint,
		Expires:     expires,
		CreatedBy:   createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshkey).Exec(context.Background())
	assert.Nil(t, err)
	return sshkey
}

func testBuildSSHKeyAssociation(t *testing.T, dbSession *db.Session, sshKeyID uuid.UUID, sshKeyGroupID uuid.UUID, createdBy uuid.UUID) *SSHKeyAssociation {
	sshkeyassociation := &SSHKeyAssociation{
		ID:            uuid.New(),
		SSHKeyID:      sshKeyID,
		SSHKeyGroupID: sshKeyGroupID,
		CreatedBy:     createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshkeyassociation).Exec(context.Background())
	assert.Nil(t, err)
	return sshkeyassociation
}

func testBuildFabric(t *testing.T, dbSession *db.Session, id *string, org string, siteID uuid.UUID, ipID uuid.UUID, isMissingOnSite bool, status *string) *Fabric {

	fb := &Fabric{
		ID:                       *id,
		Org:                      org,
		SiteID:                   siteID,
		InfrastructureProviderID: ipID,
		IsMissingOnSite:          isMissingOnSite,
		Status:                   *status,
	}

	_, err := dbSession.DB.NewInsert().Model(fb).Exec(context.Background())
	require.NoError(t, err)

	return fb
}

func testBuildInfiniBandPartition(t *testing.T, dbSession *db.Session, id *uuid.UUID, name string, description *string, org string, tenantID uuid.UUID, siteID uuid.UUID, controllerIBInfiniBandPartitionID *uuid.UUID, partitionKey *string, partitionName *string, serviceLevel *int, rateLimit *float32, mtu *int, enableSharp *bool, labels map[string]string, status *InfiniBandPartitionStatus, createdBy uuid.UUID) *InfiniBandPartition {
	pid := uuid.New()
	if id != nil {
		pid = *id
	}

	pstatus := InfiniBandPartitionStatusPending
	if status != nil {
		pstatus = *status
	}

	pt := &InfiniBandPartition{
		ID:                      pid,
		Name:                    name,
		Description:             description,
		Org:                     org,
		SiteID:                  siteID,
		TenantID:                tenantID,
		ControllerIBPartitionID: controllerIBInfiniBandPartitionID,
		PartitionKey:            partitionKey,
		PartitionName:           partitionName,
		ServiceLevel:            serviceLevel,
		RateLimit:               rateLimit,
		Mtu:                     mtu,
		EnableSharp:             enableSharp,
		Labels:                  labels,
		Status:                  pstatus,
		CreatedBy:               createdBy,
	}

	_, err := dbSession.DB.NewInsert().Model(pt).Exec(context.Background())
	require.NoError(t, err)

	return pt
}

func testBuildInfiniBandInterface(t *testing.T, dbSession *db.Session, id *uuid.UUID, siteID uuid.UUID, instanceID uuid.UUID, infiniBandPartitionID uuid.UUID, deviceInstance int, isPhysical bool, virtualFunctionID *int, guid *string, isMissingOnSite bool, status *string, createdBy uuid.UUID) *InfiniBandInterface {
	pid := uuid.New()
	if id != nil {
		pid = *id
	}

	if status == nil {
		status = cutil.GetPtr(InfiniBandInterfaceStatusPending)
	}

	ibif := &InfiniBandInterface{
		ID:                    pid,
		SiteID:                siteID,
		InstanceID:            instanceID,
		InfiniBandPartitionID: infiniBandPartitionID,
		DeviceInstance:        deviceInstance,
		IsPhysical:            isPhysical,
		VirtualFunctionID:     virtualFunctionID,
		GUID:                  guid,
		IsMissingOnSite:       isMissingOnSite,
		Status:                *status,
		CreatedBy:             createdBy,
	}

	_, err := dbSession.DB.NewInsert().Model(ibif).Exec(context.Background())
	require.NoError(t, err)

	return ibif
}

func testBuildNVLinkLogicalPartition(t *testing.T, dbSession *db.Session, id *uuid.UUID, name string, description *string, org string, tenantID uuid.UUID, siteID uuid.UUID, status *NVLinkLogicalPartitionStatus, createdBy uuid.UUID) *NVLinkLogicalPartition {
	pid := uuid.New()
	if id != nil {
		pid = *id
	}

	pstatus := NVLinkLogicalPartitionStatusPending
	if status != nil {
		pstatus = *status
	}

	nvllp := &NVLinkLogicalPartition{
		ID:          pid,
		Name:        name,
		Description: description,
		Org:         org,
		SiteID:      siteID,
		TenantID:    tenantID,
		Status:      pstatus,
		CreatedBy:   createdBy,
	}

	_, err := dbSession.DB.NewInsert().Model(nvllp).Exec(context.Background())
	require.NoError(t, err)

	return nvllp
}

func testBuildNVLinkInterface(t *testing.T, dbSession *db.Session, id *uuid.UUID, siteID uuid.UUID, instanceID uuid.UUID, nvlinkLogicalPartitionID uuid.UUID, nvlinkDomainID *uuid.UUID, device *string, DeviceInstance int, gpuGUID *string, status *string, createdBy uuid.UUID) *NVLinkInterface {
	pid := uuid.New()
	if id != nil {
		pid = *id
	}

	if status == nil {
		status = cutil.GetPtr(NVLinkInterfaceStatusPending)
	}

	nvli := &NVLinkInterface{
		ID:                       pid,
		SiteID:                   siteID,
		InstanceID:               instanceID,
		NVLinkLogicalPartitionID: nvlinkLogicalPartitionID,
		NVLinkDomainID:           nvlinkDomainID,
		Device:                   device,
		DeviceInstance:           DeviceInstance,
		GpuGUID:                  gpuGUID,
		Status:                   *status,
		CreatedBy:                createdBy,
	}

	_, err := dbSession.DB.NewInsert().Model(nvli).Exec(context.Background())
	require.NoError(t, err)

	return nvli
}

func testGenerateStarfleetID() string {
	// Example: eKTl_V61-o6x2-Uyz8jqmxkRFrfP_ggyIvzp-ITQSvc
	return fmt.Sprintf("%v-%v-%v-%v", testGenerateRandomString(8), testGenerateRandomString(4), testGenerateRandomString(22), testGenerateRandomString(6))
}

func testGenerateMacAddress(t *testing.T) string {
	buf := make([]byte, 6)
	_, err := rand.Read(buf)
	assert.NoError(t, err)
	// Set the local bit
	buf[0] = (buf[0] | 2) & 0xfe // Set local bit, ensure unicast address
	return fmt.Sprintf("%02x:%02x:%02x:%02x:%02x:%02x\n", buf[0], buf[1], buf[2], buf[3], buf[4], buf[5])
}

// testCommonTraceProviderSetup creates a test provider and span context.
func testCommonTraceProviderSetup(t *testing.T, ctx context.Context) (trace.Tracer, trace.SpanContext, context.Context) {
	provider := trace.NewNoopTracerProvider()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})

	ctx = trace.ContextWithRemoteSpanContext(ctx, sc)

	tracer := provider.Tracer("test-db-model")

	return tracer, sc, ctx
}

func testBuildImageOperatingSystem(t *testing.T, dbSession *db.Session, name string, description *string, org string, ipID *uuid.UUID, tenantID *uuid.UUID, version *string, enabelBS bool, status string, createdBy uuid.UUID) *OperatingSystem {
	os := &OperatingSystem{
		ID:                       uuid.New(),
		Name:                     name,
		Description:              description,
		Org:                      org,
		InfrastructureProviderID: ipID,
		TenantID:                 tenantID,
		Type:                     "Image",
		ImageURL:                 cutil.GetPtr("imageURL"),
		ImageSHA:                 cutil.GetPtr("imageSHA"),
		ImageAuthType:            cutil.GetPtr("imageAuthType"),
		ImageAuthToken:           cutil.GetPtr("imageAuthToken"),
		ImageDisk:                cutil.GetPtr("imageDisk"),
		RootFsID:                 cutil.GetPtr("rootFsId"),
		RootFsLabel:              cutil.GetPtr("rootFsLabel"),
		Version:                  version,
		EnableBlockStorage:       enabelBS,
		Status:                   status,
		CreatedBy:                createdBy,
	}

	_, err := dbSession.DB.NewInsert().Model(os).Exec(context.Background())
	assert.Nil(t, err)
	return os
}

func testBuildOperatingSystemSiteAssociation(t *testing.T, dbSession *db.Session, osID uuid.UUID, siteID uuid.UUID, version *string, status string, createdBy uuid.UUID) *OperatingSystemSiteAssociation {
	ossa := &OperatingSystemSiteAssociation{
		ID:                uuid.New(),
		OperatingSystemID: osID,
		SiteID:            siteID,
		Version:           version,
		Status:            status,
		CreatedBy:         createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(ossa).Exec(context.Background())
	assert.Nil(t, err)
	return ossa
}

func testBuildInstanceType(t *testing.T, dbSession *db.Session, id *uuid.UUID, name string, ipID uuid.UUID, siteID uuid.UUID, createdBy uuid.UUID) *InstanceType {
	var itID uuid.UUID
	if id != nil {
		itID = *id
	} else {
		itID = uuid.New()
	}

	it := &InstanceType{
		ID:                       itID,
		Name:                     name,
		InfrastructureProviderID: ipID,
		SiteID:                   &siteID,
		Status:                   InstanceTypeStatusReady,
		Version:                  "1.0",
		CreatedBy:                createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(it).Exec(context.Background())
	assert.Nil(t, err)
	return it
}

func testBuildDpuExtensionService(t *testing.T, dbSession *db.Session, id *uuid.UUID, name string, serviceType string, siteID uuid.UUID, tenantID uuid.UUID, version *string, status string, createdBy uuid.UUID) *DpuExtensionService {
	var desID uuid.UUID
	if id != nil {
		desID = *id
	} else {
		desID = uuid.New()
	}

	des := &DpuExtensionService{
		ID:          desID,
		Name:        name,
		ServiceType: serviceType,
		SiteID:      siteID,
		TenantID:    tenantID,
		Version:     version,
		Status:      status,
		CreatedBy:   createdBy,
	}

	if version != nil {
		des.VersionInfo = &DpuExtensionServiceVersionInfo{
			Version:        *version,
			Data:           "test-data",
			HasCredentials: true,
			Created:        db.GetCurTime(),
		}
		des.ActiveVersions = []string{*version}
	}

	_, err := dbSession.DB.NewInsert().Model(des).Exec(context.Background())
	assert.Nil(t, err)
	return des
}

func testBuildDpuExtensionServiceDeployment(t *testing.T, dbSession *db.Session, id *uuid.UUID, siteID uuid.UUID, tenantID uuid.UUID, instanceID uuid.UUID, dpuExtensionServiceID uuid.UUID, version string, status string, createdBy uuid.UUID) *DpuExtensionServiceDeployment {
	var desdID uuid.UUID
	if id != nil {
		desdID = *id
	} else {
		desdID = uuid.New()
	}

	desd := &DpuExtensionServiceDeployment{
		ID:                    desdID,
		SiteID:                siteID,
		TenantID:              tenantID,
		InstanceID:            instanceID,
		DpuExtensionServiceID: dpuExtensionServiceID,
		Version:               version,
		Status:                status,
		CreatedBy:             createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(desd).Exec(context.Background())
	assert.Nil(t, err)
	return desd
}
