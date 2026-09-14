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
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type policyProviderStub struct {
	profiles []string
	err      error
	calls    int
}

func (s *policyProviderStub) ListPowerProfiles(context.Context) ([]string, error) {
	s.calls++
	return s.profiles, s.err
}

func TestValidatePowerProfile(t *testing.T) {
	providerFailure := errors.New("DPS unavailable")
	tests := []struct {
		name        string
		dpsEnabled  bool
		profile     *string
		provider    dpsclient.PolicyProvider
		wantProfile *string
		wantCode    int
		wantCalls   int
	}{
		{
			name:        "disabled DPS trusts profile without calling DPS",
			profile:     cutil.GetPtr("trusted-profile"),
			provider:    &policyProviderStub{err: providerFailure},
			wantProfile: cutil.GetPtr("trusted-profile"),
		},
		{
			name:       "enabled DPS preserves omitted profile",
			dpsEnabled: true,
			provider:   &policyProviderStub{err: providerFailure},
		},
		{
			name:        "enabled DPS preserves explicit clear",
			dpsEnabled:  true,
			profile:     cutil.GetPtr(""),
			provider:    &policyProviderStub{err: providerFailure},
			wantProfile: cutil.GetPtr(""),
		},
		{
			name:        "enabled DPS validates and normalizes set profile",
			dpsEnabled:  true,
			profile:     cutil.GetPtr("  efficient  "),
			provider:    &policyProviderStub{profiles: []string{"efficient"}},
			wantProfile: cutil.GetPtr("efficient"),
			wantCalls:   1,
		},
		{
			name:        "enabled DPS rejects unknown profile",
			dpsEnabled:  true,
			profile:     cutil.GetPtr("unknown"),
			provider:    &policyProviderStub{profiles: []string{"efficient"}},
			wantProfile: cutil.GetPtr("unknown"),
			wantCode:    http.StatusBadRequest,
			wantCalls:   1,
		},
		{
			name:        "enabled DPS rejects whitespace-only set profile",
			dpsEnabled:  true,
			profile:     cutil.GetPtr("   "),
			provider:    &policyProviderStub{profiles: []string{"efficient"}},
			wantProfile: cutil.GetPtr("   "),
			wantCode:    http.StatusBadRequest,
		},
		{
			name:        "enabled DPS reports discovery failure",
			dpsEnabled:  true,
			profile:     cutil.GetPtr("efficient"),
			provider:    &policyProviderStub{err: providerFailure},
			wantProfile: cutil.GetPtr("efficient"),
			wantCode:    http.StatusServiceUnavailable,
			wantCalls:   1,
		},
		{
			name:        "enabled DPS reports missing client",
			dpsEnabled:  true,
			profile:     cutil.GetPtr("efficient"),
			wantProfile: cutil.GetPtr("efficient"),
			wantCode:    http.StatusServiceUnavailable,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			apiErr := ValidatePowerProfile(context.Background(), tt.dpsEnabled, tt.provider, tt.profile)
			if tt.wantCode == 0 {
				require.Nil(t, apiErr)
			} else {
				require.Error(t, apiErr)
				assert.Equal(t, tt.wantCode, apiErr.Code)
			}
			assert.Equal(t, tt.wantProfile, tt.profile)
			provider, ok := tt.provider.(*policyProviderStub)
			if ok {
				assert.Equal(t, tt.wantCalls, provider.calls)
			}
		})
	}
}

func TestNewAPIInstance(t *testing.T) {
	dbs := &cdbm.Site{
		ID:                       uuid.New(),
		Name:                     "test-name",
		Description:              cutil.GetPtr("Test Description"),
		InfrastructureProviderID: uuid.New(),
		SerialConsoleHostname:    cutil.GetPtr("test-hostname"),
	}

	instanceTypeID := uuid.New()
	dbi1 := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     "test-name",
		TenantID:                 uuid.New(),
		InfrastructureProviderID: dbs.InfrastructureProviderID,
		SiteID:                   dbs.ID,
		InstanceTypeID:           &instanceTypeID,
		VpcID:                    uuid.New(),
		MachineID:                cutil.GetPtr(uuid.NewString()),
		ControllerInstanceID:     cutil.GetPtr(uuid.New()),
		OperatingSystemID:        nil,
		IpxeScript:               cutil.GetPtr("test"),
		AlwaysBootWithCustomIpxe: true,
		UserData:                 cutil.GetPtr("test"),
		Labels: map[string]string{
			"test": "test",
		},
		TpmEkCertificate: cutil.GetPtr("test"),
		Status:           cdbm.InstanceStatusPending,
		Created:          time.Now(),
		Updated:          time.Now(),
	}

	dbsd1 := cdbm.StatusDetail{
		ID:       uuid.New(),
		EntityID: dbi1.ID.String(),
		Status:   cdbm.InstanceStatusPending,
		Message:  cutil.GetPtr("test-message"),
		Created:  time.Now(),
		Updated:  time.Now(),
	}

	dbis1 := cdbm.Interface{
		ID:          uuid.New(),
		InstanceID:  dbi1.ID,
		SubnetID:    cutil.GetPtr(uuid.New()),
		IsPhysical:  true,
		MacAddress:  cutil.GetPtr("test-mac-address"),
		IPAddresses: []string{"12.70.0.1"},
		Status:      cdbm.InterfaceStatusPending,
		Created:     time.Now(),
		Updated:     time.Now(),
	}

	secondaryVpcID1 := uuid.New()
	secondaryVpcID2 := uuid.New()
	ipv4FamilyMode := cdbm.InterfaceVpcIPFamilyModeIPv4Only

	dbis1Secondary1 := cdbm.Interface{
		ID:              uuid.New(),
		InstanceID:      dbi1.ID,
		VpcID:           &secondaryVpcID1,
		VpcIPFamilyMode: &ipv4FamilyMode,
		IsPhysical:      false,
		Status:          cdbm.InterfaceStatusPending,
		Created:         time.Now(),
		Updated:         time.Now(),
	}

	dbis1Secondary2 := cdbm.Interface{
		ID:          uuid.New(),
		InstanceID:  dbi1.ID,
		VpcPrefixID: cutil.GetPtr(uuid.New()),
		VpcPrefix: &cdbm.VpcPrefix{
			ID:    uuid.New(),
			VpcID: secondaryVpcID2,
		},
		IsPhysical: false,
		Status:     cdbm.InterfaceStatusPending,
		Created:    time.Now(),
		Updated:    time.Now(),
	}

	dbibi1 := cdbm.InfiniBandInterface{
		ID:                    uuid.New(),
		InstanceID:            dbi1.ID,
		SiteID:                dbi1.SiteID,
		InfiniBandPartitionID: uuid.New(),
		DeviceInstance:        2,
		IsPhysical:            false,
		VirtualFunctionID:     cutil.GetPtr(2),
		Status:                cdbm.InfiniBandInterfaceStatusPending,
		Created:               time.Now(),
		Updated:               time.Now(),
	}

	dbnvl1 := cdbm.NVLinkInterface{
		ID:                       uuid.New(),
		InstanceID:               dbi1.ID,
		SiteID:                   dbi1.SiteID,
		NVLinkLogicalPartitionID: uuid.New(),
		Device:                   cutil.GetPtr("NVIDIA GB200"),
		DeviceInstance:           0,
		Status:                   cdbm.NVLinkInterfaceStatusPending,
		Created:                  time.Now(),
		Updated:                  time.Now(),
	}

	instanceTypeID = uuid.New()
	dbi2 := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     "test-name",
		TenantID:                 uuid.New(),
		InfrastructureProviderID: uuid.New(),
		SiteID:                   uuid.New(),
		InstanceTypeID:           &instanceTypeID,
		VpcID:                    uuid.New(),
		MachineID:                cutil.GetPtr(uuid.NewString()),
		ControllerInstanceID:     cutil.GetPtr(uuid.New()),
		OperatingSystemID:        cutil.GetPtr(uuid.New()),
		IpxeScript:               cutil.GetPtr("test"),
		AlwaysBootWithCustomIpxe: true,
		UserData:                 cutil.GetPtr("test"),
		Status:                   cdbm.InstanceStatusReady,
		TpmEkCertificate:         cutil.GetPtr("test"),
		PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusRebooting),
		Created:                  time.Now(),
		Updated:                  time.Now(),
	}

	dbsd2 := cdbm.StatusDetail{
		ID:       uuid.New(),
		EntityID: dbi1.ID.String(),
		Status:   cdbm.InstanceStatusReady,
		Message:  cutil.GetPtr("test-message"),
		Created:  time.Now(),
		Updated:  time.Now(),
	}

	dbis2 := cdbm.Interface{
		ID:                 uuid.New(),
		InstanceID:         dbi1.ID,
		SubnetID:           cutil.GetPtr(uuid.New()),
		MachineInterfaceID: cutil.GetPtr(uuid.New()),
		Status:             cdbm.InterfaceStatusPending,
		Created:            time.Now(),
		Updated:            time.Now(),
	}

	dbdes := cdbm.DpuExtensionService{
		ID:          uuid.New(),
		Name:        "test-service",
		ServiceType: "test-type",
		Version:     cutil.GetPtr("v1"),
		VersionInfo: &cdbm.DpuExtensionServiceVersionInfo{
			Version:        "v1",
			Data:           "apiVersion: v1\nkind: Pod",
			HasCredentials: true,
			Created:        time.Now(),
		},
		Status:  cdbm.DpuExtensionServiceStatusReady,
		Created: time.Now(),
		Updated: time.Now(),
	}

	dbdesd2 := cdbm.DpuExtensionServiceDeployment{
		ID:                    uuid.New(),
		InstanceID:            dbi2.ID,
		DpuExtensionServiceID: dbdes.ID,
		DpuExtensionService:   &dbdes,
		SiteID:                dbdes.SiteID,
		TenantID:              dbdes.TenantID,
		Version:               *dbdes.Version,
		Status:                cdbm.DpuExtensionServiceDeploymentStatusRunning,
		Created:               time.Now(),
		Updated:               time.Now(),
	}

	dbskg := cdbm.SSHKeyGroup{
		ID:      uuid.New(),
		Name:    "test-group",
		Version: cutil.GetPtr("1213123"),
		Status:  cdbm.SSHKeyGroupStatusSynced,
		Created: time.Now(),
		Updated: time.Now(),
	}

	dbskg2 := cdbm.SSHKeyGroup{
		ID:      uuid.New(),
		Name:    "test-group-2",
		Version: cutil.GetPtr("1213123"),
		Status:  cdbm.SSHKeyGroupStatusSynced,
		Created: time.Now(),
		Updated: time.Now(),
	}

	type args struct {
		dbic                    *cdbm.Instance
		dbs                     *cdbm.Site
		dbsds                   []cdbm.StatusDetail
		dbis                    []cdbm.Interface
		dbibi                   []cdbm.InfiniBandInterface
		dbdesd                  []cdbm.DpuExtensionServiceDeployment
		dbnvl                   []cdbm.NVLinkInterface
		dbskg                   []cdbm.SSHKeyGroup
		expectedSecondaryVpcIDs []string
	}
	tests := []struct {
		name string
		args args
	}{
		{
			name: "test new API Instance initializer",
			args: args{
				dbic:                    dbi1,
				dbs:                     dbs,
				dbsds:                   []cdbm.StatusDetail{dbsd1},
				dbis:                    []cdbm.Interface{dbis1, dbis1Secondary2, dbis1Secondary1},
				dbibi:                   []cdbm.InfiniBandInterface{dbibi1},
				dbnvl:                   []cdbm.NVLinkInterface{dbnvl1},
				dbskg:                   []cdbm.SSHKeyGroup{dbskg, dbskg2},
				expectedSecondaryVpcIDs: []string{secondaryVpcID1.String(), secondaryVpcID2.String()},
			},
		},
		{
			name: "test new API Instance initializer with power status change",
			args: args{
				dbic:   dbi2,
				dbs:    dbs,
				dbsds:  []cdbm.StatusDetail{dbsd2},
				dbis:   []cdbm.Interface{dbis2},
				dbdesd: []cdbm.DpuExtensionServiceDeployment{dbdesd2},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIInstance(tt.args.dbic, tt.args.dbs, tt.args.dbis, tt.args.dbibi, tt.args.dbdesd, tt.args.dbnvl, tt.args.dbskg, tt.args.dbsds)
			marshalled, err := json.Marshal(got)
			assert.NoError(t, err)
			var roundTripped APIInstance
			err = json.Unmarshal(marshalled, &roundTripped)
			assert.NoError(t, err)

			assert.Equal(t, tt.args.dbic.ID.String(), got.ID)
			assert.Equal(t, tt.args.dbic.Name, got.Name)
			assert.Equal(t, tt.args.dbic.TenantID.String(), got.TenantID)
			assert.Equal(t, tt.args.dbic.InfrastructureProviderID.String(), got.InfrastructureProviderID)
			assert.Equal(t, tt.args.dbic.SiteID.String(), got.SiteID)
			assert.Equal(t, tt.args.dbic.InstanceTypeID.String(), *got.InstanceTypeID)
			assert.Equal(t, tt.args.dbic.VpcID.String(), got.VpcID)
			if got.MachineID != nil {
				assert.Equal(t, *tt.args.dbic.MachineID, *got.MachineID)
			}
			if got.OperatingSystemID != nil {
				assert.Equal(t, tt.args.dbic.OperatingSystemID.String(), *got.OperatingSystemID)
			}
			if got.IpxeScript != nil {
				assert.Equal(t, *tt.args.dbic.IpxeScript, *got.IpxeScript)
			}
			assert.Equal(t, got.AlwaysBootWithCustomIpxe, true)
			if got.UserData != nil {
				assert.Equal(t, *tt.args.dbic.UserData, *got.UserData)
			}

			if got.Labels != nil {
				assert.Equal(t, tt.args.dbic.Labels, got.Labels)
			}

			if tt.args.expectedSecondaryVpcIDs != nil {
				expectedSecondaryVpcIDs := slices.Clone(tt.args.expectedSecondaryVpcIDs)
				slices.Sort(expectedSecondaryVpcIDs)
				assert.Equal(t, expectedSecondaryVpcIDs, roundTripped.SecondaryVpcIDs)
			}

			if tt.args.dbic.PowerStatus != nil && *tt.args.dbic.PowerStatus != cdbm.InstancePowerStatusBootCompleted {
				assert.Equal(t, *tt.args.dbic.PowerStatus, got.Status)
			} else {
				assert.Equal(t, tt.args.dbic.Status, got.Status)
			}

			assert.Equal(t, tt.args.dbic.ControllerInstanceID.String(), got.ControllerInstanceID)
			assert.Equal(t, tt.args.dbic.Created, got.Created)
			assert.Equal(t, tt.args.dbic.Updated, got.Updated)

			serialConsoleURL := fmt.Sprintf("ssh://%s@%s", tt.args.dbic.ControllerInstanceID.String(), *dbs.SerialConsoleHostname)

			assert.Equal(t, serialConsoleURL, *got.SerialConsoleURL)

			assert.Equal(t, serialConsoleURL, *got.SerialConsoleURL)
			assert.Equal(t, len(tt.args.dbsds), len(got.StatusHistory))

			assert.Equal(t, len(tt.args.dbis), len(got.Interfaces))
			assert.Equal(t, tt.args.dbis[0].ID.String(), got.Interfaces[0].ID)
			assert.Equal(t, tt.args.dbis[0].InstanceID.String(), got.Interfaces[0].InstanceID)
			assert.Equal(t, tt.args.dbis[0].SubnetID.String(), *got.Interfaces[0].SubnetID)
			assert.Equal(t, tt.args.dbis[0].IsPhysical, got.Interfaces[0].IsPhysical)
			if got.Interfaces[0].MacAddress != nil {
				assert.Equal(t, *tt.args.dbis[0].MacAddress, *got.Interfaces[0].MacAddress)
			}
			assert.Equal(t, tt.args.dbis[0].IPAddresses, got.Interfaces[0].IPAddresses)
			assert.Equal(t, tt.args.dbis[0].Status, got.Interfaces[0].Status)
			assert.Equal(t, tt.args.dbis[0].Created, got.Interfaces[0].Created)
			assert.Equal(t, tt.args.dbis[0].Updated, got.Interfaces[0].Updated)

			assert.Equal(t, len(tt.args.dbibi), len(got.InfiniBandInterfaces))
			if len(tt.args.dbibi) > 0 {
				assert.Equal(t, tt.args.dbibi[0].ID.String(), got.InfiniBandInterfaces[0].ID)
				assert.Equal(t, tt.args.dbibi[0].InfiniBandPartitionID.String(), got.InfiniBandInterfaces[0].InfiniBandPartitonID)
				assert.Equal(t, tt.args.dbibi[0].DeviceInstance, got.InfiniBandInterfaces[0].DeviceInstance)
				assert.Equal(t, tt.args.dbibi[0].IsPhysical, got.InfiniBandInterfaces[0].IsPhysical)
				assert.Equal(t, tt.args.dbibi[0].VirtualFunctionID, got.InfiniBandInterfaces[0].VirtualFunctionID)
			}

			assert.Equal(t, len(tt.args.dbdesd), len(got.DpuExtensionServiceDeployments))
			if len(tt.args.dbdesd) > 0 {
				assert.Equal(t, tt.args.dbdesd[0].ID.String(), got.DpuExtensionServiceDeployments[0].ID)
				assert.Equal(t, tt.args.dbdesd[0].DpuExtensionServiceID.String(), got.DpuExtensionServiceDeployments[0].DpuExtensionService.ID)
				assert.Equal(t, tt.args.dbdesd[0].Version, got.DpuExtensionServiceDeployments[0].Version)
			}

			assert.Equal(t, len(tt.args.dbnvl), len(got.NVLinkInterfaces))
			if len(tt.args.dbnvl) > 0 {
				assert.Equal(t, tt.args.dbnvl[0].ID.String(), got.NVLinkInterfaces[0].ID)
				assert.Equal(t, tt.args.dbnvl[0].NVLinkLogicalPartitionID.String(), got.NVLinkInterfaces[0].NVLinkLogicalPartitionID)
				assert.Equal(t, tt.args.dbnvl[0].DeviceInstance, got.NVLinkInterfaces[0].DeviceInstance)
			}

			assert.Equal(t, len(tt.args.dbskg), len(got.SSHKeyGroupIDs))
			assert.Equal(t, len(tt.args.dbskg), len(got.SSHKeyGroups))

			assert.Equal(t, *tt.args.dbic.TpmEkCertificate, *got.TpmEkCertificate)

			jsonResp, err := json.Marshal(got)
			assert.NoError(t, err)
			var attrMap map[string]interface{}
			err = json.Unmarshal(jsonResp, &attrMap)
			assert.NoError(t, err)
		})
	}
}

