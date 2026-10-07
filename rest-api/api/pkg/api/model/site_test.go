// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"reflect"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewAPISite(t *testing.T) {
	type args struct {
		dbs   cdbm.Site
		dbsds []cdbm.StatusDetail
		ts    *cdbm.TenantSite
	}

	ip := cdbm.InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test-ip",
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Test Org"),
		CreatedBy:      uuid.New(),
	}

	dbs := cdbm.Site{
		ID:                            uuid.New(),
		Name:                          "test-site",
		Description:                   cutil.GetPtr("Test Site Description"),
		Org:                           "test-org",
		InfrastructureProviderID:      uuid.New(),
		InfrastructureProvider:        &ip,
		SiteControllerVersion:         cutil.GetPtr("1.0.0"),
		SiteAgentVersion:              cutil.GetPtr("1.0.0"),
		RegistrationToken:             cutil.GetPtr("test-registration-token"),
		RegistrationTokenExpiration:   cutil.GetPtr(time.Now()),
		SerialConsoleHostname:         cutil.GetPtr("nico.acme.com"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(30),
		SerialConsoleMaxSessionLength: cutil.GetPtr(60),
		Config:                        &cdbm.SiteConfig{NativeNetworking: true, VpcSlaac: true},
		Status:                        cdbm.SiteStatusRegistered,
		Created:                       time.Now(),
		Updated:                       time.Now(),
		Location: &cdbm.SiteLocation{
			City:    "San Jose",
			State:   "CA",
			Country: "USA",
		},
		Contact: &cdbm.SiteContact{
			Email: "johndoe@nvidia.com",
		},
	}

	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbs.ID.String(),
			Status:   cdbm.SiteStatusRegistered,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}

	ts := cdbm.TenantSite{
		ID:                  uuid.New(),
		TenantID:            uuid.New(),
		SiteID:              dbs.ID,
		EnableSerialConsole: true,
		CreatedBy:           uuid.New(),
		Created:             time.Now(),
		Updated:             time.Now(),
	}

	tests := []struct {
		name string
		args args
		want APISite
	}{
		{
			name: "get new APISite by Provider",
			args: args{
				dbs:   dbs,
				dbsds: dbsds,
			},
			want: APISite{
				ID:                       dbs.ID.String(),
				Name:                     dbs.Name,
				Description:              dbs.Description,
				Org:                      dbs.Org,
				InfrastructureProviderID: dbs.InfrastructureProviderID.String(),
				InfrastructureProvider: &APIInfrastructureProviderSummary{
					Org:            ip.Org,
					OrgDisplayName: ip.OrgDisplayName,
				},
				SiteControllerVersion:         dbs.SiteControllerVersion,
				SiteAgentVersion:              dbs.SiteAgentVersion,
				RegistrationToken:             dbs.RegistrationToken,
				RegistrationTokenExpiration:   dbs.RegistrationTokenExpiration,
				SerialConsoleHostname:         dbs.SerialConsoleHostname,
				IsSerialConsoleEnabled:        dbs.IsSerialConsoleEnabled,
				SerialConsoleIdleTimeout:      dbs.SerialConsoleIdleTimeout,
				SerialConsoleMaxSessionLength: dbs.SerialConsoleMaxSessionLength,
				Capabilities:                  siteConfigToAPISiteCapabilities(dbs.Config),
				IsOnline:                      true,
				Status:                        dbs.Status,
				StatusHistory: []APIStatusDetail{
					{
						Status:  dbsds[0].Status,
						Message: dbsds[0].Message,
						Created: dbsds[0].Created,
						Updated: dbsds[0].Updated,
					},
				},
				Created: dbs.Created,
				Updated: dbs.Updated,
				Location: &APISiteLocation{
					City:    "San Jose",
					State:   "CA",
					Country: "USA",
				},
			},
		},
		{
			name: "get new APISite by Tenant",
			args: args{
				dbs:   dbs,
				dbsds: dbsds,
				ts:    &ts,
			},
			want: APISite{
				ID:                       dbs.ID.String(),
				Name:                     dbs.Name,
				Description:              dbs.Description,
				Org:                      dbs.Org,
				InfrastructureProviderID: dbs.InfrastructureProviderID.String(),
				InfrastructureProvider: &APIInfrastructureProviderSummary{
					Org:            ip.Org,
					OrgDisplayName: ip.OrgDisplayName,
				},
				SiteControllerVersion:         dbs.SiteControllerVersion,
				SiteAgentVersion:              dbs.SiteAgentVersion,
				RegistrationToken:             dbs.RegistrationToken,
				RegistrationTokenExpiration:   dbs.RegistrationTokenExpiration,
				SerialConsoleHostname:         dbs.SerialConsoleHostname,
				IsSerialConsoleEnabled:        dbs.IsSerialConsoleEnabled,
				SerialConsoleIdleTimeout:      dbs.SerialConsoleIdleTimeout,
				SerialConsoleMaxSessionLength: dbs.SerialConsoleMaxSessionLength,
				IsSerialConsoleSSHKeysEnabled: cutil.GetPtr(ts.EnableSerialConsole),
				Capabilities:                  siteConfigToAPISiteCapabilities(&cdbm.SiteConfig{NativeNetworking: true, VpcSlaac: true}),
				IsOnline:                      true,
				Status:                        dbs.Status,
				StatusHistory: []APIStatusDetail{
					{
						Status:  dbsds[0].Status,
						Message: dbsds[0].Message,
						Created: dbsds[0].Created,
						Updated: dbsds[0].Updated,
					},
				},
				Created: dbs.Created,
				Updated: dbs.Updated,
				Contact: &APISiteContact{
					Email: "johndoe@nvidia.com",
				},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPISite(tt.args.dbs, tt.args.dbsds, tt.args.ts)
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, tt.want.Description, got.Description)
			assert.Equal(t, tt.want.Org, got.Org)
			assert.Equal(t, tt.want.InfrastructureProviderID, got.InfrastructureProviderID)
			assert.Equal(t, tt.want.InfrastructureProvider, got.InfrastructureProvider)
			assert.Equal(t, tt.want.SiteControllerVersion, got.SiteControllerVersion)
			assert.Equal(t, tt.want.SiteAgentVersion, got.SiteAgentVersion)
			if tt.args.ts == nil {
				assert.Equal(t, tt.want.RegistrationToken, got.RegistrationToken)
				assert.Equal(t, tt.want.RegistrationTokenExpiration, got.RegistrationTokenExpiration)
			}
			assert.Equal(t, *tt.want.SerialConsoleHostname, *got.SerialConsoleHostname)
			assert.Equal(t, tt.want.IsSerialConsoleEnabled, got.IsSerialConsoleEnabled)
			assert.Equal(t, *tt.want.SerialConsoleIdleTimeout, *got.SerialConsoleIdleTimeout)
			assert.Equal(t, *tt.want.SerialConsoleMaxSessionLength, *got.SerialConsoleMaxSessionLength)
			assert.Equal(t, *tt.want.Capabilities, *got.Capabilities)
			assert.True(t, got.Capabilities.VpcSlaac)
			assert.Equal(t, tt.want.IsOnline, got.IsOnline)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.Created, got.Created)
			assert.Equal(t, tt.want.Updated, got.Updated)
			assert.Equal(t, reflect.DeepEqual(tt.want.StatusHistory, got.StatusHistory), true)
			if tt.want.Location != nil {
				assert.NotNil(t, got.Location)
				assert.Equal(t, tt.want.Location.City, got.Location.City)
				assert.Equal(t, tt.want.Location.State, got.Location.State)
				assert.Equal(t, tt.want.Location.Country, got.Location.Country)
			}
			if tt.want.Contact != nil {
				assert.NotNil(t, got.Contact)
				assert.Equal(t, tt.want.Contact.Email, got.Contact.Email)
			}

			if tt.want.IsSerialConsoleSSHKeysEnabled != nil {
				assert.Equal(t, *tt.want.IsSerialConsoleSSHKeysEnabled, *got.IsSerialConsoleSSHKeysEnabled)
			}
		})
	}
}

