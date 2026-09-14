// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
)

func TestExpectedMachine_FromProto(t *testing.T) {
	id := uuid.New()
	siteID := uuid.New()
	linkedMachineID := uuid.New().String()
	skuID := "sku-1"
	rackID := "rack-1"
	name := "machine-1"
	manufacturer := "ACME"
	model := "M1"
	description := "primary"
	bmcIP := "10.0.0.1"
	var slot, trayIdx, host int32 = 1, 2, 3

	t.Run("nil proto leaves receiver unchanged", func(t *testing.T) {
		em := &ExpectedMachine{ID: id, SiteID: siteID, BmcMacAddress: "aa:bb"}
		em.FromProto(nil, nil)

		assert.Equal(t, id, em.ID)
		assert.Equal(t, siteID, em.SiteID)
		assert.Equal(t, "aa:bb", em.BmcMacAddress)
	})

	t.Run("invalid id leaves em.ID unchanged", func(t *testing.T) {
		em := &ExpectedMachine{ID: id}
		em.FromProto(&corev1.ExpectedMachine{
			Id:            &corev1.UUID{Value: "not-a-uuid"},
			BmcMacAddress: "aa:bb",
		}, nil)

		assert.Equal(t, id, em.ID)
		assert.Equal(t, "aa:bb", em.BmcMacAddress)
	})

	t.Run("populates all proto fields", func(t *testing.T) {
		em := &ExpectedMachine{}
		em.FromProto(&corev1.ExpectedMachine{
			Id:                       &corev1.UUID{Value: id.String()},
			BmcMacAddress:            "aa:bb:cc:dd:ee:ff",
			ChassisSerialNumber:      "CSN-1",
			SkuId:                    &skuID,
			FallbackDpuSerialNumbers: []string{"dpu-1", "dpu-2"},
			BmcIpAddress:             &bmcIP,
			RackId:                   &corev1.RackId{Id: rackID},
			Name:                     &name,
			Manufacturer:             &manufacturer,
			Model:                    &model,
			Description:              &description,
			SlotId:                   &slot,
			TrayIdx:                  &trayIdx,
			HostId:                   &host,
			Metadata: &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "env", Value: cutil.GetPtr("prod")},
				},
			},
		}, &linkedMachineID)

		assert.Equal(t, id, em.ID)
		assert.Equal(t, "aa:bb:cc:dd:ee:ff", em.BmcMacAddress)
		assert.Equal(t, "CSN-1", em.ChassisSerialNumber)
		assert.Equal(t, &skuID, em.SkuID)
		assert.Equal(t, &linkedMachineID, em.MachineID)
		assert.Equal(t, []string{"dpu-1", "dpu-2"}, em.FallbackDpuSerialNumbers)
		assert.Equal(t, &bmcIP, em.BmcIpAddress)
		if assert.NotNil(t, em.RackID) {
			assert.Equal(t, rackID, *em.RackID)
		}
		assert.Equal(t, &name, em.Name)
		assert.Equal(t, &manufacturer, em.Manufacturer)
		assert.Equal(t, &model, em.Model)
		assert.Equal(t, &description, em.Description)
		assert.Equal(t, &slot, em.SlotID)
		assert.Equal(t, &trayIdx, em.TrayIdx)
		assert.Equal(t, &host, em.HostID)
		assert.Equal(t, Labels{"env": "prod"}, em.Labels)
	})

	t.Run("populates dpfEnabled from is_dpf_enabled", func(t *testing.T) {
		em := &ExpectedMachine{}
		enabled := false
		em.FromProto(&corev1.ExpectedMachine{
			Id:            &corev1.UUID{Value: id.String()},
			BmcMacAddress: "aa:bb",
			IsDpfEnabled:  &enabled,
		}, nil)

		if assert.NotNil(t, em.IsDpfEnabled) {
			assert.False(t, *em.IsDpfEnabled)
		}
	})

	t.Run("nil linkedMachineID leaves MachineID nil", func(t *testing.T) {
		em := &ExpectedMachine{}
		em.FromProto(&corev1.ExpectedMachine{
			Id:            &corev1.UUID{Value: id.String()},
			BmcMacAddress: "aa:bb",
		}, nil)

		assert.Nil(t, em.MachineID)
	})

	t.Run("nil RackId clears em.RackID", func(t *testing.T) {
		stale := "stale-rack"
		em := &ExpectedMachine{RackID: &stale}
		em.FromProto(&corev1.ExpectedMachine{
			Id:            &corev1.UUID{Value: id.String()},
			BmcMacAddress: "aa:bb",
		}, nil)

		assert.Nil(t, em.RackID)
	})

	t.Run("populates host_lifecycle_profile", func(t *testing.T) {
		em := &ExpectedMachine{}
		em.FromProto(&corev1.ExpectedMachine{
			Id:                   &corev1.UUID{Value: id.String()},
			BmcMacAddress:        "aa:bb",
			HostLifecycleProfile: &corev1.HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
		}, nil)

		if assert.NotNil(t, em.HostLifecycleProfile.DisableLockdown) {
			assert.Equal(t, true, *em.HostLifecycleProfile.DisableLockdown)
		}
	})

	t.Run("missing host_lifecycle_profile clears the field", func(t *testing.T) {
		em := &ExpectedMachine{HostLifecycleProfile: HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)}}
		em.FromProto(&corev1.ExpectedMachine{
			Id:            &corev1.UUID{Value: id.String()},
			BmcMacAddress: "aa:bb",
		}, nil)

		assert.Nil(t, em.HostLifecycleProfile.DisableLockdown)
	})
}

func TestHostLifecycleProfile_ToProto(t *testing.T) {
	t.Run("unset profile maps to nil proto", func(t *testing.T) {
		assert.Nil(t, HostLifecycleProfile{}.ToProto())
	})

	t.Run("disableLockdown true maps to populated proto", func(t *testing.T) {
		got := HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)}.ToProto()
		if assert.NotNil(t, got) {
			assert.Equal(t, true, got.GetDisableLockdown())
		}
	})

	t.Run("disableLockdown false maps to populated proto", func(t *testing.T) {
		got := HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)}.ToProto()
		if assert.NotNil(t, got) {
			assert.NotNil(t, got.DisableLockdown)
			assert.Equal(t, false, got.GetDisableLockdown())
		}
	})
}

func TestHostLifecycleProfile_FromProto(t *testing.T) {
	t.Run("nil proto clears receiver", func(t *testing.T) {
		h := HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)}
		h.FromProto(nil)
		assert.Nil(t, h.DisableLockdown)
	})

	t.Run("populates disableLockdown", func(t *testing.T) {
		h := HostLifecycleProfile{}
		h.FromProto(&corev1.HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)})
		if assert.NotNil(t, h.DisableLockdown) {
			assert.Equal(t, true, *h.DisableLockdown)
		}
	})
}

