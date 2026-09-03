// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// TestSpectrumXAttachment_ToProto proves the Site config is derived from the persisted row,
// which is what keeps the attachment list Core receives in agreement with the DB.
func TestSpectrumXAttachment_ToProto(t *testing.T) {
	partitionID := uuid.New()
	const device = "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC"

	t.Run("populates partition, device and type", func(t *testing.T) {
		sxa := &SpectrumXAttachment{
			SpectrumXPartitionID: partitionID,
			Device:               device,
			DeviceInstance:       2,
			AttachmentType:       SpectrumXAttachmentTypePhysical,
		}

		got := sxa.ToProto()
		require.NotNil(t, got)
		assert.Equal(t, partitionID.String(), got.GetSpxPartitionId().GetValue())
		assert.Equal(t, device, got.Device)
		assert.Equal(t, uint32(2), got.DeviceInstance)
		assert.Equal(t, corev1.SpxAttachmentType_Physical, got.AttachmentType)
		assert.Nil(t, got.VirtualFunctionId, "an unset virtual function must stay unset on the wire")
	})

	t.Run("carries a set virtual function", func(t *testing.T) {
		sxa := &SpectrumXAttachment{
			SpectrumXPartitionID: partitionID,
			Device:               device,
			AttachmentType:       SpectrumXAttachmentTypeOVN,
			VirtualFunctionID:    cutil.GetPtr(3),
		}

		got := sxa.ToProto()
		assert.Equal(t, corev1.SpxAttachmentType_Ovn, got.AttachmentType)
		require.NotNil(t, got.VirtualFunctionId)
		assert.Equal(t, uint32(3), *got.VirtualFunctionId)
	})

	// API-side validation rejects an unknown type long before a row is written, so the
	// zero enum is the safe mapping rather than a failure the reconciler cannot act on.
	t.Run("an unrecognized stored type maps to Physical", func(t *testing.T) {
		sxa := &SpectrumXAttachment{SpectrumXPartitionID: partitionID, AttachmentType: SpectrumXAttachmentType("Bogus")}
		assert.Equal(t, corev1.SpxAttachmentType_Physical, sxa.ToProto().AttachmentType)
	})
}

func testSpectrumXSetupSchema(t *testing.T, dbSession *db.Session) {
	err := dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*SpectrumXPartition)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*SpectrumXAttachment)(nil))
	require.NoError(t, err)
}

// spectrumXFixture is the Site, Tenant, Instance and Partition chain a SpectrumX
// Attachment needs before it can be inserted.
type spectrumXFixture struct {
	tenant    *Tenant
	site      *Site
	instance  *Instance
	partition *SpectrumXPartition
	user      *User
}

func testBuildSpectrumXFixture(t *testing.T, dbSession *db.Session) spectrumXFixture {
	ctx := context.Background()

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)

	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoetenant@test.com"), cutil.GetPtr("Tenant"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)

	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	vpc := testInstanceBuildVpc(t, dbSession, ip, st, tn, "testVpc")
	instanceType := testInstanceBuildInstanceType(t, dbSession, ip, "testInstanceType")
	machine := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, &instanceType.ID, cutil.GetPtr("mcTypeTest"))
	allocation := testInstanceBuildAllocation(t, dbSession, ip, tn, st, "testAllocation")
	_ = testBuildAllocationConstraint(t, dbSession, allocation, AllocationResourceTypeInstanceType, instanceType.ID, AllocationConstraintTypeReserved, 10, uuid.New())
	operatingSystem := testInstanceBuildOperatingSystem(t, dbSession, "testOS")

	instance, err := NewInstanceDAO(dbSession).Create(ctx, nil, InstanceCreateInput{
		Name:                     "test-instance",
		TenantID:                 tn.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   st.ID,
		InstanceTypeID:           &instanceType.ID,
		VpcID:                    vpc.ID,
		MachineID:                &machine.ID,
		Hostname:                 cutil.GetPtr("test.com"),
		OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
		IpxeScript:               cutil.GetPtr("ipxe"),
		InfinityRCRStatus:        cutil.GetPtr("RESOURCE_GRANTED"),
		Status:                   InstanceStatusPending,
		CreatedBy:                tnu.ID,
	})
	require.NoError(t, err)
	require.NotNil(t, instance)

	partition, err := NewSpectrumXPartitionDAO(dbSession).Create(ctx, nil, SpectrumXPartitionCreateInput{
		Name:      "test-spectrumx-partition",
		TenantOrg: tn.Org,
		SiteID:    st.ID,
		TenantID:  tn.ID,
		Status:    SpectrumXPartitionStatusPending,
		CreatedBy: tnu.ID,
	})
	require.NoError(t, err)
	require.NotNil(t, partition)

	return spectrumXFixture{tenant: tn, site: st, instance: instance, partition: partition, user: tnu}
}