func TestAPISiteCreateRequest_Validate(t *testing.T) {
	type fields struct {
		Name                  string
		Description           *string
		SerialConsoleHostname *string
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "validate create request success",
			fields: fields{
				Name:                  "test-site",
				Description:           cutil.GetPtr("Test Site Description"),
				SerialConsoleHostname: cutil.GetPtr("nico.acme.com"),
			},
			wantErr: false,
		},
		{
			name: "validate create request failure, missing name",
			fields: fields{
				Description:           cutil.GetPtr("Test Site Description"),
				SerialConsoleHostname: cutil.GetPtr("nico.acme.com"),
			},
			wantErr: true,
		},
		{
			name: "validate create request failure, invalid hostname",
			fields: fields{
				Name:                  "test-site",
				SerialConsoleHostname: cutil.GetPtr("$xyz"),
			},
			wantErr: true,
		},
		{
			name: "validate create request success when serial console params are set",
			fields: fields{
				Name:                  "test-site",
				Description:           cutil.GetPtr("Test Site Description"),
				SerialConsoleHostname: cutil.GetPtr("nico.acme.com"),
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ascr := APISiteCreateRequest{
				Name:                  tt.fields.Name,
				Description:           tt.fields.Description,
				SerialConsoleHostname: tt.fields.SerialConsoleHostname,
			}
			err := ascr.Validate()
			require.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPISiteUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		Name                          *string
		Description                   *string
		RenewRegistrationToken        *bool
		SerialConsoleHostname         *string
		IsSerialConsoleEnabled        *bool
		SerialConsoleIdleTimeout      *int
		SerialConsoleMaxSessionLength *int
		IsSerialConsoleSSHKeysEnabled *bool
		Capabilities                  *APISiteCapabilitiesUpdateRequest
	}
	tests := []struct {
		name       string
		fields     fields
		isProvider bool
		isTenant   bool
		wantErr    bool
	}{
		{
			name: "validate update request success when requested by Provider",
			fields: fields{
				Name:                   cutil.GetPtr("test-site"),
				Description:            cutil.GetPtr("Test Site Description"),
				SerialConsoleHostname:  cutil.GetPtr("nico.acme.com"),
				RenewRegistrationToken: cutil.GetPtr(true),
			},
			isProvider: true,
			wantErr:    false,
		},
		{
			name:    "validate update request success, no changes",
			fields:  fields{},
			wantErr: false,
		},
		{
			name: "validate update request success, serial console params specified",
			fields: fields{
				SerialConsoleHostname:         cutil.GetPtr("nico.acme.com"),
				IsSerialConsoleEnabled:        cutil.GetPtr(true),
				SerialConsoleIdleTimeout:      cutil.GetPtr(10),
				SerialConsoleMaxSessionLength: cutil.GetPtr(20),
			},
			isProvider: true,
			wantErr:    false,
		},
		{
			name: "validate update request failure, invalid serial console params specified",
			fields: fields{
				SerialConsoleIdleTimeout:      cutil.GetPtr(-1),
				SerialConsoleMaxSessionLength: cutil.GetPtr(0),
			},
			isProvider: true,
			wantErr:    true,
		},
		{
			name: "validate update request failure, Tenant serial console param specified by Provider",
			fields: fields{
				IsSerialConsoleSSHKeysEnabled: cutil.GetPtr(true),
			},
			isProvider: true,
			wantErr:    true,
		},
		{
			name: "validate update request success, serial console host name can be cleared",
			fields: fields{
				SerialConsoleHostname: cutil.GetPtr(""),
			},
			isProvider: true,
			wantErr:    false,
		},
		{
			name: "validate update request failure, Tenant configuring this value is no longer supported",
			fields: fields{
				IsSerialConsoleSSHKeysEnabled: cutil.GetPtr(true),
			},
			isTenant: true,
			wantErr:  true,
		},
		{
			name: "validate update request failure, Tenant changing Provider specific attributes not allowed",
			fields: fields{
				Name:                   cutil.GetPtr("test-site"),
				Description:            cutil.GetPtr("Test Site Description"),
				SerialConsoleHostname:  cutil.GetPtr("nico.acme.com"),
				IsSerialConsoleEnabled: cutil.GetPtr(true),
				RenewRegistrationToken: cutil.GetPtr(true),
			},
			isTenant: true,
			wantErr:  true,
		},
		{
			name: "validate update request failure, Tenant configuring capabilities not allowed",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					NativeNetworking: cutil.GetPtr(true),
				},
			},
			isTenant: true,
			wantErr:  true,
		},
		{
			name: "validate update request success, Provider configuring capabilities",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					NativeNetworking: cutil.GetPtr(false),
				},
			},
			isProvider: true,
			wantErr:    false,
		},
		{
			name: "validate update request success, Provider enabling DPS power management",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					DPSPowerManagement: cutil.GetPtr(true),
				},
			},
			isProvider: true,
			wantErr:    false,
		},
		{
			name: "validate update request success, Provider disabling DPS power management",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					DPSPowerManagement: cutil.GetPtr(false),
				},
			},
			isProvider: true,
			wantErr:    false,
		},
		{
			name: "validate update request failure, Provider disabling inventory-managed Flow",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					Flow: cutil.GetPtr(false),
				},
			},
			isProvider: true,
			wantErr:    true,
		},
		{
			name: "validate update request failure, Provider enabling inventory-managed Flow",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					Flow: cutil.GetPtr(true),
				},
			},
			isProvider: true,
			wantErr:    true,
		},
		{
			name: "validate update request failure, Provider disabling inventory capability",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					VpcSlaac: cutil.GetPtr(false),
				},
			},
			isProvider: true,
			wantErr:    true,
		},
		{
			name: "validate update request failure, Provider enabling inventory capability",
			fields: fields{
				Capabilities: &APISiteCapabilitiesUpdateRequest{
					VpcSlaac: cutil.GetPtr(true),
				},
			},
			isProvider: true,
			wantErr:    true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			asur := APISiteUpdateRequest{
				Name:                          tt.fields.Name,
				Description:                   tt.fields.Description,
				RenewRegistrationToken:        tt.fields.RenewRegistrationToken,
				SerialConsoleHostname:         tt.fields.SerialConsoleHostname,
				IsSerialConsoleEnabled:        tt.fields.IsSerialConsoleEnabled,
				SerialConsoleIdleTimeout:      tt.fields.SerialConsoleIdleTimeout,
				SerialConsoleMaxSessionLength: tt.fields.SerialConsoleMaxSessionLength,
				IsSerialConsoleSSHKeysEnabled: tt.fields.IsSerialConsoleSSHKeysEnabled,
				Capabilities:                  tt.fields.Capabilities,
			}
			err := asur.Validate(tt.isProvider, tt.isTenant)

			if tt.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestSiteConfigToAPISiteCapabilities(t *testing.T) {
	tests := []struct {
		name                   string
		cfg                    *cdbm.SiteConfig
		wantVpcSlaac           bool
		wantDPSPowerManagement bool
	}{
		{
			name: "missing Site config",
		},
		{
			name: "disabled DPS power management",
			cfg:  &cdbm.SiteConfig{},
		},
		{
			name:                   "stored capabilities",
			cfg:                    &cdbm.SiteConfig{VpcSlaac: true, DPSPowerManagement: true},
			wantVpcSlaac:           true,
			wantDPSPowerManagement: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := siteConfigToAPISiteCapabilities(tt.cfg)
			require.NotNil(t, got)
			assert.Equal(t, tt.wantVpcSlaac, got.VpcSlaac)
			assert.Equal(t, tt.wantDPSPowerManagement, got.DPSPowerManagement)
		})
	}
}