func TestAPIInstancePowerProfile(t *testing.T) {
	profile := "performance"
	instance := &cdbm.Instance{PowerProfile: &profile}

	got := NewAPIInstance(instance, nil, nil, nil, nil, nil, nil, nil)
	require.NotNil(t, got.PowerProfile)
	assert.Equal(t, profile, *got.PowerProfile)
	assert.True(t, (&APIInstanceUpdateRequest{PowerProfile: &profile}).IsUpdateRequest())
}

func TestAPIInstanceUpdateRequest_PowerProfileJSON(t *testing.T) {
	tests := []struct {
		name    string
		payload string
		want    *string
	}{
		{name: "omitted", payload: `{}`},
		{name: "null", payload: `{"powerProfile":null}`},
		{name: "clear", payload: `{"powerProfile":""}`, want: cutil.GetPtr("")},
		{name: "set", payload: `{"powerProfile":"performance"}`, want: cutil.GetPtr("performance")},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var request APIInstanceUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(tt.payload), &request))
			assert.Equal(t, tt.want, request.PowerProfile)
		})
	}
}

func TestAPIInstanceCreateRequest_Validate(t *testing.T) {
	type fields struct {
		Name                           string
		Description                    *string
		TenantID                       string
		InstanceTypeID                 string
		VpcID                          string
		SecondaryVpcIDs                []string
		OperatingSystemID              *string
		IpxeScript                     *string
		UserData                       *string
		Interfaces                     []APIInterfaceCreateOrUpdateRequest
		InfiniBandInterfaces           []APIInfiniBandInterfaceCreateOrUpdateRequest
		SpectrumXAttachments           []APISpectrumXAttachmentCreateOrUpdateRequest
		DpuExtensionServiceDeployments []APIDpuExtensionServiceDeploymentRequest
		NVLinkInterfaces               []APINVLinkInterfaceCreateOrUpdateRequest
		Labels                         map[string]string
		MachineLabelSelector           map[string]string
	}
	tooManyMachineLabelSelector := make(map[string]string, util.LabelCountMax+1)
	for i := 0; i <= util.LabelCountMax; i++ {
		tooManyMachineLabelSelector[fmt.Sprintf("key-%d", i)] = "value"
	}
	tests := []struct {
		name                 string
		fields               fields
		checkDefaultPhysical bool
		wantErr              bool
		wantErrorMessage     string
	}{
		{
			name: "test valid Instance with subnet create request",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			checkDefaultPhysical: true,
			wantErr:              false,
		},
		{
			name: "test valid Instance with Machine label selector",
			fields: fields{
				Name:                 "test-name",
				TenantID:             uuid.NewString(),
				InstanceTypeID:       uuid.NewString(),
				VpcID:                uuid.NewString(),
				OperatingSystemID:    cutil.GetPtr(uuid.NewString()),
				Interfaces:           []APIInterfaceCreateOrUpdateRequest{{SubnetID: cutil.GetPtr(uuid.NewString())}},
				MachineLabelSelector: map[string]string{"custom-machine-label": "required-value"},
			},
		},
		{
			name: "test invalid Instance with NUL in Machine label selector",
			fields: fields{
				Name:                 "test-name",
				TenantID:             uuid.NewString(),
				InstanceTypeID:       uuid.NewString(),
				VpcID:                uuid.NewString(),
				OperatingSystemID:    cutil.GetPtr(uuid.NewString()),
				Interfaces:           []APIInterfaceCreateOrUpdateRequest{{SubnetID: cutil.GetPtr(uuid.NewString())}},
				MachineLabelSelector: map[string]string{"custom-machine-label": "required\x00value"},
			},
			wantErr:          true,
			wantErrorMessage: "machineLabelSelector: machine label selector keys and values must not contain NUL characters",
		},
		{
			name: "test invalid Instance with too many Machine label selector",
			fields: fields{
				Name:                 "test-name",
				TenantID:             uuid.NewString(),
				InstanceTypeID:       uuid.NewString(),
				VpcID:                uuid.NewString(),
				OperatingSystemID:    cutil.GetPtr(uuid.NewString()),
				Interfaces:           []APIInterfaceCreateOrUpdateRequest{{SubnetID: cutil.GetPtr(uuid.NewString())}},
				MachineLabelSelector: tooManyMachineLabelSelector,
			},
			wantErr:          true,
			wantErrorMessage: "machineLabelSelector: up to 10 key/value pairs can be specified in labels",
		},
		{
			name: "test valid Instance with InfiniBand interface create request",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				InfiniBandInterfaces: []APIInfiniBandInterfaceCreateOrUpdateRequest{
					{
						InfiniBandPartitionID: uuid.NewString(),
						Device:                "MT28908 Family [ConnectX-6]",
						DeviceInstance:        0,
						IsPhysical:            true,
					},
					{
						InfiniBandPartitionID: uuid.NewString(),
						Device:                "MT28908 Family [ConnectX-6]",
						DeviceInstance:        1,
						IsPhysical:            true,
					},
				},
			},
			checkDefaultPhysical: true,
			wantErr:              false,
		},
		{
			name: "test valid Instance create request - DpuExtensionServiceDeployment create request",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				DpuExtensionServiceDeployments: []APIDpuExtensionServiceDeploymentRequest{
					{
						DpuExtensionServiceID: uuid.NewString(),
						Version:               "v1",
					},
					{
						DpuExtensionServiceID: uuid.NewString(),
						Version:               "v2",
					},
				},
			},
		},
		{
			name: "test valid Instance create request - physical interface specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID:   cutil.GetPtr(uuid.NewString()),
						IsPhysical: true,
					},
					{
						SubnetID:   cutil.GetPtr(uuid.NewString()),
						IsPhysical: false,
					},
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test valid Instance create request - invalid names are specified names exceeded 256 char",
			fields: fields{
				Name:              "apvhhigcgctlgiwtbrgldkegmnwuqcibutndlholygxvhzrpinziepszvpmopvzkybykrwgvzojtssorabkrnawgjzeuuerphsnecipubeuzrpewkfuvwoeybagaxpvjvzvbzqznyfmcpbxrhbdkhewiepykfjeejeqatswgrlhqkgnvwqmatejufnsjgelcugcoccybywdrnlyvsegsegorygwdvurgktpuzyrsoutspsnyzynliaxwseazqmimp",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance create request - invalid description are specified description exceeded 1024 char",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("rkbfdrkybjqtrvgeqjubtkeoolyhxicmnisgpaffhdldzgciilwbwqglwglqiibcntguwihfxttrwqvyneomiqaseaxfaeblkavuskxhpbxjbzvbtdbxmrqmgekhqtbsqhgiuirtppkurmwtzemljjzajnwqaqijefyvoqufjopbkepizjurtgezlivtwkmemfdjtkdskbtqkrkcpozdwjplhszfabwhfygxonkgjgctlulkvkqzxrngzuqgrkunwcafpamvynsjtvayqdlrbafirygnlrngxhkxccowqygidzocwbnyhebvtisdimfjqnceznffsiscdshbsrdhnggyskqawlltbmidtisehwryfwmfpvisjyqgcoekxemhixvsxgrwkhlqmkhbtrcnhrdfsakumvqtggmymnomwxvxnqdlcqzlerjrvzuxhunmdilxawtuxgmsnljhdromaoelxgzfsgnhttulfqwzxvufqpoxadlbnmbtlckxgrzfirvscbpzanhrhgvrsdmkqfahfridkdlhgbjvdenhibsfjdgbaiszwyqilhslwvxtstrtmdvumdtpruduviujwytepzxzexbjnuizhpvufzcjicrlaqsvufrivvsgbcrqaztasucyjtfefbxoolzmgshhjeegmwcziqgjhninqphkvgpguquikakbljemkvbpfajumhlaiahaijvdsodklqsxorsdtiksqfhmnrutafnyqqswfmfzahwmucyhwzgxthnedcfuaptaqzylwuljaxybgyenybekgkfgmpxmkcvcmyxyazykchibcugfcwxezqcwmghjweppuisyttkaodulvgzpwfzdtstvvotulpookzpwxoyqhdzcljsguieoijbsoprtfbgzuoiiogudmjpabkfiydqyatymujnfpdhugbijaawzbohqfkvlbaojsasbyrxrapdphqmnqabpbrcowvxfwlfabbmijdabqacvpokeogjhhmoswfqulgzly"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance create request - accept description as empty string",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr(""),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID:   cutil.GetPtr(uuid.NewString()),
						IsPhysical: true,
					},
					{
						SubnetID:   cutil.GetPtr(uuid.NewString()),
						IsPhysical: false,
					},
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance create request - invalid VPC ID",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             "invalid-uuid",
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - invalid Subnet ID",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr("invalid-uuid"),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request, no Subnet specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces:        []APIInterfaceCreateOrUpdateRequest{},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance create request, Invalid Subnet, multiple physical interface has been set",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID:   cutil.GetPtr(uuid.NewString()),
						IsPhysical: true,
					},
					{
						SubnetID:   cutil.GetPtr(uuid.NewString()),
						IsPhysical: true,
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance create request, DpuExtensionServiceDeployment create request with invalid DpuExtensionServiceID",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				DpuExtensionServiceDeployments: []APIDpuExtensionServiceDeploymentRequest{
					{
						DpuExtensionServiceID: "invalid-uuid",
						Version:               "v1",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance create request, DpuExtensionServiceDeployment create request with duplicate service and version",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				DpuExtensionServiceDeployments: []APIDpuExtensionServiceDeploymentRequest{
					{
						DpuExtensionServiceID: "c35af8c2-6073-4752-b3cb-eff40de45a20",
						Version:               "v1",
					},
					{
						DpuExtensionServiceID: "c35af8c2-6073-4752-b3cb-eff40de45a20",
						Version:               "v1",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance create request, no Interfaces specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
			},
			wantErr: true,
		},
		{
			name: "test valid Instance create request, subnet Interfaces specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test valid Instance create request, Vpcprefix Interfaces specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						VpcPrefixID: cutil.GetPtr(uuid.NewString()),
						IsPhysical:  true,
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance create request, both subnet and vpcprefix specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
					{
						VpcPrefixID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance create request, no Operating System specified",
			fields: fields{
				Name:           "test-name",
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test-ipxe-script"),
				UserData:       cutil.GetPtr("test-user-data"),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance create request, no Operating System and empty ipxeScript specified",
			fields: fields{
				Name:           "test-name",
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr(""),
				UserData:       cutil.GetPtr("test-user-data"),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance create request - valid labels are specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				Labels: map[string]string{
					"name":        "a-nv100-instance",
					"description": "",
				},
			},
			wantErr: false,
		},
		{
			name: "test valid Instance create request - invalid labels are specified key is empty",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				Labels: map[string]string{
					"name": "a-nv200=instance",
					"":     "test",
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance create request - invalid labels are specified both key and value are empty",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				Labels: map[string]string{
					"name": "a-nv300=instance",
					"":     "",
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance create request - invalid labels are specified key has char more than 255",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				Labels: map[string]string{
					"ynGJz5YL06ZA9OZfNTUTId77jNrYPilVe9MLea1MGXHwueojLYbhvheA7obW06etCPpAis8199Giktlpn1cxDDIsHiFcGr5JR25XXhB16F3DNnTUwdoFgsB1wH1uXioISTohUo2CjXKIbrGaXQoMM6oBPTJFyIMwph3fJx4lNa8cPPP5Vdfq2WKjuVeS5t2Dv8rlNjguc3J3siYlkuL7KxEmkModeq9W0j1plWv1yVKjBxVeu013XzCCicVSPtj2HQFO2O6rtg": "a-nv300=instance",
				},
			},
			wantErr:          true,
			wantErrorMessage: "label key must contain at least 1 character and a maximum of 255 characters.",
		},
		{
			name: "test invalid Instance create request, secondary VPCs require vpcPrefix interfaces",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				SecondaryVpcIDs:   []string{uuid.NewString()},
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr:          true,
			wantErrorMessage: "`secondaryVpcIds` can only be specified when `vpcPrefixId` or `vpcId` is specified within `interfaces`",
		},
		{
			name: "test valid Instance create request, NVLink Interfaces specified",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				NVLinkInterfaces: []APINVLinkInterfaceCreateOrUpdateRequest{
					{
						NVLinkLogicalPartitionID: uuid.NewString(),
						DeviceInstance:           0,
					},
				},
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance create request, NVLink Interfaces specified with device instance out of range",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				NVLinkInterfaces: []APINVLinkInterfaceCreateOrUpdateRequest{
					{
						NVLinkLogicalPartitionID: uuid.NewString(),
						DeviceInstance:           4,
					},
				},
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr:          true,
			wantErrorMessage: "deviceInstance: deviceInstance must be between 0 and 3",
		},
		{
			name: "test valid Instance with SpectrumX attachment create request",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				SpectrumXAttachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
					{
						SpectrumXPartitionID: uuid.NewString(),
						Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
						DeviceInstance:       cutil.GetPtr(0),
						AttachmentType:       SpectrumXAttachmentTypePhysical,
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test valid Instance create request with userData at max length",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr(strings.Repeat("a", util.MaxUserDataBytes)),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test Instance create request failed, invalid SpectrumX attachment type",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
				SpectrumXAttachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
					{
						SpectrumXPartitionID: uuid.NewString(),
						Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
						DeviceInstance:       cutil.GetPtr(0),
						AttachmentType:       "Bogus",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance create request with userData exceeding max length",
			fields: fields{
				Name:              "test-name",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr(strings.Repeat("a", util.MaxUserDataBytes+1)),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr:          true,
			wantErrorMessage: "userData: " + validationErrorUserDataLength,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			icr := APIInstanceCreateRequest{
				Name:                           tt.fields.Name,
				Description:                    tt.fields.Description,
				TenantID:                       tt.fields.TenantID,
				InstanceTypeID:                 &tt.fields.InstanceTypeID,
				VpcID:                          tt.fields.VpcID,
				SecondaryVpcIDs:                tt.fields.SecondaryVpcIDs,
				OperatingSystemID:              tt.fields.OperatingSystemID,
				IpxeScript:                     tt.fields.IpxeScript,
				UserData:                       tt.fields.UserData,
				Interfaces:                     tt.fields.Interfaces,
				InfiniBandInterfaces:           tt.fields.InfiniBandInterfaces,
				SpectrumXAttachments:           tt.fields.SpectrumXAttachments,
				DpuExtensionServiceDeployments: tt.fields.DpuExtensionServiceDeployments,
				NVLinkInterfaces:               tt.fields.NVLinkInterfaces,
				Labels:                         tt.fields.Labels,
				MachineLabelSelector:           tt.fields.MachineLabelSelector,
			}

			err := icr.Validate()
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIInstanceCreateRequest.Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}

			if tt.wantErrorMessage != "" && err != nil {
				assert.Contains(t, err.Error(), tt.wantErrorMessage)
			}

			if tt.checkDefaultPhysical {
				if tt.fields.Interfaces != nil {
					assert.True(t, tt.fields.Interfaces[0].IsPhysical)
				}
			}
		})
	}
}