// TestSpectrumXPartitionSQLDAO_Lifecycle proves the Partition row survives the create,
// inventory-update, clear and delete path the reconciler and handlers drive it through.
func TestSpectrumXPartitionSQLDAO_Lifecycle(t *testing.T) {
	ctx := context.Background()

	dbSession := testInitDB(t)
	defer dbSession.Close()
	testSpectrumXSetupSchema(t, dbSession)

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	sxpDAO := NewSpectrumXPartitionDAO(dbSession)

	// A create that omits the VNI is the allocate-on-Site path, so the column has to
	// persist as NULL rather than 0.
	created, err := sxpDAO.Create(ctx, nil, SpectrumXPartitionCreateInput{
		Name:        "east-west-net",
		Description: cutil.GetPtr("east-west"),
		TenantOrg:   tn.Org,
		SiteID:      st.ID,
		TenantID:    tn.ID,
		Labels:      map[string]string{"env": "prod"},
		Status:      SpectrumXPartitionStatusPending,
		CreatedBy:   tnu.ID,
	})
	require.NoError(t, err)
	require.NotNil(t, created)
	assert.Equal(t, "east-west-net", created.Name)
	assert.Nil(t, created.VNI)
	assert.False(t, created.IsMissingOnSite)
	assert.Equal(t, SpectrumXPartitionStatusPending, created.Status)

	// Inventory writes the Site-allocated VNI and the promotion to Ready.
	updated, err := sxpDAO.Update(ctx, nil, SpectrumXPartitionUpdateInput{
		SpectrumXPartitionID: created.ID,
		VNI:                  cutil.GetPtr(10200),
		Status:               cutil.GetPtr(SpectrumXPartitionStatusReady),
		IsMissingOnSite:      cutil.GetPtr(true),
	})
	require.NoError(t, err)
	require.NotNil(t, updated.VNI)
	assert.Equal(t, 10200, *updated.VNI)
	assert.Equal(t, SpectrumXPartitionStatusReady, updated.Status)
	assert.True(t, updated.IsMissingOnSite)

	// An unrecognized status is rejected rather than written through.
	_, err = sxpDAO.Update(ctx, nil, SpectrumXPartitionUpdateInput{
		SpectrumXPartitionID: created.ID,
		Status:               cutil.GetPtr(SpectrumXPartitionStatus("Bogus")),
	})
	assert.Error(t, err)

	// Tenant-scoped list is what the GetAll handler relies on for isolation.
	byTenant, total, err := sxpDAO.GetAll(ctx, nil, SpectrumXPartitionFilterInput{
		TenantIDs: []uuid.UUID{tn.ID},
		SiteIDs:   []uuid.UUID{st.ID},
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 1, total)
	require.Len(t, byTenant, 1)
	assert.Equal(t, created.ID, byTenant[0].ID)

	_, total, err = sxpDAO.GetAll(ctx, nil, SpectrumXPartitionFilterInput{
		TenantIDs: []uuid.UUID{uuid.New()},
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 0, total)

	cleared, err := sxpDAO.Clear(ctx, nil, SpectrumXPartitionClearInput{
		SpectrumXPartitionID: created.ID,
		Description:          true,
		VNI:                  true,
		Labels:               true,
	})
	require.NoError(t, err)
	assert.Nil(t, cleared.Description)
	assert.Nil(t, cleared.VNI)
	assert.Nil(t, cleared.Labels)

	require.NoError(t, sxpDAO.Delete(ctx, nil, created.ID))
	_, err = sxpDAO.Get(ctx, nil, created.ID, nil)
	assert.ErrorIs(t, err, db.ErrDoesNotExist)

	// A soft-deleted Partition is invisible by default, reachable through IncludeDeleted,
	// and restored by clearing the timestamp.
	_, total, err = sxpDAO.GetAll(ctx, nil, SpectrumXPartitionFilterInput{
		SpectrumXPartitionIDs: []uuid.UUID{created.ID},
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 0, total)

	deleted, total, err := sxpDAO.GetAll(ctx, nil, SpectrumXPartitionFilterInput{
		SpectrumXPartitionIDs: []uuid.UUID{created.ID},
		IncludeDeleted:        true,
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 1, total)
	require.Len(t, deleted, 1)
	require.NotNil(t, deleted[0].Deleted)

	restored, err := sxpDAO.Clear(ctx, nil, SpectrumXPartitionClearInput{
		SpectrumXPartitionID: created.ID,
		Deleted:              true,
	})
	require.NoError(t, err)
	assert.Nil(t, restored.Deleted)

	_, err = sxpDAO.Get(ctx, nil, created.ID, nil)
	assert.NoError(t, err, "an undeleted Partition must be reachable again")
}

// TestSpectrumXAttachmentSQLDAO_CreateMultiple proves the batch insert returns rows in the
// caller's order, which is what keeps the Core attachment config index-aligned with the
// status list the Site reports back.
func TestSpectrumXAttachmentSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()

	dbSession := testInitDB(t)
	defer dbSession.Close()
	testSpectrumXSetupSchema(t, dbSession)

	fx := testBuildSpectrumXFixture(t, dbSession)
	sxaDAO := NewSpectrumXAttachmentDAO(dbSession)

	inputs := []SpectrumXAttachmentCreateInput{}
	for i := range 3 {
		inputs = append(inputs, SpectrumXAttachmentCreateInput{
			InstanceID:           fx.instance.ID,
			SiteID:               fx.site.ID,
			SpectrumXPartitionID: fx.partition.ID,
			Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
			DeviceInstance:       i,
			AttachmentType:       SpectrumXAttachmentTypePhysical,
			Status:               SpectrumXAttachmentStatusPending,
			CreatedBy:            fx.user.ID,
		})
	}

	created, err := sxaDAO.CreateMultiple(ctx, nil, inputs)
	require.NoError(t, err)
	require.Len(t, created, 3)
	for i, sxa := range created {
		assert.Equal(t, i, sxa.DeviceInstance, "batch result must preserve input order")
		assert.Equal(t, SpectrumXAttachmentStatusPending, sxa.Status)
		assert.Nil(t, sxa.MacAddress)
		assert.Nil(t, sxa.IPAddress)
	}

	empty, err := sxaDAO.CreateMultiple(ctx, nil, nil)
	require.NoError(t, err)
	assert.Empty(t, empty)
}

// TestSpectrumXAttachmentSQLDAO_Lifecycle proves the attachment row carries the
// Site-allocated MAC and IP that inventory writes, and that the delete paths the
// Partition-delete gate and Site teardown rely on both work.
func TestSpectrumXAttachmentSQLDAO_Lifecycle(t *testing.T) {
	ctx := context.Background()

	dbSession := testInitDB(t)
	defer dbSession.Close()
	testSpectrumXSetupSchema(t, dbSession)

	fx := testBuildSpectrumXFixture(t, dbSession)
	sxaDAO := NewSpectrumXAttachmentDAO(dbSession)

	created, err := sxaDAO.Create(ctx, nil, SpectrumXAttachmentCreateInput{
		InstanceID:           fx.instance.ID,
		SiteID:               fx.site.ID,
		SpectrumXPartitionID: fx.partition.ID,
		Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
		DeviceInstance:       0,
		AttachmentType:       SpectrumXAttachmentTypePhysical,
		Status:               SpectrumXAttachmentStatusPending,
		CreatedBy:            fx.user.ID,
	})
	require.NoError(t, err)
	require.NotNil(t, created)

	// Inventory writes the Site-allocated MAC, IP and the promotion to Ready.
	updated, err := sxaDAO.Update(ctx, nil, SpectrumXAttachmentUpdateInput{
		SpectrumXAttachmentID: created.ID,
		MacAddress:            cutil.GetPtr("00:11:22:33:44:55"),
		IPAddress:             cutil.GetPtr("192.0.2.15"),
		Status:                cutil.GetPtr(SpectrumXAttachmentStatusReady),
		IsMissingOnSite:       cutil.GetPtr(true),
	})
	require.NoError(t, err)
	require.NotNil(t, updated.MacAddress)
	assert.Equal(t, "00:11:22:33:44:55", *updated.MacAddress)
	require.NotNil(t, updated.IPAddress)
	assert.Equal(t, "192.0.2.15", *updated.IPAddress)
	assert.Equal(t, SpectrumXAttachmentStatusReady, updated.Status)
	assert.True(t, updated.IsMissingOnSite)

	// The Partition-delete gate reads attachments by Partition ID, and the Instance
	// response reads them by Instance ID.
	byPartition, total, err := sxaDAO.GetAll(ctx, nil, SpectrumXAttachmentFilterInput{
		SpectrumXPartitionIDs: []uuid.UUID{fx.partition.ID},
	}, paginator.PageInput{}, []string{SpectrumXPartitionRelationName})
	require.NoError(t, err)
	assert.Equal(t, 1, total)
	require.Len(t, byPartition, 1)
	require.NotNil(t, byPartition[0].SpectrumXPartition, "SpectrumXPartition relation must be loadable")
	assert.Equal(t, fx.partition.ID, byPartition[0].SpectrumXPartition.ID)

	_, total, err = sxaDAO.GetAll(ctx, nil, SpectrumXAttachmentFilterInput{
		InstanceIDs: []uuid.UUID{fx.instance.ID},
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 1, total)

	cleared, err := sxaDAO.Clear(ctx, nil, SpectrumXAttachmentClearInput{
		SpectrumXAttachmentID: created.ID,
		MacAddress:            true,
		IPAddress:             true,
		VirtualFunctionID:     true,
	})
	require.NoError(t, err)
	assert.Nil(t, cleared.MacAddress)
	assert.Nil(t, cleared.IPAddress)
	assert.Nil(t, cleared.VirtualFunctionID)

	require.NoError(t, sxaDAO.DeleteAllBySiteID(ctx, nil, fx.site.ID))
	_, err = sxaDAO.Get(ctx, nil, created.ID, nil)
	assert.ErrorIs(t, err, db.ErrDoesNotExist)

	// A soft-deleted Attachment is invisible by default, reachable through IncludeDeleted,
	// and restored by clearing the timestamp.
	_, total, err = sxaDAO.GetAll(ctx, nil, SpectrumXAttachmentFilterInput{
		SpectrumXAttachmentIDs: []uuid.UUID{created.ID},
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 0, total)

	deleted, total, err := sxaDAO.GetAll(ctx, nil, SpectrumXAttachmentFilterInput{
		SpectrumXAttachmentIDs: []uuid.UUID{created.ID},
		IncludeDeleted:         true,
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 1, total)
	require.Len(t, deleted, 1)
	require.NotNil(t, deleted[0].Deleted)

	restored, err := sxaDAO.Clear(ctx, nil, SpectrumXAttachmentClearInput{
		SpectrumXAttachmentID: created.ID,
		Deleted:               true,
	})
	require.NoError(t, err)
	assert.Nil(t, restored.Deleted)

	_, err = sxaDAO.Get(ctx, nil, created.ID, nil)
	assert.NoError(t, err, "an undeleted Attachment must be reachable again")
}