func TestExpectedMachine_ToProto_HostLifecycleProfile(t *testing.T) {
	t.Run("sets host_lifecycle_profile when disableLockdown present", func(t *testing.T) {
		em := &ExpectedMachine{
			ID:                   uuid.New(),
			BmcMacAddress:        "aa:bb:cc:dd:ee:ff",
			ChassisSerialNumber:  "CSN-1",
			HostLifecycleProfile: HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
		}
		proto := em.ToProto(ExpectedMachineCredentials{})
		if assert.NotNil(t, proto.GetHostLifecycleProfile()) {
			assert.Equal(t, true, proto.GetHostLifecycleProfile().GetDisableLockdown())
		}
	})

	t.Run("leaves host_lifecycle_profile unset when no setting present", func(t *testing.T) {
		em := &ExpectedMachine{
			ID:                  uuid.New(),
			BmcMacAddress:       "aa:bb:cc:dd:ee:ff",
			ChassisSerialNumber: "CSN-1",
		}
		proto := em.ToProto(ExpectedMachineCredentials{})
		assert.Nil(t, proto.GetHostLifecycleProfile())
	})
}

func TestExpectedMachine_ToProto(t *testing.T) {
	id := uuid.New()
	enabled := true
	disabled := false

	t.Run("sets is_dpf_enabled when stored", func(t *testing.T) {
		em := &ExpectedMachine{
			ID:                  id,
			BmcMacAddress:       "aa:bb:cc:dd:ee:ff",
			ChassisSerialNumber: "CSN-1",
			IsDpfEnabled:        &enabled,
		}
		proto := em.ToProto(ExpectedMachineCredentials{})
		assert.True(t, *proto.IsDpfEnabled)
	})

	t.Run("omits is_dpf_enabled when unset", func(t *testing.T) {
		em := &ExpectedMachine{
			ID:                  id,
			BmcMacAddress:       "aa:bb:cc:dd:ee:ff",
			ChassisSerialNumber: "CSN-1",
		}
		proto := em.ToProto(ExpectedMachineCredentials{})
		assert.Nil(t, proto.IsDpfEnabled)
		assert.False(t, proto.DpfEnabled)
	})

	t.Run("forwards false value", func(t *testing.T) {
		em := &ExpectedMachine{
			ID:                  id,
			BmcMacAddress:       "aa:bb:cc:dd:ee:ff",
			ChassisSerialNumber: "CSN-1",
			IsDpfEnabled:        &disabled,
		}
		proto := em.ToProto(ExpectedMachineCredentials{})
		if assert.NotNil(t, proto.IsDpfEnabled) {
			assert.False(t, *proto.IsDpfEnabled)
		}
		assert.False(t, proto.DpfEnabled)
	})
}

