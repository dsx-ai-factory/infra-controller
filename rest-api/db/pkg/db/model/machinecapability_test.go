// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"encoding/json"
	"sort"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"
)

func TestMachineCapability_Equal(t *testing.T) {
	makeCap := func() *MachineCapability {
		return &MachineCapability{
			Type:             MachineCapabilityTypeCPU,
			Name:             "cap-1",
			Cores:            cutil.GetPtr(8),
			Threads:          cutil.GetPtr(16),
			Count:            cutil.GetPtr(1),
			Vendor:           cutil.GetPtr("Intel"),
			Capacity:         cutil.GetPtr("64GB"),
			Frequency:        cutil.GetPtr("3.0GHz"),
			HardwareRevision: cutil.GetPtr("rev-1"),
			InactiveDevices:  []int{1, 2},
			Index:            0,
		}
	}

	t.Run("nil equals nil", func(t *testing.T) {
		var a, b *MachineCapability
		assert.True(t, a.Equal(b))
	})
	t.Run("nil does not equal non-nil", func(t *testing.T) {
		var a *MachineCapability
		b := makeCap()
		assert.False(t, a.Equal(b))
		assert.False(t, b.Equal(a))
	})
	t.Run("identical caps are equal", func(t *testing.T) {
		assert.True(t, makeCap().Equal(makeCap()))
	})
	t.Run("differing Name returns false", func(t *testing.T) {
		a := makeCap()
		b := makeCap()
		b.Name = "cap-2"
		assert.False(t, a.Equal(b))
	})
	t.Run("differing pointer field returns false", func(t *testing.T) {
		a := makeCap()
		b := makeCap()
		b.Cores = cutil.GetPtr(4)
		assert.False(t, a.Equal(b))
	})
	t.Run("differing InactiveDevices slice returns false", func(t *testing.T) {
		a := makeCap()
		b := makeCap()
		b.InactiveDevices = []int{1, 3}
		assert.False(t, a.Equal(b))
	})
}

func TestMachineCapability_MapKey(t *testing.T) {
	dpu := MachineCapabilityDeviceTypeDPU
	spx := MachineCapabilityDeviceTypeSPX
	empty := MachineCapabilityDeviceType("")
	tests := []struct {
		name       string
		deviceType *MachineCapabilityDeviceType
		want       string
	}{
		{name: "generic", want: "Network:10:ConnectX-8"},
		{name: "generic empty device type", deviceType: &empty, want: "Network:10:ConnectX-8"},
		{name: "DPU", deviceType: &dpu, want: "Network:10:ConnectX-8:DPU"},
		{name: "SPX", deviceType: &spx, want: "Network:10:ConnectX-8:SPX"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mc := &MachineCapability{
				Type:       MachineCapabilityTypeNetwork,
				Name:       "ConnectX-8",
				DeviceType: tt.deviceType,
			}
			assert.Equal(t, tt.want, mc.MapKey())
		})
	}

	t.Run("name delimiter cannot impersonate device type", func(t *testing.T) {
		generic := MachineCapabilityMapKey(MachineCapabilityTypeNetwork, "ConnectX-8:SPX", nil)
		typed := MachineCapabilityMapKey(MachineCapabilityTypeNetwork, "ConnectX-8", &spx)
		assert.NotEqual(t, generic, typed)
	})
}