func TestAPIBatchInstanceCreateRequest_Validate(t *testing.T) {
	tooManyMachineLabelSelector := make(map[string]string, util.LabelCountMax+1)
	for i := 0; i <= util.LabelCountMax; i++ {
		tooManyMachineLabelSelector[fmt.Sprintf("key-%d", i)] = "value"
	}
	tests := []struct {
		name             string
		req              APIBatchInstanceCreateRequest
		wantErr          bool
		wantErrorMessage string
	}{
		{
			name: "succeeds without requested interface ip",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:     "test-batch",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test ipxe"),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{SubnetID: cutil.GetPtr(uuid.NewString())},
				},
			},
			wantErr: false,
		},
		{
			name: "succeeds with Machine label selector",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:           "test-batch",
				Count:                2,
				TenantID:             uuid.NewString(),
				InstanceTypeID:       uuid.NewString(),
				VpcID:                uuid.NewString(),
				IpxeScript:           cutil.GetPtr("test ipxe"),
				Interfaces:           []APIInterfaceCreateOrUpdateRequest{{SubnetID: cutil.GetPtr(uuid.NewString())}},
				MachineLabelSelector: map[string]string{"custom-machine-label": "required-value"},
			},
		},
		{
			name: "fails with NUL in Machine label selector",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:           "test-batch",
				Count:                2,
				TenantID:             uuid.NewString(),
				InstanceTypeID:       uuid.NewString(),
				VpcID:                uuid.NewString(),
				IpxeScript:           cutil.GetPtr("test ipxe"),
				Interfaces:           []APIInterfaceCreateOrUpdateRequest{{SubnetID: cutil.GetPtr(uuid.NewString())}},
				MachineLabelSelector: map[string]string{"custom\x00machine-label": "required-value"},
			},
			wantErr:          true,
			wantErrorMessage: "machineLabelSelector: machine label selector keys and values must not contain NUL characters",
		},
		{
			name: "fails with too many Machine label selector",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:           "test-batch",
				Count:                2,
				TenantID:             uuid.NewString(),
				InstanceTypeID:       uuid.NewString(),
				VpcID:                uuid.NewString(),
				IpxeScript:           cutil.GetPtr("test ipxe"),
				Interfaces:           []APIInterfaceCreateOrUpdateRequest{{SubnetID: cutil.GetPtr(uuid.NewString())}},
				MachineLabelSelector: tooManyMachineLabelSelector,
			},
			wantErr:          true,
			wantErrorMessage: "machineLabelSelector: up to 10 key/value pairs can be specified in labels",
		},
		{
			name: "fails when any interface uses requested ip",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:     "test-batch",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test ipxe"),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						VpcPrefixID: cutil.GetPtr(uuid.NewString()),
						IPAddress:   cutil.GetPtr("10.0.0.11"),
					},
				},
			},
			wantErr:          true,
			wantErrorMessage: "batch instance create does not support `ipAddress` on interfaces",
		},
		{
			name: "succeeds with SpectrumX attachment create request",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:     "test-batch",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test ipxe"),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{SubnetID: cutil.GetPtr(uuid.NewString())},
				},
				SpectrumXAttachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
					{
						SpectrumXPartitionID: uuid.NewString(),
						Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
						DeviceInstance:       cutil.GetPtr(0),
						AttachmentType:       SpectrumXAttachmentTypePhysical,
					},
				},
			},
			wantErr: false,
		},
		{
			name: "fails with invalid SpectrumX attachment type",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:     "test-batch",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test ipxe"),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{SubnetID: cutil.GetPtr(uuid.NewString())},
				},
				SpectrumXAttachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
					{
						SpectrumXPartitionID: uuid.NewString(),
						Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
						DeviceInstance:       cutil.GetPtr(0),
						AttachmentType:       "Bogus",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "fails with userData exceeding max length",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:     "test-batch",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test ipxe"),
				UserData:       cutil.GetPtr(strings.Repeat("a", util.MaxUserDataBytes+1)),
				Interfaces:     []APIInterfaceCreateOrUpdateRequest{{SubnetID: cutil.GetPtr(uuid.NewString())}},
			},
			wantErr:          true,
			wantErrorMessage: "userData: " + validationErrorUserDataLength,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIBatchInstanceCreateRequest.Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}

			if tt.wantErrorMessage != "" && err != nil {
				assert.Contains(t, err.Error(), tt.wantErrorMessage)
			}
		})
	}
}

