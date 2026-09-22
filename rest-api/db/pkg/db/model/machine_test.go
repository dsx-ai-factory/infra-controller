// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"
)

// reset the tables needed for Machine tests
func testMachineSetupSchema(t *testing.T, dbSession *db.Session) {
	testInstanceTypeSetupSchema(t, dbSession)
	// create Machine table
	err := dbSession.DB.ResetModel(context.Background(), (*Machine)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*MachineCapability)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*MachineInterface)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*MachineInstanceType)(nil))
	assert.Nil(t, err)
}

func testMachineBuildMachine(t *testing.T, dbSession *db.Session, ip uuid.UUID, site uuid.UUID, instanceTypeID *uuid.UUID, controllerMachineType *string) *Machine {
	return testMachineBuildMachineWithID(t, dbSession, ip, site, instanceTypeID, controllerMachineType, uuid.NewString())
}

func testMachineBuildMachineWithID(t *testing.T, dbSession *db.Session, ip uuid.UUID, site uuid.UUID, instanceTypeID *uuid.UUID, controllerMachineType *string, id string) *Machine {
	defMacAddr := "00:1B:44:11:3A:B7"
	m := &Machine{
		ID:                       id,
		InfrastructureProviderID: ip,
		SiteID:                   site,
		InstanceTypeID:           instanceTypeID,
		ControllerMachineID:      uuid.New().String(),
		ControllerMachineType:    controllerMachineType,
		Vendor:                   cutil.GetPtr("test-vendor"),
		ProductName:              cutil.GetPtr("test-product"),
		SerialNumber:             cutil.GetPtr(uuid.NewString()),
		Metadata:                 nil,
		Health:                   nil,
		DefaultMacAddress:        &defMacAddr,
		Status:                   MachineStatusInitializing,
	}
	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func testMachineBuildMachineInstanceType(t *testing.T, dbSession *db.Session, machineID string, insttypeID uuid.UUID) *MachineInstanceType {
	m := &MachineInstanceType{
		ID:             uuid.New(),
		MachineID:      machineID,
		InstanceTypeID: insttypeID,
	}
	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func TestMachine_MatchesLabelSelector(t *testing.T) {
	tests := []struct {
		name    string
		machine *Machine
		filters map[string]string
		want    bool
	}{
		{
			name: "nil Machine",
		},
		{
			name:    "empty filters",
			machine: &Machine{},
			want:    true,
		},
		{
			name:    "exact match",
			machine: &Machine{Labels: map[string]string{"failure-domain": "fd-a"}},
			filters: map[string]string{"failure-domain": "fd-a"},
			want:    true,
		},
		{
			name:    "subset match",
			machine: &Machine{Labels: map[string]string{"failure-domain": "fd-a", "power-domain": "pd-2"}},
			filters: map[string]string{"failure-domain": "fd-a"},
			want:    true,
		},
		{
			name:    "missing key",
			machine: &Machine{Labels: map[string]string{"power-domain": "pd-2"}},
			filters: map[string]string{"failure-domain": "fd-a"},
		},
		{
			name:    "different value",
			machine: &Machine{Labels: map[string]string{"failure-domain": "fd-b"}},
			filters: map[string]string{"failure-domain": "fd-a"},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.want, test.machine.MatchesLabelSelector(test.filters))
		})
	}
}

func TestMachineSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)
	ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	defMacAddr := "00:1B:44:11:3A:B7"
	meta := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
	health := map[string]interface{}{"source": "aggregate-health", "alerts": [1]map[string]interface{}{
		{"id": "test-id", "target": "test-target", "message": "test-message", "classifications": [1]string{"test-classification"}},
	}}
	vendor := "testvendor"
	hostname := "testmachine"
	productName := "testproduct"
	serialNumber := uuid.NewString()
	maintenanceMessage := "testmaintenance"
	networkHealthMessage := "testnetworkhealth"
	labels := map[string]string{"key1": "value1", "key2": "value2"}

	msd := NewMachineDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []MachineCreateInput
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			inputs: []MachineCreateInput{
				{
					MachineID:                uuid.NewString(),
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					ControllerMachineID:      uuid.NewString(),
					InstanceTypeID:           &ins.ID,
					ControllerMachineType:    ins.ControllerMachineType,
					Vendor:                   &vendor,
					ProductName:              &productName,
					SerialNumber:             &serialNumber,
					Metadata:                 meta,
					IsInMaintenance:          true,
					MaintenanceMessage:       &maintenanceMessage,
					IsNetworkDegraded:        true,
					NetworkHealthMessage:     &networkHealthMessage,
					Health:                   health,
					Hostname:                 &hostname,
					DefaultMacAddress:        &defMacAddr,
					Labels:                   labels,
					Status:                   MachineStatusInitializing,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with nullable field",
			inputs: []MachineCreateInput{
				{
					MachineID:                uuid.NewString(),
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					ControllerMachineID:      uuid.NewString(),
					InstanceTypeID:           &ins.ID,
					ControllerMachineType:    ins.ControllerMachineType,
					ProductName:              nil,
					SerialNumber:             &serialNumber,
					Metadata:                 nil,
					IsInMaintenance:          true,
					MaintenanceMessage:       &maintenanceMessage,
					IsNetworkDegraded:        false,
					NetworkHealthMessage:     nil,
					Hostname:                 nil,
					DefaultMacAddress:        &defMacAddr,
					Labels:                   labels,
					Status:                   MachineStatusInitializing,
				},
				{
					MachineID:                uuid.NewString(),
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					InstanceTypeID:           nil,
					ControllerMachineID:      uuid.NewString(),
					ControllerMachineType:    nil,
					ProductName:              &productName,
					SerialNumber:             nil,
					Metadata:                 meta,
					IsInMaintenance:          false,
					MaintenanceMessage:       nil,
					IsNetworkDegraded:        true,
					NetworkHealthMessage:     &networkHealthMessage,
					Hostname:                 &hostname,
					DefaultMacAddress:        &defMacAddr,
					Labels:                   labels,
					Status:                   MachineStatusInitializing,
				},
				{
					MachineID:                uuid.NewString(),
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					InstanceTypeID:           &ins.ID,
					ControllerMachineID:      uuid.NewString(),
					ControllerMachineType:    ins.ControllerMachineType,
					Vendor:                   &vendor,
					ProductName:              nil,
					SerialNumber:             nil,
					IsInMaintenance:          false,
					MaintenanceMessage:       nil,
					IsNetworkDegraded:        false,
					NetworkHealthMessage:     nil,
					Metadata:                 meta,
					Hostname:                 nil,
					DefaultMacAddress:        nil,
					Labels:                   nil,
					Status:                   MachineStatusInitializing,
				},
			},
			expectError: false,
		},
		{
			desc: "failure - foreign key violation on infrastructure_provider_id",
			inputs: []MachineCreateInput{
				{
					MachineID:                uuid.NewString(),
					InfrastructureProviderID: uuid.New(),
					SiteID:                   site.ID,
					InstanceTypeID:           &ins.ID,
					ControllerMachineID:      uuid.NewString(),
					ControllerMachineType:    ins.ControllerMachineType,
					Vendor:                   &vendor,
					ProductName:              &productName,
					SerialNumber:             &serialNumber,
					Metadata:                 meta,
					Health:                   health,
					DefaultMacAddress:        &defMacAddr,
					Labels:                   labels,
					Status:                   MachineStatusInitializing,
				},
				{
					MachineID:                uuid.NewString(),
					InfrastructureProviderID: ip.ID,
					SiteID:                   uuid.New(),
					InstanceTypeID:           &ins.ID,
					ControllerMachineID:      uuid.NewString(),
					ControllerMachineType:    ins.ControllerMachineType,
					Vendor:                   &vendor,
					ProductName:              &productName,
					SerialNumber:             &serialNumber,
					Metadata:                 meta,
					Health:                   health,
					DefaultMacAddress:        &defMacAddr,
					Labels:                   labels,
					Status:                   MachineStatusInitializing,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				mc, err := msd.Create(ctx, nil, input)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, mc)
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}

				if err != nil {
					t.Logf("%s", err.Error())
					return
				}

				assert.Equal(t, input.Metadata, mc.Metadata, mc.Metadata)
			}
		})
	}
}