// reset the tables needed for ExpectedMachine tests
func testExpectedMachineSetupSchema(t *testing.T, dbSession *db.Session) {
	ctx := context.Background()
	// create User table
	err := dbSession.DB.ResetModel(ctx, (*User)(nil))
	assert.Nil(t, err)
	// create InfrastructureProvider table
	err = dbSession.DB.ResetModel(ctx, (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(ctx, (*Site)(nil))
	assert.Nil(t, err)
	// create InstanceType table (required by Machine)
	err = dbSession.DB.ResetModel(ctx, (*InstanceType)(nil))
	assert.Nil(t, err)
	// create SKU table
	err = dbSession.DB.ResetModel(ctx, (*SKU)(nil))
	assert.Nil(t, err)
	// create Machine table (must be before ExpectedMachine due to foreign key)
	err = dbSession.DB.ResetModel(ctx, (*Machine)(nil))
	assert.Nil(t, err)
	// create ExpectedMachine table
	err = dbSession.DB.ResetModel(ctx, (*ExpectedMachine)(nil))
	assert.Nil(t, err)

	// Add deferrable unique constraint for (bmc_mac_address, site_id) combination
	// This constraint is defined in migration 20260112100000_expected_machine_unique_bmc_site.go
	// DEFERRABLE INITIALLY DEFERRED allows batch operations like MAC swaps
	_, err = dbSession.DB.Exec("ALTER TABLE expected_machine DROP CONSTRAINT IF EXISTS expected_machine_bmc_mac_address_site_id_key")
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE expected_machine ADD CONSTRAINT expected_machine_bmc_mac_address_site_id_key UNIQUE (bmc_mac_address, site_id) DEFERRABLE INITIALLY DEFERRED")
	assert.Nil(t, err)
}

func TestExpectedMachineSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	emsd := NewExpectedMachineDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	// Create a second site for cross-site tests
	site2 := TestBuildSite(t, dbSession, ip, "test-site-2", user)

	// Pre-create an ExpectedMachine to test duplicate MAC constraint
	testMacAddress := "AA:BB:CC:DD:EE:FF"
	_, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       testMacAddress,
		ChassisSerialNumber: "CHASSIS-PREEXISTING",
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)

	tests := []struct {
		desc               string
		inputs             []ExpectedMachineCreateInput
		expectError        bool
		errorContains      string
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			inputs: []ExpectedMachineCreateInput{
				{
					ExpectedMachineID:        uuid.New(),
					SiteID:                   site.ID,
					BmcMacAddress:            "00:1B:44:11:3A:B7",
					ChassisSerialNumber:      "CHASSIS123",
					FallbackDpuSerialNumbers: []string{"DPU001", "DPU002"},
					BmcIpAddress:             cutil.GetPtr("192.168.1.10"),
					Labels: map[string]string{
						"environment": "test",
						"location":    "datacenter1",
					},
					CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with nullable fields",
			inputs: []ExpectedMachineCreateInput{
				{
					ExpectedMachineID:        uuid.New(),
					SiteID:                   site.ID,
					BmcMacAddress:            "00:1B:44:11:3A:B8",
					ChassisSerialNumber:      "CHASSIS789",
					FallbackDpuSerialNumbers: []string{"DPU003"},
					Labels: map[string]string{
						"environment": "production",
					},
					CreatedBy: user.ID,
				},
				{
					ExpectedMachineID:        uuid.New(),
					SiteID:                   site.ID,
					BmcMacAddress:            "00:1B:44:11:3A:B9",
					ChassisSerialNumber:      "CHASSIS456",
					FallbackDpuSerialNumbers: nil,
					Labels:                   nil,
					CreatedBy:                user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "fail to create duplicate MAC address in same site",
			inputs: []ExpectedMachineCreateInput{
				{
					ExpectedMachineID:   uuid.New(),
					SiteID:              site.ID,
					BmcMacAddress:       testMacAddress,
					ChassisSerialNumber: "CHASSIS-DUPLICATE",
					CreatedBy:           user.ID,
				},
			},
			expectError:   true,
			errorContains: "duplicate key value",
		},
		{
			desc: "succeed creating same MAC address in different site",
			inputs: []ExpectedMachineCreateInput{
				{
					ExpectedMachineID:   uuid.New(),
					SiteID:              site2.ID,
					BmcMacAddress:       testMacAddress,
					ChassisSerialNumber: "CHASSIS-DIFFERENT-SITE",
					CreatedBy:           user.ID,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				em, err := emsd.Create(ctx, nil, input)
				if err != nil {
					assert.True(t, tc.expectError, "Expected error but got none")
					assert.Nil(t, em)
					if tc.errorContains != "" {
						assert.Contains(t, err.Error(), tc.errorContains, "Error should contain expected substring")
					}
				} else {
					assert.False(t, tc.expectError, "Expected success but got error: %v", err)
					assert.NotNil(t, em)
					assert.Equal(t, input.BmcMacAddress, em.BmcMacAddress)
					assert.Equal(t, input.ChassisSerialNumber, em.ChassisSerialNumber)
					assert.Equal(t, input.FallbackDpuSerialNumbers, em.FallbackDpuSerialNumbers)
					assert.Equal(t, input.BmcIpAddress, em.BmcIpAddress)
					assert.Equal(t, Labels(input.Labels), em.Labels)
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
			}
		})
	}
}

func testExpectedMachineSQLDAOCreateExpectedMachines(ctx context.Context, t *testing.T, dbSession *db.Session) (created []ExpectedMachine) {
	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	// Create a SKU and assign it to one ExpectedMachine to exercise includeRelations later
	ssd := NewSkuDAO(dbSession)
	sku, err := ssd.Create(ctx, nil, SkuCreateInput{SkuID: "sku-test-1", Components: &SkuComponents{}, SiteID: site.ID})
	assert.NoError(t, err)
	assert.NotNil(t, sku)

	var createInputs []ExpectedMachineCreateInput
	{
		// ExpectedMachine set 1
		createInputs = append(createInputs, ExpectedMachineCreateInput{
			ExpectedMachineID:        uuid.New(),
			SiteID:                   site.ID,
			BmcMacAddress:            "00:1B:44:11:3A:B7",
			ChassisSerialNumber:      "CHASSIS123",
			FallbackDpuSerialNumbers: []string{"DPU001", "DPU002"},
			Labels: map[string]string{
				"environment": "test",
				"location":    "datacenter1",
			},
			CreatedBy: user.ID,
		})

		// ExpectedMachine set 2
		createInputs = append(createInputs, ExpectedMachineCreateInput{
			ExpectedMachineID:        uuid.New(),
			SiteID:                   site.ID,
			BmcMacAddress:            "00:1B:44:11:3A:B8",
			ChassisSerialNumber:      "CHASSIS789",
			SkuID:                    &sku.ID,
			FallbackDpuSerialNumbers: []string{"DPU003"},
			Labels: map[string]string{
				"environment": "production",
			},
			CreatedBy: user.ID,
		})

		// ExpectedMachine set 3
		createInputs = append(createInputs, ExpectedMachineCreateInput{
			ExpectedMachineID:        uuid.New(),
			SiteID:                   site.ID,
			BmcMacAddress:            "00:1B:44:11:3A:B9",
			ChassisSerialNumber:      "CHASSIS456",
			FallbackDpuSerialNumbers: nil,
			Labels:                   nil,
			CreatedBy:                user.ID,
		})
	}

	emsd := NewExpectedMachineDAO(dbSession)

	// ExpectedMachine created
	for _, input := range createInputs {
		emCre, _ := emsd.Create(ctx, nil, input)
		assert.NotNil(t, emCre)
		created = append(created, *emCre)
	}

	return
}

func TestExpectedMachineSQLDAO_GetDistinctLabelKeys(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)
	created := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)
	dao := NewExpectedMachineDAO(dbSession)
	_, err := dao.Update(ctx, nil, ExpectedMachineUpdateInput{
		ExpectedMachineID: created[0].ID,
		Labels: map[string]string{
			"environment":               "test",
			"location":                  "datacenter1",
			"nvidia.com/failure-domain": "fd-a",
		},
	})
	require.NoError(t, err)

	tests := []struct {
		name      string
		filter    ExpectedMachineFilterInput
		page      paginator.PageInput
		want      []string
		wantTotal int
	}{
		{name: "distinct sorted keys", want: []string{"environment", "location", "nvidia.com/failure-domain"}, wantTotal: 3},
		{
			name: "descending second result",
			page: paginator.PageInput{
				Offset: cutil.GetPtr(1), Limit: cutil.GetPtr(1),
				OrderBy: &paginator.OrderBy{Field: LabelKeyOrderByDefault, Order: paginator.OrderDescending},
			},
			want: []string{"location"}, wantTotal: 3,
		},
		{name: "empty site scope", filter: ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{}}, want: []string{}, wantTotal: 0},
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

func TestExpectedMachineSQLDAO_GetDistinctLabelValues(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)
	created := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)
	dao := NewExpectedMachineDAO(dbSession)
	_, err := dao.Update(ctx, nil, ExpectedMachineUpdateInput{
		ExpectedMachineID: created[0].ID,
		Labels: map[string]string{
			"environment":               "test",
			"location":                  "datacenter1",
			"nvidia.com/failure-domain": "fd-a",
		},
	})
	assert.NoError(t, err)

	tests := []struct {
		name      string
		labelKey  string
		filter    ExpectedMachineFilterInput
		page      paginator.PageInput
		want      []string
		wantTotal int
	}{
		{name: "distinct sorted values", labelKey: "environment", want: []string{"production", "test"}, wantTotal: 2},
		{name: "missing key", labelKey: "missing", want: []string{}, wantTotal: 0},
		{name: "slash in key", labelKey: "nvidia.com/failure-domain", want: []string{"fd-a"}, wantTotal: 1},
		{name: "site scope", labelKey: "location", filter: ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{created[0].SiteID}}, want: []string{"datacenter1"}, wantTotal: 1},
		{
			name: "descending second result", labelKey: "environment",
			page: paginator.PageInput{
				Offset: cutil.GetPtr(1), Limit: cutil.GetPtr(1),
				OrderBy: &paginator.OrderBy{Field: LabelValueOrderByDefault, Order: paginator.OrderDescending},
			},
			want: []string{"production"}, wantTotal: 2,
		},
		{name: "empty site scope", labelKey: "environment", filter: ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{}}, want: []string{}, wantTotal: 0},
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

func TestExpectedMachineSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	emsExp := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)
	emsd := NewExpectedMachineDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		em                 ExpectedMachine
		expectError        bool
		expectedErrVal     error
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when ExpectedMachine exists on [0]",
			em:                 emsExp[0],
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "GetById success when ExpectedMachine exists on [1]",
			em:          emsExp[1],
			expectError: false,
		},
		{
			desc: "GetById success when ExpectedMachine not found",
			em: ExpectedMachine{
				ID: uuid.New(),
			},
			expectError:    true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := emsd.Get(ctx, nil, tc.em.ID, nil, false)
			assert.Equal(t, tc.expectError, err != nil)
			if tc.expectError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.Equal(t, tc.em.ID, tmp.ID)
				assert.Equal(t, tc.em.BmcMacAddress, tmp.BmcMacAddress)
				assert.Equal(t, tc.em.ChassisSerialNumber, tmp.ChassisSerialNumber)
				assert.Equal(t, tc.em.FallbackDpuSerialNumbers, tmp.FallbackDpuSerialNumbers)
				assert.Equal(t, tc.em.Labels, tmp.Labels)
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

func TestExpectedMachineSQLDAO_Get_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	created := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)
	req := NewExpectedMachineDAO(dbSession)

	got, err := req.Get(ctx, nil, created[1].ID, []string{SiteRelationName, SkuRelationName}, false)
	assert.NoError(t, err)
	assert.NotNil(t, got)
	assert.NotNil(t, got.Site)
	assert.NotNil(t, got.Sku)
}

func TestExpectedMachineSQLDAO_Get_MachineRelation(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	// Create Machine first
	machine := &Machine{
		ID:                       uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(machine).Exec(ctx)
	assert.NoError(t, err)

	// Create ExpectedMachine with MachineID reference
	emsd := NewExpectedMachineDAO(dbSession)
	em, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:B7",
		ChassisSerialNumber: "CHASSIS-TEST-001",
		MachineID:           &machine.ID,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)
	assert.NotNil(t, em)
	assert.Equal(t, &machine.ID, em.MachineID)

	// Query ExpectedMachine with Machine relation
	got, err := emsd.Get(ctx, nil, em.ID, []string{MachineRelationName}, false)
	assert.NoError(t, err)
	assert.NotNil(t, got)
	assert.NotNil(t, got.Machine, "Machine relation should be populated")
	assert.Equal(t, machine.ID, got.Machine.ID)
}