func TestInstanceCreateRequestsValidatePowerProfile(t *testing.T) {
	profiles := []struct {
		name    string
		value   *string
		wantErr bool
	}{
		{name: "omitted"},
		{name: "empty", value: cutil.GetPtr(""), wantErr: true},
		{name: "non-empty", value: cutil.GetPtr("balanced")},
	}

	validators := []struct {
		name     string
		validate func(*string) error
	}{
		{
			name: "single create",
			validate: func(powerProfile *string) error {
				return (APIInstanceCreateRequest{
					Name:              "test-instance",
					TenantID:          uuid.NewString(),
					InstanceTypeID:    cutil.GetPtr(uuid.NewString()),
					VpcID:             uuid.NewString(),
					OperatingSystemID: cutil.GetPtr(uuid.NewString()),
					PowerProfile:      powerProfile,
					Interfaces: []APIInterfaceCreateOrUpdateRequest{
						{SubnetID: cutil.GetPtr(uuid.NewString())},
					},
				}).Validate()
			},
		},
		{
			name: "batch create",
			validate: func(powerProfile *string) error {
				return (APIBatchInstanceCreateRequest{
					NamePrefix:        "test-batch",
					Count:             2,
					TenantID:          uuid.NewString(),
					InstanceTypeID:    uuid.NewString(),
					VpcID:             uuid.NewString(),
					OperatingSystemID: cutil.GetPtr(uuid.NewString()),
					PowerProfile:      powerProfile,
					Interfaces: []APIInterfaceCreateOrUpdateRequest{
						{SubnetID: cutil.GetPtr(uuid.NewString())},
					},
				}).Validate()
			},
		},
	}

	for _, validator := range validators {
		for _, profile := range profiles {
			t.Run(validator.name+"/"+profile.name, func(t *testing.T) {
				err := validator.validate(profile.value)
				if profile.wantErr {
					require.Error(t, err)
					assert.Contains(t, err.Error(), "`powerProfile` must not be empty")
					return
				}
				require.NoError(t, err)
			})
		}
	}
}

func TestValidateMachineLabelSelector(t *testing.T) {
	tooManyFilters := make(map[string]string, util.LabelCountMax+1)
	for i := 0; i <= util.LabelCountMax; i++ {
		tooManyFilters[fmt.Sprintf("key-%d", i)] = "value"
	}

	tests := []struct {
		name    string
		filters map[string]string
		wantErr bool
	}{
		{
			name: "empty",
		},
		{
			name: "valid",
			filters: map[string]string{
				"failure-domain": "fd-a",
				"power-domain":   "pd-2",
			},
		},
		{
			name:    "too many",
			filters: tooManyFilters,
			wantErr: true,
		},
		{
			name:    "empty key",
			filters: map[string]string{"": "value"},
			wantErr: true,
		},
		{
			name:    "empty value allowed",
			filters: map[string]string{"key": ""},
		},
		{
			name:    "NUL in key",
			filters: map[string]string{"key\x00suffix": "value"},
			wantErr: true,
		},
		{
			name:    "NUL in value",
			filters: map[string]string{"key": "value\x00suffix"},
			wantErr: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := validateMachineLabelSelector(test.filters)
			if !test.wantErr {
				require.NoError(t, err)
				return
			}

			require.Error(t, err)
			assert.IsType(t, validation.Errors{}, err)
			assert.Contains(t, err.Error(), "machineLabelSelector")
		})
	}
}

func TestInstanceRequestsAcceptVpcSelectedSecondaryInterfaces(t *testing.T) {
	tests := []struct {
		name     string
		validate func() error
	}{
		{
			name: "instance create",
			validate: func() error {
				request := APIInstanceCreateRequest{
					Name:              "test-instance",
					TenantID:          uuid.NewString(),
					InstanceTypeID:    cutil.GetPtr(uuid.NewString()),
					VpcID:             uuid.NewString(),
					SecondaryVpcIDs:   []string{uuid.NewString()},
					OperatingSystemID: cutil.GetPtr(uuid.NewString()),
					Interfaces: []APIInterfaceCreateOrUpdateRequest{
						{VpcPrefixID: cutil.GetPtr(uuid.NewString())},
						{VpcID: cutil.GetPtr(uuid.NewString()), IPFamilies: []IPFamily{IPFamilyIPv4}},
					},
				}
				return request.Validate()
			},
		},
		{
			name: "batch instance create",
			validate: func() error {
				request := APIBatchInstanceCreateRequest{
					NamePrefix:      "test-batch",
					Count:           2,
					TenantID:        uuid.NewString(),
					InstanceTypeID:  uuid.NewString(),
					VpcID:           uuid.NewString(),
					SecondaryVpcIDs: []string{uuid.NewString()},
					IpxeScript:      cutil.GetPtr("test ipxe"),
					Interfaces: []APIInterfaceCreateOrUpdateRequest{
						{VpcID: cutil.GetPtr(uuid.NewString()), IPFamilies: []IPFamily{IPFamilyIPv4}},
					},
				}
				return request.Validate()
			},
		},
		{
			name: "instance update",
			validate: func() error {
				request := APIInstanceUpdateRequest{
					SecondaryVpcIDs: []string{uuid.NewString()},
					Interfaces: []APIInterfaceCreateOrUpdateRequest{
						{VpcID: cutil.GetPtr(uuid.NewString()), IPFamilies: []IPFamily{IPFamilyIPv4}},
					},
				}
				return request.Validate()
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.NoError(t, tt.validate())
		})
	}
}

func TestAPIInstanceCreateRequest_ValidateAndSetOperatingSystemData(t *testing.T) {

	cfg1 := config.NewConfig()

	cfg1.SetSitePhoneHomeUrl("http://localhost/local")

	type fields struct {
		Name                     string
		Description              *string
		TenantID                 string
		InstanceTypeID           string
		VpcID                    string
		OperatingSystemID        *string
		IpxeScript               *string
		UserData                 *string
		PhoneHomeEnabled         *bool
		AlwaysBootWithCustomIpxe *bool
	}

	os := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr(util.TestCommonCloudInit),
		PhoneHomeEnabled: true,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		AllowOverride:    true,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	osNoOverride := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr(util.TestCommonCloudInit),
		PhoneHomeEnabled: true,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		AllowOverride:    false,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	imageOs := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "image_os",
		IpxeScript:       nil,
		UserData:         nil,
		PhoneHomeEnabled: false,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		AllowOverride:    false,
		Type:             cdbm.OperatingSystemTypeImage,
		CreatedBy:        uuid.New(),
	}

	imageOSDeactivated := new(cdbm.OperatingSystem)
	*imageOSDeactivated = *imageOs
	imageOSDeactivated.IsActive = false
	imageOSDeactivated.ID = uuid.New()

	// Phone-home is enabled and the stored user-data has a phone-home URL,
	// but it is not the configured one: what NICo wrote back when the
	// configured URL was different.
	osStaleURL := &cdbm.OperatingSystem{
		ID:         uuid.New(),
		Name:       "ab",
		IpxeScript: cutil.GetPtr("original ipxe"),
		UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: http://169.254.169.254:7777/latest/meta-data/phone_home
`),
		PhoneHomeEnabled: true,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		AllowOverride:    true,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	tests := []struct {
		name                     string
		fields                   fields
		cfg                      *config.Config
		os                       *cdbm.OperatingSystem
		wantUserData             *string
		userDataSearches         []string
		userDataNegativeSearches []string
		wantErr                  bool
	}{
		{
			name: "ipxe os selected, os has user-data, no override allowed, user-data specified, should fail",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr("some user data"),
			},
			os:      osNoOverride,
			cfg:     cfg1,
			wantErr: true,
		},
		{
			name: "ipxe os selected, os has user-data, no override allowed, user-data not specified, should succeed",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
			},
			os:      osNoOverride,
			cfg:     cfg1,
			wantErr: false,
		},
		{
			name: "ipxe os selected, os user-data over max length, user-data not specified, should fail",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
			},
			os: &cdbm.OperatingSystem{
				ID:               uuid.New(),
				Name:             "ab",
				IpxeScript:       cutil.GetPtr("original ipxe"),
				UserData:         cutil.GetPtr("a: " + strings.Repeat("b", util.MaxUserDataBytes)),
				PhoneHomeEnabled: false,
				IsActive:         true,
				Status:           cdbm.OperatingSystemStatusReady,
				AllowOverride:    true,
				Type:             cdbm.OperatingSystemTypeIPXE,
				CreatedBy:        uuid.New(),
			},
			cfg:     cfg1,
			wantErr: true,
		},
		{
			name: "image os selected, iPXE specified, should fail",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          nil,
			},
			os:      imageOs,
			cfg:     cfg1,
			wantErr: true,
		},
		{
			name: "image os selected, phone-enabled, should succeed",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        nil,
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			os:      imageOs,
			cfg:     cfg1,
			wantErr: false,
		},
		{
			name: "deactivated image os selected, should fail",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          nil,
			},
			os:      imageOSDeactivated,
			cfg:     cfg1,
			wantErr: true,
		},
		{
			name: "image os selected, AlwaysBootWithCustomIpxe selected, should fail",
			fields: fields{
				Name:                     "test-name",
				Description:              cutil.GetPtr("Test description"),
				TenantID:                 uuid.NewString(),
				InstanceTypeID:           uuid.NewString(),
				VpcID:                    uuid.NewString(),
				OperatingSystemID:        cutil.GetPtr(uuid.NewString()),
				IpxeScript:               nil,
				UserData:                 nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: cutil.GetPtr(true),
			},
			os:      imageOs,
			cfg:     cfg1,
			wantErr: true,
		},
		{
			name: "os is nil, no iPXE specified, should fail",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        nil,
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr: true,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is nil when OS in nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr: false,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is invalid when OS in nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          cutil.GetPtr("test"),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr: true,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance create request when phone home flag is disabled, userData is non YAML and OS in nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          cutil.GetPtr("test-user-data"),
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr: false,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance create request when phone home flag is not specified, userData is non YAML and OS in nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          cutil.GetPtr("test-user-data"),
			},
			wantErr: false,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance create request when phone home flag is specified but disabled, userData is non YAML, and OS in nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          cutil.GetPtr("test-user-data:\ninvalid\n"),
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantUserData: cutil.GetPtr("test-user-data:\ninvalid\n"),
			wantErr:      false,
			cfg:          cfg1,
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is valid when OS in nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          cutil.GetPtr(util.TestCommonCloudInit),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr: false,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is technically valid but functionally empty when OS in nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          cutil.GetPtr("#cloud-config"),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr: true,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is technically valid but functionally empty when OS in nil, but phonehome disabled",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				UserData:          cutil.GetPtr("#cloud-config"),
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr: false,
			cfg:     cfg1,
		},
		{
			name: "test valid Instance PhoneHome enabled create request when OS in present with phonehome",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(os.ID.String()),
				UserData:          nil,
				PhoneHomeEnabled:  nil,
			},
			wantErr: false,
			cfg:     cfg1,
			os:      os,
		},
		{
			name: "test valid Instance PhoneHome enabled create request when OS in present with phonehome, userData does't contain phone home url",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(os.ID.String()),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr: false,
			cfg:     cfg1,
			os:      os,
		},
		{
			name: "test valid Instance PhoneHome disabled create request, no userData override, OS blob has stale NICo phone-home URL",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(osStaleURL.ID.String()),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr: false,
			cfg:     cfg1,
			os:      osStaleURL,
			// NICo authored the OS blob's block, so it is removed by key
			// without matching the URL: neither the key nor the stale URL
			// survives, while the rest of the document is preserved.
			userDataSearches:         []string{"package_update"},
			userDataNegativeSearches: []string{"phone_home", "169.254.169.254"},
		},
		{
			name: "test valid Instance PhoneHome disabled create request with caller-supplied userData carrying its own phone-home block",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          uuid.NewString(),
				InstanceTypeID:    uuid.NewString(),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(osStaleURL.ID.String()),
				UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: https://collector.example.com/hook
`),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			wantErr: false,
			cfg:     cfg1,
			os:      osStaleURL,
			// The document being edited is the caller's, so removal stays
			// URL-matched and their hook survives.
			userDataSearches:         []string{"collector.example.com", "package_update"},
			userDataNegativeSearches: []string{"169.254.169.254"},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			icr := APIInstanceCreateRequest{
				Name:                     tt.fields.Name,
				Description:              tt.fields.Description,
				TenantID:                 tt.fields.TenantID,
				InstanceTypeID:           &tt.fields.InstanceTypeID,
				VpcID:                    tt.fields.VpcID,
				OperatingSystemID:        tt.fields.OperatingSystemID,
				IpxeScript:               tt.fields.IpxeScript,
				UserData:                 tt.fields.UserData,
				PhoneHomeEnabled:         tt.fields.PhoneHomeEnabled,
				AlwaysBootWithCustomIpxe: tt.fields.AlwaysBootWithCustomIpxe,
			}

			err := icr.ValidateAndSetOperatingSystemData(cfg1, tt.os)
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIInstanceCreateRequest.ValidateAndSetOperatingSystemData() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}

			if err != nil {
				return
			}

			if tt.wantUserData != nil {
				assert.NotNil(t, icr.UserData)
				assert.Contains(t, *icr.UserData, *tt.wantUserData)
			}

			if len(tt.userDataSearches) > 0 {
				require.NotNil(t, icr.UserData)
				for _, search := range tt.userDataSearches {
					assert.Contains(t, *icr.UserData, search)
				}
			}

			if icr.UserData != nil {
				for _, search := range tt.userDataNegativeSearches {
					assert.NotContains(t, *icr.UserData, search)
				}
			}

			if (icr.PhoneHomeEnabled != nil && *icr.PhoneHomeEnabled) || (icr.PhoneHomeEnabled == nil && tt.os != nil && tt.os.PhoneHomeEnabled) {
				assert.NotNil(t, icr.UserData)
				assert.Contains(t, *icr.UserData, tt.cfg.GetSitePhoneHomeUrl())
			}

			if icr.PhoneHomeEnabled != nil && !*icr.PhoneHomeEnabled {
				assert.NotContains(t, *icr.UserData, tt.cfg.GetSitePhoneHomeUrl())
			}
		})
	}
}