func TestMachineCapabilitySQLDAO_Create(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	mach := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &ins.ID, ins.ControllerMachineType)

	mcd := NewMachineCapabilityDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		mcs                []MachineCapability
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create machine capability for CPU",
			mcs: []MachineCapability{
				{
					MachineID:        &mach.ID,
					InstanceTypeID:   nil,
					Type:             MachineCapabilityTypeCPU,
					Name:             "AMD Opteron Series x10",
					Frequency:        cutil.GetPtr("3.0 Ghz"),
					Count:            cutil.GetPtr(2),
					Cores:            cutil.GetPtr(128),
					Threads:          cutil.GetPtr(256),
					HardwareRevision: cutil.GetPtr("v.12345"),
					Index:            5,
					Info: map[string]interface{}{
						"Version": "2.0",
					},
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create instance capability for Memory",
			mcs: []MachineCapability{
				{
					MachineID:      nil,
					InstanceTypeID: &ins.ID,
					Type:           MachineCapabilityTypeMemory,
					Name:           "Corsair Vengeance LPX",
					Frequency:      cutil.GetPtr("3200 Mhz"),
					Capacity:       cutil.GetPtr("128GB"),
					Count:          cutil.GetPtr(4),
				},
			},
			expectError: false,
		},
		{
			desc: "create instance capability for Network",
			mcs: []MachineCapability{
				{
					MachineID:      nil,
					InstanceTypeID: &ins.ID,
					Type:           MachineCapabilityTypeNetwork,
					Name:           "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller",
					Capacity:       cutil.GetPtr("100GB"),
					Count:          cutil.GetPtr(2),
					DeviceType:     (*MachineCapabilityDeviceType)(cutil.GetPtr("DPU")),
				},
			},
			expectError: false,
		},
		{
			desc: "create machine capability for InfiniBand",
			mcs: []MachineCapability{
				{
					MachineID:       &mach.ID,
					InstanceTypeID:  nil,
					Type:            MachineCapabilityTypeInfiniBand,
					Name:            "MT28908 Family [ConnectX-6]",
					Vendor:          cutil.GetPtr("Mellanox Technologies"),
					Count:           cutil.GetPtr(2),
					InactiveDevices: []int{2, 4},
				},
			},
			expectError: false,
		},
		{
			desc: "create with both machine and instance set to nil",
			mcs: []MachineCapability{
				{
					MachineID:      nil,
					InstanceTypeID: nil,
					Type:           MachineCapabilityTypeMemory,
					Name:           "Corsair Vengeance LPX",
					Frequency:      cutil.GetPtr("3200 Mhz"),
					Capacity:       cutil.GetPtr("128GB"),
					Count:          cutil.GetPtr(4),
					Info: map[string]interface{}{
						"DDR": "v4",
					},
				},
			},
			expectError: true,
		},
		{
			desc: "create with invalid capability type",
			mcs: []MachineCapability{
				{
					MachineID:      &mach.ID,
					InstanceTypeID: nil,
					Type:           "",
					Name:           "AMD Opteron Series x10",
					Frequency:      cutil.GetPtr("3.0 Ghz"),
					Count:          cutil.GetPtr(2),
				},
			},
			expectError: true,
		},
		{
			desc: "create with non-empty but unknown capability type",
			mcs: []MachineCapability{
				{
					MachineID:      &mach.ID,
					InstanceTypeID: nil,
					Type:           "Mystery",
					Name:           "AMD Opteron Series x10",
					Frequency:      cutil.GetPtr("3.0 Ghz"),
					Count:          cutil.GetPtr(2),
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.mcs {

				mc, err := mcd.Create(ctx, nil, MachineCapabilityCreateInput{
					MachineID:        i.MachineID,
					InstanceTypeID:   i.InstanceTypeID,
					Type:             i.Type,
					Name:             i.Name,
					Frequency:        i.Frequency,
					Capacity:         i.Capacity,
					Vendor:           i.Vendor,
					Cores:            i.Cores,
					Threads:          i.Threads,
					HardwareRevision: i.HardwareRevision,
					Count:            i.Count,
					DeviceType:       i.DeviceType,
					Info:             i.Info,
					Index:            i.Index,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, mc)
					assert.Nil(t, err)
				} else {
					assert.Nil(t, mc)
				}

				if err != nil {
					t.Logf("%s", err.Error())
					return
				}

				assert.Equal(t, i.Index, mc.Index)

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

func testMachineCapabilitySQLDAOCreateSlice(ctx context.Context, t *testing.T, dbSession *db.Session) []MachineCapability {
	ip, site, ins := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	mach := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &ins.ID, ins.ControllerMachineType)

	mcd := NewMachineCapabilityDAO(dbSession)

	mcsExp := []MachineCapability{
		{
			MachineID:      &mach.ID,
			InstanceTypeID: nil,
			Type:           MachineCapabilityTypeCPU,
			Name:           "AMD Opteron Series x10",
			Frequency:      cutil.GetPtr("3.0 Ghz"),
			Count:          cutil.GetPtr(2),
			Info: map[string]interface{}{
				"Version": "2.0",
			},
		},
		{
			MachineID:      &mach.ID,
			InstanceTypeID: nil,
			Type:           MachineCapabilityTypeCPU,
			Name:           "Corsair Vengeance LPX DDR4",
			Frequency:      cutil.GetPtr("3200 Mhz"),
			Capacity:       cutil.GetPtr("32 GB"),
			Count:          cutil.GetPtr(2),
		},
		{
			MachineID:      &mach.ID,
			InstanceTypeID: nil,
			Type:           MachineCapabilityTypeStorage,
			Name:           "Dell Ent NVMe CM6 RI 1.92TB",
			Capacity:       cutil.GetPtr("1.92TB"),
			Count:          cutil.GetPtr(4),
		},
		{
			MachineID:      &mach.ID,
			InstanceTypeID: nil,
			Type:           MachineCapabilityTypeStorage,
			Name:           "Dell Ent NVMe CM6 RI 1.92TB",
			Capacity:       cutil.GetPtr("1.92TB"),
			Count:          cutil.GetPtr(4),
		},
		{
			MachineID:      &mach.ID,
			InstanceTypeID: nil,
			Type:           MachineCapabilityTypeNetwork,
			Name:           "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller",
			Capacity:       cutil.GetPtr("100GB"),
			Count:          cutil.GetPtr(2),
			DeviceType:     (*MachineCapabilityDeviceType)(cutil.GetPtr("")),
		},
		{
			MachineID:      &mach.ID,
			InstanceTypeID: nil,
			Type:           MachineCapabilityTypeInfiniBand,
			Name:           "MT28908 Family [ConnectX-6]",
			Vendor:         cutil.GetPtr("Mellanox Technologies"),
			Count:          cutil.GetPtr(2),
		},
	}

	// MachineCapability created
	for i := 0; i < len(mcsExp); i++ {
		mcCre, _ := mcd.Create(ctx, nil,
			MachineCapabilityCreateInput{
				MachineID:        mcsExp[i].MachineID,
				InstanceTypeID:   mcsExp[i].InstanceTypeID,
				Type:             mcsExp[i].Type,
				Name:             mcsExp[i].Name,
				Frequency:        mcsExp[i].Frequency,
				Capacity:         mcsExp[i].Capacity,
				Vendor:           mcsExp[i].Vendor,
				Cores:            mcsExp[i].Cores,
				DeviceType:       mcsExp[i].DeviceType,
				Threads:          mcsExp[i].Threads,
				HardwareRevision: mcsExp[i].HardwareRevision,
				Count:            mcsExp[i].Count,
				InactiveDevices:  mcsExp[i].InactiveDevices,
				Info:             mcsExp[i].Info,
			},
		)
		assert.NotNil(t, mcCre)
		mcsExp[i].ID = mcCre.ID
	}

	return mcsExp
}

func TestMachineCapabilitySQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	mcsExp := testMachineCapabilitySQLDAOCreateSlice(ctx, t, dbSession)
	mcd := NewMachineCapabilityDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		mc                 MachineCapability
		expectError        bool
		expectedErrVal     error
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when MachineCapability exists on [1]",
			mc:                 mcsExp[0],
			expectError:        false,
			paramRelations:     []string{InstanceTypeRelationName},
			verifyChildSpanner: true,
		},
		{
			desc:           "GetById success when MachineCapability exists on [2]",
			mc:             mcsExp[1],
			expectError:    false,
			paramRelations: []string{InstanceTypeRelationName},
		},
		{
			desc: "GetById success when MachineCapability not found",
			mc: MachineCapability{
				ID: uuid.New(),
			},
			paramRelations: []string{InstanceTypeRelationName},
			expectError:    true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := mcd.GetByID(ctx, nil, tc.mc.ID, tc.paramRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if tc.expectError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.mc.ID, tmp.ID)
				assert.EqualValues(t, tc.mc.MachineID, tmp.MachineID)
				assert.EqualValues(t, tc.mc.InstanceTypeID, tmp.InstanceTypeID)
				assert.EqualValues(t, tc.mc.Type, tmp.Type)
				assert.EqualValues(t, tc.mc.Name, tmp.Name)
				assert.EqualValues(t, tc.mc.Frequency, tmp.Frequency)
				assert.EqualValues(t, tc.mc.Capacity, tmp.Capacity)
				assert.EqualValues(t, tc.mc.Vendor, tmp.Vendor)
				assert.EqualValues(t, tc.mc.Count, tmp.Count)
				assert.EqualValues(t, tc.mc.Info, tmp.Info)
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

func TestMachineCapabilitySQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	mcd := NewMachineCapabilityDAO(dbSession)

	itCapCount := 0

	ip, site, it := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")
	mcd.Create(ctx, nil, MachineCapabilityCreateInput{InstanceTypeID: &it.ID, Type: MachineCapabilityTypeCPU, Name: "Test Capability", Frequency: cutil.GetPtr("3.2 GHz"), Count: cutil.GetPtr(2)})
	mcd.Create(ctx, nil, MachineCapabilityCreateInput{InstanceTypeID: &it.ID, Type: MachineCapabilityTypeMemory, Name: "Test Capability", Capacity: cutil.GetPtr("32GB"), Count: cutil.GetPtr(4)})
	itCapCount += 2

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"test-role"})
	it2 := TestBuildInstanceType(t, dbSession, "lg.x86", ip, site, user)
	mcd.Create(ctx, nil, MachineCapabilityCreateInput{InstanceTypeID: &it2.ID, Type: MachineCapabilityTypeCPU, Name: "Test Capability", Frequency: cutil.GetPtr("4 GHz"), Count: cutil.GetPtr(1)})
	itCapCount++

	ms := []Machine{}
	mcs := []MachineCapability{}
	var lastEntry *MachineCapability
	totalMachineCount := 10

	lastCount := 0
	totalCount := totalMachineCount*len(MachineCapabilityTypeChoiceMap) + itCapCount

	for i := 0; i < totalMachineCount; i++ {
		m := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &it.ID, it.ControllerMachineType)
		ms = append(ms, *m)

		capTypes := make([]string, 0, len(MachineCapabilityTypeChoiceMap))
		for cap := range MachineCapabilityTypeChoiceMap {
			capTypes = append(capTypes, string(cap))
		}
		sort.Strings(capTypes)

		for _, cap := range capTypes {
			capType := MachineCapabilityType(cap)
			var vendor *string
			var deviceType *MachineCapabilityDeviceType
			if capType == MachineCapabilityTypeInfiniBand {
				vendor = cutil.GetPtr("Test Vendor")
			}
			if i == 0 && capType == MachineCapabilityTypeNetwork {
				dt := MachineCapabilityDeviceTypeDPU
				deviceType = &dt
			}

			var inactiveDevices []int
			if capType == MachineCapabilityTypeInfiniBand {
				inactiveDevices = []int{1, 3}
			}

			mc, err := mcd.Create(ctx, nil, MachineCapabilityCreateInput{
				MachineID: &m.ID, Type: capType,
				Name:            "Test Capability",
				Frequency:       cutil.GetPtr("3 GHz"),
				Capacity:        cutil.GetPtr("12 TB"),
				Vendor:          vendor,
				Count:           cutil.GetPtr(1),
				DeviceType:      deviceType,
				InactiveDevices: inactiveDevices,
			})
			assert.NoError(t, err)
			mcs = append(mcs, *mc)

			// Track last one
			lastCount = +1
			if lastCount == totalCount {
				lastEntry = mc
			}
		}
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		MachineIDs         []string
		InstanceTypeIDs    []uuid.UUID
		Type               *string
		Name               *string
		Frequency          *string
		Capacity           *string
		Vendor             *string
		Count              *int
		DeviceType         *string
		InactiveDevices    []int
		offset             *int
		limit              *int
		orderBy            *paginator.OrderBy
		firstEntry         *MachineCapability
		expectedCount      int
		expectedTotal      *int
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns all objects",
			MachineIDs:         nil,
			InstanceTypeIDs:    nil,
			Type:               nil,
			Name:               nil,
			Frequency:          nil,
			Capacity:           nil,
			Vendor:             nil,
			Count:              nil,
			DeviceType:         nil,
			expectedCount:      paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			verifyChildSpanner: true,
		},
		{
			desc:            "GetAll with relation returns all objects",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            nil,
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   paginator.DefaultLimit,
			expectedTotal:   &totalCount,
			paramRelations:  []string{InstanceTypeRelationName},
		},
		{
			desc:            "GetAll with machine id filter",
			MachineIDs:      []string{ms[0].ID, ms[1].ID},
			InstanceTypeIDs: nil,
			Type:            nil,
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   len(MachineCapabilityTypeChoiceMap) * 2,
		},
		{
			desc:            "GetAll with non-existent machine id filter",
			MachineIDs:      []string{"test-id"},
			InstanceTypeIDs: nil,
			Type:            nil,
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   0,
		},
		{
			desc:            "GetAll with instance id filter",
			MachineIDs:      nil,
			InstanceTypeIDs: []uuid.UUID{it.ID},
			Type:            nil,
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   2,
		},
		{
			desc:            "GetAll with Type filter",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            db.GetTypedStrPtr(MachineCapabilityTypeGPU),
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           nil,
			expectedCount:   totalMachineCount,
		},
		{
			desc:            "GetAll with Type and Name filter",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            db.GetTypedStrPtr(MachineCapabilityTypeGPU),
			Name:            &mcs[0].Name,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   totalMachineCount,
		},
		{
			desc:            "GetAll with Type and Capacity filter",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            cutil.GetPtr(string(mcs[1].Type)),
			Name:            nil,
			Frequency:       nil,
			Capacity:        mcs[1].Capacity,
			Vendor:          nil,
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   totalMachineCount,
		},
		{
			desc:            "GetAll with Type and Frequency filter",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            cutil.GetPtr(string(mcs[1].Type)),
			Name:            nil,
			Frequency:       mcs[1].Frequency,
			Vendor:          nil,
			Capacity:        nil,
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   totalMachineCount,
		},
		{
			desc:            "GetAll with Type and Count filter",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            db.GetTypedStrPtr(MachineCapabilityTypeGPU),
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           cutil.GetPtr(1),
			expectedCount:   totalMachineCount,
		},
		{
			desc:            "GetAll with Vendor filter",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            nil,
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          cutil.GetPtr("Test Vendor"),
			Count:           nil,
			DeviceType:      nil,
			expectedCount:   totalMachineCount,
		},
		{
			desc:            "GetAll with DeviceType filter",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            nil,
			Name:            nil,
			Frequency:       nil,
			Capacity:        nil,
			Vendor:          nil,
			Count:           nil,
			DeviceType:      cutil.GetPtr("DPU"),
			expectedCount:   1,
		},
		{
			desc:            "GetAll with InactiveDevices filter",
			InactiveDevices: []int{1, 3},
			expectedCount:   totalMachineCount,
		},
		{
			desc:            "GetAll with limit returns objects",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            nil,
			offset:          cutil.GetPtr(0),
			limit:           cutil.GetPtr(5),
			expectedCount:   5,
			expectedTotal:   &totalCount,
		},
		{
			desc:            "GetAll with offset returns objects",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            nil,
			offset:          cutil.GetPtr(5),
			expectedCount:   paginator.DefaultLimit,
			expectedTotal:   &totalCount,
		},
		{
			desc:            "GetAll with order by returns objects",
			MachineIDs:      nil,
			InstanceTypeIDs: nil,
			Type:            nil,
			orderBy: &paginator.OrderBy{
				Field: "type",
				Order: paginator.OrderDescending,
			},
			firstEntry:    lastEntry, // last one created entry which would be the first in descending sort
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := mcd.GetAll(ctx, nil, tc.MachineIDs, tc.InstanceTypeIDs, tc.Type, tc.Name, tc.Frequency, tc.Capacity, tc.Vendor, tc.Count, tc.DeviceType, tc.InactiveDevices, tc.paramRelations,
				tc.offset, tc.limit, tc.orderBy)
			if err != nil {
				t.Logf("%s", err.Error())
			}
			assert.Equal(t, tc.expectedCount, len(got))
			if len(tc.paramRelations) > 0 {
				assert.NotNil(t, got[0].InstanceType)
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.Type, got[0].Type)
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

func TestMachineCapabilitySQLDAO_GetAllDistinct(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	mcd := NewMachineCapabilityDAO(dbSession)

	ip, site, it := testMachineInstanceTypeBuildInstanceType(t, dbSession, "sm.x86")

	ms := []Machine{}
	mcs := []MachineCapability{}

	totalMachineCount := 10
	for i := 0; i < totalMachineCount; i++ {
		m := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &it.ID, it.ControllerMachineType)
		ms = append(ms, *m)

		capTypes := make([]string, 0, len(MachineCapabilityTypeChoiceMap))
		for cap := range MachineCapabilityTypeChoiceMap {
			capTypes = append(capTypes, string(cap))
		}
		sort.Strings(capTypes)

		for _, cap := range capTypes {
			capType := MachineCapabilityType(cap)
			var deviceType *MachineCapabilityDeviceType
			if i == 0 && capType == MachineCapabilityTypeNetwork {
				dt := MachineCapabilityDeviceTypeDPU
				deviceType = &dt
			}
			var inactiveDevices []int
			if capType == MachineCapabilityTypeInfiniBand {
				inactiveDevices = []int{1, 3}
			}
			mc, err := mcd.Create(ctx, nil, MachineCapabilityCreateInput{
				MachineID:       &m.ID,
				InstanceTypeID:  &it.ID,
				Type:            capType,
				Name:            "Test Capability",
				Frequency:       cutil.GetPtr("3 GHz"),
				Capacity:        cutil.GetPtr("12 TB"),
				Vendor:          cutil.GetPtr("Test Vendor"),
				Count:           cutil.GetPtr(1),
				DeviceType:      deviceType,
				InactiveDevices: inactiveDevices,
			})
			assert.NoError(t, err)
			mcs = append(mcs, *mc)
		}
	}

	// totalCount := totalMachineCount * len(MachineCapabilityTypeChoiceMap)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		MachineIDs         []string
		InstanceTypeID     *uuid.UUID
		Type               *string
		Name               *string
		Frequency          *string
		Capacity           *string
		Vendor             *string
		Count              *int
		DeviceType         *string
		InactiveDevices    []int
		expectedCount      int
		expectedTotal      *int
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns all objects",
			MachineIDs:         nil,
			InstanceTypeID:     nil,
			Type:               nil,
			Name:               nil,
			Frequency:          nil,
			Capacity:           nil,
			Vendor:             nil,
			Count:              nil,
			DeviceType:         nil,
			expectedCount:      len(MachineCapabilityTypeChoiceMap) + 1,
			verifyChildSpanner: true,
		},
		{
			desc:           "GetAll with machine id filter",
			MachineIDs:     []string{ms[0].ID, ms[1].ID},
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  len(MachineCapabilityTypeChoiceMap) + 1,
		},
		{
			desc:           "GetAll with non-existent machine id filter",
			MachineIDs:     []string{"test-id"},
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  0,
		},
		{
			desc:           "GetAll with instance id filter",
			MachineIDs:     nil,
			InstanceTypeID: &it.ID,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  len(MachineCapabilityTypeChoiceMap) + 1,
		},
		{
			desc:           "GetAll with Type filter",
			MachineIDs:     nil,
			InstanceTypeID: nil,
			Type:           db.GetTypedStrPtr(MachineCapabilityTypeCPU),
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  1,
		},
		{
			desc:           "GetAll with Type and Name filter",
			MachineIDs:     nil,
			InstanceTypeID: nil,
			Type:           cutil.GetPtr(string(mcs[0].Type)),
			Name:           &mcs[0].Name,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  1,
		},
		{
			desc:           "GetAll with Type and Capacity filter",
			MachineIDs:     nil,
			InstanceTypeID: nil,
			Type:           cutil.GetPtr(string(mcs[1].Type)),
			Name:           nil,
			Frequency:      nil,
			Capacity:       mcs[1].Capacity,
			Vendor:         nil,
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  1,
		},
		{
			desc:           "GetAll with Type and Frequency filter",
			MachineIDs:     nil,
			InstanceTypeID: nil,
			Type:           cutil.GetPtr(string(mcs[1].Type)),
			Name:           nil,
			Frequency:      mcs[1].Frequency,
			Vendor:         nil,
			Capacity:       nil,
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  1,
		},
		{
			desc:           "GetAll with Type and Count filter",
			MachineIDs:     nil,
			InstanceTypeID: nil,
			Type:           cutil.GetPtr(string(mcs[1].Type)),
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			Count:          mcs[1].Count,
			DeviceType:     nil,
			expectedCount:  1,
		},
		{
			desc:           "GetAll with Vendor filter",
			MachineIDs:     nil,
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         cutil.GetPtr("Test Vendor"),
			Count:          nil,
			DeviceType:     nil,
			expectedCount:  len(MachineCapabilityTypeChoiceMap) + 1,
		},
		{
			desc:           "GetAll with DeviceType filter",
			MachineIDs:     nil,
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			Count:          nil,
			DeviceType:     cutil.GetPtr("DPU"),
			expectedCount:  1,
		},
		{
			desc:            "GetAll with InactiveDevices filter",
			InactiveDevices: []int{1, 3},
			expectedCount:   1,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := mcd.GetAllDistinct(ctx, nil, tc.MachineIDs, tc.InstanceTypeID, tc.Type, tc.Name, tc.Frequency, tc.Capacity, tc.Vendor, tc.Count, tc.DeviceType, tc.InactiveDevices, nil, cutil.GetPtr(paginator.TotalLimit), nil)
			assert.NoError(t, err)
			assert.Equal(t, tc.expectedCount, len(got))

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
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

func TestMachineCapabilitySQLDAO_Update(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	mcsExp := testMachineCapabilitySQLDAOCreateSlice(ctx, t, dbSession)
	mcd := NewMachineCapabilityDAO(dbSession)
	assert.NotNil(t, mcd)

	name := "Corsair Vengeance LPX DDR4"
	frequency := "3200 MHz"
	capacity := "32 GB"
	count := 4
	deviceType := "DPU"

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		ID                 uuid.UUID
		mc                 *MachineCapability
		MachineID          *string
		InstanceTypeID     *uuid.UUID
		Type               *MachineCapabilityType
		Name               *string
		Frequency          *string
		Capacity           *string
		Vendor             *string
		Count              *int
		DeviceType         *MachineCapabilityDeviceType
		Info               map[string]interface{}
		Index              *int
		Threads            *int
		Cores              *int
		HardwareRevision   *string
		InactiveDevices    []int
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "Update instance id",
			ID:                 mcsExp[0].ID,
			mc:                 &mcsExp[0],
			MachineID:          nil,
			InstanceTypeID:     mcsExp[3].InstanceTypeID,
			Type:               nil,
			Name:               nil,
			Frequency:          nil,
			Capacity:           nil,
			Count:              nil,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:           "Update machine ID",
			ID:             mcsExp[3].ID,
			mc:             &mcsExp[3],
			MachineID:      mcsExp[0].MachineID,
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Count:          nil,
			expectedError:  false,
		},
		{
			desc:             "Update Capability Type and Name, Frequency, Capacity, Cores, Threads, HardwareRevision, Count",
			ID:               mcsExp[2].ID,
			mc:               &mcsExp[2],
			MachineID:        nil,
			InstanceTypeID:   nil,
			Type:             cutil.GetPtr(MachineCapabilityTypeMemory),
			Name:             cutil.GetPtr(name),
			Frequency:        cutil.GetPtr(frequency),
			Capacity:         cutil.GetPtr(capacity),
			Count:            cutil.GetPtr(count),
			Cores:            cutil.GetPtr(128),
			Threads:          cutil.GetPtr(256),
			HardwareRevision: cutil.GetPtr("v.12345"),
			Index:            cutil.GetPtr(1000),
			InactiveDevices:  []int{2, 4},
			expectedError:    false,
		},
		{
			desc:           "Update Vendor",
			ID:             mcsExp[3].ID,
			mc:             &mcsExp[3],
			MachineID:      nil,
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         cutil.GetPtr("Test Vendor"),
			Count:          nil,
			expectedError:  false,
		},
		{
			desc:           "Update DeviceType for Network",
			ID:             mcsExp[4].ID,
			mc:             &mcsExp[4],
			MachineID:      nil,
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Vendor:         nil,
			DeviceType:     cutil.GetPtr(MachineCapabilityDeviceType(deviceType)),
			Count:          nil,
			expectedError:  false,
		},
		{
			desc:           "Update to invalid Capability Type",
			ID:             mcsExp[2].ID,
			mc:             &mcsExp[2],
			MachineID:      nil,
			InstanceTypeID: nil,
			Type:           cutil.GetPtr(MachineCapabilityType("invalid-type")),
			Name:           &name,
			Frequency:      &frequency,
			Capacity:       &capacity,
			Count:          &count,
			expectedError:  true,
		},
		{
			desc:           "Update info",
			ID:             mcsExp[2].ID,
			mc:             &mcsExp[2],
			MachineID:      nil,
			InstanceTypeID: nil,
			Type:           nil,
			Name:           nil,
			Frequency:      nil,
			Capacity:       nil,
			Count:          nil,
			Info: map[string]interface{}{
				"test": "test",
			},
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := mcd.Update(ctx, nil,
				MachineCapabilityUpdateInput{
					ID:               tc.ID,
					MachineID:        tc.MachineID,
					InstanceTypeID:   tc.InstanceTypeID,
					Type:             tc.Type,
					Name:             tc.Name,
					Frequency:        tc.Frequency,
					Capacity:         tc.Capacity,
					Vendor:           tc.Vendor,
					Count:            tc.Count,
					DeviceType:       tc.DeviceType,
					Info:             tc.Info,
					Index:            tc.Index,
					Threads:          tc.Threads,
					Cores:            tc.Cores,
					InactiveDevices:  tc.InactiveDevices,
					HardwareRevision: tc.HardwareRevision,
				},
			)

			if tc.expectedError {
				return
			} else {
				assert.NoError(t, err)
			}

			assert.Nil(t, err)
			assert.NotNil(t, got)

			if tc.MachineID != nil {
				assert.Equal(t, *tc.MachineID, *got.MachineID)
			}
			if tc.InstanceTypeID != nil {
				assert.Equal(t, *tc.InstanceTypeID, *got.InstanceTypeID)
			}
			if tc.Type != nil {
				assert.Equal(t, *tc.Type, got.Type)
			}
			if tc.Name != nil {
				assert.Equal(t, *tc.Name, got.Name)
			}
			if tc.Frequency != nil {
				assert.Equal(t, *tc.Frequency, *got.Frequency)
			}
			if tc.Capacity != nil {
				assert.Equal(t, *tc.Capacity, *got.Capacity)
			}
			if tc.Vendor != nil {
				assert.Equal(t, *tc.Vendor, *got.Vendor)
			}
			if tc.Count != nil {
				assert.Equal(t, *tc.Count, *got.Count)
			}
			if tc.DeviceType != nil {
				assert.Equal(t, *tc.DeviceType, *got.DeviceType)
			}
			if tc.Info != nil {
				assert.Equal(t, tc.Info, got.Info)
			}
			if tc.Threads != nil {
				assert.Equal(t, *tc.Threads, *got.Threads)
			}
			if tc.Cores != nil {
				assert.Equal(t, *tc.Cores, *got.Cores)
			}
			if tc.Index != nil {
				assert.Equal(t, *tc.Index, got.Index)
			}
			if tc.HardwareRevision != nil {
				assert.Equal(t, *tc.HardwareRevision, *got.HardwareRevision)
			}
			if tc.InactiveDevices != nil {
				assert.Equal(t, tc.InactiveDevices, got.InactiveDevices)
			}

			assert.NotEqual(t, got.Updated.String(), tc.mc.Updated.String())

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestMachineCapabilitySQLDAO_ClearFromParams(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	mcsExp := testMachineCapabilitySQLDAOCreateSlice(ctx, t, dbSession)
	mcd := NewMachineCapabilityDAO(dbSession)
	assert.NotNil(t, mcd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                string
		mc                  MachineCapability
		paramMachineID      bool
		paramInstanceTypeID bool
		paramFrequency      bool
		paramCapacity       bool
		paramVendor         bool
		paramInfo           bool
		expectError         bool
		expectUpdate        bool
		verifyChildSpanner  bool
	}{
		{
			desc:                "can clear MachineID",
			mc:                  mcsExp[0],
			paramMachineID:      true,
			paramInstanceTypeID: false,
			paramFrequency:      false,
			paramCapacity:       false,
			paramInfo:           false,
			expectError:         false,
			expectUpdate:        true,
			verifyChildSpanner:  true,
		},
		{
			desc:                "cannot clear both InstanceTypeID and MachineID",
			mc:                  mcsExp[0],
			paramMachineID:      true,
			paramInstanceTypeID: true,
			paramFrequency:      false,
			paramCapacity:       false,
			paramInfo:           false,
			expectError:         true,
			expectUpdate:        false,
		},
		{
			desc:                "nop when no cleared fields are specified",
			mc:                  mcsExp[1],
			paramMachineID:      false,
			paramInstanceTypeID: false,
			paramFrequency:      false,
			paramCapacity:       false,
			paramInfo:           false,
			expectError:         false,
			expectUpdate:        true,
		},
		{
			desc:                "can clear capacity, frequency and info",
			mc:                  mcsExp[1],
			paramMachineID:      false,
			paramInstanceTypeID: false,
			paramFrequency:      true,
			paramCapacity:       true,
			paramInfo:           true,
			expectUpdate:        true,
		},
		{
			desc:                "can clear capacity, frequency and info",
			mc:                  mcsExp[1],
			paramMachineID:      false,
			paramInstanceTypeID: false,
			paramFrequency:      false,
			paramCapacity:       false,
			paramVendor:         true,
			paramInfo:           false,
			expectUpdate:        true,
		},
		{
			desc:                "can clear InstanceTypeID",
			mc:                  mcsExp[3],
			paramMachineID:      false,
			paramInstanceTypeID: true,
			paramFrequency:      false,
			paramCapacity:       false,
			paramInfo:           false,
			expectError:         false,
			expectUpdate:        true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := mcd.ClearFromParams(ctx, nil, tc.mc.ID,
				tc.paramMachineID, tc.paramInstanceTypeID, tc.paramFrequency, tc.paramCapacity, tc.paramVendor, tc.paramInfo)

			if tc.expectError {
				assert.NotNil(t, err)
				return
			}

			assert.Nil(t, err)
			assert.NotNil(t, tmp)

			if tc.paramMachineID {
				assert.Nil(t, tmp.MachineID)
			}
			if tc.paramInstanceTypeID {
				assert.Nil(t, tmp.InstanceTypeID)
			}
			if tc.paramFrequency {
				assert.Nil(t, tmp.Frequency)
			}
			if tc.paramCapacity {
				assert.Nil(t, tmp.Capacity)
			}
			if tc.paramVendor {
				assert.Nil(t, tmp.Vendor)
			}
			if tc.paramInfo {
				assert.Nil(t, tmp.Info)
			}
			if tc.expectUpdate {
				assert.True(t, tmp.Updated.After(tc.mc.Updated))
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

func TestMachineCapabilitySQLDAO_DeleteByID(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	mcsExp := testMachineCapabilitySQLDAOCreateSlice(ctx, t, dbSession)

	mcDAO := NewMachineCapabilityDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		mcID               uuid.UUID
		purge              bool
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "test deleting existing object success",
			mcID:               mcsExp[1].ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "test deleting non-existent object success",
			mcID:          uuid.New(),
			expectedError: false,
		},
		{
			desc:          "test deleting existing object with purge success",
			mcID:          mcsExp[2].ID,
			purge:         true,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := mcDAO.DeleteByID(ctx, nil, tc.mcID, tc.purge)

			if tc.expectedError {
				assert.Error(t, err)

				// Check that object was not deleted
				tmp, serr := mcDAO.GetByID(ctx, nil, tc.mcID, nil)
				assert.NoError(t, serr)
				assert.NotNil(t, tmp)
				return
			}

			var res MachineCapability

			if tc.purge {
				err = dbSession.DB.NewSelect().Model(&res).Where("mc.id = ?", tc.mcID).WhereAllWithDeleted().Scan(ctx)
			} else {
				err = dbSession.DB.NewSelect().Model(&res).Where("mc.id = ?", tc.mcID).Scan(ctx)
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

func TestMachineCapability_GetStrInfo(t *testing.T) {
	type fields struct {
		Info map[string]interface{}
	}
	type args struct {
		name string
	}

	tests := []struct {
		name   string
		fields fields
		args   args
		want   *string
	}{
		{
			name: "test existing key returns string value",
			fields: fields{
				Info: map[string]interface{}{
					"foo": "bar",
				},
			},
			args: args{
				name: "foo",
			},
			want: cutil.GetPtr("bar"),
		},
		{
			name: "test non-existent key returns nil",
			fields: fields{
				Info: map[string]interface{}{
					"foo": "bar",
				},
			},
			args: args{
				name: "baz",
			},
			want: nil,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mc := &MachineCapability{
				Info: tt.fields.Info,
			}

			val := mc.GetStrInfo(tt.args.name)
			if tt.want != nil {
				require.NotNil(t, val)
				assert.Equal(t, *tt.want, *val)
			} else {
				assert.Nil(t, val)
			}
		})
	}
}

func TestMachineCapability_GetIntInfo(t *testing.T) {
	type fields struct {
		Info map[string]interface{}
	}
	type args struct {
		name string
	}
	tests := []struct {
		name   string
		fields fields
		args   args
		want   *int
	}{
		{
			name: "test existing key from info read from DB returns int value",
			fields: fields{
				Info: map[string]interface{}{
					"foo": json.Number('5'),
				},
			},
			args: args{
				name: "foo",
			},
			want: cutil.GetPtr(5),
		},
		{
			name: "test existing key from info populated in struct returns int value",
			fields: fields{
				Info: map[string]interface{}{
					"foo": 5,
				},
			},
			args: args{
				name: "foo",
			},
			want: cutil.GetPtr(5),
		},
		{
			name: "test non-existent key returns nil",
			fields: fields{
				Info: map[string]interface{}{
					"foo": 5,
				},
			},
			args: args{
				name: "baz",
			},
			want: nil,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mc := &MachineCapability{
				Info: tt.fields.Info,
			}

			val := mc.GetIntInfo(tt.args.name)
			if tt.want != nil {
				require.NotNil(t, val)
				assert.Equal(t, *tt.want, *val)
			} else {
				assert.Nil(t, val)
			}
		})
	}
}

func TestMachineCapability_ToProto(t *testing.T) {
	count := 4
	cores := 16
	threads := 32
	freq := "3.5GHz"
	vendor := "ACME"
	rev := "v1"

	t.Run("populates all fields from a CPU capability", func(t *testing.T) {
		mc := &MachineCapability{
			Type:             MachineCapabilityTypeCPU,
			Name:             "cpu-0",
			Frequency:        &freq,
			Vendor:           &vendor,
			HardwareRevision: &rev,
			Count:            &count,
			Cores:            &cores,
			Threads:          &threads,
		}
		proto := mc.ToProto()
		require.NotNil(t, proto)
		assert.Equal(t, corev1.MachineCapabilityType_CAP_TYPE_CPU, proto.CapabilityType)
		require.NotNil(t, proto.Name)
		assert.Equal(t, "cpu-0", *proto.Name)
		require.NotNil(t, proto.Frequency)
		assert.Equal(t, freq, *proto.Frequency)
		require.NotNil(t, proto.Vendor)
		assert.Equal(t, vendor, *proto.Vendor)
		require.NotNil(t, proto.HardwareRevision)
		assert.Equal(t, rev, *proto.HardwareRevision)
		require.NotNil(t, proto.Count)
		assert.Equal(t, uint32(4), *proto.Count)
		require.NotNil(t, proto.Cores)
		assert.Equal(t, uint32(16), *proto.Cores)
		require.NotNil(t, proto.Threads)
		assert.Equal(t, uint32(32), *proto.Threads)
		assert.Nil(t, proto.DeviceType)
		assert.Nil(t, proto.InactiveDevices)
	})

	deviceTypeCases := []struct {
		name       string
		deviceType MachineCapabilityDeviceType
		want       corev1.MachineCapabilityDeviceType
	}{
		{
			name:       "maps Network + DPU device type to the proto enum",
			deviceType: MachineCapabilityDeviceTypeDPU,
			want:       corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU,
		},
		{
			name:       "maps Network + SPX device type to the proto enum",
			deviceType: MachineCapabilityDeviceTypeSPX,
			want:       corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPX,
		},
	}
	for _, tc := range deviceTypeCases {
		t.Run(tc.name, func(t *testing.T) {
			mc := &MachineCapability{
				Type:       MachineCapabilityTypeNetwork,
				Name:       "ConnectX-8",
				DeviceType: &tc.deviceType,
			}
			proto := mc.ToProto()
			assert.Equal(t, corev1.MachineCapabilityType_CAP_TYPE_NETWORK, proto.CapabilityType)
			require.NotNil(t, proto.DeviceType)
			assert.Equal(t, tc.want, *proto.DeviceType)
		})
	}

	t.Run("maps InfiniBand InactiveDevices into a Uint32List", func(t *testing.T) {
		mc := &MachineCapability{
			Type:            MachineCapabilityTypeInfiniBand,
			Name:            "ib-0",
			InactiveDevices: []int{2, 5},
		}
		proto := mc.ToProto()
		require.NotNil(t, proto.InactiveDevices)
		assert.Equal(t, []uint32{2, 5}, proto.InactiveDevices.Items)
	})

	t.Run("unknown type leaves CapabilityType as zero, unknown device type drops to nil", func(t *testing.T) {
		unknown := MachineCapabilityDeviceType("Unknown")
		mc := &MachineCapability{Type: "Mystery", Name: "x", DeviceType: &unknown}
		proto := mc.ToProto()
		assert.Equal(t, corev1.MachineCapabilityType(0), proto.CapabilityType)
		assert.Nil(t, proto.DeviceType)
	})
}

func TestMachineCapability_FromProto(t *testing.T) {
	name := "cpu-0"
	freq := "3.5GHz"
	capacity := "1024"
	vendor := "ACME"
	hwRev := "v1"
	var count, cores, threads uint32 = 8, 16, 32
	deviceType := corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU

	t.Run("nil attrs is no-op", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeGPU}
		mc.FromProto(nil, 5)
		assert.Equal(t, MachineCapabilityTypeGPU, mc.Type)
		assert.Equal(t, 0, mc.Index)
	})

	t.Run("happy path with all fields", func(t *testing.T) {
		mc := &MachineCapability{}
		mc.FromProto(&corev1.InstanceTypeMachineCapabilityFilterAttributes{
			CapabilityType:   corev1.MachineCapabilityType_CAP_TYPE_CPU,
			Name:             &name,
			Frequency:        &freq,
			Capacity:         &capacity,
			Vendor:           &vendor,
			HardwareRevision: &hwRev,
			Count:            &count,
			Cores:            &cores,
			Threads:          &threads,
			DeviceType:       &deviceType,
			InactiveDevices:  &corev1.Uint32List{Items: []uint32{0, 1}},
		}, 7)

		assert.Equal(t, MachineCapabilityTypeCPU, mc.Type)
		assert.Equal(t, "cpu-0", mc.Name)
		assert.Equal(t, &freq, mc.Frequency)
		assert.Equal(t, &capacity, mc.Capacity)
		assert.Equal(t, &vendor, mc.Vendor)
		assert.Equal(t, &hwRev, mc.HardwareRevision)
		require.NotNil(t, mc.Count)
		assert.Equal(t, 8, *mc.Count)
		require.NotNil(t, mc.Cores)
		assert.Equal(t, 16, *mc.Cores)
		require.NotNil(t, mc.Threads)
		assert.Equal(t, 32, *mc.Threads)
		require.NotNil(t, mc.DeviceType)
		assert.Equal(t, MachineCapabilityDeviceTypeDPU, *mc.DeviceType)
		assert.Equal(t, []int{0, 1}, mc.InactiveDevices)
		assert.Equal(t, 7, mc.Index)
	})

	t.Run("unknown CapabilityType leaves Type empty (caller must Validate)", func(t *testing.T) {
		mc := &MachineCapability{}
		mc.FromProto(&corev1.InstanceTypeMachineCapabilityFilterAttributes{
			CapabilityType: corev1.MachineCapabilityType(9999),
			Name:           &name,
		}, 0)
		assert.Equal(t, MachineCapabilityType(""), mc.Type)
		assert.Equal(t, "cpu-0", mc.Name)
	})

	t.Run("nil Name leaves Name empty (caller must Validate)", func(t *testing.T) {
		mc := &MachineCapability{}
		mc.FromProto(&corev1.InstanceTypeMachineCapabilityFilterAttributes{
			CapabilityType: corev1.MachineCapabilityType_CAP_TYPE_CPU,
		}, 0)
		assert.Equal(t, MachineCapabilityTypeCPU, mc.Type)
		assert.Equal(t, "", mc.Name)
	})

	deviceTypeCases := []struct {
		name           string
		capabilityType corev1.MachineCapabilityType
		deviceType     corev1.MachineCapabilityDeviceType
		want           MachineCapabilityDeviceType
	}{
		{
			name:           "unknown DeviceType remains present for caller validation",
			capabilityType: corev1.MachineCapabilityType_CAP_TYPE_GPU,
			deviceType:     corev1.MachineCapabilityDeviceType(9999),
			want:           MachineCapabilityDeviceType(""),
		},
		{
			name:           "SPX DeviceType is preserved",
			capabilityType: corev1.MachineCapabilityType_CAP_TYPE_NETWORK,
			deviceType:     corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPX,
			want:           MachineCapabilityDeviceTypeSPX,
		},
	}
	for _, tc := range deviceTypeCases {
		t.Run(tc.name, func(t *testing.T) {
			mc := &MachineCapability{}
			mc.FromProto(&corev1.InstanceTypeMachineCapabilityFilterAttributes{
				CapabilityType: tc.capabilityType,
				Name:           &name,
				DeviceType:     &tc.deviceType,
			}, 0)
			require.NotNil(t, mc.DeviceType)
			assert.Equal(t, tc.want, *mc.DeviceType)
		})
	}
}

func TestMachineCapability_Validate(t *testing.T) {
	dpu := MachineCapabilityDeviceTypeDPU
	nvlink := MachineCapabilityDeviceTypeNVLink
	spx := MachineCapabilityDeviceTypeSPX

	t.Run("populated capability is valid", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeCPU, Name: "cpu-0"}
		assert.NoError(t, mc.Validate())
	})
	t.Run("empty Type errors", func(t *testing.T) {
		mc := &MachineCapability{Type: "", Name: "cpu-0"}
		assert.Error(t, mc.Validate())
	})
	t.Run("invalid Type errors", func(t *testing.T) {
		mc := &MachineCapability{Type: "Bogus", Name: "cpu-0"}
		assert.Error(t, mc.Validate())
	})
	t.Run("empty Name errors", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeCPU, Name: ""}
		assert.Error(t, mc.Validate())
	})
	t.Run("Name with leading whitespace errors", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeCPU, Name: " cpu-0"}
		assert.Error(t, mc.Validate())
	})
	t.Run("Name with trailing whitespace errors", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeCPU, Name: "cpu-0 "}
		assert.Error(t, mc.Validate())
	})
	t.Run("single-character Name errors (too short)", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeCPU, Name: "x"}
		assert.Error(t, mc.Validate())
	})

	deviceTypeCases := []struct {
		name       string
		capType    MachineCapabilityType
		capName    string
		deviceType *MachineCapabilityDeviceType
		wantError  bool
	}{
		{name: "Network with DPU device type is valid", capType: MachineCapabilityTypeNetwork, capName: "net-0", deviceType: &dpu},
		{name: "Network with SPX device type is valid", capType: MachineCapabilityTypeNetwork, capName: "net-0", deviceType: &spx},
		{name: "Network with NVLink device type errors", capType: MachineCapabilityTypeNetwork, capName: "net-0", deviceType: &nvlink, wantError: true},
		{name: "GPU with NVLink device type is valid", capType: MachineCapabilityTypeGPU, capName: "gpu-0", deviceType: &nvlink},
		{name: "GPU with DPU device type errors", capType: MachineCapabilityTypeGPU, capName: "gpu-0", deviceType: &dpu, wantError: true},
		{name: "CPU with any device type errors", capType: MachineCapabilityTypeCPU, capName: "cpu-0", deviceType: &dpu, wantError: true},
	}
	for _, tc := range deviceTypeCases {
		t.Run(tc.name, func(t *testing.T) {
			mc := &MachineCapability{Type: tc.capType, Name: tc.capName, DeviceType: tc.deviceType}
			err := mc.Validate()
			if tc.wantError {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}

	t.Run("InfiniBand with InactiveDevices is valid", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeInfiniBand, Name: "ib-0", InactiveDevices: []int{0, 1}}
		assert.NoError(t, mc.Validate())
	})
	t.Run("CPU with InactiveDevices errors", func(t *testing.T) {
		mc := &MachineCapability{Type: MachineCapabilityTypeCPU, Name: "cpu-0", InactiveDevices: []int{0, 1}}
		assert.Error(t, mc.Validate())
	})
}