func TestExpectedMachineSQLDAO_Get_MachineRelation_NoMatch(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	// Create ExpectedMachine without MachineID reference
	emsd := NewExpectedMachineDAO(dbSession)
	em, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:B7",
		ChassisSerialNumber: "CHASSIS-TEST-002",
		MachineID:           nil,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)
	assert.NotNil(t, em)
	assert.Nil(t, em.MachineID)

	// Create Machine but don't associate it
	machine := &Machine{
		ID:                       uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusReady,
	}
	_, err = dbSession.DB.NewInsert().Model(machine).Exec(ctx)
	assert.NoError(t, err)

	// Query ExpectedMachine with Machine relation
	got, err := emsd.Get(ctx, nil, em.ID, []string{MachineRelationName}, false)
	assert.NoError(t, err)
	assert.NotNil(t, got)
	assert.Nil(t, got.Machine, "Machine relation should be nil when MachineID is null")
}

func TestExpectedMachineSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	emsd := NewExpectedMachineDAO(dbSession)

	// Create test data
	created := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		filter             ExpectedMachineFilterInput
		pageInput          paginator.PageInput
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns all objects",
			expectedCount:      3,
			expectedTotal:      cutil.GetPtr(3),
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "GetAll with BmcMacAddress filter returns objects",
			filter: ExpectedMachineFilterInput{
				BmcMacAddresses: []string{created[0].BmcMacAddress},
			},
			expectedCount: 1,
			expectedError: false,
		},

		{
			desc: "GetAll with ChassisSerialNumber filter returns objects",
			filter: ExpectedMachineFilterInput{
				ChassisSerialNumbers: []string{created[0].ChassisSerialNumber},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with search query filter returns objects",
			filter: ExpectedMachineFilterInput{
				SearchQuery: cutil.GetPtr("CHASSIS123"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with ExpectedMachineIDs filter returns objects",
			filter: ExpectedMachineFilterInput{
				ExpectedMachineIDs: []uuid.UUID{created[0].ID, created[2].ID},
			},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc: "GetAll with limit returns objects",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(2),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns objects",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(1),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with order by returns objects",
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderDescending,
				},
			},
			expectedCount: 3,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := emsd.GetAll(ctx, nil, tc.filter, tc.pageInput, nil)
			if err != nil {
				t.Logf("%s", err.Error())
			}

			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
			}

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

func TestExpectedMachineSQLDAO_GetAll_MachineIDsFilter(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	// Create machines
	machine1 := &Machine{
		ID:                       uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(machine1).Exec(ctx)
	assert.NoError(t, err)

	machine2 := &Machine{
		ID:                       uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusReady,
	}
	_, err = dbSession.DB.NewInsert().Model(machine2).Exec(ctx)
	assert.NoError(t, err)

	// Create ExpectedMachines
	emsd := NewExpectedMachineDAO(dbSession)

	em1, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:B1",
		ChassisSerialNumber: "CHASSIS-001",
		MachineID:           &machine1.ID,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)

	em2, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:B2",
		ChassisSerialNumber: "CHASSIS-002",
		MachineID:           &machine2.ID,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)

	_, err = emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:B3",
		ChassisSerialNumber: "CHASSIS-003",
		MachineID:           nil,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)

	// Test filter by single MachineID
	got, total, err := emsd.GetAll(ctx, nil, ExpectedMachineFilterInput{
		MachineIDs: []string{machine1.ID},
	}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 1, total)
	assert.Equal(t, 1, len(got))
	assert.Equal(t, em1.ID, got[0].ID)

	// Test filter by multiple MachineIDs
	got, total, err = emsd.GetAll(ctx, nil, ExpectedMachineFilterInput{
		MachineIDs: []string{machine1.ID, machine2.ID},
	}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 2, total)
	assert.Equal(t, 2, len(got))

	// Verify the correct ExpectedMachines were returned
	foundEM1 := false
	foundEM2 := false
	for _, em := range got {
		if em.ID == em1.ID {
			foundEM1 = true
		}
		if em.ID == em2.ID {
			foundEM2 = true
		}
	}
	assert.True(t, foundEM1, "ExpectedMachine 1 should be in results")
	assert.True(t, foundEM2, "ExpectedMachine 2 should be in results")
}

func TestExpectedMachineSQLDAO_GetAll_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	req := NewExpectedMachineDAO(dbSession)
	_ = testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)

	got, _, err := req.GetAll(ctx, nil, ExpectedMachineFilterInput{}, paginator.PageInput{}, []string{SiteRelationName, SkuRelationName})
	assert.NoError(t, err)
	assert.Equal(t, 3, len(got))

	withSku := 0
	withoutSku := 0
	for _, em := range got {
		assert.NotNil(t, em.Site)
		if em.Sku != nil {
			withSku++
		} else {
			withoutSku++
		}
	}

	assert.Equal(t, 1, withSku)
	assert.Equal(t, 2, withoutSku)
}

func TestExpectedMachineSQLDAO_GetAll_MachineRelation(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	emsd := NewExpectedMachineDAO(dbSession)

	// Create Machine first
	machine1 := &Machine{
		ID:                       uuid.NewString(),
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		ControllerMachineID:      uuid.NewString(),
		Status:                   MachineStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(machine1).Exec(ctx)
	assert.NoError(t, err)

	// Create ExpectedMachine with Machine reference
	em1, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:B7",
		ChassisSerialNumber: "CHASSIS-001",
		MachineID:           &machine1.ID,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)

	// Create ExpectedMachine without Machine reference
	em2, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "AA:BB:CC:DD:EE:FF",
		ChassisSerialNumber: "CHASSIS-002",
		MachineID:           nil,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)

	// Query all ExpectedMachines with Machine relation
	got, total, err := emsd.GetAll(ctx, nil, ExpectedMachineFilterInput{}, paginator.PageInput{}, []string{MachineRelationName})
	assert.NoError(t, err)
	assert.Equal(t, 2, total)
	assert.Equal(t, 2, len(got))

	// Verify relations
	withMachine := 0
	withoutMachine := 0
	for _, em := range got {
		if em.Machine != nil {
			withMachine++
			if em.ID == em1.ID {
				assert.Equal(t, machine1.ID, em.Machine.ID)
			}
		} else {
			withoutMachine++
			assert.Equal(t, em2.ID, em.ID)
		}
	}

	assert.Equal(t, 1, withMachine, "One ExpectedMachine should have a matching Machine")
	assert.Equal(t, 1, withoutMachine, "One ExpectedMachine should not have a matching Machine")
}