func TestAPIBatchInstanceCreateRequest_ValidateAndSetOperatingSystemData(t *testing.T) {
	cfg1 := config.NewConfig()
	cfg1.SetSitePhoneHomeUrl("http://localhost/local")

	os := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr(util.TestCommonCloudInit),
		PhoneHomeEnabled: true,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		AllowOverride:    true,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	// Phone-home is enabled and the stored user-data has a phone-home URL,
	// but it is not the configured one: what NICo wrote back when the
	// configured URL was different.
	osStaleURL := &cdbm.OperatingSystem{
		ID:         uuid.New(),
		Name:       "ab",
		IpxeScript: cutil.GetPtr("original ipxe"),
		UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: http://169.254.169.254:7777/latest/meta-data/phone_home
`),
		PhoneHomeEnabled: true,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		AllowOverride:    true,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	tests := []struct {
		name                     string
		request                  *APIBatchInstanceCreateRequest
		os                       *cdbm.OperatingSystem
		userDataSearches         []string
		userDataNegativeSearches []string
		wantErr                  bool
	}{
		{
			name: "PhoneHome enabled from OS, no overrides, phone-home URL inserted",
			request: &APIBatchInstanceCreateRequest{
				NamePrefix:     "worker",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
			},
			os:               os,
			wantErr:          false,
			userDataSearches: []string{cfg1.GetSitePhoneHomeUrl()},
		},
		{
			name: "PhoneHome disabled in request, no userData override, OS blob has stale NICo phone-home URL",
			request: &APIBatchInstanceCreateRequest{
				NamePrefix:       "worker",
				Count:            2,
				TenantID:         uuid.NewString(),
				InstanceTypeID:   uuid.NewString(),
				VpcID:            uuid.NewString(),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			os:      osStaleURL,
			wantErr: false,
			// NICo authored the OS blob's block, so it is removed by key
			// without matching the URL: neither the key nor the stale URL
			// survives, while the rest of the document is preserved.
			userDataSearches:         []string{"package_update"},
			userDataNegativeSearches: []string{"phone_home", "169.254.169.254"},
		},
		{
			name: "PhoneHome disabled in request with caller-supplied userData carrying its own phone-home block",
			request: &APIBatchInstanceCreateRequest{
				NamePrefix:       "worker",
				Count:            2,
				TenantID:         uuid.NewString(),
				InstanceTypeID:   uuid.NewString(),
				VpcID:            uuid.NewString(),
				PhoneHomeEnabled: cutil.GetPtr(false),
				UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: https://collector.example.com/hook
`),
			},
			os:      osStaleURL,
			wantErr: false,
			// The document being edited is the caller's, so removal stays
			// URL-matched and their hook survives.
			userDataSearches:         []string{"collector.example.com", "package_update"},
			userDataNegativeSearches: []string{"169.254.169.254"},
		},
		{
			name: "os user-data over max length inherited fails",
			request: &APIBatchInstanceCreateRequest{
				NamePrefix:     "worker",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
			},
			os: &cdbm.OperatingSystem{
				ID:            uuid.New(),
				Name:          "ab",
				Type:          cdbm.OperatingSystemTypeIPXE,
				IpxeScript:    cutil.GetPtr("original ipxe"),
				UserData:      cutil.GetPtr("a: " + strings.Repeat("b", util.MaxUserDataBytes)),
				IsActive:      true,
				AllowOverride: true,
			},
			wantErr: true,
		},
		{
			name: "no OS and no iPXE script fails",
			request: &APIBatchInstanceCreateRequest{
				NamePrefix:     "worker",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			bicr := tt.request

			err := bicr.ValidateAndSetOperatingSystemData(cfg1, tt.os)
			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)

			if len(tt.userDataSearches) > 0 {
				require.NotNil(t, bicr.UserData)
				for _, search := range tt.userDataSearches {
					assert.Contains(t, *bicr.UserData, search)
				}
			}

			if bicr.UserData != nil {
				for _, search := range tt.userDataNegativeSearches {
					assert.NotContains(t, *bicr.UserData, search)
				}
			}
		})
	}
}

func TestAPIInstanceUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		Name                     *string
		Description              *string
		Labels                   map[string]string
		TriggerReboot            *bool
		RebootWithCustomIpxe     *bool
		ApplyUpdatesOnReboot     *bool
		OperatingSystemID        *string
		IpxeScript               *string
		UserData                 *string
		PhoneHomeEnabled         *bool
		AlwaysBootWithCustomIpxe *bool
		SecondaryVpcIDs          []string
		Interfaces               []APIInterfaceCreateOrUpdateRequest
		InfiniBandInterfaces     []APIInfiniBandInterfaceCreateOrUpdateRequest
		SpectrumXAttachments     []APISpectrumXAttachmentCreateOrUpdateRequest
		NVLinkInterfaces         []APINVLinkInterfaceCreateOrUpdateRequest
		SSHKeyGroupIDs           []string
		NetworkSecurityGroupID   *string
	}
	tests := []struct {
		name              string
		fields            fields
		wantErr           bool
		wantUpdateRequest *bool
	}{
		{
			name: "test valid Instance config update request",
			fields: fields{
				Name:              cutil.GetPtr("test-name"),
				Description:       cutil.GetPtr("Test description"),
				Labels:            map[string]string{"name": "test-name", "description": "Test description"},
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				PhoneHomeEnabled:  cutil.GetPtr(true),
				SecondaryVpcIDs:   []string{uuid.NewString()},
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						VpcPrefixID: cutil.GetPtr(uuid.NewString()),
						IsPhysical:  false,
					},
				},
				InfiniBandInterfaces: []APIInfiniBandInterfaceCreateOrUpdateRequest{
					{
						InfiniBandPartitionID: uuid.NewString(),
						Device:                "MT28908 Family [ConnectX-6]",
						DeviceInstance:        0,
						IsPhysical:            true,
					},
					{
						InfiniBandPartitionID: uuid.NewString(),
						Device:                "MT28908 Family [ConnectX-6]",
						DeviceInstance:        1,
						IsPhysical:            true,
					},
				},
				SSHKeyGroupIDs:         []string{uuid.NewString()},
				NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()),
			},
			wantErr:           false,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test valid Instance reboot request",
			fields: fields{
				TriggerReboot:        cutil.GetPtr(true),
				RebootWithCustomIpxe: cutil.GetPtr(true),
				ApplyUpdatesOnReboot: cutil.GetPtr(true),
			},
			wantErr:           false,
			wantUpdateRequest: cutil.GetPtr(false),
		},
		{
			name: "test invalid Instance update request, name exceeded 256 char",
			fields: fields{
				Name:        cutil.GetPtr("apvhhigcgctlgiwtbrgldkegmnwuqcibutndlholygxvhzrpinziepszvpmopvzkybykrwgvzojtssorabkrnawgjzeuuerphsnecipubeuzrpewkfuvwoeybagaxpvjvzvbzqznyfmcpbxrhbdkhewiepykfjeejeqatswgrlhqkgnvwqmatejufnsjgelcugcoccybywdrnlyvsegsegorygwdvurgktpuzyrsoutspsnyzynliaxwseazqmimp"),
				Description: cutil.GetPtr("Test description"),
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance update request, description exceeded 1024 char",
			fields: fields{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("rkbfdrkybjqtrvgeqjubtkeoolyhxicmnisgpaffhdldzgciilwbwqglwglqiibcntguwihfxttrwqvyneomiqaseaxfaeblkavuskxhpbxjbzvbtdbxmrqmgekhqtbsqhgiuirtppkurmwtzemljjzajnwqaqijefyvoqufjopbkepizjurtgezlivtwkmemfdjtkdskbtqkrkcpozdwjplhszfabwhfygxonkgjgctlulkvkqzxrngzuqgrkunwcafpamvynsjtvayqdlrbafirygnlrngxhkxccowqygidzocwbnyhebvtisdimfjqnceznffsiscdshbsrdhnggyskqawlltbmidtisehwryfwmfpvisjyqgcoekxemhixvsxgrwkhlqmkhbtrcnhrdfsakumvqtggmymnomwxvxnqdlcqzlerjrvzuxhunmdilxawtuxgmsnljhdromaoelxgzfsgnhttulfqwzxvufqpoxadlbnmbtlckxgrzfirvscbpzanhrhgvrsdmkqfahfridkdlhgbjvdenhibsfjdgbaiszwyqilhslwvxtstrtmdvumdtpruduviujwytepzxzexbjnuizhpvufzcjicrlaqsvufrivvsgbcrqaztasucyjtfefbxoolzmgshhjeegmwcziqgjhninqphkvgpguquikakbljemkvbpfajumhlaiahaijvdsodklqsxorsdtiksqfhmnrutafnyqqswfmfzahwmucyhwzgxthnedcfuaptaqzylwuljaxybgyenybekgkfgmpxmkcvcmyxyazykchibcugfcwxezqcwmghjweppuisyttkaodulvgzpwfzdtstvvotulpookzpwxoyqhdzcljsguieoijbsoprtfbgzuoiiogudmjpabkfiydqyatymujnfpdhugbijaawzbohqfkvlbaojsasbyrxrapdphqmnqabpbrcowvxfwlfabbmijdabqacvpokeogjhhmoswfqulgzly"),
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test valid Instance update request, accept description as empty string",
			fields: fields{
				Name:              cutil.GetPtr("test-name"),
				Description:       cutil.GetPtr(""),
				Labels:            map[string]string{"name": "test-name", "description": ""},
				IpxeScript:        cutil.GetPtr("#ipxe\ndefault"),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				PhoneHomeEnabled:  cutil.GetPtr(true),
				InfiniBandInterfaces: []APIInfiniBandInterfaceCreateOrUpdateRequest{
					{
						InfiniBandPartitionID: uuid.NewString(),
						Device:                "MT28908 Family [ConnectX-6]",
						DeviceInstance:        1,
						IsPhysical:            true,
					},
				},
				SSHKeyGroupIDs:         []string{uuid.NewString()},
				NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()),
			},
			wantErr:           false,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test valid Instance update request ApplyUpdatesOnReboot can't be specifed",
			fields: fields{
				Name:                 cutil.GetPtr("test-name"),
				Description:          cutil.GetPtr("Test description"),
				ApplyUpdatesOnReboot: cutil.GetPtr(true),
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance config update request, invalid InfiniBand Interface device instance",
			fields: fields{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("Test description"),
				InfiniBandInterfaces: []APIInfiniBandInterfaceCreateOrUpdateRequest{
					{
						InfiniBandPartitionID: uuid.NewString(),
						Device:                "MT28908 Family [ConnectX-6]",
						DeviceInstance:        -1,
						IsPhysical:            true,
					},
				},
				SSHKeyGroupIDs:         []string{uuid.NewString()},
				NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()),
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance config update request, invalid Interface device instance",
			fields: fields{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("Test description"),
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						VpcPrefixID:    cutil.GetPtr(uuid.NewString()),
						Device:         cutil.GetPtr("MT28908 Family [ConnectX-6]"),
						DeviceInstance: cutil.GetPtr(-1),
						IsPhysical:     true,
					},
				},
				SSHKeyGroupIDs:         []string{uuid.NewString()},
				NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()),
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance update request, NVLink Interfaces specified with device instance out of range",
			fields: fields{
				Name:        cutil.GetPtr("test-invalid-nvlink-interfaces-device-instance-out-of-range"),
				Description: cutil.GetPtr("Test description"),
				NVLinkInterfaces: []APINVLinkInterfaceCreateOrUpdateRequest{
					{
						NVLinkLogicalPartitionID: uuid.NewString(),
						DeviceInstance:           4,
					},
				},
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						VpcPrefixID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance update request, secondary VPCs require interfaces",
			fields: fields{
				SecondaryVpcIDs: []string{uuid.NewString()},
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance update request, secondary VPCs require vpcPrefix interfaces",
			fields: fields{
				SecondaryVpcIDs: []string{uuid.NewString()},
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{
						SubnetID: cutil.GetPtr(uuid.NewString()),
					},
				},
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test valid Instance update request, SpectrumX attachments",
			fields: fields{
				SpectrumXAttachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
					{
						SpectrumXPartitionID: uuid.NewString(),
						Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
						DeviceInstance:       cutil.GetPtr(0),
						AttachmentType:       SpectrumXAttachmentTypeOVN,
					},
				},
			},
			wantErr:           false,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance update request, SpectrumX attachment missing device",
			fields: fields{
				SpectrumXAttachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
					{
						SpectrumXPartitionID: uuid.NewString(),
						DeviceInstance:       cutil.GetPtr(0),
						AttachmentType:       SpectrumXAttachmentTypePhysical,
					},
				},
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
		{
			name: "test invalid Instance update request, userData exceeding max length",
			fields: fields{
				UserData: cutil.GetPtr(strings.Repeat("a", util.MaxUserDataBytes+1)),
			},
			wantErr:           true,
			wantUpdateRequest: cutil.GetPtr(true),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			itur := APIInstanceUpdateRequest{
				Name:                     tt.fields.Name,
				Description:              tt.fields.Description,
				Labels:                   tt.fields.Labels,
				TriggerReboot:            tt.fields.TriggerReboot,
				RebootWithCustomIpxe:     tt.fields.RebootWithCustomIpxe,
				ApplyUpdatesOnReboot:     tt.fields.ApplyUpdatesOnReboot,
				OperatingSystemID:        tt.fields.OperatingSystemID,
				IpxeScript:               tt.fields.IpxeScript,
				UserData:                 tt.fields.UserData,
				PhoneHomeEnabled:         tt.fields.PhoneHomeEnabled,
				AlwaysBootWithCustomIpxe: tt.fields.AlwaysBootWithCustomIpxe,
				SecondaryVpcIDs:          tt.fields.SecondaryVpcIDs,
				Interfaces:               tt.fields.Interfaces,
				InfiniBandInterfaces:     tt.fields.InfiniBandInterfaces,
				SpectrumXAttachments:     tt.fields.SpectrumXAttachments,
				NVLinkInterfaces:         tt.fields.NVLinkInterfaces,
				SSHKeyGroupIDs:           tt.fields.SSHKeyGroupIDs,
				NetworkSecurityGroupID:   tt.fields.NetworkSecurityGroupID,
			}
			err := itur.Validate()
			if tt.wantErr {
				assert.Error(t, err)
				if tt.wantUpdateRequest != nil {
					assert.Equal(t, *tt.wantUpdateRequest, itur.IsUpdateRequest())
				}
			} else {
				assert.NoError(t, err)
				// Verify at least one interface is physical
				if len(itur.Interfaces) > 0 {
					assert.True(t, itur.Interfaces[0].IsPhysical)
				}
			}
		})
	}
}

