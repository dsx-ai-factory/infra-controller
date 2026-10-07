// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestMachineInstanceType_ToRemoveAssociationRequestProto(t *testing.T) {
	mit := &MachineInstanceType{MachineID: "machine-1"}
	req := mit.ToRemoveAssociationRequestProto()
	assert.NotNil(t, req)
	assert.Equal(t, "machine-1", req.MachineId)
}

// reset the tables needed for MachineInstanceType tests
func testMachineInstanceTypeSetupSchema(t *testing.T, dbSession *db.Session) {
	testInstanceTypeSetupSchema(t, dbSession)
	// create Machine table
	err := dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
	assert.Nil(t, err)
	// create Machine Instance Type table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineInstanceType)(nil))
	assert.Nil(t, err)
}

func testMachineInstanceTypeBuildInstanceType(t *testing.T, dbSession *db.Session, s string) (*InfrastructureProvider, *Site, *InstanceType) {
	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, s+"IP")
	site := testInstanceTypeBuildSite(t, dbSession, ip, s+"Site")
	user := testInstanceTypeBuildUser(t, dbSession, s+"User")

	itsd := NewInstanceTypeDAO(dbSession)
	ins, err := itsd.Create(
		context.Background(), nil, InstanceTypeCreateInput{Name: s, DisplayName: cutil.GetPtr(s + " display name"), Description: cutil.GetPtr(s + " description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: nil, SiteID: &site.ID, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)

	assert.Nil(t, err)
	assert.NotNil(t, ins)

	return ip, site, ins
}

func TestMachineInstanceTypeSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineInstanceTypeSetupSchema(t, dbSession)
	ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	mc := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ins.ID), ins.ControllerMachineType)
	mitsd := NewMachineInstanceTypeDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		mis                []MachineInstanceType
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			mis: []MachineInstanceType{
				{
					MachineID: mc.ID, InstanceTypeID: ins.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "failure - foreign key violation on instance_type_id",
			mis: []MachineInstanceType{
				{
					MachineID: mc.ID, InstanceTypeID: uuid.New(),
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on machine_id",
			mis: []MachineInstanceType{
				{
					MachineID: uuid.NewString(), InstanceTypeID: ins.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.mis {
				mi, err := mitsd.Create(ctx, nil, MachineInstanceTypeCreateInput{
					MachineID:      i.MachineID,
					InstanceTypeID: i.InstanceTypeID,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, mi)
				}
				if mi != nil {
					_, erro := mi.GetIndentedJSON()
					assert.Nil(t, erro)
					assert.Equal(t, mi.MachineID, i.MachineID)
					assert.Equal(t, mi.InstanceTypeID, i.InstanceTypeID)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func testMachineInstanceTypePopulateDB(t *testing.T) (int, [5]string, []*InstanceType, []string, []*MachineInstanceType) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineInstanceTypeSetupSchema(t, dbSession)

	// Create InstanceType's
	instanceTypeNames := [5]string{"ex.sm.x86", "sm.x86", "med.x86", "big.x86", "large.x86"}
	const instanceTypeCount = len(instanceTypeNames)

	var instanceTypes []*InstanceType
	var machineIDs []string
	var machineInstanceTypes []*MachineInstanceType

	mitsd := NewMachineInstanceTypeDAO(dbSession)

	for i := 0; i < instanceTypeCount; i++ {
		ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, instanceTypeNames[i])
		instanceTypes = append(instanceTypes, ins)

		// Create Machine IDs
		for j := 0; j < instanceTypeCount; j++ {
			mc := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ins.ID), ins.ControllerMachineType)
			machineIDs = append(machineIDs, mc.ID)
		}
	}

	// Create Machine Instance Types
	for i := 0; i < instanceTypeCount*instanceTypeCount; i++ {
		mit, err := mitsd.Create(ctx, nil, MachineInstanceTypeCreateInput{
			MachineID:      machineIDs[i],
			InstanceTypeID: instanceTypes[i/instanceTypeCount].ID,
		})
		assert.Nil(t, err)
		machineInstanceTypes = append(machineInstanceTypes, mit)
	}

	return instanceTypeCount, instanceTypeNames, instanceTypes, machineIDs, machineInstanceTypes
}

func TestMachineInstanceTypeSQLDAO_GetByID(t *testing.T) {
	numInstances, instDescr, inst, machineID, machineInstanceTypes := testMachineInstanceTypePopulateDB(t)
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	mitsd := NewMachineInstanceTypeDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	// Verify GetByID with relation
	for i := 0; i < numInstances*numInstances; i++ {
		nv, err := mitsd.GetByID(ctx, nil, machineInstanceTypes[i].ID, []string{InstanceTypeRelationName})
		assert.Nil(t, err)
		if err == nil {
			_, erro := nv.GetIndentedJSON()
			assert.Nil(t, erro)
			assert.Equal(t, nv.MachineID, machineID[i])
			assert.Equal(t, nv.InstanceTypeID, inst[i/numInstances].ID)
			assert.NotNil(t, nv.InstanceType)
			assert.Equal(t, nv.InstanceType.Name, instDescr[i/numInstances])

			span := otrace.SpanFromContext(ctx)
			assert.True(t, span.SpanContext().IsValid())
		}

	}

	// Verify GetByID without relation
	for i := 0; i < numInstances*numInstances; i++ {
		nv, err := mitsd.GetByID(ctx, nil, machineInstanceTypes[i].ID, nil)
		assert.Nil(t, err)
		if err == nil {
			_, erro := nv.GetIndentedJSON()
			assert.Nil(t, erro)
			assert.Equal(t, nv.MachineID, machineID[i])
			assert.Equal(t, nv.InstanceTypeID, inst[i/numInstances].ID)
			assert.Nil(t, nv.InstanceType)

			span := otrace.SpanFromContext(ctx)
			assert.True(t, span.SpanContext().IsValid())
		}
	}

	// Verify GetByID fails
	for i := 0; i < numInstances; i++ {
		_, err := mitsd.GetByID(ctx, nil, uuid.New(), nil)
		assert.NotNil(t, err)
		assert.Equal(t, db.ErrDoesNotExist, err)

		span := otrace.SpanFromContext(ctx)
		assert.True(t, span.SpanContext().IsValid())
	}
}

func TestMachineInstanceTypeSQLDAO_GetAll(t *testing.T) {
	numInstances, _, inst, machineID, _ := testMachineInstanceTypePopulateDB(t)
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	mitsd := NewMachineInstanceTypeDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	// Verify GetAll by Instance Type ID
	for i := 0; i < numInstances; i++ {
		nv, _, err := mitsd.GetAll(ctx, nil, MachineInstanceTypeFilterInput{
			InstanceTypeIDs: []uuid.UUID{inst[i].ID},
		}, paginator.PageInput{}, []string{InstanceTypeRelationName})
		assert.Nil(t, err)
		assert.NotNil(t, nv)
		assert.Equal(t, len(nv), numInstances)
		for j := 0; j < numInstances; j++ {
			_, serr := nv[j].GetIndentedJSON()
			assert.Nil(t, serr)
			assert.Equal(t, nv[j].MachineID, machineID[i*numInstances+j])
			assert.Equal(t, nv[j].InstanceTypeID, inst[i].ID)
			assert.NotNil(t, nv[j].InstanceType)
		}

		span := otrace.SpanFromContext(ctx)
		assert.True(t, span.SpanContext().IsValid())
	}

	// Verify GetAll by Machine ID
	for i := 0; i < numInstances*numInstances; i++ {
		nv, _, err := mitsd.GetAll(ctx, nil, MachineInstanceTypeFilterInput{
			MachineID: &machineID[i],
		}, paginator.PageInput{}, nil)
		assert.Nil(t, err)
		assert.NotNil(t, nv)
		assert.Equal(t, len(nv), 1)
		for j := 0; j < len(nv); j++ {
			_, serr := nv[j].GetIndentedJSON()
			assert.Nil(t, serr)
			assert.Equal(t, nv[j].MachineID, machineID[i])
			assert.Equal(t, nv[j].InstanceTypeID, inst[i/numInstances].ID)
			assert.Nil(t, nv[j].InstanceType)
		}

		span := otrace.SpanFromContext(ctx)
		assert.True(t, span.SpanContext().IsValid())
	}

	// Verify GetAll, no filters
	nv, _, err := mitsd.GetAll(ctx, nil, MachineInstanceTypeFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(50)}, nil)
	assert.Nil(t, err)
	assert.NotNil(t, nv)
	assert.Equal(t, len(nv), numInstances*numInstances)
	for i := 0; i < numInstances*numInstances; i++ {
		_, erro := nv[i].GetIndentedJSON()
		assert.Nil(t, erro)
		assert.Equal(t, nv[i].MachineID, machineID[i])
		assert.Equal(t, nv[i].InstanceTypeID, inst[i/numInstances].ID)
		assert.Nil(t, nv[i].InstanceType)
	}

	// Verify GetAll, no filters, offset
	nv, total, err := mitsd.GetAll(ctx, nil, MachineInstanceTypeFilterInput{}, paginator.PageInput{Offset: cutil.GetPtr(10)}, nil)
	assert.Nil(t, err)
	assert.Equal(t, numInstances*numInstances-10, len(nv))
	assert.Equal(t, numInstances*numInstances, total)

	// Verify GetAll, no filters, limit
	nv, total, err = mitsd.GetAll(ctx, nil, MachineInstanceTypeFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(10)}, nil)
	assert.Nil(t, err)
	assert.Equal(t, 10, len(nv))
	assert.Equal(t, numInstances*numInstances, total)

	// Verify GetAll, no filters, orderBy
	orderBy := &paginator.OrderBy{
		Field: "created",
		Order: paginator.OrderDescending,
	}
	nv, total, err = mitsd.GetAll(ctx, nil, MachineInstanceTypeFilterInput{}, paginator.PageInput{OrderBy: orderBy}, nil)
	assert.Nil(t, err)
	assert.Equal(t, paginator.DefaultLimit, len(nv))
	assert.Equal(t, numInstances*numInstances, total)
}

func TestMachineInstanceTypeSQLDAO_Update(t *testing.T) {
	numInstances, _, inst, machineID, machineInstanceTypes := testMachineInstanceTypePopulateDB(t)
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	mitsd := NewMachineInstanceTypeDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	// 1st instance type [0-num] set to instanceType 2
	for i := 0; i < numInstances; i++ {
		mi, err := mitsd.Update(ctx, nil, MachineInstanceTypeUpdateInput{
			MachineInstanceTypeID: machineInstanceTypes[i].ID,
			InstanceTypeID:        &inst[1].ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, mi)
		assert.Equal(t, mi.MachineID, machineID[i])
		assert.Equal(t, mi.InstanceTypeID, inst[1].ID)

		if mi.Updated.String() == inst[i].Updated.String() {
			t.Errorf("mi.Updated = %v, want different value", mi.Updated)
		}

		span := otrace.SpanFromContext(ctx)
		assert.True(t, span.SpanContext().IsValid())
	}

	// 1st machine id [0-num] set to machine id 2
	for i := 0; i < numInstances; i++ {
		mi, err := mitsd.Update(ctx, nil, MachineInstanceTypeUpdateInput{
			MachineInstanceTypeID: machineInstanceTypes[i].ID,
			MachineID:             &machineID[numInstances+i],
		})
		assert.Nil(t, err)
		assert.NotNil(t, mi)
		assert.Equal(t, mi.MachineID, machineID[numInstances+i])
		assert.Equal(t, mi.InstanceTypeID, inst[1].ID)

		if mi.Updated.String() == inst[i].Updated.String() {
			t.Errorf("mi.Updated = %v, want different value", mi.Updated)
		}

		span := otrace.SpanFromContext(ctx)
		assert.True(t, span.SpanContext().IsValid())
	}

	// Set both machine id and instance type [0-num] to original
	for i := 0; i < numInstances; i++ {
		mi, err := mitsd.Update(ctx, nil, MachineInstanceTypeUpdateInput{
			MachineInstanceTypeID: machineInstanceTypes[i].ID,
			MachineID:             &machineID[i],
			InstanceTypeID:        &inst[0].ID,
		})
		assert.Nil(t, err)
		assert.NotNil(t, mi)
		assert.Equal(t, mi.MachineID, machineID[i])
		assert.Equal(t, mi.InstanceTypeID, inst[0].ID)
	}

	// Set to non-existent instanceType - foreign key violation
	for i := 0; i < numInstances; i++ {
		dummyUUID := uuid.New()
		mi, err := mitsd.Update(ctx, nil, MachineInstanceTypeUpdateInput{
			MachineInstanceTypeID: machineInstanceTypes[i].ID,
			InstanceTypeID:        &dummyUUID,
		})
		assert.NotNil(t, err)
		assert.Nil(t, mi)
	}
}

func TestMachineInstanceTypeSQLDAO_Delete(t *testing.T) {
	_, _, _, _, machineInstanceTypes := testMachineInstanceTypePopulateDB(t)
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	mitsd := NewMachineInstanceTypeDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		miID               uuid.UUID
		purge              bool
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "test deleting existing object success",
			miID:               machineInstanceTypes[1].ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "test deleting non-existent object success",
			miID:          uuid.New(),
			expectedError: false,
		},
		{
			desc:          "test deleting existing object success with purge",
			miID:          machineInstanceTypes[2].ID,
			purge:         true,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := mitsd.Delete(ctx, nil, tc.miID, tc.purge)

			if tc.expectedError {
				assert.Error(t, err)

				// Check that object was not deleted
				tmp, serr := mitsd.GetByID(ctx, nil, tc.miID, nil)
				assert.NoError(t, serr)
				assert.NotNil(t, tmp)
				return
			}

			var res MachineInstanceType

			if tc.purge {
				err = dbSession.DB.NewSelect().Model(&res).Where("mit.id = ?", tc.miID).WhereAllWithDeleted().Scan(ctx)
			} else {
				err = dbSession.DB.NewSelect().Model(&res).Where("mit.id = ?", tc.miID).Scan(ctx)
			}
			assert.ErrorIs(t, err, sql.ErrNoRows)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestMachineInstanceTypeSQLDAO_DeleteAllByInstanceTypeID(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx            context.Context
		tx             *db.Tx
		instanceTypeID uuid.UUID
		purge          bool
	}

	_, _, instanceTypes, _, _ := testMachineInstanceTypePopulateDB(t)

	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "can delete all objects by instance type id",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            ctx,
				tx:             nil,
				instanceTypeID: instanceTypes[0].ID,
			},
			verifyChildSpanner: true,
		},
		{
			name: "can delete all objects by instance type id with purge",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:            ctx,
				tx:             nil,
				instanceTypeID: instanceTypes[1].ID,
			},
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mitsd := NewMachineInstanceTypeDAO(tt.fields.dbSession)
			err := mitsd.DeleteAllByInstanceTypeID(tt.args.ctx, tt.args.tx, tt.args.instanceTypeID, tt.args.purge)

			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			// Check that all instances of the instance type have been deleted
			var res []MachineInstanceType

			if tt.args.purge {
				err = dbSession.DB.NewSelect().Model(&res).Where("mit.instance_type_id = ?", tt.args.instanceTypeID).WhereAllWithDeleted().Scan(ctx)

			} else {
				err = dbSession.DB.NewSelect().Model(&res).Where("mit.instance_type_id = ?", tt.args.instanceTypeID).Scan(ctx)
			}

			assert.NoError(t, err)
			assert.Len(t, res, 0)

			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