// TestExpectedMachineSQLDAO_GetAll_includeSkuRelation_withSiteFilter tests the critical case
// where filtering by SiteIDs while including the Sku relation requires proper table aliasing.
// Both ExpectedMachine and SKU tables have a site_id column, so without the "em." prefix,
// the query becomes ambiguous and will fail with: "column reference 'site_id' is ambiguous"
func TestExpectedMachineSQLDAO_GetAll_includeSkuRelation_withSiteFilter(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test data
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	// Create a SKU
	ssd := NewSkuDAO(dbSession)
	sku, err := ssd.Create(ctx, nil, SkuCreateInput{
		SkuID:      "test-sku-123",
		Components: &SkuComponents{},
		SiteID:     site.ID,
	})
	assert.NoError(t, err)
	assert.NotNil(t, sku)

	// Create ExpectedMachines with SKU
	emDAO := NewExpectedMachineDAO(dbSession)
	em1, err := emDAO.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:11:22:33:44:55",
		ChassisSerialNumber: "CHASSIS-001",
		SkuID:               &sku.ID,
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)
	assert.NotNil(t, em1)

	// This query will fail with "column reference 'site_id' is ambiguous" if the filter
	// uses "site_id" instead of "em.site_id" because:
	// 1. ExpectedMachine table (alias: em) has a site_id column
	// 2. SKU table (alias: sk) ALSO has a site_id column
	// 3. When we include the Sku relation, the query does a LEFT JOIN
	// 4. Without the alias, PostgreSQL doesn't know which table's site_id to use
	got, total, err := emDAO.GetAll(
		ctx,
		nil,
		ExpectedMachineFilterInput{
			SiteIDs: []uuid.UUID{site.ID}, // This filter triggers the ambiguous column issue
		},
		paginator.PageInput{},
		[]string{SkuRelationName}, // Including Sku relation creates the ambiguity
	)

	// Test assertions
	assert.NoError(t, err, "GetAll should not fail with ambiguous column error when using proper table alias")
	assert.Equal(t, 1, total)
	assert.Equal(t, 1, len(got))
	assert.Equal(t, em1.ID, got[0].ID)
	assert.NotNil(t, got[0].Sku, "Sku relation should be loaded")
	assert.Equal(t, sku.ID, got[0].Sku.ID)
}

func TestExpectedMachineSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	emsExp := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)
	emsd := NewExpectedMachineDAO(dbSession)
	assert.NotNil(t, emsd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		input              ExpectedMachineUpdateInput
		expectedError      bool
		errorContains      string
		verifyChildSpanner bool
	}{
		{
			desc: "Update BMC MAC address",
			input: ExpectedMachineUpdateInput{
				ExpectedMachineID: emsExp[0].ID,
				BmcMacAddress:     cutil.GetPtr("00:1B:44:11:3A:C1"),
			},
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "Update chassis serial number",
			input: ExpectedMachineUpdateInput{
				ExpectedMachineID:   emsExp[1].ID,
				ChassisSerialNumber: cutil.GetPtr("NEWCHASSIS789"),
			},
			expectedError: false,
		},
		{
			desc: "Update fallback DPU serial numbers",
			input: ExpectedMachineUpdateInput{
				ExpectedMachineID:        emsExp[2].ID,
				FallbackDpuSerialNumbers: []string{"DPU004", "DPU005", "DPU006"},
			},
			expectedError: false,
		},
		{
			desc: "Update labels",
			input: ExpectedMachineUpdateInput{
				ExpectedMachineID: emsExp[0].ID,
				Labels: map[string]string{
					"environment": "staging",
					"owner":       "team-alpha",
				},
			},
			expectedError: false,
		},
		{
			desc: "fail to update to duplicate MAC address in same site",
			input: ExpectedMachineUpdateInput{
				ExpectedMachineID: emsExp[2].ID,
				BmcMacAddress:     cutil.GetPtr(emsExp[1].BmcMacAddress), // emsExp[1] hasn't been modified yet
			},
			expectedError: true,
			errorContains: "duplicate key value",
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := emsd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if err != nil {
				t.Logf("%s", err.Error())
				if tc.errorContains != "" {
					assert.Contains(t, err.Error(), tc.errorContains, "Error should contain expected substring")
				}
			}
			if !tc.expectedError {
				assert.Nil(t, err)
				assert.NotNil(t, got)
				if tc.input.BmcMacAddress != nil {
					assert.Equal(t, *tc.input.BmcMacAddress, got.BmcMacAddress)
				}
				if tc.input.ChassisSerialNumber != nil {
					assert.Equal(t, *tc.input.ChassisSerialNumber, got.ChassisSerialNumber)
				}
				if tc.input.FallbackDpuSerialNumbers != nil {
					assert.Equal(t, tc.input.FallbackDpuSerialNumbers, got.FallbackDpuSerialNumbers)
				}
				if tc.input.Labels != nil {
					assert.Equal(t, Labels(tc.input.Labels), got.Labels)
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

func TestExpectedMachineSQLDAO_HostLifecycleProfile(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	emsd := NewExpectedMachineDAO(dbSession)
	emID := uuid.New()

	// Create with disable_lockdown = true and confirm it persists.
	created, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:    emID,
		SiteID:               site.ID,
		BmcMacAddress:        "00:1B:44:11:3A:C0",
		ChassisSerialNumber:  "CHASSIS-HLP",
		HostLifecycleProfile: HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
		CreatedBy:            user.ID,
	})
	assert.NoError(t, err)
	if assert.NotNil(t, created.HostLifecycleProfile.DisableLockdown) {
		assert.Equal(t, true, *created.HostLifecycleProfile.DisableLockdown)
	}

	got, err := emsd.Get(ctx, nil, emID, nil, false)
	assert.NoError(t, err)
	if assert.NotNil(t, got.HostLifecycleProfile.DisableLockdown) {
		assert.Equal(t, true, *got.HostLifecycleProfile.DisableLockdown)
	}

	// Update disable_lockdown to false.
	_, err = emsd.Update(ctx, nil, ExpectedMachineUpdateInput{
		ExpectedMachineID:    emID,
		HostLifecycleProfile: &HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)},
	})
	assert.NoError(t, err)
	got, err = emsd.Get(ctx, nil, emID, nil, false)
	assert.NoError(t, err)
	if assert.NotNil(t, got.HostLifecycleProfile.DisableLockdown) {
		assert.Equal(t, false, *got.HostLifecycleProfile.DisableLockdown)
	}

	// Updating another field without the profile preserves the existing value.
	_, err = emsd.Update(ctx, nil, ExpectedMachineUpdateInput{
		ExpectedMachineID:   emID,
		ChassisSerialNumber: cutil.GetPtr("CHASSIS-HLP-2"),
	})
	assert.NoError(t, err)
	got, err = emsd.Get(ctx, nil, emID, nil, false)
	assert.NoError(t, err)
	if assert.NotNil(t, got.HostLifecycleProfile.DisableLockdown) {
		assert.Equal(t, false, *got.HostLifecycleProfile.DisableLockdown)
	}

	// Creating without a profile persists the empty (unset) default.
	em2ID := uuid.New()
	em2, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   em2ID,
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:C1",
		ChassisSerialNumber: "CHASSIS-HLP-DEFAULT",
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)
	assert.Nil(t, em2.HostLifecycleProfile.DisableLockdown)

	got2, err := emsd.Get(ctx, nil, em2ID, nil, false)
	assert.NoError(t, err)
	assert.Nil(t, got2.HostLifecycleProfile.DisableLockdown)
}

func TestExpectedMachineSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	emsExp := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)
	emsd := NewExpectedMachineDAO(dbSession)
	assert.NotNil(t, emsd)
	bmcEM, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              emsExp[0].SiteID,
		BmcMacAddress:       "00:1B:44:11:3A:CA",
		ChassisSerialNumber: "CHASSIS-CLEAR-BMC-IP",
		BmcIpAddress:        cutil.GetPtr("192.0.2.10"),
		CreatedBy:           emsExp[0].CreatedBy,
	})
	require.NoError(t, err)
	require.NotNil(t, bmcEM)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		em                 ExpectedMachine
		input              ExpectedMachineClearInput
		expectedUpdate     bool
		verifyChildSpanner bool
	}{
		{
			desc: "can clear FallbackDpuSerialNumbers",
			em:   emsExp[1],
			input: ExpectedMachineClearInput{
				FallbackDpuSerialNumbers: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear Labels",
			em:   emsExp[0],
			input: ExpectedMachineClearInput{
				Labels: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear BmcIpAddress",
			em:   *bmcEM,
			input: ExpectedMachineClearInput{
				BmcIpAddress: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear multiple fields",
			em:   emsExp[2],
			input: ExpectedMachineClearInput{
				FallbackDpuSerialNumbers: true,
				Labels:                   true,
			},
			expectedUpdate: true,
		},
		{
			desc:           "nop when no cleared fields are specified",
			em:             emsExp[2],
			input:          ExpectedMachineClearInput{},
			expectedUpdate: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tc.input.ExpectedMachineID = tc.em.ID
			tmp, err := emsd.Clear(ctx, nil, tc.input)
			assert.Nil(t, err)
			assert.NotNil(t, tmp)
			if tc.input.FallbackDpuSerialNumbers {
				assert.Nil(t, tmp.FallbackDpuSerialNumbers)
			}
			if tc.input.Labels {
				assert.Nil(t, tmp.Labels)
			}
			if tc.input.BmcIpAddress {
				assert.Nil(t, tmp.BmcIpAddress)
			}

			if tc.expectedUpdate {
				assert.True(t, tmp.Updated.After(tc.em.Updated))
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

func TestExpectedMachineSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	emsExp := testExpectedMachineSQLDAOCreateExpectedMachines(ctx, t, dbSession)
	emsd := NewExpectedMachineDAO(dbSession)
	assert.NotNil(t, emsd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		emID               uuid.UUID
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object success",
			emID:               emsExp[1].ID,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			desc:    "delete non-existent object success",
			emID:    uuid.New(),
			wantErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := emsd.Delete(ctx, nil, tc.emID)

			if tc.wantErr {
				assert.Error(t, err)
				return
			}

			var res ExpectedMachine

			err = dbSession.DB.NewSelect().Model(&res).Where("em.id = ?", tc.emID).Scan(ctx)
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

func TestExpectedMachineSQLDAO_CreateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	// Create a SKU for testing
	ssd := NewSkuDAO(dbSession)
	sku, err := ssd.Create(ctx, nil, SkuCreateInput{SkuID: "sku-test-batch", Components: &SkuComponents{}, SiteID: site.ID})
	assert.NoError(t, err)
	assert.NotNil(t, sku)

	emsd := NewExpectedMachineDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []ExpectedMachineCreateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "create batch of three expected machines",
			inputs: []ExpectedMachineCreateInput{
				{
					ExpectedMachineID:        uuid.New(),
					SiteID:                   site.ID,
					BmcMacAddress:            "00:1B:44:11:3A:C1",
					ChassisSerialNumber:      "CHASSIS-BATCH-001",
					SkuID:                    &sku.ID,
					FallbackDpuSerialNumbers: []string{"DPU001", "DPU002"},
					Labels: map[string]string{
						"environment": "test",
						"batch":       "1",
					},
					CreatedBy: user.ID,
				},
				{
					ExpectedMachineID:   uuid.New(),
					SiteID:              site.ID,
					BmcMacAddress:       "00:1B:44:11:3A:C2",
					ChassisSerialNumber: "CHASSIS-BATCH-002",
					CreatedBy:           user.ID,
				},
				{
					ExpectedMachineID:        uuid.New(),
					SiteID:                   site.ID,
					BmcMacAddress:            "00:1B:44:11:3A:C3",
					ChassisSerialNumber:      "CHASSIS-BATCH-003",
					FallbackDpuSerialNumbers: []string{"DPU003"},
					Labels: map[string]string{
						"environment": "production",
					},
					CreatedBy: user.ID,
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "create batch with empty input",
			inputs:             []ExpectedMachineCreateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
		{
			desc: "create batch with single expected machine",
			inputs: []ExpectedMachineCreateInput{
				{
					ExpectedMachineID:   uuid.New(),
					SiteID:              site.ID,
					BmcMacAddress:       "00:1B:44:11:3A:C4",
					ChassisSerialNumber: "CHASSIS-SINGLE-001",
					CreatedBy:           user.ID,
				},
			},
			expectError:   false,
			expectedCount: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := emsd.CreateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify each created expected machine has a valid ID and timestamps
				// Also verify that results are returned in the same order as inputs
				for i, em := range got {
					assert.NotEqual(t, uuid.Nil, em.ID)
					assert.Equal(t, tc.inputs[i].BmcMacAddress, em.BmcMacAddress, "result order should match input order")
					assert.Equal(t, tc.inputs[i].ChassisSerialNumber, em.ChassisSerialNumber)
					assert.Equal(t, tc.inputs[i].FallbackDpuSerialNumbers, em.FallbackDpuSerialNumbers)
					assert.Equal(t, Labels(tc.inputs[i].Labels), em.Labels)
					assert.NotZero(t, em.Created)
					assert.NotZero(t, em.Updated)
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

func TestExpectedMachineSQLDAO_UpdateMultiple_MacSwap(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	emsd := NewExpectedMachineDAO(dbSession)

	// Create two ExpectedMachines with different MAC addresses
	macAddress1 := "AA:BB:CC:DD:EE:01"
	macAddress2 := "AA:BB:CC:DD:EE:02"

	em1, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       macAddress1,
		ChassisSerialNumber: "CHASSIS-SWAP-001",
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)
	assert.NotNil(t, em1)

	em2, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       macAddress2,
		ChassisSerialNumber: "CHASSIS-SWAP-002",
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)
	assert.NotNil(t, em2)

	// Attempt to swap MAC addresses in a batch operation
	// This succeeds because the unique constraint is deferrable (checks are deferred until commit)
	results, err := emsd.UpdateMultiple(ctx, nil, []ExpectedMachineUpdateInput{
		{
			ExpectedMachineID: em1.ID,
			BmcMacAddress:     cutil.GetPtr(macAddress2), // Give em1 the MAC from em2
		},
		{
			ExpectedMachineID: em2.ID,
			BmcMacAddress:     cutil.GetPtr(macAddress1), // Give em2 the MAC from em1
		},
	})

	assert.NoError(t, err, "Swapping MAC addresses in a batch operation should succeed with deferrable constraint")
	assert.NotNil(t, results)
	assert.Equal(t, 2, len(results))

	// Verify the swap was successful
	updatedEM1, err := emsd.Get(ctx, nil, em1.ID, nil, false)
	assert.NoError(t, err)
	assert.Equal(t, macAddress2, updatedEM1.BmcMacAddress, "em1 should now have em2's original MAC address")

	updatedEM2, err := emsd.Get(ctx, nil, em2.ID, nil, false)
	assert.NoError(t, err)
	assert.Equal(t, macAddress1, updatedEM2.BmcMacAddress, "em2 should now have em1's original MAC address")
}

func TestExpectedMachineSQLDAO_LockForUpdateSerializesOverlappingBatches(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)
	emsd := NewExpectedMachineDAO(dbSession)

	first, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "AA:BB:CC:DD:EE:11",
		ChassisSerialNumber: "CHASSIS-LOCK-001",
		BmcIpAddress:        cutil.GetPtr("192.0.2.11"),
		CreatedBy:           user.ID,
	})
	require.NoError(t, err)
	second, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "AA:BB:CC:DD:EE:12",
		ChassisSerialNumber: "CHASSIS-LOCK-002",
		BmcIpAddress:        cutil.GetPtr("192.0.2.12"),
		CreatedBy:           user.ID,
	})
	require.NoError(t, err)

	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()

	firstBatchName := "first-batch"
	secondBatchName := "second-batch"
	firstLocked := make(chan struct{})
	releaseFirst := make(chan struct{})
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
			if err := emsd.LockForUpdate(ctx, tx, []uuid.UUID{second.ID, first.ID}); err != nil {
				return err
			}
			close(firstLocked)
			select {
			case <-releaseFirst:
			case <-ctx.Done():
				return ctx.Err()
			}
			if _, err := emsd.Clear(ctx, tx, ExpectedMachineClearInput{
				ExpectedMachineID: second.ID,
				BmcIpAddress:      true,
			}); err != nil {
				return err
			}
			_, err := emsd.UpdateMultiple(ctx, tx, []ExpectedMachineUpdateInput{
				{ExpectedMachineID: second.ID, Name: &firstBatchName},
				{ExpectedMachineID: first.ID, Name: &firstBatchName},
			})
			return err
		})
	}()

	select {
	case <-firstLocked:
	case err := <-firstDone:
		t.Fatalf("first transaction completed before acquiring its row locks: %v", err)
	case <-ctx.Done():
		firstErr := <-firstDone
		t.Fatalf("timed out waiting for the first transaction to acquire its row locks: %v (transaction: %v)", ctx.Err(), firstErr)
	}

	secondStarted := make(chan struct{})
	secondLockAcquired := make(chan struct{})
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
			close(secondStarted)
			if err := emsd.LockForUpdate(ctx, tx, []uuid.UUID{first.ID, second.ID}); err != nil {
				return err
			}
			close(secondLockAcquired)
			if _, err := emsd.Clear(ctx, tx, ExpectedMachineClearInput{
				ExpectedMachineID: first.ID,
				BmcIpAddress:      true,
			}); err != nil {
				return err
			}
			_, err := emsd.UpdateMultiple(ctx, tx, []ExpectedMachineUpdateInput{
				{ExpectedMachineID: first.ID, Name: &secondBatchName},
				{ExpectedMachineID: second.ID, Name: &secondBatchName},
			})
			return err
		})
	}()
	select {
	case <-secondStarted:
	case err := <-secondDone:
		close(releaseFirst)
		firstErr := <-firstDone
		t.Fatalf("second transaction completed before attempting its canonical lock: %v (first transaction: %v)", err, firstErr)
	case <-ctx.Done():
		close(releaseFirst)
		firstErr := <-firstDone
		secondErr := <-secondDone
		t.Fatalf(
			"timed out waiting for the second transaction to attempt its canonical lock: %v (first transaction: %v, second transaction: %v)",
			ctx.Err(),
			firstErr,
			secondErr,
		)
	}

	// The second batch must wait at the full-row lock pass. If that lock returns
	// before the first transaction completes, the partial-lock deadlock is
	// possible again.
	var secondErr error
	secondFinishedEarly := false
	secondLockAcquiredEarly := false
	select {
	case <-secondLockAcquired:
		secondLockAcquiredEarly = true
	case secondErr = <-secondDone:
		secondFinishedEarly = true
	case <-time.After(100 * time.Millisecond):
	}
	close(releaseFirst)

	require.NoError(t, <-firstDone)
	if !secondFinishedEarly {
		secondErr = <-secondDone
	}
	require.False(t, secondLockAcquiredEarly, "the competing batch should wait at the canonical lock pass")
	require.False(t, secondFinishedEarly, "the competing batch should wait for the first transaction")
	require.NoError(t, secondErr)

	for _, expectedMachineID := range []uuid.UUID{first.ID, second.ID} {
		stored, err := emsd.Get(ctx, nil, expectedMachineID, nil, false)
		require.NoError(t, err)
		assert.Nil(t, stored.BmcIpAddress)
		assert.Equal(t, &secondBatchName, stored.Name)
	}
}