func TestAPIInstanceUpdateRequest_ValidateAndSetOperatingSystemData(t *testing.T) {

	cfg1 := config.NewConfig()

	cfg1.SetSitePhoneHomeUrl("http://localhost/local")

	// iPXE OS
	osPxe := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		Type:             cdbm.OperatingSystemTypeIPXE,
		IpxeScript:       cutil.GetPtr("925e37ee-29b5-11ef-ba3c-8752bb7488f6"),
		UserData:         cutil.GetPtr("{'hostname': 'd2def8d8-29b2-11ef-81e6-07a09293ef16'}"),
		PhoneHomeEnabled: true,
		IsActive:         true,
		AllowOverride:    true,
	}

	osPxeOverrideForbidden := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		Type:             cdbm.OperatingSystemTypeIPXE,
		IpxeScript:       cutil.GetPtr("925e37ee-29b5-11ef-ba3c-8752bb7488f6"),
		UserData:         cutil.GetPtr("{'hostname': 'd2def8d8-29b2-11ef-81e6-07a09293ef16'}"),
		PhoneHomeEnabled: true,
		AllowOverride:    false,
	}

	// Image OS
	osImage := &cdbm.OperatingSystem{
		ID:            uuid.New(),
		Name:          "ab",
		Type:          cdbm.OperatingSystemTypeImage,
		UserData:      cutil.GetPtr("{'hostname': 'd2def8d8-29b2-11ef-81e6-07a09293ef16'}"),
		IsActive:      true,
		AllowOverride: true,
	}

	// Instance without ipxe and user-data.
	// This should be used for simulating an instance
	// that was created with either no OS specified
	// or with an OS that didn't have these values
	// set.  This is because an instance created
	// with an OS specified would have inherited these
	// values from the OS upon creation.
	instanceNoVals := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     "",
		IpxeScript:               nil,
		AlwaysBootWithCustomIpxe: false,
		PhoneHomeEnabled:         false,
		UserData:                 nil,
	}

	// Instance with ipxe and user-data.
	instanceWithVals := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     "",
		IpxeScript:               cutil.GetPtr("#!ipxe 9ea0c946-29af-11ef-b798-df4626ad0292"),
		AlwaysBootWithCustomIpxe: true,
		PhoneHomeEnabled:         true,
		UserData:                 cutil.GetPtr("{'hostname': '815f5bd8-29b2-11ef-b3b1-ab4be50a4e4d'}"),
	}

	tests := []struct {
		name     string
		request  *APIInstanceUpdateRequest
		cfg      *config.Config
		os       *cdbm.OperatingSystem
		instance *cdbm.Instance
		wantErr  bool
	}{
		{
			name: "os nil, ipxe not set, expect error",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				IpxeScript:               nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: nil,
			},
			cfg:      cfg1,
			os:       nil,
			instance: instanceNoVals,
			wantErr:  true,
		},
		// If clearing the OS means _all_ overrides must be explicitly passed in, then
		// this should fail.
		{
			name: "os nil, ipxe in instance, expect failure",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				IpxeScript:               nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: nil,
				OperatingSystemID:        cutil.GetPtr(""),
			},
			cfg:      cfg1,
			os:       nil,
			instance: instanceWithVals,
			wantErr:  true,
		},
		{
			name: "os image, ipxe set, expect error",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        cutil.GetPtr(uuid.NewString()),
				IpxeScript:               cutil.GetPtr("anything"),
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: nil,
			},
			cfg:      cfg1,
			os:       osImage,
			instance: instanceWithVals,
			wantErr:  true,
		},
		{
			name: "os image, no ipxe set, always boot ipxe set in req, expect error",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        cutil.GetPtr(uuid.NewString()),
				IpxeScript:               nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: cutil.GetPtr(true),
			},
			cfg:      cfg1,
			os:       osImage,
			instance: instanceNoVals,
			wantErr:  true,
		},
		{
			name: "os image, no ipxe set, always boot ipxe set in instance, expect error",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        cutil.GetPtr(uuid.NewString()),
				IpxeScript:               nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: nil,
			},
			cfg:      cfg1,
			os:       osImage,
			instance: instanceWithVals,
			wantErr:  true,
		},
		{
			name: "os image, no ipxe set, phone home set in req, expect no error",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        cutil.GetPtr(uuid.NewString()),
				IpxeScript:               nil,
				PhoneHomeEnabled:         cutil.GetPtr(true),
				AlwaysBootWithCustomIpxe: nil,
			},
			cfg:      cfg1,
			os:       osImage,
			instance: instanceNoVals,
			wantErr:  false,
		},
		{
			name: "os image, no ipxe set, phone home set in instance, expect error",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        cutil.GetPtr(uuid.NewString()),
				IpxeScript:               nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: nil,
			},
			cfg:      cfg1,
			os:       osImage,
			instance: instanceWithVals,
			wantErr:  true,
		},
		{
			name: "OS ID nonnil, os nonnil ipxe, instance values nil, req values nil, expect success",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        cutil.GetPtr(uuid.NewString()),
				IpxeScript:               nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: nil,
			},
			cfg:      cfg1,
			os:       osPxe,
			instance: instanceNoVals,
			wantErr:  false,
		},
		{
			name: "OS ID nil, os nonnil ipxe, instance values nonnil, req values nil, expect success",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        nil,
				IpxeScript:               nil,
				PhoneHomeEnabled:         nil,
				AlwaysBootWithCustomIpxe: nil,
			},
			cfg:      cfg1,
			os:       osPxe,
			instance: instanceWithVals,
			wantErr:  false,
		},
		{
			name: "OS ID nil, os nonnil ipxe, instance values nil, req values nonnil, expect success",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 instanceWithVals.UserData,
				OperatingSystemID:        nil,
				IpxeScript:               instanceWithVals.IpxeScript,
				PhoneHomeEnabled:         &instanceWithVals.PhoneHomeEnabled,
				AlwaysBootWithCustomIpxe: &instanceWithVals.AlwaysBootWithCustomIpxe,
			},
			cfg:      cfg1,
			os:       osPxe,
			instance: instanceNoVals,
			wantErr:  false,
		},
		{
			name: "OS ID nil, os nonnil ipxe, original os has user-data, override not allowed, instance values nil, req values nonnil, expect failure",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 instanceWithVals.UserData,
				OperatingSystemID:        nil,
				IpxeScript:               instanceWithVals.IpxeScript,
				PhoneHomeEnabled:         &instanceWithVals.PhoneHomeEnabled,
				AlwaysBootWithCustomIpxe: &instanceWithVals.AlwaysBootWithCustomIpxe,
			},
			cfg:      cfg1,
			os:       osPxeOverrideForbidden,
			instance: instanceNoVals,
			wantErr:  true,
		},
		{
			name: "OS ID nil, os nonnil ipxe, original os has user-data, override not allowed, instance values nil, req user-data nil, expect success",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 nil,
				OperatingSystemID:        nil,
				IpxeScript:               instanceWithVals.IpxeScript,
				PhoneHomeEnabled:         &instanceWithVals.PhoneHomeEnabled,
				AlwaysBootWithCustomIpxe: &instanceWithVals.AlwaysBootWithCustomIpxe,
			},
			cfg:      cfg1,
			os:       osPxeOverrideForbidden,
			instance: instanceNoVals,
			wantErr:  false,
		},
		{
			name: "OS ID nil, os nonnil ipxe, instance values nil, req values nil, expect failure",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 instanceWithVals.UserData,
				OperatingSystemID:        nil,
				IpxeScript:               nil,
				PhoneHomeEnabled:         &instanceWithVals.PhoneHomeEnabled,
				AlwaysBootWithCustomIpxe: &instanceWithVals.AlwaysBootWithCustomIpxe,
			},
			cfg:      cfg1,
			os:       osPxe,
			instance: instanceNoVals,
			wantErr:  true,
		},
		{
			name: "OS ID nil, os nonnil ipxe, set valid but technically empty user-data, phonehome enabled, expect failure",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 cutil.GetPtr("#cloud-config"),
				OperatingSystemID:        nil,
				IpxeScript:               cutil.GetPtr("anything"),
				PhoneHomeEnabled:         cutil.GetPtr(true),
				AlwaysBootWithCustomIpxe: cutil.GetPtr(false),
			},
			cfg:      cfg1,
			os:       osPxe,
			instance: instanceNoVals,
			wantErr:  true,
		},
		{
			name: "OS ID nil, os nonnil ipxe, set valid but technically empty user-data, phonehome disabled, expect success",
			request: &APIInstanceUpdateRequest{
				Name:                     cutil.GetPtr("test-name"),
				Description:              cutil.GetPtr("Test description"),
				UserData:                 cutil.GetPtr("#cloud-config"),
				OperatingSystemID:        nil,
				IpxeScript:               cutil.GetPtr("anything"),
				PhoneHomeEnabled:         cutil.GetPtr(false),
				AlwaysBootWithCustomIpxe: cutil.GetPtr(false),
			},
			cfg:      cfg1,
			os:       osPxe,
			instance: instanceNoVals,
			wantErr:  false,
		},
		{
			name: "os nil, instance user-data over max length inherited, expect failure",
			request: &APIInstanceUpdateRequest{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("Test description"),
			},
			cfg: cfg1,
			os:  nil,
			instance: &cdbm.Instance{
				ID:               uuid.New(),
				IpxeScript:       cutil.GetPtr("#!ipxe"),
				PhoneHomeEnabled: false,
				UserData:         cutil.GetPtr("a: " + strings.Repeat("b", util.MaxUserDataBytes)),
			},
			wantErr: true,
		},
		{
			// Same request as the case above, but the Instance has a base OS,
			// which takes the merge branch that leaves the request's user-data
			// nil. The stored blob still reaches the Site either way.
			name: "os nonnil, no OS change, instance user-data over max length inherited, expect failure",
			request: &APIInstanceUpdateRequest{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("Test description"),
			},
			cfg: cfg1,
			os:  osPxe,
			instance: &cdbm.Instance{
				ID:               uuid.New(),
				IpxeScript:       cutil.GetPtr("#!ipxe"),
				PhoneHomeEnabled: false,
				UserData:         cutil.GetPtr("a: " + strings.Repeat("b", util.MaxUserDataBytes)),
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			iur := tt.request

			err := iur.ValidateAndSetOperatingSystemData(tt.cfg, tt.instance, tt.os)
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIInstanceUpdateRequest.ValidateAndSetOperatingSystemData() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
		})
	}
}