func testMachineSQLDAOCreateMachines(ctx context.Context, t *testing.T, dbSession *db.Session) (created []Machine) {
	var createInputs []MachineCreateInput
	{
		// Machine expected set 1, machine 1
		ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
		defaultMacAddress := "00:1B:44:11:3A:B7"
		hostname := "testmachine"
		vendor := "testvendor"
		productName := "testproductname"
		serialNumber := uuid.NewString()
		meta := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
		labels := map[string]string{"key1": "value1", "key2": "value2"}

		createInputs = append(createInputs, MachineCreateInput{
			MachineID:                uuid.NewString(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins.ID,
			ControllerMachineID:      uuid.New().String(),
			ControllerMachineType:    ins.ControllerMachineType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			DefaultMacAddress:        &defaultMacAddress,
			IsInMaintenance:          false,
			IsNetworkDegraded:        false,
			Metadata:                 meta,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   MachineStatusInitializing,
		})

		// Machine expected set 1, machine 2
		defaultMacAddress = "00:1B:44:11:3A:B8"
		createInputs = append(createInputs, MachineCreateInput{
			MachineID:                uuid.NewString(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins.ID,
			ControllerMachineID:      uuid.New().String(),
			ControllerMachineType:    ins.ControllerMachineType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			IsInMaintenance:          false,
			IsNetworkDegraded:        false,
			DefaultMacAddress:        &defaultMacAddress,
			Metadata:                 meta,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   MachineStatusInitializing,
		})

		// Machine expected set 2, machine 1
		ip, site, ins = testMachineInstanceTypeBuildInstanceType(t, dbSession, "med.x86")
		defaultMacAddress = "00:1B:44:11:3B:A7"
		meta = &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
		createInputs = append(createInputs, MachineCreateInput{
			MachineID:                uuid.NewString(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins.ID,
			ControllerMachineID:      uuid.New().String(),
			ControllerMachineType:    ins.ControllerMachineType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			IsInMaintenance:          false,
			IsNetworkDegraded:        false,
			DefaultMacAddress:        &defaultMacAddress,
			Metadata:                 meta,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   MachineStatusReady,
		})

		// Machine expected set 2, machine 2
		defaultMacAddress = "00:1B:44:11:3B:A8"
		createInputs = append(createInputs, MachineCreateInput{
			MachineID:                uuid.NewString(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins.ID,
			ControllerMachineID:      uuid.New().String(),
			ControllerMachineType:    ins.ControllerMachineType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			IsInMaintenance:          false,
			IsNetworkDegraded:        false,
			DefaultMacAddress:        &defaultMacAddress,
			Metadata:                 meta,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   MachineStatusReady,
		})
	}

	msd := NewMachineDAO(dbSession)

	// Machine created
	for _, input := range createInputs {
		mcCre, _ := msd.Create(ctx, nil, input)
		assert.NotNil(t, mcCre)
		created = append(created, *mcCre)
	}

	return
}

func TestMachineSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	mcsExp := testMachineSQLDAOCreateMachines(ctx, t, dbSession)
	msd := NewMachineDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		mc                 Machine
		expectError        bool
		expectedErrVal     error
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when Machine exists on [1]",
			mc:                 mcsExp[0],
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "GetById success when Machine exists on [2]",
			mc:          mcsExp[1],
			expectError: false,
		},
		{
			desc:           "GetById success when Machine exists and include relations",
			mc:             mcsExp[1],
			expectError:    false,
			paramRelations: []string{"Site", "InfrastructureProvider"},
		},
		{
			desc: "GetById success when Machine not found",
			mc: Machine{
				ID: uuid.NewString(),
			},
			expectError:    true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := msd.GetByID(ctx, nil, tc.mc.ID, tc.paramRelations, false)
			assert.Equal(t, tc.expectError, err != nil)
			if tc.expectError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.mc.ID, tmp.ID)
				assert.EqualValues(t, tc.mc.InfrastructureProviderID, tmp.InfrastructureProviderID)
				assert.EqualValues(t, tc.mc.SiteID, tmp.SiteID)
				assert.EqualValues(t, tc.mc.InstanceTypeID, tmp.InstanceTypeID)
				assert.EqualValues(t, tc.mc.ControllerMachineType, tmp.ControllerMachineType)
				assert.EqualValues(t, tc.mc.Metadata, tmp.Metadata)
				assert.EqualValues(t, tc.mc.DefaultMacAddress, tmp.DefaultMacAddress)
				assert.EqualValues(t, tc.mc.Hostname, tmp.Hostname)
				t.Logf("Should be %v and is %v\n", tc.mc.Status, tmp.Status)
				assert.EqualValues(t, tc.mc.Status, tmp.Status)
				assert.EqualValues(t, tc.mc.Labels, tmp.Labels)

				if len(tc.paramRelations) > 0 {
					assert.EqualValues(t, tc.mc.SiteID, tmp.Site.ID)
					assert.EqualValues(t, tc.mc.InfrastructureProviderID, tmp.InfrastructureProvider.ID)
				}
			} else {
				t.Logf("%s", err.Error())
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestMachineSQLDAO_GetCountByStatus(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	var createInputs []MachineCreateInput
	{
		// Machine expected set 1, machine 1
		ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
		defaultMacAddress := "00:1B:44:11:3A:B7"
		hostname := "testhostname"
		vendor := "testvendor"
		productName := "testproductname"
		serialNumber := uuid.NewString()
		meta := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
		labels := map[string]string{"key1": "value1", "key2": "value2"}

		createInputs = append(createInputs, MachineCreateInput{
			MachineID:                uuid.NewString(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins.ID,
			ControllerMachineID:      uuid.New().String(),
			ControllerMachineType:    ins.ControllerMachineType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			DefaultMacAddress:        &defaultMacAddress,
			Metadata:                 meta,
			IsInMaintenance:          false,
			IsNetworkDegraded:        false,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   MachineStatusInitializing,
		})

		// Machine expected set 1, machine 2
		defaultMacAddress = "00:1B:44:11:3A:B8"
		createInputs = append(createInputs, MachineCreateInput{
			MachineID:                uuid.NewString(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins.ID,
			ControllerMachineID:      uuid.New().String(),
			ControllerMachineType:    ins.ControllerMachineType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			DefaultMacAddress:        &defaultMacAddress,
			Metadata:                 meta,
			IsInMaintenance:          false,
			IsNetworkDegraded:        false,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   MachineStatusInitializing,
		})
	}

	msd := NewMachineDAO(dbSession)
	// Machine created
	var created []Machine
	for _, input := range createInputs {
		mcCre, _ := msd.Create(ctx, nil, input)
		assert.NotNil(t, mcCre)
		created = append(created, *mcCre)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		id                 uuid.UUID
		fields             fields
		args               args
		wantErr            error
		wantEmpty          bool
		wantCount          int
		wantStatusMap      map[string]int
		reqIP              *uuid.UUID
		reqSite            *uuid.UUID
		reqInstanceType    *uuid.UUID
		verifyChildSpanner bool
	}{
		{
			name: "get machine status count by infrastructure provider with machine returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 2,
			wantStatusMap: map[string]int{
				MachineStatusUnknown:         0,
				MachineStatusReady:           0,
				MachineStatusInUse:           0,
				MachineStatusDecommissioning: 0,
				MachineStatusDecommissioned:  0,
				MachineStatusError:           0,
				MachineStatusReset:           0,
				MachineStatusMaintenance:     0,
				MachineStatusInitializing:    2,
				"total":                      2,
			},
			reqIP:              cutil.GetPtr(created[1].InfrastructureProviderID),
			verifyChildSpanner: true,
		},
		{
			name: "get machine status count by unexisted infrastructure provider with no machine returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: true,
			reqIP:     cutil.GetPtr(uuid.New()),
		},
		{
			name: "get machine status count with no filter machine returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantCount: 2,
			wantStatusMap: map[string]int{
				MachineStatusUnknown:         0,
				MachineStatusReady:           0,
				MachineStatusInUse:           0,
				MachineStatusDecommissioning: 0,
				MachineStatusDecommissioned:  0,
				MachineStatusError:           0,
				MachineStatusReset:           0,
				MachineStatusMaintenance:     0,
				MachineStatusInitializing:    2,
				"total":                      2,
			},
			wantErr:   nil,
			wantEmpty: false,
		},
		{
			name: "get machine status count by instance type  with machine returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 2,
			wantStatusMap: map[string]int{
				MachineStatusUnknown:         0,
				MachineStatusReady:           0,
				MachineStatusInUse:           0,
				MachineStatusDecommissioning: 0,
				MachineStatusDecommissioned:  0,
				MachineStatusError:           0,
				MachineStatusReset:           0,
				MachineStatusMaintenance:     0,
				MachineStatusInitializing:    2,
				"total":                      2,
			},
			reqInstanceType: created[1].InstanceTypeID,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			msd := MachineSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := msd.GetCountByStatus(tt.args.ctx, nil, tt.reqIP, tt.reqSite, tt.reqInstanceType)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if tt.wantEmpty {
				assert.EqualValues(t, got["total"], 0)
			}
			if err == nil && !tt.wantEmpty {
				assert.EqualValues(t, tt.wantStatusMap, got)
				if len(got) > 0 {
					assert.EqualValues(t, got[MachineStatusInitializing], 2)
					assert.EqualValues(t, got["total"], tt.wantCount)
				}
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestMachineSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	msd := NewMachineDAO(dbSession)

	totalCount := 30

	// Machine Set 1
	ms1 := []Machine{}
	mist1 := []MachineInstanceType{}

	ip, site, ins1 := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	metai1 := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
	for i := 0; i < totalCount/2; i++ {
		macAddress := testGenerateMacAddress(t)
		st := MachineStatusInitializing
		// Make at least one with READY status
		if i == 0 {
			st = MachineStatusReady
		}
		hostname := fmt.Sprintf("testhostname-%d", i)
		vendor := "testvendor"
		productName := "testproductname"
		serialNumber := uuid.NewString()
		maintenanceMessage := "testmaintenance"
		hwSkuDeviceType := "testhwskudevicetype1"
		labels := map[string]string{"key1": "value1", "key2": "value2"}

		mid := uuid.NewString()
		m, _ := msd.Create(ctx, nil, MachineCreateInput{
			MachineID:                mid,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins1.ID,
			ControllerMachineID:      mid,
			ControllerMachineType:    ins1.ControllerMachineType,
			HwSkuDeviceType:          &hwSkuDeviceType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			Metadata:                 metai1,
			IsInMaintenance:          true,
			MaintenanceMessage:       &maintenanceMessage,
			IsNetworkDegraded:        false,
			DefaultMacAddress:        &macAddress,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   st,
		})

		ms1 = append(ms1, *m)

		// Create Machine Instance Type
		mist := testMachineBuildMachineInstanceType(t, dbSession, m.ID, ins1.ID)
		assert.NotNil(t, mist)
		mist1 = append(mist1, *mist)
		assert.NotNil(t, mist1)
	}

	// Machine Capabilities
	mcd := NewMachineCapabilityDAO(dbSession)
	_, err := mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[0].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeInfiniBand, Name: "MT28908 Family [ConnectX-6]", Frequency: cutil.GetPtr("3 GHz"), Capacity: cutil.GetPtr("12 TB"), Vendor: cutil.GetPtr("Mellanox Technologies"), Count: cutil.GetPtr(2)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[0].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeInfiniBand, Name: "MT2910 Family [ConnectX-7]", Frequency: cutil.GetPtr("6 GHz"), Capacity: cutil.GetPtr("20 TB"), Vendor: cutil.GetPtr("Mellanox Technologies"), Count: cutil.GetPtr(8)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[1].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeStorage, Name: "Dell Ent NVMe CM6 RI 1.92TB", Capacity: cutil.GetPtr("1.92TB"), Vendor: cutil.GetPtr("Test Vendor"), Count: cutil.GetPtr(2)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[1].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeInfiniBand, Name: "MT2910 Family [ConnectX-7]", Frequency: cutil.GetPtr("6 GHz"), Capacity: cutil.GetPtr("20 TB"), Vendor: cutil.GetPtr("Mellanox Technologies"), Count: cutil.GetPtr(8)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[1].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeCPU, Name: "CPU Capability", Frequency: cutil.GetPtr("4 GHz"), Capacity: cutil.GetPtr("10 TB"), Vendor: cutil.GetPtr("Test Vendor"), Count: cutil.GetPtr(1)})
	assert.NoError(t, err)

	// Add a capability to a third machine, but we'll then delete the capability
	// so that we can be sure we only ever match on non-deleted capabilities.
	mcToDelete, err := mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[2].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeInfiniBand, Name: "MT2910 Family [ConnectX-7]", Frequency: cutil.GetPtr("6 GHz"), Capacity: cutil.GetPtr("20 TB"), Vendor: cutil.GetPtr("Mellanox Technologies"), Count: cutil.GetPtr(8)})
	assert.NoError(t, err)

	// Delete the capability
	mcd.DeleteByID(ctx, nil, mcToDelete.ID, false)

	// Machine Set 2
	ms2 := []Machine{}
	ip, site, ins2 := testMachineInstanceTypeBuildInstanceType(t, dbSession, "med.x86")
	metai2 := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}

	for i := 0; i < totalCount/2; i++ {
		// Create Machine Instance Type for first 2 Machines in this set
		var itID *uuid.UUID
		if i < 2 {
			itID = &ins2.ID
		}

		macAddress := testGenerateMacAddress(t)
		hostname := fmt.Sprintf("testhostname-%d", i)
		vendor := "testvendor"
		productName := "testproductname"
		hwSkuDeviceType := "testhwskudevicetype2"
		serialNumber := uuid.NewString()
		networkHealthMessage := "testnetworkhealth"
		labels := map[string]string{"key1": "value1", "key2": "value2"}

		mid := uuid.NewString()
		m, _ := msd.Create(ctx, nil, MachineCreateInput{
			MachineID:                mid,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           itID,
			ControllerMachineID:      mid,
			ControllerMachineType:    ins2.ControllerMachineType,
			HwSkuDeviceType:          &hwSkuDeviceType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			Metadata:                 metai2,
			IsInMaintenance:          false,
			IsNetworkDegraded:        true,
			NetworkHealthMessage:     &networkHealthMessage,
			DefaultMacAddress:        &macAddress,
			Hostname:                 &hostname,
			Labels:                   labels,
			Status:                   MachineStatusInitializing,
		})

		if i < 2 {
			// Create Machine Instance Type
			mist := testMachineBuildMachineInstanceType(t, dbSession, m.ID, ins2.ID)
			assert.NotNil(t, mist)
		}
		ms2 = append(ms2, *m)
	}

	// Change assignment of one of the Machines in Set 1
	ms1[0].IsAssigned = true
	_, err = msd.Update(ctx, nil, MachineUpdateInput{
		MachineID:  ms1[0].ID,
		IsAssigned: cutil.GetPtr(true),
	})
	assert.Nil(t, err)

	// Give one Machine a distinct placement label for exact label-filter tests.
	ms1[1].Labels["failure-domain"] = "fd-a"
	_, err = msd.Update(ctx, nil, MachineUpdateInput{
		MachineID: ms1[1].ID,
		Labels:    ms1[1].Labels,
	})
	assert.NoError(t, err)

	// set one of machines in set 2 to be missing on site
	ms2[0].IsMissingOnSite = true
	_, err = msd.Update(ctx, nil, MachineUpdateInput{
		MachineID:       ms2[2].ID,
		Status:          cutil.GetPtr(MachineStatusError),
		IsMissingOnSite: cutil.GetPtr(true),
	})
	assert.Nil(t, err)

	dummyID := uuid.New()

	controllerMachineID, _ := uuid.Parse(ms1[0].ControllerMachineID)
	assert.NotNil(t, controllerMachineID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             MachineFilterInput
		pageInput          paginator.PageInput
		firstEntry         *Machine
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns all objects",
			expectedCount:      paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:           "GetAll with relations returns all objects",
			expectedCount:  paginator.DefaultLimit,
			expectedTotal:  &totalCount,
			expectedError:  false,
			paramRelations: []string{"Site", "InfrastructureProvider"},
		},
		{
			desc: "GetAll with ip filters returns objects",
			filter: MachineFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ms1[0].InfrastructureProviderID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with site filters returns objects",
			filter: MachineFilterInput{
				SiteIDs: []uuid.UUID{ms2[0].SiteID},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "GetAll with is assigned filters returns objects",
			filter: MachineFilterInput{
				SiteIDs:    []uuid.UUID{ms1[0].SiteID},
				IsAssigned: cutil.GetPtr(true),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with exact Machine label selector returns matching objects",
			filter: MachineFilterInput{
				Labels: map[string]string{
					"failure-domain": "fd-a",
					"key1":           "value1",
				},
			},
			expectedCount: 1,
			firstEntry:    &ms1[1],
			expectedError: false,
		},
		{
			desc: "GetAll with a non-matching Machine label value returns no objects",
			filter: MachineFilterInput{
				Labels: map[string]string{"failure-domain": "missing"},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with unknown ip filters returns no objects",
			filter: MachineFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{dummyID},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with hostname filters returns objects",
			filter: MachineFilterInput{
				SiteIDs:  []uuid.UUID{ms1[0].SiteID},
				Hostname: cutil.GetPtr("testhostname-1"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with infiniband capabilitytype filters returns objects",
			filter: MachineFilterInput{
				CapabilityType: db.GetTypedStrPtr(MachineCapabilityTypeInfiniBand),
			},
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderAscending,
				},
			},
			expectedCount: 2,
			firstEntry:    &ms1[0],
			expectedError: false,
		},
		{
			desc: "GetAll with cpu capabilitytype filters returns objects",
			filter: MachineFilterInput{
				CapabilityType: db.GetTypedStrPtr(MachineCapabilityTypeCPU),
			},
			expectedCount: 1,
			firstEntry:    &ms1[1],
			expectedError: false,
		},
		{
			desc: "GetAll with capability name filter",
			filter: MachineFilterInput{
				CapabilityNames: []string{"MT2910 Family [ConnectX-7]"},
			},
			expectedCount: 2,
			firstEntry:    &ms1[0],
			expectedError: false,
		},
		{
			desc: "GetAll with multiple capability names filter",
			filter: MachineFilterInput{
				CapabilityNames: []string{"MT2910 Family [ConnectX-7]", "MT28908 Family [ConnectX-6]"},
			},
			expectedCount: 2,
			firstEntry:    &ms1[0],
			expectedError: false,
		},
		{
			desc: "GetAll with limit returns objects",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(5),
			},
			expectedCount: 5,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns objects",
			filter: MachineFilterInput{
				SiteIDs: []uuid.UUID{ms2[0].SiteID},
			},
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(5),
			},
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with order by returns objects",
			filter: MachineFilterInput{
				SiteIDs: []uuid.UUID{ms2[0].SiteID},
			},
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderDescending,
				},
			},
			firstEntry:    &ms2[14], // Last entry would have the highest created time
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with status filters returns objects",
			filter: MachineFilterInput{
				Statuses: []string{MachineStatusInitializing},
			},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount - 2),
			expectedError: false,
		},
		{
			desc: "GetAll with ids filters returns objects",
			filter: MachineFilterInput{
				MachineIDs: []string{
					ms2[0].ID,
					ms2[1].ID,
					ms2[2].ID,
				},
			},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc: "GetAll with status and ids filters returns objects",
			filter: MachineFilterInput{
				Statuses: []string{MachineStatusInitializing},
				MachineIDs: []string{
					ms2[0].ID,
					ms2[1].ID,
				},
			},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc: "GetAll with status search query filters returns objects",
			filter: MachineFilterInput{
				SearchQuery: cutil.GetPtr(MachineStatusInitializing),
			},
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "id",
					Order: paginator.OrderDescending,
				},
			},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount - 2),
			expectedError: false,
		},
		{
			desc: "GetAll with machine id search query filters returns objects",
			filter: MachineFilterInput{
				SearchQuery: cutil.GetPtr(ms2[0].ID),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with machine vendor search query filters returns objects",
			filter: MachineFilterInput{
				SearchQuery: cutil.GetPtr("testvendor"),
			},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
		{
			desc: "GetAll with machine hostname search query filters returns objects",
			filter: MachineFilterInput{
				Hostname: cutil.GetPtr("testhostname-0"),
			},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc: "GetAll with hasInstanceType set to true returns all objects",
			filter: MachineFilterInput{
				HasInstanceType: cutil.GetPtr(true),
			},
			expectedCount: totalCount/2 + 2,
			expectedTotal: cutil.GetPtr(totalCount/2 + 2),
			expectedError: false,
		},
		{
			desc: "GetAll with hasInstanceType false returns all objects",
			filter: MachineFilterInput{
				HasInstanceType: cutil.GetPtr(false),
			},
			expectedCount: totalCount/2 - 2,
			expectedTotal: cutil.GetPtr(totalCount/2 - 2),
			expectedError: false,
		},
		{
			desc: "GetAll with hasInstanceType true and status Ready returns all objects",
			filter: MachineFilterInput{
				Statuses:        []string{MachineStatusReady},
				HasInstanceType: cutil.GetPtr(true),
			},
			expectedCount: 1,
			expectedTotal: cutil.GetPtr(1),
			expectedError: false,
		},
		{
			desc: "GetAll with hasInstanceType false and status Ready returns all objects",
			filter: MachineFilterInput{
				Statuses:        []string{MachineStatusReady},
				HasInstanceType: cutil.GetPtr(false),
			},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},
		{
			desc: "GetAll with instanceTypeID filter returns all objects",
			filter: MachineFilterInput{
				InstanceTypeIDs: []uuid.UUID{ins1.ID},
			},
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc: "GetAll with controllerMachineID filter returns all objects",
			filter: MachineFilterInput{
				ControllerMachineID: cutil.GetPtr(controllerMachineID.String()),
			},
			expectedCount: 1,
			expectedTotal: cutil.GetPtr(1),
			expectedError: false,
		},
		{
			desc: "GetAll with status search query filter for labels returns objects",
			filter: MachineFilterInput{
				SearchQuery: cutil.GetPtr("key1"),
			},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
		{
			desc: "filter with HwSkuDeviceTypes 1",
			filter: MachineFilterInput{
				HwSkuDeviceTypes: []string{"testhwskudevicetype1"},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "filter with HwSkuDeviceTypes 2",
			filter: MachineFilterInput{
				HwSkuDeviceTypes: []string{"testhwskudevicetype2"},
			},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc: "filter with HwSkuDeviceTypes 3",
			filter: MachineFilterInput{
				HwSkuDeviceTypes: []string{"testhwskudevicetype3"},
			},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc: "GetAll with isMissingOnSite filter returns objects",
			filter: MachineFilterInput{
				IsMissingOnSite: cutil.GetPtr(true),
			},
			expectedCount: 1,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := msd.GetAll(ctx, nil, tc.filter, tc.pageInput, tc.paramRelations)
			if err != nil {
				t.Logf("%s", err.Error())
			}

			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
				if len(tc.paramRelations) > 0 {
					assert.NotNil(t, got[0].Site)
					assert.NotNil(t, got[0].InfrastructureProvider)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.ID, got[0].ID)
			}

			if tc.filter.InstanceTypeIDs != nil {
				if len(got) > 0 {
					assert.Equal(t, tc.filter.InstanceTypeIDs[0], *got[0].InstanceTypeID)
				}
			}
			if tc.filter.ControllerMachineID != nil {
				if len(got) > 0 {
					assert.Equal(t, *tc.filter.ControllerMachineID, got[0].ControllerMachineID)
				}
			}
			if tc.filter.Hostname != nil {
				if len(got) > 0 {
					assert.Equal(t, tc.filter.Hostname, got[0].Hostname)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}

	t.Run("includes soft-deleted Machines when requested", func(t *testing.T) {
		machineID := ms1[0].ID
		err := msd.Delete(ctx, nil, machineID, false)
		require.NoError(t, err)

		active, activeTotal, err := msd.GetAll(
			ctx,
			nil,
			MachineFilterInput{MachineIDs: []string{machineID}},
			paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		assert.Empty(t, active)
		assert.Zero(t, activeTotal)

		withDeleted, totalWithDeleted, err := msd.GetAll(
			ctx,
			nil,
			MachineFilterInput{MachineIDs: []string{machineID}, IncludeDeleted: true},
			paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, withDeleted, 1)
		assert.Equal(t, 1, totalWithDeleted)
		assert.NotNil(t, withDeleted[0].Deleted)
	})
}

func TestMachineSQLDAO_GetDistinctLabelKeys(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineInstanceTypeSetupSchema(t, dbSession)
	dao := NewMachineDAO(dbSession)
	ip, site, instanceType := testMachineInstanceTypeBuildInstanceType(t, dbSession, "label-keys")

	for i, labels := range []map[string]string{
		{"failure-domain": "fd-b", "nvidia.com/failure-domain": "fd-b"},
		{"failure-domain": "fd-a"},
		{"power-domain": "pd-1"},
		nil,
	} {
		machineID := fmt.Sprintf("label-keys-%d", i)
		_, err := dao.Create(ctx, nil, MachineCreateInput{
			MachineID: machineID, InfrastructureProviderID: ip.ID, SiteID: site.ID,
			InstanceTypeID: &instanceType.ID, ControllerMachineID: machineID,
			ControllerMachineType: instanceType.ControllerMachineType,
			Labels:                labels, Status: MachineStatusInitializing,
		})
		require.NoError(t, err)
	}

	tests := []struct {
		name      string
		filter    MachineFilterInput
		page      paginator.PageInput
		want      []string
		wantTotal int
	}{
		{name: "distinct sorted keys", want: []string{"failure-domain", "nvidia.com/failure-domain", "power-domain"}, wantTotal: 3},
		{
			name: "descending second result",
			page: paginator.PageInput{
				Offset: cutil.GetPtr(1), Limit: cutil.GetPtr(1),
				OrderBy: &paginator.OrderBy{Field: LabelKeyOrderByDefault, Order: paginator.OrderDescending},
			},
			want: []string{"nvidia.com/failure-domain"}, wantTotal: 3,
		},
		{name: "empty site scope", filter: MachineFilterInput{SiteIDs: []uuid.UUID{}}, want: []string{}, wantTotal: 0},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, total, err := dao.GetDistinctLabelKeys(ctx, nil, tt.filter, tt.page)
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
			assert.Equal(t, tt.wantTotal, total)
		})
	}
}

func TestMachineSQLDAO_GetDistinctLabelValues(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineInstanceTypeSetupSchema(t, dbSession)
	dao := NewMachineDAO(dbSession)
	ip, site, instanceType := testMachineInstanceTypeBuildInstanceType(t, dbSession, "label-values")

	for i, labels := range []map[string]string{
		{"failure-domain": "fd-b", "nvidia.com/failure-domain": "fd-b"},
		{"failure-domain": "fd-a"},
		{"failure-domain": "fd-b", "power-domain": "pd-1"},
		nil,
	} {
		machineID := fmt.Sprintf("label-values-%d", i)
		_, err := dao.Create(ctx, nil, MachineCreateInput{
			MachineID: machineID, InfrastructureProviderID: ip.ID, SiteID: site.ID,
			InstanceTypeID: &instanceType.ID, ControllerMachineID: machineID,
			ControllerMachineType: instanceType.ControllerMachineType,
			Labels:                labels, Status: MachineStatusInitializing,
		})
		assert.NoError(t, err)
	}

	tests := []struct {
		name      string
		labelKey  string
		filter    MachineFilterInput
		page      paginator.PageInput
		want      []string
		wantTotal int
	}{
		{name: "distinct sorted values", labelKey: "failure-domain", want: []string{"fd-a", "fd-b"}, wantTotal: 2},
		{name: "different key", labelKey: "power-domain", want: []string{"pd-1"}, wantTotal: 1},
		{name: "missing key", labelKey: "missing", want: []string{}, wantTotal: 0},
		{name: "slash in key", labelKey: "nvidia.com/failure-domain", want: []string{"fd-b"}, wantTotal: 1},
		{name: "provider scope", labelKey: "failure-domain", filter: MachineFilterInput{InfrastructureProviderIDs: []uuid.UUID{ip.ID}}, want: []string{"fd-a", "fd-b"}, wantTotal: 2},
		{
			name: "descending second result", labelKey: "failure-domain",
			page: paginator.PageInput{
				Offset: cutil.GetPtr(1), Limit: cutil.GetPtr(1),
				OrderBy: &paginator.OrderBy{Field: LabelValueOrderByDefault, Order: paginator.OrderDescending},
			},
			want: []string{"fd-a"}, wantTotal: 2,
		},
		{name: "empty site scope", labelKey: "failure-domain", filter: MachineFilterInput{SiteIDs: []uuid.UUID{}}, want: []string{}, wantTotal: 0},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, total, err := dao.GetDistinctLabelValues(ctx, nil, tt.labelKey, tt.filter, tt.page)
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
			assert.Equal(t, tt.wantTotal, total)
		})
	}
}

func TestMachineSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	mcsExp := testMachineSQLDAOCreateMachines(ctx, t, dbSession)
	msd := NewMachineDAO(dbSession)
	assert.NotNil(t, msd)
	dummyID := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		input              MachineUpdateInput
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc: "Update ip metadata and status",
			input: MachineUpdateInput{
				MachineID:                mcsExp[1].ID,
				InfrastructureProviderID: &mcsExp[2].InfrastructureProviderID,
				Metadata:                 mcsExp[2].Metadata,
				Status:                   &mcsExp[2].Status,
			},
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "Update vendor, product name and serial number",
			input: MachineUpdateInput{
				MachineID:                mcsExp[1].ID,
				InfrastructureProviderID: &mcsExp[2].InfrastructureProviderID,
				Vendor:                   mcsExp[2].Vendor,
				ProductName:              mcsExp[2].ProductName,
				SerialNumber:             mcsExp[2].SerialNumber,
			},
			expectedError:      false,
			verifyChildSpanner: true,
		}, {
			desc: "Update site ControllerMachineType n DefaultMacAddress",
			input: MachineUpdateInput{
				MachineID:             mcsExp[1].ID,
				SiteID:                &mcsExp[2].SiteID,
				ControllerMachineType: mcsExp[2].ControllerMachineType,
				DefaultMacAddress:     mcsExp[2].DefaultMacAddress,
			},
			expectedError: false,
		},
		{
			desc: "Update ControllerMachineID",
			input: MachineUpdateInput{
				MachineID:           mcsExp[1].ID,
				ControllerMachineID: &mcsExp[2].ControllerMachineID,
			},
			expectedError: false,
		},
		{
			desc: "Update unknown site",
			input: MachineUpdateInput{
				MachineID: mcsExp[1].ID,
				SiteID:    &dummyID,
			},
			expectedError: true,
		},
		{
			desc: "Update is assigned",
			input: MachineUpdateInput{
				MachineID:  mcsExp[1].ID,
				IsAssigned: cutil.GetPtr(true),
			},
			expectedError: false,
		},
		{
			desc: "Update instance type",
			input: MachineUpdateInput{
				MachineID:      mcsExp[1].ID,
				InstanceTypeID: mcsExp[2].InstanceTypeID,
			},
			expectedError: false,
		},
		{
			desc: "Update hostname",
			input: MachineUpdateInput{
				MachineID:      mcsExp[1].ID,
				InstanceTypeID: mcsExp[2].InstanceTypeID,
				Hostname:       cutil.GetPtr("testmachine"),
			},
			expectedError: false,
		},
		{
			desc: "Update isMissingOnSite",
			input: MachineUpdateInput{
				MachineID:       mcsExp[1].ID,
				InstanceTypeID:  mcsExp[2].InstanceTypeID,
				IsMissingOnSite: cutil.GetPtr(true),
			},
			expectedError: false,
		},
		{
			desc: "Update maintenance & network health attributes",
			input: MachineUpdateInput{
				MachineID:            mcsExp[1].ID,
				InstanceTypeID:       mcsExp[2].InstanceTypeID,
				IsInMaintenance:      cutil.GetPtr(true),
				MaintenanceMessage:   cutil.GetPtr("test maintenance message"),
				IsNetworkDegraded:    cutil.GetPtr(true),
				NetworkHealthMessage: cutil.GetPtr("test network health message"),
			},
			expectedError: false,
		},
		{
			desc: "Update labels",
			input: MachineUpdateInput{
				MachineID: mcsExp[1].ID,
				Labels: map[string]string{
					"key1": "value1",
					"key3": "value3",
				},
			},
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := msd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if err != nil {
				t.Logf("%s", err.Error())
			}
			if !tc.expectedError {
				assert.Nil(t, err)
				assert.NotNil(t, got)
				if tc.input.InfrastructureProviderID != nil {
					assert.Equal(t, *tc.input.InfrastructureProviderID, got.InfrastructureProviderID)
				}
				if tc.input.SiteID != nil {
					assert.Equal(t, *tc.input.SiteID, got.SiteID)
				}
				if tc.input.InstanceTypeID != nil {
					assert.Equal(t, tc.input.InstanceTypeID, got.InstanceTypeID)
				}
				if tc.input.ControllerMachineID != nil {
					assert.Equal(t, *tc.input.ControllerMachineID, got.ControllerMachineID)
				}
				if tc.input.ControllerMachineType != nil {
					assert.Equal(t, *tc.input.ControllerMachineType, *got.ControllerMachineType)
				}
				if tc.input.Vendor != nil {
					assert.Equal(t, *tc.input.Vendor, *got.Vendor)
				}
				if tc.input.ProductName != nil {
					assert.Equal(t, *tc.input.ProductName, *got.ProductName)
				}
				if tc.input.SerialNumber != nil {
					assert.Equal(t, *tc.input.SerialNumber, *got.SerialNumber)
				}
				if tc.input.Metadata != nil {
					assert.Equal(t, tc.input.Metadata, got.Metadata)
				}
				if tc.input.IsInMaintenance != nil {
					assert.Equal(t, *tc.input.IsInMaintenance, got.IsInMaintenance)
				}
				if tc.input.MaintenanceMessage != nil {
					assert.Equal(t, *tc.input.MaintenanceMessage, *got.MaintenanceMessage)
				}
				if tc.input.IsNetworkDegraded != nil {
					assert.Equal(t, *tc.input.IsNetworkDegraded, got.IsNetworkDegraded)
				}
				if tc.input.NetworkHealthMessage != nil {
					assert.Equal(t, *tc.input.NetworkHealthMessage, *got.NetworkHealthMessage)
				}
				if tc.input.DefaultMacAddress != nil {
					assert.Equal(t, *tc.input.DefaultMacAddress, *got.DefaultMacAddress)
				}
				if tc.input.Hostname != nil {
					assert.Equal(t, *tc.input.Hostname, *got.Hostname)
				}
				if tc.input.IsAssigned != nil {
					assert.Equal(t, *tc.input.IsAssigned, got.IsAssigned)
				}
				if tc.input.Labels != nil {
					assert.Equal(t, tc.input.Labels, got.Labels)
				}
				if tc.input.Status != nil {
					assert.Equal(t, *tc.input.Status, got.Status)
				}
				if tc.input.IsMissingOnSite != nil {
					assert.Equal(t, *tc.input.IsMissingOnSite, got.IsMissingOnSite)
				}

				if got.Updated.String() == mcsExp[1].Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
				}

				if tc.input.Metadata != nil {
					assert.Equal(t, tc.input.Metadata, got.Metadata, got.Metadata)
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}
			}
		})
	}
}

func TestMachineSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	mcsExp := testMachineSQLDAOCreateMachines(ctx, t, dbSession)
	msd := NewMachineDAO(dbSession)
	assert.NotNil(t, msd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		m                  Machine
		input              MachineClearInput
		expectedUpdate     bool
		verifyChildSpanner bool
	}{
		{
			desc: "can clear InstanceTypeID",
			m:    mcsExp[0],
			input: MachineClearInput{
				InstanceTypeID: true,
			},
			expectedUpdate:     true,
			verifyChildSpanner: true,
		},
		{
			desc: "can clear ControllerMachineType",
			m:    mcsExp[0],
			input: MachineClearInput{
				ControllerMachineType: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear Vendor",
			m:    mcsExp[0],
			input: MachineClearInput{
				Vendor: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear ProductName",
			m:    mcsExp[0],
			input: MachineClearInput{
				ProductName: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear Product Serial Number",
			m:    mcsExp[0],
			input: MachineClearInput{
				ControllerMachineType: true,
				SerialNumber:          true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear Metadata",
			m:    mcsExp[1],
			input: MachineClearInput{
				Metadata: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear ControllerMachineType n DefaultMacAddress",
			m:    mcsExp[2],
			input: MachineClearInput{
				ControllerMachineType: true,
				DefaultMacAddress:     true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear maintenance and network health message",
			m:    mcsExp[3],
			input: MachineClearInput{
				MaintenanceMessage:   true,
				NetworkHealthMessage: true,
			},
			expectedUpdate: false,
		},
		{
			desc:           "nop when no cleared fields are specified",
			m:              mcsExp[3],
			input:          MachineClearInput{},
			expectedUpdate: false,
		},
		{
			desc: "can clear Hostname",
			m:    mcsExp[2],
			input: MachineClearInput{
				ControllerMachineType: true,
				DefaultMacAddress:     true,
				Hostname:              true,
			},
			expectedUpdate: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tc.input.MachineID = tc.m.ID
			tmp, err := msd.Clear(ctx, nil, tc.input)
			assert.Nil(t, err)
			assert.NotNil(t, tmp)
			if tc.input.InstanceTypeID {
				assert.Nil(t, tmp.InstanceTypeID)
			}
			if tc.input.ControllerMachineType {
				assert.Nil(t, tmp.ControllerMachineType)
			}
			if tc.input.ProductName {
				assert.Nil(t, tmp.ProductName)
			}
			if tc.input.SerialNumber {
				assert.Nil(t, tmp.SerialNumber)
			}
			if tc.input.Metadata {
				assert.Nil(t, tmp.Metadata)
			}
			if tc.input.MaintenanceMessage {
				assert.Nil(t, tmp.MaintenanceMessage)
			}
			if tc.input.NetworkHealthMessage {
				assert.Nil(t, tmp.NetworkHealthMessage)
			}
			if tc.input.DefaultMacAddress {
				assert.Nil(t, tmp.DefaultMacAddress)
			}
			if tc.input.Hostname {
				assert.Nil(t, tmp.Hostname)
			}

			if tc.expectedUpdate {
				assert.True(t, tmp.Updated.After(tc.m.Updated))
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}

	t.Run("can clear soft-delete timestamp", func(t *testing.T) {
		machineID := mcsExp[3].ID
		err := msd.Delete(ctx, nil, machineID, false)
		require.NoError(t, err)

		var deleted Machine
		err = dbSession.DB.NewSelect().
			Model(&deleted).
			Where("m.id = ?", machineID).
			WhereAllWithDeleted().
			Scan(ctx)
		require.NoError(t, err)
		require.NotNil(t, deleted.Deleted)

		restored, err := msd.Clear(ctx, nil, MachineClearInput{
			MachineID: machineID,
			Deleted:   true,
		})
		require.NoError(t, err)
		require.NotNil(t, restored)
		assert.Nil(t, restored.Deleted)

		active, err := msd.GetByID(ctx, nil, machineID, nil, false)
		require.NoError(t, err)
		assert.Equal(t, machineID, active.ID)
	})
}

func TestMachineSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	mcsExp := testMachineSQLDAOCreateMachines(ctx, t, dbSession)
	msd := NewMachineDAO(dbSession)
	assert.NotNil(t, msd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		mID                string
		purge              bool
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object success",
			mID:                mcsExp[1].ID,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			desc:    "delete non-existent object success",
			mID:     uuid.NewString(),
			wantErr: false,
		},
		{
			desc:    "purge existing object success",
			mID:     mcsExp[2].ID,
			purge:   true,
			wantErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := msd.Delete(ctx, nil, tc.mID, tc.purge)

			if tc.wantErr {
				assert.Error(t, err)
				return
			}

			var res Machine

			if tc.purge {
				err = dbSession.DB.NewSelect().Model(&res).Where("m.id = ?", tc.mID).WhereAllWithDeleted().Scan(ctx)
			} else {
				err = dbSession.DB.NewSelect().Model(&res).Where("m.id = ?", tc.mID).Scan(ctx)
			}
			assert.ErrorIs(t, err, sql.ErrNoRows)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestMachineSQLDAO_GetCount(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	msd := NewMachineDAO(dbSession)

	totalCount := 30

	// Machine Set 1
	ms1 := []Machine{}
	mist1 := []MachineInstanceType{}

	ip, site, ins1 := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	metai1 := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
	for i := 0; i < totalCount/2; i++ {
		macAddress := testGenerateMacAddress(t)
		st := MachineStatusInitializing
		// Make at least one with READY status
		if i == 0 {
			st = MachineStatusReady
		}
		hostname := fmt.Sprintf("testhostname-%d", i)
		vendor := "testvendor"
		productName := "testproductname"
		serialNumber := uuid.NewString()
		maintenanceMessage := "testmaintenance"
		hwSkuDeviceType := "testhwskudevicetype1"
		mid := uuid.NewString()
		m, _ := msd.Create(ctx, nil, MachineCreateInput{
			MachineID:                mid,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ins1.ID,
			ControllerMachineID:      mid,
			ControllerMachineType:    ins1.ControllerMachineType,
			HwSkuDeviceType:          &hwSkuDeviceType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			Metadata:                 metai1,
			IsInMaintenance:          true,
			MaintenanceMessage:       &maintenanceMessage,
			IsNetworkDegraded:        false,
			DefaultMacAddress:        &macAddress,
			Hostname:                 &hostname,
			Status:                   st,
		})

		ms1 = append(ms1, *m)

		// Create Machine Instance Type
		mist := testMachineBuildMachineInstanceType(t, dbSession, m.ID, ins1.ID)
		assert.NotNil(t, mist)
		mist1 = append(mist1, *mist)
		assert.NotNil(t, mist1)
	}

	// Machine Capabilities
	mcd := NewMachineCapabilityDAO(dbSession)
	_, err := mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[0].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeInfiniBand, Name: "MT28908 Family [ConnectX-6]", Frequency: cutil.GetPtr("3 GHz"), Capacity: cutil.GetPtr("12 TB"), Vendor: cutil.GetPtr("Mellanox Technologies"), Count: cutil.GetPtr(2)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[0].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeInfiniBand, Name: "MT2910 Family [ConnectX-7]", Frequency: cutil.GetPtr("6 GHz"), Capacity: cutil.GetPtr("20 TB"), Vendor: cutil.GetPtr("Mellanox Technologies"), Count: cutil.GetPtr(8)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[1].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeStorage, Name: "Dell Ent NVMe CM6 RI 1.92TB", Capacity: cutil.GetPtr("1.92TB"), Vendor: cutil.GetPtr("Test Vendor"), Count: cutil.GetPtr(2)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[1].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeInfiniBand, Name: "MT2910 Family [ConnectX-7]", Frequency: cutil.GetPtr("6 GHz"), Capacity: cutil.GetPtr("20 TB"), Vendor: cutil.GetPtr("Mellanox Technologies"), Count: cutil.GetPtr(8)})
	assert.NoError(t, err)

	_, err = mcd.Create(ctx, nil, MachineCapabilityCreateInput{MachineID: &ms1[1].ID, InstanceTypeID: &ins1.ID, Type: MachineCapabilityTypeCPU, Name: "CPU Capability", Frequency: cutil.GetPtr("4 GHz"), Capacity: cutil.GetPtr("10 TB"), Vendor: cutil.GetPtr("Test Vendor"), Count: cutil.GetPtr(1)})
	assert.NoError(t, err)

	// Machine Set 2
	ms2 := []Machine{}
	ip, site, ins2 := testMachineInstanceTypeBuildInstanceType(t, dbSession, "med.x86")
	metai2 := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
	for i := 0; i < totalCount/2; i++ {
		// Create Machine Instance Type for first 2 Machines in this set
		var itID *uuid.UUID
		if i < 2 {
			itID = &ins2.ID
		}

		macAddress := testGenerateMacAddress(t)
		hostname := fmt.Sprintf("testhostname-%d", i)
		vendor := "testvendor"
		productName := "testproductname"
		serialNumber := uuid.NewString()
		networkHealthMessage := "testnetworkhealth"
		hwSkuDeviceType := "testhwskudevicetype2"
		mid := uuid.NewString()
		m, _ := msd.Create(ctx, nil, MachineCreateInput{
			MachineID:                mid,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           itID,
			ControllerMachineID:      mid,
			ControllerMachineType:    ins2.ControllerMachineType,
			HwSkuDeviceType:          &hwSkuDeviceType,
			Vendor:                   &vendor,
			ProductName:              &productName,
			SerialNumber:             &serialNumber,
			Metadata:                 metai2,
			IsInMaintenance:          false,
			IsNetworkDegraded:        true,
			NetworkHealthMessage:     &networkHealthMessage,
			DefaultMacAddress:        &macAddress,
			Hostname:                 &hostname,
			Status:                   MachineStatusInitializing,
		})

		if i < 2 {
			// Create Machine Instance Type
			mist := testMachineBuildMachineInstanceType(t, dbSession, m.ID, ins2.ID)
			assert.NotNil(t, mist)
		}
		ms2 = append(ms2, *m)
	}

	// Change assignment of one of the Machines in Set 1
	ms1[0].IsAssigned = true
	_, err = msd.Update(ctx, nil, MachineUpdateInput{
		MachineID:  ms1[0].ID,
		IsAssigned: cutil.GetPtr(true),
	})
	assert.Nil(t, err)

	dummyID := uuid.New()

	controllerMachineID, _ := uuid.Parse(ms1[0].ControllerMachineID)
	assert.NotNil(t, controllerMachineID)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc          string
		filter        MachineFilterInput
		expectedCount int
	}{
		{
			desc:          "empty filter",
			expectedCount: totalCount,
		},
		{
			desc: "filter with InfrastructureProviderID",
			filter: MachineFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{ms1[0].InfrastructureProviderID},
			},
			expectedCount: totalCount / 2,
		},
		{
			desc: "filter with SiteID",
			filter: MachineFilterInput{
				SiteIDs: []uuid.UUID{ms2[0].SiteID},
			},
			expectedCount: totalCount / 2,
		},
		{
			desc: "filter with SiteID and IsAssigned",
			filter: MachineFilterInput{
				SiteIDs:    []uuid.UUID{ms1[0].SiteID},
				IsAssigned: cutil.GetPtr(true),
			},
			expectedCount: 1,
		},
		{
			desc: "filter with unknown InfrastructureProviderID",
			filter: MachineFilterInput{
				InfrastructureProviderIDs: []uuid.UUID{dummyID},
			},
			expectedCount: 0,
		},
		{
			desc: "filter with SiteID and Hostname",
			filter: MachineFilterInput{
				SiteIDs:  []uuid.UUID{ms1[0].SiteID},
				Hostname: cutil.GetPtr("testhostname-1"),
			},
			expectedCount: 1,
		},
		{
			desc: "filter with CapabilityType",
			filter: MachineFilterInput{
				CapabilityType: db.GetTypedStrPtr(MachineCapabilityTypeInfiniBand),
			},
			expectedCount: 2,
		},
		{
			desc: "filter with CapabilityName",
			filter: MachineFilterInput{
				CapabilityNames: []string{"MT2910 Family [ConnectX-7]"},
			},
			expectedCount: 2,
		},
		{
			desc: "filter with Status",
			filter: MachineFilterInput{
				Statuses: []string{MachineStatusInitializing},
			},
			expectedCount: 29,
		},
		{
			desc: "filter with MachineIDs",
			filter: MachineFilterInput{
				MachineIDs: []string{
					ms2[0].ID,
					ms2[1].ID,
					ms2[2].ID,
				},
			},
			expectedCount: 3,
		},
		{
			desc: "filter with Status and MachineIDs",
			filter: MachineFilterInput{
				Statuses: []string{MachineStatusInitializing},
				MachineIDs: []string{
					ms2[0].ID,
					ms2[1].ID,
				},
			},
			expectedCount: 2,
		},
		{
			desc: "filter with SearchQuery with Status",
			filter: MachineFilterInput{
				SearchQuery: cutil.GetPtr(MachineStatusReady),
			},
			expectedCount: 1,
		},
		{
			desc: "filter with SearchQuery with MachineID",
			filter: MachineFilterInput{
				SearchQuery: cutil.GetPtr(ms2[0].ID),
			},
			expectedCount: 1,
		},
		{
			desc: "filter with Hostname",
			filter: MachineFilterInput{
				Hostname: cutil.GetPtr("testhostname-0"),
			},
			expectedCount: 2,
		},
		{
			desc: "filter with HasInstanceType",
			filter: MachineFilterInput{
				HasInstanceType: cutil.GetPtr(true),
			},
			expectedCount: totalCount/2 + 2,
		},
		{
			desc: "filter with Status and HasInstanceType",
			filter: MachineFilterInput{
				Statuses:        []string{MachineStatusReady},
				HasInstanceType: cutil.GetPtr(true),
			},
			expectedCount: 1,
		},
		{
			desc: "filter with InstanceTypeIDs",
			filter: MachineFilterInput{
				InstanceTypeIDs: []uuid.UUID{ins1.ID},
			},
			expectedCount: totalCount / 2,
		},
		{
			desc: "filter with ControllerMachineID",
			filter: MachineFilterInput{
				ControllerMachineID: cutil.GetPtr(controllerMachineID.String()),
			},
			expectedCount: 1,
		},
		{
			desc: "filter with HwSkuDeviceTypes1",
			filter: MachineFilterInput{
				HwSkuDeviceTypes: []string{"testhwskudevicetype1"},
			},
			expectedCount: totalCount / 2,
		},
		{
			desc: "filter with HwSkuDeviceTypes2",
			filter: MachineFilterInput{
				HwSkuDeviceTypes: []string{"testhwskudevicetype2"},
			},
			expectedCount: totalCount / 2,
		},
		{
			desc: "filter with HwSkuDeviceTypes3",
			filter: MachineFilterInput{
				HwSkuDeviceTypes: []string{"testhwskudevicetype3"},
			},
			expectedCount: 0,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			count, err := msd.GetCount(ctx, nil, tc.filter)
			if err != nil {
				t.Logf("%s", err.Error())
			}
			assert.Equal(t, tc.expectedCount, count)
		})
	}
}

func TestMachineSQLDAO_GetHealth(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	msd := NewMachineDAO(dbSession)

	ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	defMacAddr := "00:1B:44:11:3A:B7"
	meta := &SiteControllerMachine{&corev1.Machine{Id: &corev1.MachineId{Id: "foo"}}}
	health := map[string]interface{}{"source": "aggregate-health", "alerts": []map[string]interface{}{
		{"id": "test-id", "target": "test-target", "message": "test-message", "classifications": [1]string{"test-classification"}},
	}, "successes": []map[string]interface{}{}}
	vendor := "testvendor"
	hostname := "testmachine"
	productName := "testproduct"
	serialNumber := uuid.NewString()
	maintenanceMessage := "testmaintenance"
	networkHealthMessage := "testnetworkhealth"

	id := uuid.NewString()
	_, err := msd.Create(ctx, nil, MachineCreateInput{
		MachineID:                id,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		InstanceTypeID:           &ins.ID,
		ControllerMachineType:    ins.ControllerMachineType,
		Vendor:                   &vendor,
		ProductName:              &productName,
		SerialNumber:             &serialNumber,
		Metadata:                 meta,
		IsInMaintenance:          true,
		MaintenanceMessage:       &maintenanceMessage,
		IsNetworkDegraded:        true,
		NetworkHealthMessage:     &networkHealthMessage,
		Health:                   health,
		Hostname:                 &hostname,
		DefaultMacAddress:        &defMacAddr,
		Status:                   MachineStatusInitializing,
	})

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	assert.True(t, err == nil)

	m, err := msd.GetByID(ctx, nil, id, []string{}, false)
	assert.Equal(t, nil, err)
	health_report, err := m.GetHealth()
	assert.Equal(t, nil, err)

	assert.Equal(t, 1, len(health_report.Alerts))
	assert.Equal(t, "test-target", *health_report.Alerts[0].Target)
	assert.Equal(t, MachineHealth{
		"aggregate-health",
		nil,
		[]HealthProbeSuccess{},
		[]HealthProbeAlert{{
			"test-id",
			health_report.Alerts[0].Target,
			nil,
			"test-message",
			nil,
			[]string{"test-classification"},
		}},
	}, *health_report)
}

func TestMachineSQLDAO_UpdateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)
	ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	msd := NewMachineDAO(dbSession)

	// Create test machines
	m1, err := msd.Create(ctx, nil, MachineCreateInput{
		MachineID:                uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		InstanceTypeID:           &ins.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusInitializing,
	})
	assert.Nil(t, err)

	m2, err := msd.Create(ctx, nil, MachineCreateInput{
		MachineID:                uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusInitializing,
	})
	assert.Nil(t, err)

	m3, err := msd.Create(ctx, nil, MachineCreateInput{
		MachineID:                uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusInitializing,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []MachineUpdateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "batch update three machines",
			inputs: []MachineUpdateInput{
				{
					MachineID: m1.ID,
					Status:    cutil.GetPtr(MachineStatusReady),
					Hostname:  cutil.GetPtr("machine1-updated"),
					Labels:    map[string]string{"updated": "true"},
				},
				{
					MachineID:      m2.ID,
					Status:         cutil.GetPtr(MachineStatusInUse),
					InstanceTypeID: &ins.ID,
				},
				{
					MachineID: m3.ID,
					Status:    cutil.GetPtr(MachineStatusMaintenance),
					Labels:    map[string]string{"env": "test"},
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "batch update with empty input",
			inputs:             []MachineUpdateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
		{
			desc: "batch update single machine",
			inputs: []MachineUpdateInput{
				{
					MachineID: m1.ID,
					Status:    cutil.GetPtr(MachineStatusReady),
				},
			},
			expectError:   false,
			expectedCount: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := msd.UpdateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify updates and that results are returned in the same order as inputs
				for i, machine := range got {
					assert.Equal(t, tc.inputs[i].MachineID, machine.ID, "result order should match input order")
					if tc.inputs[i].Status != nil {
						assert.Equal(t, *tc.inputs[i].Status, machine.Status)
					}
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestMachineSQLDAO_UpdateMultiple_ExceedsMaxBatchItems(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	msd := NewMachineDAO(dbSession)

	// Create inputs exceeding MaxBatchItems
	inputs := make([]MachineUpdateInput, db.MaxBatchItems+1)
	for i := range inputs {
		inputs[i] = MachineUpdateInput{
			MachineID: uuid.NewString(),
			Status:    cutil.GetPtr(MachineStatusReady),
		}
	}

	_, err := msd.UpdateMultiple(ctx, nil, inputs)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "batch size")
	assert.Contains(t, err.Error(), "exceeds maximum allowed")
}

// TestMachineSQLDAO_UpdateMultiple_AllFields verifies that ALL fields in MachineUpdateInput
// are correctly handled by UpdateMultiple. This test will fail if any field is missed.
func TestMachineSQLDAO_UpdateMultiple_AllFields(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)
	ip, site, instanceType := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	msd := NewMachineDAO(dbSession)

	// Create a machine with minimal fields
	m, err := msd.Create(ctx, nil, MachineCreateInput{
		MachineID:                uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusInitializing,
	})
	assert.NoError(t, err)

	// Update with ALL fields set to new values
	input := MachineUpdateInput{
		MachineID:             m.ID,
		InstanceTypeID:        &instanceType.ID,
		ControllerMachineID:   cutil.GetPtr("new-controller-id"),
		ControllerMachineType: cutil.GetPtr("new-controller-type"),
		HwSkuDeviceType:       cutil.GetPtr("new-hw-sku"),
		Vendor:                cutil.GetPtr("new-vendor"),
		ProductName:           cutil.GetPtr("new-product"),
		SerialNumber:          cutil.GetPtr("new-serial"),
		IsInMaintenance:       cutil.GetPtr(true),
		IsUsableByTenant:      cutil.GetPtr(true),
		MaintenanceMessage:    cutil.GetPtr("maintenance message"),
		IsNetworkDegraded:     cutil.GetPtr(true),
		NetworkHealthMessage:  cutil.GetPtr("network health message"),
		Health:                map[string]interface{}{"status": "healthy"},
		DefaultMacAddress:     cutil.GetPtr("aa:bb:cc:dd:ee:ff"),
		Hostname:              cutil.GetPtr("new-hostname"),
		IsAssigned:            cutil.GetPtr(true),
		Status:                cutil.GetPtr(MachineStatusReady),
		Labels:                map[string]string{"env": "prod", "team": "infra"},
		IsMissingOnSite:       cutil.GetPtr(true),
	}

	results, err := msd.UpdateMultiple(ctx, nil, []MachineUpdateInput{input})
	assert.NoError(t, err)
	assert.Len(t, results, 1)

	updated := results[0]

	// Verify ALL fields were updated correctly
	assert.Equal(t, m.ID, updated.ID)
	assert.Equal(t, &instanceType.ID, updated.InstanceTypeID, "InstanceTypeID not updated")
	assert.Equal(t, "new-controller-id", updated.ControllerMachineID, "ControllerMachineID not updated")
	assert.Equal(t, "new-controller-type", *updated.ControllerMachineType, "ControllerMachineType not updated")
	assert.Equal(t, "new-hw-sku", *updated.HwSkuDeviceType, "HwSkuDeviceType not updated")
	assert.Equal(t, "new-vendor", *updated.Vendor, "Vendor not updated")
	assert.Equal(t, "new-product", *updated.ProductName, "ProductName not updated")
	assert.Equal(t, "new-serial", *updated.SerialNumber, "SerialNumber not updated")
	assert.True(t, updated.IsInMaintenance, "IsInMaintenance not updated")
	assert.True(t, updated.IsUsableByTenant, "IsUsableByTenant not updated")
	assert.Equal(t, "maintenance message", *updated.MaintenanceMessage, "MaintenanceMessage not updated")
	assert.True(t, updated.IsNetworkDegraded, "IsNetworkDegraded not updated")
	assert.Equal(t, "network health message", *updated.NetworkHealthMessage, "NetworkHealthMessage not updated")
	assert.NotNil(t, updated.Health, "Health not updated")
	assert.Equal(t, "aa:bb:cc:dd:ee:ff", *updated.DefaultMacAddress, "DefaultMacAddress not updated")
	assert.Equal(t, "new-hostname", *updated.Hostname, "Hostname not updated")
	assert.True(t, updated.IsAssigned, "IsAssigned not updated")
	assert.Equal(t, MachineStatusReady, updated.Status, "Status not updated")
	assert.Equal(t, map[string]string{"env": "prod", "team": "infra"}, updated.Labels, "Labels not updated")
	assert.True(t, updated.IsMissingOnSite, "IsMissingOnSite not updated")
}

func TestSiteControllerMachine_GetNormalizedState(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		in   string
		want string
	}{
		{"empty", "", ""},
		{"trim_no_brace", "  Ready/InUse  ", "Ready/InUse"},
		{"prefix_before_json", `Ready { "k": 1 }`, "Ready"},
		{"first_brace_only", "A{B", "A"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {

			scm := SiteControllerMachine{Machine: &corev1.Machine{State: tt.in}}
			assert.Equal(t, tt.want, scm.GetNormalizedState())
		})
	}
}

func TestMachine_GetControllerState(t *testing.T) {
	t.Parallel()
	var nilMachine *Machine
	assert.Equal(t, "", nilMachine.GetControllerState())

	mNoMeta := &Machine{Metadata: nil}
	assert.Equal(t, "", mNoMeta.GetControllerState())

	scm := &SiteControllerMachine{Machine: &corev1.Machine{State: `Ready { "x": 1 }`}}
	mMeta := &Machine{Metadata: scm}
	assert.Equal(t, "Ready", mMeta.GetControllerState())
	assert.Equal(t, "Ready", scm.GetNormalizedState())
}

func TestMachine_ToMaintenanceRequestProto(t *testing.T) {
	t.Run("enable with reference", func(t *testing.T) {
		ref := "scheduled work"
		m := &Machine{ID: "m-1"}
		req := m.ToMaintenanceRequestProto(corev1.MaintenanceOperation_Enable, &ref)
		assert.NotNil(t, req)
		assert.NotNil(t, req.HostId)
		assert.Equal(t, "m-1", req.HostId.Id)
		assert.Equal(t, corev1.MaintenanceOperation_Enable, req.Operation)
		assert.Equal(t, &ref, req.Reference)
	})
	t.Run("disable with no reference", func(t *testing.T) {
		m := &Machine{ID: "m-1"}
		req := m.ToMaintenanceRequestProto(corev1.MaintenanceOperation_Disable, nil)
		assert.NotNil(t, req)
		assert.NotNil(t, req.HostId)
		assert.Equal(t, "m-1", req.HostId.Id)
		assert.Equal(t, corev1.MaintenanceOperation_Disable, req.Operation)
		assert.Nil(t, req.Reference)
	})
}

func TestMachine_ToMetadataUpdateRequestProto(t *testing.T) {
	labelVal := "prod"
	labels := []*corev1.Label{{Key: "env", Value: &labelVal}}

	t.Run("uses Machine.ID as Name fallback when metadata is nil", func(t *testing.T) {
		m := &Machine{ID: "m-1"}
		req := m.ToMetadataUpdateRequestProto(labels)
		assert.NotNil(t, req)
		assert.NotNil(t, req.MachineId)
		assert.Equal(t, "m-1", req.MachineId.Id)
		assert.NotNil(t, req.Metadata)
		assert.Equal(t, "m-1", req.Metadata.Name)
		assert.Equal(t, labels, req.Metadata.Labels)
	})

	t.Run("uses stored metadata Name when present", func(t *testing.T) {
		m := &Machine{
			ID: "m-1",
			Metadata: &SiteControllerMachine{
				Machine: &corev1.Machine{Metadata: &corev1.Metadata{Name: "stored-name"}},
			},
		}
		req := m.ToMetadataUpdateRequestProto(labels)
		assert.Equal(t, "stored-name", req.Metadata.Name)
	})
}