func TestMachineCapabilitySQLDAO_GetGPUStatsBySite(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "gpu-stats-ip")
	site1 := testInstanceTypeBuildSite(t, dbSession, ip, "gpu-stats-site-1")
	site2 := testInstanceTypeBuildSite(t, dbSession, ip, "gpu-stats-site-2")

	// A second provider/site to confirm provider scoping excludes other providers.
	otherIP := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "gpu-stats-other-ip")
	otherSite := testInstanceTypeBuildSite(t, dbSession, otherIP, "gpu-stats-other-site")

	mA := testMachineBuildMachine(t, dbSession, ip.ID, site1.ID, nil, nil)
	mB := testMachineBuildMachine(t, dbSession, ip.ID, site1.ID, nil, nil)
	mC := testMachineBuildMachine(t, dbSession, ip.ID, site1.ID, nil, nil)
	mD := testMachineBuildMachine(t, dbSession, ip.ID, site2.ID, nil, nil)
	mE := testMachineBuildMachine(t, dbSession, otherIP.ID, otherSite.ID, nil, nil)

	mcDAO := NewMachineCapabilityDAO(dbSession)

	buildGPU := func(machineID, name string, count *int) *MachineCapability {
		mc, err := mcDAO.Create(ctx, nil, MachineCapabilityCreateInput{
			MachineID: &machineID,
			Type:      MachineCapabilityTypeGPU,
			Name:      name,
			Count:     count,
		})
		require.Nil(t, err)
		return mc
	}

	const h100 = "NVIDIA H100"
	const a100 = "NVIDIA A100"

	// site1: H100 on mA(8), mB(8), mC(nil -> 1 via COALESCE); A100 on mB(4)
	buildGPU(mA.ID, h100, cutil.GetPtr(8))
	buildGPU(mB.ID, h100, cutil.GetPtr(8))
	buildGPU(mB.ID, a100, cutil.GetPtr(4))
	buildGPU(mC.ID, h100, nil)

	// non-GPU capability must be excluded by the type filter
	_, err := mcDAO.Create(ctx, nil, MachineCapabilityCreateInput{
		MachineID: &mC.ID,
		Type:      MachineCapabilityTypeCPU,
		Name:      "Intel Xeon",
		Count:     cutil.GetPtr(2),
	})
	require.Nil(t, err)

	// site2: H100 on mD(8); a soft-deleted GPU capability must be excluded
	buildGPU(mD.ID, h100, cutil.GetPtr(8))
	deletedCap := buildGPU(mD.ID, h100, cutil.GetPtr(100))
	require.Nil(t, mcDAO.DeleteByID(ctx, nil, deletedCap.ID, false))

	// other provider
	buildGPU(mE.ID, h100, cutil.GetPtr(16))

	toMap := func(rows []GPUSiteStat) map[uuid.UUID]map[string]GPUSiteStat {
		m := map[uuid.UUID]map[string]GPUSiteStat{}
		for _, r := range rows {
			if m[r.SiteID] == nil {
				m[r.SiteID] = map[string]GPUSiteStat{}
			}
			m[r.SiteID][r.Name] = r
		}
		return m
	}

	t.Run("provider-wide aggregation", func(t *testing.T) {
		rows, err := mcDAO.GetGPUStatsBySite(ctx, nil, &ip.ID, nil)
		require.Nil(t, err)
		m := toMap(rows)

		// Only the two sites belonging to this provider are present.
		assert.Len(t, m, 2)

		assert.Equal(t, 17, m[site1.ID][h100].GPUs)
		assert.Equal(t, 3, m[site1.ID][h100].Machines)
		assert.Equal(t, 4, m[site1.ID][a100].GPUs)
		assert.Equal(t, 1, m[site1.ID][a100].Machines)

		assert.Equal(t, 8, m[site2.ID][h100].GPUs)
		assert.Equal(t, 1, m[site2.ID][h100].Machines)
	})

	t.Run("site-scoped aggregation", func(t *testing.T) {
		rows, err := mcDAO.GetGPUStatsBySite(ctx, nil, &ip.ID, &site1.ID)
		require.Nil(t, err)
		m := toMap(rows)

		assert.Len(t, m, 1)
		assert.Equal(t, 17, m[site1.ID][h100].GPUs)
		assert.Equal(t, 4, m[site1.ID][a100].GPUs)
	})
}