func TestAPIInstanceUpdateRequest_ValidateAndSetOperatingSystemData_Phonehome(t *testing.T) {
	cfg1 := config.NewConfig()

	cfg1.SetSitePhoneHomeUrl("http://localhost/local")

	// iPXE OS with user-data
	os1 := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr("#cloud-config\n{'hostname': 'd2def8d8-29b2-11ef-81e6-07a09293ef16'}"),
		PhoneHomeEnabled: true,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		AllowOverride:    true,
	}

	// Instance with ipxe and user-data.
	instance1 := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     "",
		IpxeScript:               cutil.GetPtr("#!ipxe 9ea0c946-29af-11ef-b798-df4626ad0292"),
		AlwaysBootWithCustomIpxe: true,
		PhoneHomeEnabled:         true,
		UserData:                 cutil.GetPtr("#cloud-config\n{'hostname': '815f5bd8-29b2-11ef-b3b1-ab4be50a4e4d'}"),
	}

	// Instance with ipxe and user-data.
	instance2 := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     "",
		IpxeScript:               cutil.GetPtr("#!ipxe 9ea0c946-29af-11ef-b798-df4626ad0292"),
		AlwaysBootWithCustomIpxe: true,
		PhoneHomeEnabled:         true,
	}

	// Instance with ipxe and user-data.
	instance3 := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     "",
		IpxeScript:               cutil.GetPtr("#!ipxe 9ea0c946-29af-11ef-b798-df4626ad0292"),
		AlwaysBootWithCustomIpxe: true,
		PhoneHomeEnabled:         false,
	}

	// Phone-home is enabled and the stored user-data has a phone-home URL,
	// but it is not the configured one: what NICo wrote back when the
	// configured URL was different.
	instanceStaleURL := &cdbm.Instance{
		ID:               uuid.New(),
		Name:             "",
		IpxeScript:       cutil.GetPtr("#!ipxe 9ea0c946-29af-11ef-b798-df4626ad0292"),
		PhoneHomeEnabled: true,
		UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: http://169.254.169.254:7777/latest/meta-data/phone_home
`),
	}

	// Phone-home was never enabled, so the stored phone-home block is the
	// caller's, not NICo's.
	instanceForeignPhoneHome := &cdbm.Instance{
		ID:               uuid.New(),
		Name:             "",
		IpxeScript:       cutil.GetPtr("#!ipxe 9ea0c946-29af-11ef-b798-df4626ad0292"),
		PhoneHomeEnabled: false,
		UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: https://collector.example.com/hook
`),
	}

	// Same drift, but stored on the OS the update request switches to.
	osStaleURL := &cdbm.OperatingSystem{
		ID:         uuid.New(),
		Name:       "ab",
		IpxeScript: cutil.GetPtr("original ipxe"),
		UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: http://169.254.169.254:7777/latest/meta-data/phone_home
`),
		PhoneHomeEnabled: true,
		IsActive:         true,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		AllowOverride:    true,
	}

	tests := []struct {
		name                     string
		request                  *APIInstanceUpdateRequest
		cfg                      *config.Config
		os                       *cdbm.OperatingSystem
		userDataExactMatch       *string
		userDataSearches         []string
		userDataNegativeSearches []string
		instance                 *cdbm.Instance
		wantErr                  bool
	}{
		{
			name: "test valid Instance PhoneHome enabled update request when userData is nil when OS is nil, expect success",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				UserData:         nil,
				IpxeScript:       cutil.GetPtr("#!ipxe d5b692a8-29af-11ef-90ef-d392e60d87b2"),
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			cfg:      cfg1,
			instance: instance1,
			wantErr:  false,
			userDataSearches: []string{
				cfg1.GetSitePhoneHomeUrl(),
				// If the instance had user-data, adn no user-data override is being sent
				// and the OS is not being cleared, we should find the instance's
				// user-data.
				"815f5bd8-29b2-11ef-b3b1-ab4be50a4e4d",
			},
		},
		{
			name: "test invalid Instance PhoneHome enabled update request when userData is invalid and OS is nil, expect failure",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				UserData:         cutil.GetPtr("test-user-data"),
				PhoneHomeEnabled: cutil.GetPtr(true),
				IpxeScript:       cutil.GetPtr("#!ipxe e9d97138-29af-11ef-8b2c-57634a01308c"),
			},
			wantErr:  true,
			instance: instance1,
			cfg:      cfg1,
		},
		{
			name: "test valid Instance update request when phone home flag is disabled, userData is non-YAML and OS is nil",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				UserData:         cutil.GetPtr("test-user-data"),
				PhoneHomeEnabled: cutil.GetPtr(false),
				IpxeScript:       cutil.GetPtr("#!ipxe e9d97138-29af-11ef-8b2c-57634a01308c"),
			},
			wantErr:  false,
			instance: instance1,
			cfg:      cfg1,
		},
		{
			name: "test valid Instance update request when phone home flag is unspecified, userData is non-YAML and OS is nil",
			request: &APIInstanceUpdateRequest{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("Test description"),
				UserData:    cutil.GetPtr("test-user-data"),
				IpxeScript:  cutil.GetPtr("#!ipxe e9d97138-29af-11ef-8b2c-57634a01308c"),
			},
			wantErr:  false,
			instance: instance3,
			cfg:      cfg1,
		},
		{
			name: "test valid Instance PhoneHome enabled update request when userData is valid when OS is nil",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				IpxeScript:       cutil.GetPtr("#!ipxe ecf9ec1c-29af-11ef-a963-8f9524fd4f22"),
				UserData:         cutil.GetPtr(util.TestCommonCloudInit),
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			wantErr:  false,
			instance: instance1,
			cfg:      cfg1,
			userDataSearches: []string{
				"#cloud-config",
				cfg1.GetSitePhoneHomeUrl(),
			},
			userDataNegativeSearches: []string{
				"815f5bd8-29b2-11ef-b3b1-ab4be50a4e4d",
			},
		},
		{
			name: "test valid Instance PhoneHome enabled update request when OS in present with phonehome, userData doesn't contain phone home url",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				UserData:         nil,
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instance1,
			os:       os1,
			userDataNegativeSearches: []string{
				cfg1.GetSitePhoneHomeUrl(),
			},
		},
		{
			name: "PhoneHome enabled in request, no base os change, os nonnil, expect instance user-data, expect success",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instance1,
			os:       os1,
			userDataSearches: []string{
				"#cloud-config",
				cfg1.GetSitePhoneHomeUrl(),
				"815f5bd8-29b2-11ef-b3b1-ab4be50a4e4d",
			},
		},
		{
			name: "PhoneHome enabled in request, base os change, os nonnil, empty user-data, expect instance user-data, expect success",
			request: &APIInstanceUpdateRequest{
				Name:              cutil.GetPtr("test-name"),
				Description:       cutil.GetPtr("Test description"),
				PhoneHomeEnabled:  cutil.GetPtr(true),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr(""),
			},
			wantErr:            false,
			cfg:                cfg1,
			instance:           instance1,
			os:                 os1,
			userDataExactMatch: cutil.GetPtr(fmt.Sprintf(SitePhoneHomeCloudInit, cfg1.GetSitePhoneHomeUrl())),
		},
		{
			name: "PhoneHome enabled in instance and request updates only base OS",
			request: &APIInstanceUpdateRequest{
				Name:              cutil.GetPtr("test-name"),
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instance1,
			os:       os1,
			userDataSearches: []string{
				cfg1.GetSitePhoneHomeUrl(),
				"d2def8d8-29b2-11ef-81e6-07a09293ef16",
			},
		},
		{
			name: "PhoneHome enabled in instance and request updates only user-data",
			request: &APIInstanceUpdateRequest{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("Test description"),
				UserData:    cutil.GetPtr("{'hostname': '563d2b4c-29b2-11ef-ad4f-df06abe3358c'}"),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instance1,
			os:       os1,
			userDataSearches: []string{
				cfg1.GetSitePhoneHomeUrl(),
				"563d2b4c-29b2-11ef-ad4f-df06abe3358c",
			},
		},

		{
			name: "PhoneHome enabled in instance with no user-data and request updates only user-data",
			request: &APIInstanceUpdateRequest{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("Test description"),
				UserData:    cutil.GetPtr("{'hostname': '563d2b4c-29b2-11ef-ad4f-df06abe3358c'}"),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instance2,
			os:       os1,
			userDataSearches: []string{
				cfg1.GetSitePhoneHomeUrl(),
				"563d2b4c-29b2-11ef-ad4f-df06abe3358c",
			},
			userDataNegativeSearches: []string{
				// It should not find the value of the OS-level user-data.
				// This could be a case where the OS had user-data but it was intentionally emptied for the instance.
				"d2def8d8-29b2-11ef-81e6-07a09293ef16",
			},
		},
		{
			name: "PhoneHome disabled in request, no user-data override, instance blob has stale NICo phone-home URL",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instanceStaleURL,
			// NICo authored the stored block, so it is removed by key without
			// matching the URL: neither the key nor the stale URL survives,
			// while the rest of the document is preserved.
			userDataSearches: []string{"package_update"},
			userDataNegativeSearches: []string{
				"phone_home",
				"169.254.169.254",
			},
		},
		{
			name: "PhoneHome disabled in request with caller-supplied user-data, instance blob has stale NICo phone-home URL",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				PhoneHomeEnabled: cutil.GetPtr(false),
				UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: https://collector.example.com/hook
`),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instanceStaleURL,
			// The stored blob is NICo's, but the document being edited is the
			// caller's, so removal stays URL-matched and their hook survives.
			userDataSearches: []string{"collector.example.com", "package_update"},
			userDataNegativeSearches: []string{
				"169.254.169.254",
			},
		},
		{
			name: "PhoneHome disabled in request, no user-data override, instance blob carries a caller phone-home block",
			request: &APIInstanceUpdateRequest{
				Name:             cutil.GetPtr("test-name"),
				Description:      cutil.GetPtr("Test description"),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instanceForeignPhoneHome,
			// Phone-home was never enabled on the instance, so the stored
			// block is the caller's: removal stays URL-matched and their
			// hook survives.
			userDataSearches: []string{"collector.example.com", "package_update"},
		},
		{
			name: "PhoneHome disabled in request while switching base OS whose blob has stale NICo phone-home URL",
			request: &APIInstanceUpdateRequest{
				Name:              cutil.GetPtr("test-name"),
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:  false,
			cfg:      cfg1,
			instance: instance3,
			os:       osStaleURL,
			// The new base OS's blob is what gets stored, and NICo authored
			// its block, so it is removed by key without matching the URL.
			userDataSearches: []string{"package_update"},
			userDataNegativeSearches: []string{
				"phone_home",
				"169.254.169.254",
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			iur := tt.request

			err := iur.ValidateAndSetOperatingSystemData(tt.cfg, tt.instance, tt.os)
			if err != nil {
				if !tt.wantErr {
					assert.NoError(t, err)
				}
				return
			}

			// If phone-home is enabled, user-data can't be nil
			if iur.PhoneHomeEnabled != nil && *iur.PhoneHomeEnabled {
				assert.NotNil(t, iur.UserData)
			}

			// Even if phone-home isn't enabled, we might expect some pre-existing
			// user-data
			if tt.userDataExactMatch != nil {
				assert.Equal(t, *tt.userDataExactMatch, *iur.UserData)
			}

			for _, search := range tt.userDataSearches {
				assert.Contains(t, *iur.UserData, search)
			}

			for _, search := range tt.userDataNegativeSearches {
				assert.NotContains(t, *iur.UserData, search)
			}
		})
	}
}

func TestAPIInstanceDeleteRequest_Validate(t *testing.T) {
	type fields struct {
		MachineHealthIssue *APIMachineHealthIssue
		IsRepairTenant     *bool
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test valid Instance delete request",
			fields: fields{
				MachineHealthIssue: &APIMachineHealthIssue{
					Category: "Hardware",
					Summary:  cutil.GetPtr("Test summary"),
					Details:  cutil.GetPtr("Test details"),
				},
				IsRepairTenant: cutil.GetPtr(true),
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance delete request - invalid machine health issue category",
			fields: fields{
				MachineHealthIssue: &APIMachineHealthIssue{
					Category: "Invalid",
				},
				IsRepairTenant: cutil.GetPtr(true),
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance delete request - required machine health issue summary",
			fields: fields{
				MachineHealthIssue: &APIMachineHealthIssue{
					Category: "Hardware",
				},
				IsRepairTenant: cutil.GetPtr(true),
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance delete request - invalid category",
			fields: fields{
				MachineHealthIssue: &APIMachineHealthIssue{
					Category: "Storage",
					Summary:  cutil.GetPtr("Test summary"),
					Details:  cutil.GetPtr(""),
				},
				IsRepairTenant: cutil.GetPtr(true),
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			idr := APIInstanceDeleteRequest{
				MachineHealthIssue: tt.fields.MachineHealthIssue,
			}

			err := idr.Validate()
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIInstanceDeleteRequest.Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
		})
	}
}

// TestAPIInstanceCreateRequest_Validate_Auto exercises the `auto` /
// `interfaces` exclusivity rules introduced for zero-DPU instances.
func TestAPIInstanceCreateRequest_Validate_Auto(t *testing.T) {
	tests := []struct {
		name             string
		req              APIInstanceCreateRequest
		wantErr          bool
		wantErrorMessage string
	}{
		{
			name: "auto=true with empty interfaces succeeds",
			req: APIInstanceCreateRequest{
				Name:              "auto-instance",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    cutil.GetPtr(uuid.NewString()),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				AutoNetwork:       true,
				Interfaces:        nil,
			},
			wantErr: false,
		},
		{
			name: "auto=true with interfaces is rejected",
			req: APIInstanceCreateRequest{
				Name:              "auto-instance",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    cutil.GetPtr(uuid.NewString()),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				AutoNetwork:       true,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{SubnetID: cutil.GetPtr(uuid.NewString())},
				},
			},
			wantErr:          true,
			wantErrorMessage: "`interfaces` must be empty when `autoNetwork` is true",
		},
		{
			name: "auto=false with empty interfaces is rejected",
			req: APIInstanceCreateRequest{
				Name:              "manual-instance",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    cutil.GetPtr(uuid.NewString()),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				AutoNetwork:       false,
				Interfaces:        nil,
			},
			wantErr:          true,
			wantErrorMessage: "at least one Interface must be specified",
		},
		{
			name: "auto=true with secondaryVpcIds is rejected",
			req: APIInstanceCreateRequest{
				Name:              "auto-instance",
				TenantID:          uuid.NewString(),
				InstanceTypeID:    cutil.GetPtr(uuid.NewString()),
				VpcID:             uuid.NewString(),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				AutoNetwork:       true,
				SecondaryVpcIDs:   []string{uuid.NewString()},
			},
			wantErr:          true,
			wantErrorMessage: "`secondaryVpcIds` is not supported when `autoNetwork` is true",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
			if tt.wantErrorMessage != "" && err != nil {
				assert.Contains(t, err.Error(), tt.wantErrorMessage)
			}
		})
	}
}