func TestExpectedMachineSQLDAO_LockForUpdateErrors(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	emsd := NewExpectedMachineDAO(dbSession)
	missingID := uuid.New()
	tests := []struct {
		name            string
		lock            func() error
		target          error
		messageContains string
	}{
		{
			name: "transaction is required",
			lock: func() error {
				return emsd.LockForUpdate(ctx, nil, []uuid.UUID{missingID})
			},
			messageContains: "transaction is required",
		},
		{
			name: "missing row keeps typed error",
			lock: func() error {
				return db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
					return emsd.LockForUpdate(ctx, tx, []uuid.UUID{missingID})
				})
			},
			target: db.ErrDoesNotExist,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := test.lock()
			require.Error(t, err)
			if test.target != nil {
				require.ErrorIs(t, err, test.target)
			}
			if test.messageContains != "" {
				require.ErrorContains(t, err, test.messageContains)
			}
		})
	}
}

func TestExpectedMachineSQLDAO_UpdateMultiple(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	// Create a SKU for testing
	ssd := NewSkuDAO(dbSession)
	sku, err := ssd.Create(ctx, nil, SkuCreateInput{SkuID: "sku-test-update", Components: &SkuComponents{}, SiteID: site.ID})
	assert.NoError(t, err)
	assert.NotNil(t, sku)

	emsd := NewExpectedMachineDAO(dbSession)

	// Create test expected machines
	em1, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:D1",
		ChassisSerialNumber: "CHASSIS-UPDATE-001",
		CreatedBy:           user.ID,
	})
	assert.Nil(t, err)

	em2, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:D2",
		ChassisSerialNumber: "CHASSIS-UPDATE-002",
		Labels: map[string]string{
			"original": "label",
		},
		CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	em3, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:        uuid.New(),
		SiteID:                   site.ID,
		BmcMacAddress:            "00:1B:44:11:3A:D3",
		ChassisSerialNumber:      "CHASSIS-UPDATE-003",
		FallbackDpuSerialNumbers: []string{"DPU001"},
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		inputs             []ExpectedMachineUpdateInput
		expectError        bool
		expectedCount      int
		verifyChildSpanner bool
	}{
		{
			desc: "batch update three expected machines",
			inputs: []ExpectedMachineUpdateInput{
				{
					ExpectedMachineID:   em1.ID,
					BmcMacAddress:       cutil.GetPtr("00:1B:44:11:3A:E1"),
					ChassisSerialNumber: cutil.GetPtr("CHASSIS-MODIFIED-001"),
					SkuID:               &sku.ID,
					Labels: map[string]string{
						"updated": "true",
					},
				},
				{
					ExpectedMachineID:        em2.ID,
					BmcMacAddress:            cutil.GetPtr("00:1B:44:11:3A:E2"),
					FallbackDpuSerialNumbers: []string{"DPU002", "DPU003"},
				},
				{
					ExpectedMachineID: em3.ID,
					Labels: map[string]string{
						"environment": "production",
						"tier":        "critical",
					},
				},
			},
			expectError:        false,
			expectedCount:      3,
			verifyChildSpanner: true,
		},
		{
			desc:               "batch update with empty input",
			inputs:             []ExpectedMachineUpdateInput{},
			expectError:        false,
			expectedCount:      0,
			verifyChildSpanner: false,
		},
		{
			desc: "batch update single expected machine",
			inputs: []ExpectedMachineUpdateInput{
				{
					ExpectedMachineID:   em1.ID,
					ChassisSerialNumber: cutil.GetPtr("CHASSIS-SINGLE-UPDATE"),
				},
			},
			expectError:   false,
			expectedCount: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := emsd.UpdateMultiple(ctx, nil, tc.inputs)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectedCount, len(got))
				// Verify updates and that results are returned in the same order as inputs
				for i, em := range got {
					assert.Equal(t, tc.inputs[i].ExpectedMachineID, em.ID, "result order should match input order")
					if tc.inputs[i].BmcMacAddress != nil {
						assert.Equal(t, *tc.inputs[i].BmcMacAddress, em.BmcMacAddress)
					}
					if tc.inputs[i].ChassisSerialNumber != nil {
						assert.Equal(t, *tc.inputs[i].ChassisSerialNumber, em.ChassisSerialNumber)
					}
					if tc.inputs[i].FallbackDpuSerialNumbers != nil {
						assert.Equal(t, tc.inputs[i].FallbackDpuSerialNumbers, em.FallbackDpuSerialNumbers)
					}
					if tc.inputs[i].Labels != nil {
						assert.Equal(t, Labels(tc.inputs[i].Labels), em.Labels)
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

// TestExpectedMachineSQLDAO_UpdateMultiple_BmcIpAddress guards against a BMC
// address supplied for one row clearing an address omitted by another row.
func TestExpectedMachineSQLDAO_UpdateMultiple_BmcIpAddress(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)
	emsd := NewExpectedMachineDAO(dbSession)

	originalUpdatedIP := "192.0.2.50"
	originalPreservedIP := "192.0.2.51"
	updatedIP := "192.0.2.52"
	originalUpdatedName := "original-updated-ip-machine"
	originalPreservedName := "original-preserved-ip-machine"
	updatedIPName := "updated-ip-machine"
	preservedIPName := "preserved-ip-machine"
	emUpdated, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:F4",
		ChassisSerialNumber: "CHASSIS-BMC-IP-A",
		BmcIpAddress:        &originalUpdatedIP,
		Name:                &originalUpdatedName,
		CreatedBy:           user.ID,
	})
	require.NoError(t, err)
	emPreserved, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:F5",
		ChassisSerialNumber: "CHASSIS-BMC-IP-B",
		BmcIpAddress:        &originalPreservedIP,
		Name:                &originalPreservedName,
		CreatedBy:           user.ID,
	})
	require.NoError(t, err)

	got, err := emsd.UpdateMultiple(ctx, nil, []ExpectedMachineUpdateInput{
		{
			ExpectedMachineID: emUpdated.ID,
			BmcIpAddress:      &updatedIP,
			Name:              &updatedIPName,
		},
		{
			ExpectedMachineID: emPreserved.ID,
			Name:              &preservedIPName,
		},
	})
	require.NoError(t, err)
	if assert.Len(t, got, 2) {
		assert.Equal(t, &updatedIP, got[0].BmcIpAddress)
		assert.Equal(t, &updatedIPName, got[0].Name)
		assert.Equal(t, &originalPreservedIP, got[1].BmcIpAddress)
		assert.Equal(t, &preservedIPName, got[1].Name)
	}

	stored, err := emsd.Get(ctx, nil, emPreserved.ID, nil, false)
	require.NoError(t, err)
	assert.Equal(t, &originalPreservedIP, stored.BmcIpAddress)
	assert.Equal(t, &preservedIPName, stored.Name)
}