func TestAPISiteCapabilitiesUpdateRequest_ToSiteConfig(t *testing.T) {
	tests := []struct {
		name     string
		existing *cdbm.SiteConfig
		request  APISiteCapabilitiesUpdateRequest
		want     *cdbm.SiteConfig
	}{
		{
			name: "cannot clear inventory-managed Flow",
			existing: &cdbm.SiteConfig{
				NativeNetworking: true,
				Flow:             true,
			},
			request: APISiteCapabilitiesUpdateRequest{
				NativeNetworking: cutil.GetPtr(false),
				Flow:             cutil.GetPtr(false),
			},
			want: &cdbm.SiteConfig{Flow: true},
		},
		{
			name: "cannot clear inventory-managed VPC SLAAC",
			existing: &cdbm.SiteConfig{
				NativeNetworking: true,
				VpcSlaac:         true,
			},
			request: APISiteCapabilitiesUpdateRequest{
				NativeNetworking: cutil.GetPtr(false),
				VpcSlaac:         cutil.GetPtr(false),
			},
			want: &cdbm.SiteConfig{VpcSlaac: true},
		},
		{
			name: "cannot enable inventory-managed VPC SLAAC",
			existing: &cdbm.SiteConfig{
				NetworkSecurityGroup: true,
			},
			request: APISiteCapabilitiesUpdateRequest{
				NetworkSecurityGroup: cutil.GetPtr(false),
				VpcSlaac:             cutil.GetPtr(true),
			},
			want: &cdbm.SiteConfig{},
		},
		{
			name: "cannot initialize inventory-managed VPC SLAAC",
			request: APISiteCapabilitiesUpdateRequest{
				VpcSlaac: cutil.GetPtr(true),
			},
			want: &cdbm.SiteConfig{},
		},
		{
			name: "updates DPS power management",
			existing: &cdbm.SiteConfig{
				NativeNetworking: true,
			},
			request: APISiteCapabilitiesUpdateRequest{
				DPSPowerManagement: cutil.GetPtr(true),
			},
			want: &cdbm.SiteConfig{
				NativeNetworking:   true,
				DPSPowerManagement: true,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.request.ToSiteConfig(tt.existing)
			assert.Equal(t, tt.want, got)
		})
	}
}