// TestAPIBatchInstanceCreateRequest_Validate_Auto mirrors the create-side
// exclusivity rules for the batch endpoint.
func TestAPIBatchInstanceCreateRequest_Validate_Auto(t *testing.T) {
	tests := []struct {
		name             string
		req              APIBatchInstanceCreateRequest
		wantErr          bool
		wantErrorMessage string
	}{
		{
			name: "auto=true with empty interfaces succeeds",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:     "auto-batch",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test ipxe"),
				AutoNetwork:    true,
			},
			wantErr: false,
		},
		{
			name: "auto=true with interfaces is rejected",
			req: APIBatchInstanceCreateRequest{
				NamePrefix:     "auto-batch",
				Count:          2,
				TenantID:       uuid.NewString(),
				InstanceTypeID: uuid.NewString(),
				VpcID:          uuid.NewString(),
				IpxeScript:     cutil.GetPtr("test ipxe"),
				AutoNetwork:    true,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{SubnetID: cutil.GetPtr(uuid.NewString())},
				},
			},
			wantErr:          true,
			wantErrorMessage: "`interfaces` must be empty when `autoNetwork` is true",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
			if tt.wantErrorMessage != "" && err != nil {
				assert.Contains(t, err.Error(), tt.wantErrorMessage)
			}
		})
	}
}

// TestAPIInstanceUpdateRequest_Validate_Auto covers the auto/interfaces
// exclusivity rule when toggling auto via the update endpoint.
func TestAPIInstanceUpdateRequest_Validate_Auto(t *testing.T) {
	autoTrue := true
	autoFalse := false
	tests := []struct {
		name             string
		req              APIInstanceUpdateRequest
		wantErr          bool
		wantErrorMessage string
	}{
		{
			name:    "auto unset leaves validation untouched",
			req:     APIInstanceUpdateRequest{AutoNetwork: nil},
			wantErr: false,
		},
		{
			name:    "auto=true with no interfaces succeeds",
			req:     APIInstanceUpdateRequest{AutoNetwork: &autoTrue},
			wantErr: false,
		},
		{
			name: "auto=true with interfaces is rejected",
			req: APIInstanceUpdateRequest{
				AutoNetwork: &autoTrue,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{SubnetID: cutil.GetPtr(uuid.NewString())},
				},
			},
			wantErr:          true,
			wantErrorMessage: "`interfaces` must be empty when `autoNetwork` is true",
		},
		{
			name: "auto=false with interfaces succeeds",
			req: APIInstanceUpdateRequest{
				AutoNetwork: &autoFalse,
				Interfaces: []APIInterfaceCreateOrUpdateRequest{
					{SubnetID: cutil.GetPtr(uuid.NewString())},
				},
			},
			wantErr: false,
		},
		{
			name: "auto=true with secondaryVpcIds is rejected",
			req: APIInstanceUpdateRequest{
				AutoNetwork:     &autoTrue,
				SecondaryVpcIDs: []string{uuid.NewString()},
			},
			wantErr:          true,
			wantErrorMessage: "`secondaryVpcIds` is not supported when `autoNetwork` is true",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("Validate() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}
			if tt.wantErrorMessage != "" && err != nil {
				assert.Contains(t, err.Error(), tt.wantErrorMessage)
			}
		})
	}
}

func TestAPIInstanceDeleteRequest_ToProto(t *testing.T) {
	id := uuid.New()
	ctrlID := uuid.New()
	userID := uuid.New()
	tenantID := uuid.New()
	org := "test-tenant-org"
	orgDisplayName := "Test Tenant Org"
	dbUser := &cdbm.User{ID: userID}
	instance := &cdbm.Instance{
		ID:                   id,
		ControllerInstanceID: &ctrlID,
		Tenant: &cdbm.Tenant{
			ID:             tenantID,
			Org:            org,
			OrgDisplayName: &orgDisplayName,
		},
	}

	assertDeleteAttribution := func(t *testing.T, got *corev1.InstanceReleaseRequest) {
		t.Helper()
		require.NotNil(t, got.DeleteAttribution)
		require.NotNil(t, got.DeleteAttribution.InitiatedBy)
		assert.Equal(t, org, got.DeleteAttribution.InitiatedBy.Org)
		assert.Equal(t, orgDisplayName, got.DeleteAttribution.InitiatedBy.OrgDisplayName)
		assert.Equal(t, userID.String(), got.DeleteAttribution.InitiatedBy.UserId)
		assert.Equal(t, tenantID.String(), got.DeleteAttribution.InitiatedBy.TenantId)
	}

	t.Run("empty request sources only the canonical ID", func(t *testing.T) {
		req := APIInstanceDeleteRequest{}
		got := req.ToProto(instance, dbUser)
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, ctrlID.String(), got.Id.Value)
		assert.Nil(t, got.Issue)
		assert.Nil(t, got.IsRepairTenant)
		assertDeleteAttribution(t, got)
	})

	t.Run("overlays MachineHealthIssue with summary and details", func(t *testing.T) {
		req := APIInstanceDeleteRequest{
			MachineHealthIssue: &APIMachineHealthIssue{
				Category: MachineIssueCategoryHardware,
				Summary:  cutil.GetPtr("burnt out NIC"),
				Details:  cutil.GetPtr("port 0 returned link-down for 30 minutes"),
			},
		}
		got := req.ToProto(instance, dbUser)
		require.NotNil(t, got)
		require.NotNil(t, got.Issue)
		assert.Equal(t, corev1.IssueCategory_HARDWARE, got.Issue.Category)
		assert.Equal(t, "burnt out NIC", got.Issue.Summary)
		assert.Equal(t, "port 0 returned link-down for 30 minutes", got.Issue.Details)
		assertDeleteAttribution(t, got)
	})

	t.Run("MachineHealthIssue without optional pointers leaves Summary and Details empty", func(t *testing.T) {
		req := APIInstanceDeleteRequest{
			MachineHealthIssue: &APIMachineHealthIssue{
				Category: MachineIssueCategoryOther,
			},
		}
		got := req.ToProto(instance, dbUser)
		require.NotNil(t, got.Issue)
		assert.Equal(t, corev1.IssueCategory_OTHER, got.Issue.Category)
		assert.Equal(t, "", got.Issue.Summary)
		assert.Equal(t, "", got.Issue.Details)
		assertDeleteAttribution(t, got)
	})

	t.Run("overlays IsRepairTenant when set", func(t *testing.T) {
		req := APIInstanceDeleteRequest{IsRepairTenant: cutil.GetPtr(true)}
		got := req.ToProto(instance, dbUser)
		require.NotNil(t, got.IsRepairTenant)
		assert.True(t, *got.IsRepairTenant)
		assertDeleteAttribution(t, got)
	})

	t.Run("uses Instance ID when ControllerInstanceID is nil", func(t *testing.T) {
		bare := &cdbm.Instance{
			ID: id,
			Tenant: &cdbm.Tenant{
				ID:  tenantID,
				Org: org,
			},
		}
		req := APIInstanceDeleteRequest{}
		got := req.ToProto(bare, dbUser)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		require.NotNil(t, got.DeleteAttribution)
		require.NotNil(t, got.DeleteAttribution.InitiatedBy)
		assert.Equal(t, org, got.DeleteAttribution.InitiatedBy.Org)
		assert.Equal(t, "", got.DeleteAttribution.InitiatedBy.OrgDisplayName)
		assert.Equal(t, userID.String(), got.DeleteAttribution.InitiatedBy.UserId)
		assert.Equal(t, tenantID.String(), got.DeleteAttribution.InitiatedBy.TenantId)
	})
}

func TestValidateInfiniBandRequestForMachineCapability(t *testing.T) {
	deviceType := cdbm.MachineCapabilityDeviceType("")
	machineIbCaps := []cdbm.MachineCapability{
		{
			Type:            cdbm.MachineCapabilityTypeInfiniBand,
			Name:            "MT28908 Family [ConnectX-6]",
			Vendor:          cutil.GetPtr("Mellanox Technologies"),
			Count:           cutil.GetPtr(3),
			DeviceType:      &deviceType,
			InactiveDevices: []int{1, 3},
		},
	}

	t.Run("satisfied when requested device instance is active on machine", func(t *testing.T) {
		req := APIInstanceCreateRequest{
			InfiniBandInterfaces: []APIInfiniBandInterfaceCreateOrUpdateRequest{
				{Device: "MT28908 Family [ConnectX-6]", DeviceInstance: 0, IsPhysical: true},
			},
		}
		match := req.ValidateInfiniBandRequestForMachineCapability(machineIbCaps)
		assert.True(t, match.Satisfied)
		assert.True(t, match.CountSatisfiable)
		assert.Equal(t, []int{0, 2}, match.SuggestedByDevice["MT28908 Family [ConnectX-6]"])
	})

	t.Run("not satisfied but count satisfiable when requested device instance is inactive on machine", func(t *testing.T) {
		req := APIInstanceCreateRequest{
			InfiniBandInterfaces: []APIInfiniBandInterfaceCreateOrUpdateRequest{
				{Device: "MT28908 Family [ConnectX-6]", DeviceInstance: 1, IsPhysical: true},
			},
		}
		match := req.ValidateInfiniBandRequestForMachineCapability(machineIbCaps)
		assert.False(t, match.Satisfied)
		assert.True(t, match.CountSatisfiable)
		assert.Equal(t, []int{0}, match.UnsatisfiedRequestIndices)
	})
}

func TestValidateSpectrumXAttachments(t *testing.T) {
	device := "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC"
	attachment := func(deviceInstance int, attachmentType SpectrumXAttachmentType, virtualFunctionID *int) APISpectrumXAttachmentCreateOrUpdateRequest {
		return APISpectrumXAttachmentCreateOrUpdateRequest{
			SpectrumXPartitionID: uuid.NewString(),
			Device:               device,
			DeviceInstance:       cutil.GetPtr(deviceInstance),
			AttachmentType:       attachmentType,
			VirtualFunctionID:    virtualFunctionID,
		}
	}
	overCap := make([]APISpectrumXAttachmentCreateOrUpdateRequest, 0, MaxSpectrumXAttachmentCount+1)
	for i := range MaxSpectrumXAttachmentCount + 1 {
		overCap = append(overCap, attachment(i, SpectrumXAttachmentTypePhysical, nil))
	}

	tests := []struct {
		name        string
		attachments []APISpectrumXAttachmentCreateOrUpdateRequest
		wantErr     bool
	}{
		{
			name:        "empty list is valid so an update can clear attachments",
			attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{},
		},
		{
			name: "distinct device instances are valid",
			attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
				attachment(0, SpectrumXAttachmentTypePhysical, nil),
				attachment(1, SpectrumXAttachmentTypePhysical, nil),
			},
		},
		{
			name: "duplicate device instance is rejected",
			attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
				attachment(0, SpectrumXAttachmentTypePhysical, nil),
				attachment(0, SpectrumXAttachmentTypeOVN, nil),
			},
			wantErr: true,
		},
		{
			name: "same device at distinct device instances is valid",
			attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
				attachment(0, SpectrumXAttachmentTypePhysical, nil),
				attachment(1, SpectrumXAttachmentTypeOVN, nil),
			},
		},
		{
			name:        "attachment count above the cap is rejected",
			attachments: overCap,
			wantErr:     true,
		},
		{
			name: "per-attachment error propagates",
			attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{
				attachment(0, "Bogus", nil),
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := ValidateSpectrumXAttachments(tt.attachments)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