// TestExpectedMachineSQLDAO_UpdateMultiple_HostLifecycleProfile guards against a
// mixed batch clearing host_lifecycle_profile on rows that did not set it.
// Because the shared bulk UPDATE applies one column list to every row, the field
// must be applied separately to only the rows that provide it; rows that omit it
// must keep their persisted profile.
func TestExpectedMachineSQLDAO_UpdateMultiple_HostLifecycleProfile(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testExpectedMachineSetupSchema(t, dbSession)

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	emsd := NewExpectedMachineDAO(dbSession)

	// emA starts with disable_lockdown=true and is updated with an empty profile
	// object. That must behave like omission and preserve the stored value.
	emA, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:    uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        "00:1B:44:11:3A:F1",
		ChassisSerialNumber:  "CHASSIS-HLP-A",
		HostLifecycleProfile: HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
		CreatedBy:            user.ID,
	})
	assert.NoError(t, err)

	// emB starts with disable_lockdown=false and is the only row that sets a
	// profile in the batch (flipping it to true).
	emB, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:    uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        "00:1B:44:11:3A:F2",
		ChassisSerialNumber:  "CHASSIS-HLP-B",
		HostLifecycleProfile: HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)},
		CreatedBy:            user.ID,
	})
	assert.NoError(t, err)

	// emC starts unset and is updated WITHOUT a profile.
	emC, err := emsd.Create(ctx, nil, ExpectedMachineCreateInput{
		ExpectedMachineID:   uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       "00:1B:44:11:3A:F3",
		ChassisSerialNumber: "CHASSIS-HLP-C",
		CreatedBy:           user.ID,
	})
	assert.NoError(t, err)

	_, err = emsd.UpdateMultiple(ctx, nil, []ExpectedMachineUpdateInput{
		{
			ExpectedMachineID:    emA.ID,
			ChassisSerialNumber:  cutil.GetPtr("CHASSIS-HLP-A2"),
			HostLifecycleProfile: &HostLifecycleProfile{},
		},
		{
			ExpectedMachineID:    emB.ID,
			HostLifecycleProfile: &HostLifecycleProfile{DisableLockdown: cutil.GetPtr(true)},
		},
		{
			ExpectedMachineID: emC.ID,
			Labels:            map[string]string{"env": "test"},
		},
	})
	assert.NoError(t, err)

	// emA: other field changed, empty profile object preserved the stored value.
	gotA, err := emsd.Get(ctx, nil, emA.ID, nil, false)
	assert.NoError(t, err)
	assert.Equal(t, "CHASSIS-HLP-A2", gotA.ChassisSerialNumber)
	if assert.NotNil(t, gotA.HostLifecycleProfile.DisableLockdown, "omitted profile must be preserved") {
		assert.Equal(t, true, *gotA.HostLifecycleProfile.DisableLockdown)
	}

	// emB: profile explicitly updated false -> true.
	gotB, err := emsd.Get(ctx, nil, emB.ID, nil, false)
	assert.NoError(t, err)
	if assert.NotNil(t, gotB.HostLifecycleProfile.DisableLockdown) {
		assert.Equal(t, true, *gotB.HostLifecycleProfile.DisableLockdown)
	}

	// emC: other field changed, unset profile stays unset.
	gotC, err := emsd.Get(ctx, nil, emC.ID, nil, false)
	assert.NoError(t, err)
	assert.Equal(t, Labels{"env": "test"}, gotC.Labels)
	assert.Nil(t, gotC.HostLifecycleProfile.DisableLockdown, "omitted profile (unset) must stay unset")
}
