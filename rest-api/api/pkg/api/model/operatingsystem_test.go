// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdmu "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestAPIOperatingSystemCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIOperatingSystemCreateRequest
		expectErr bool
	}{
		{
			desc:      "error when Name is not provided",
			obj:       APIOperatingSystemCreateRequest{Description: cutil.GetPtr("ab"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: false},
			expectErr: true,
		},
		{
			desc:      "error when userData exceeds max length",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr(strings.Repeat("a", util.MaxUserDataBytes+1)), IsCloudInit: true, AllowOverride: false},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APIOperatingSystemCreateRequest{Name: "a", Description: cutil.GetPtr("ab"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: false},
			expectErr: true,
		},
		{
			desc:      "ok when description is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: false},
			expectErr: false,
		},
		{
			desc:      "error when InfrastructID is not nil",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: cutil.GetPtr(uuid.New().String()), TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: false},
			expectErr: true,
		},
		{
			desc:      "error when IpxeScript is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr(""), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: false},
			expectErr: true,
		},
		{
			desc:      "error when ImageURL is invalid",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("imagenet/iso"), UserData: cutil.GetPtr("ud"), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when ImageSHA is invalid",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), ImageSHA: cutil.GetPtr("tttt"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), UserData: cutil.GetPtr("ud"), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when only ImageURL is specified not other image attribute",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://image.net/iso")},
			expectErr: true,
		},
		{
			desc:      "error when only ImageURL is specified but not site ID",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://image.net/iso")},
			expectErr: true,
		},
		{
			desc:      "error when siteIDs are specified but no imageURL",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", SiteIDs: []string{uuid.NewString()}},
			expectErr: true,
		},
		{
			desc:      "error when ImageDisk is invalid",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://image.net/iso"), ImageDisk: cutil.GetPtr("tttt/"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when ImageURL not specified but ImageAuthType is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageAuthType: cutil.GetPtr(""), UserData: nil, AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when ImageAuthToken is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), ImageAuthToken: cutil.GetPtr(""), UserData: nil, AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when both RootFsID and RootFsLabel are populated",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), RootFsLabel: cutil.GetPtr("test-label"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when imageURL not specified but RootFsID is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), RootFsID: cutil.GetPtr(""), UserData: cutil.GetPtr("ud"), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when imageURL not specified but RootFsLabel is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), RootFsLabel: cutil.GetPtr(""), UserData: cutil.GetPtr("ud"), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when imageURL and imageAuthToken are specified but no imageAuthType",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", ImageURL: cutil.GetPtr("http://image.net/iso"), ImageAuthToken: cutil.GetPtr("rsa")},
			expectErr: true,
		},
		{
			desc:      "error when imageURL and imageAuthToken are specified but imageAuthType is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", ImageURL: cutil.GetPtr("http://image.net/iso"), ImageAuthToken: cutil.GetPtr("rsa"), ImageAuthType: cutil.GetPtr("")},
			expectErr: true,
		},
		{
			desc:      "error when imageURL and imageAuthType are specified but no imageAuthToken",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", ImageURL: cutil.GetPtr("http://image.net/iso"), ImageAuthType: cutil.GetPtr("Bearer")},
			expectErr: true,
		},
		{
			desc:      "error when imageURL and imageAuthType are specified but imageAuthToken is empty",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", ImageURL: cutil.GetPtr("http://image.net/iso"), ImageAuthType: cutil.GetPtr("Bearer"), ImageAuthToken: cutil.GetPtr("")},
			expectErr: true,
		},
		{
			desc:      "error when imageURL and imageAuthToken are specified but imageAuthType is invalid",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", ImageURL: cutil.GetPtr("http://image.net/iso"), ImageAuthType: cutil.GetPtr("VAPID"), ImageAuthToken: cutil.GetPtr("rsa")},
			expectErr: true,
		},
		{
			desc:      "error when IpxeScript and ImageURL both specified",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), ImageURL: cutil.GetPtr("http://iso.net/iso"), UserData: cutil.GetPtr("ud"), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when IpxeScript and SiteID both specified",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), SiteIDs: []string{uuid.NewString()}, UserData: cutil.GetPtr("ud"), AllowOverride: true},
			expectErr: true,
		},
		{
			desc:      "error when IpxeScript specified but enableBlockStorage is true",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: true, EnableBlockStorage: true},
			expectErr: true,
		},
		{
			desc:      "error when imageURL is specified but neither rootFsLabel nor rootFsID is",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", ImageURL: cutil.GetPtr("http://image.net/iso")},
			expectErr: true,
		},
		{
			desc:      "error when imageURL is specified but multiple sites specified",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), SiteIDs: []string{uuid.NewString(), uuid.NewString()}, ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), IsCloudInit: true, AllowOverride: false},
			expectErr: true,
		},
		{
			desc:      "error when enableBlockStorage is true",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: false, EnableBlockStorage: true},
			expectErr: true,
		},
		{
			desc:      "ok when all fields are speced",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr("ud"), IsCloudInit: true, AllowOverride: false, EnableBlockStorage: false},
			expectErr: false,
		},
		{
			desc:      "ok when only required valid ipxe fields present",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", TenantID: cutil.GetPtr(uuid.New().String()), IpxeScript: cutil.GetPtr("ipxe"), IsCloudInit: true, AllowOverride: false},
			expectErr: false,
		},
		{
			desc:      "ok when only required valid image fields present",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), SiteIDs: []string{uuid.NewString()}, ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), IsCloudInit: true, AllowOverride: false},
			expectErr: false,
		},
		{
			desc:      "ok when ImageDisk is a by-id path",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), SiteIDs: []string{uuid.NewString()}, ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), ImageDisk: cutil.GetPtr("/dev/disk/by-id/nvme-Dell_DC_NVMe_CD7_U.2_960GB_Z3W0A01DTXBH-extra-long"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e")},
			expectErr: false,
		},
		{
			desc:      "error when ImageDisk is a by-id partition alias",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), SiteIDs: []string{uuid.NewString()}, ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), ImageDisk: cutil.GetPtr("/dev/disk/by-id/nvme-Dell_DC_NVMe_CD7_U.2_960GB_Z3W0A01DTXBH-part1"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e")},
			expectErr: true,
		},
		{
			desc:      "ok when empty strings specified for optional image fields",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", TenantID: cutil.GetPtr(uuid.New().String()), ImageURL: cutil.GetPtr("http://iso.net/iso"), SiteIDs: []string{uuid.NewString()}, ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), IsCloudInit: true, AllowOverride: false, ImageDisk: cutil.GetPtr(""), ImageAuthType: cutil.GetPtr(""), ImageAuthToken: cutil.GetPtr("")},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPIOperatingSystemUpdateRequest_Validate(t *testing.T) {
	existingImageBasedOS := &cdbm.OperatingSystem{
		ID:        uuid.New(),
		Name:      "ab",
		ImageURL:  cutil.GetPtr("https://oldimagepath.iso"),
		ImageSHA:  cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"),
		RootFsID:  cutil.GetPtr("fsID"),
		Status:    cdbm.OperatingSystemStatusPending,
		Type:      cdbm.OperatingSystemTypeImage,
		CreatedBy: uuid.New(),
	}

	existingIpxeBasedOS := &cdbm.OperatingSystem{
		ID:         uuid.New(),
		Name:       "ab",
		IpxeScript: cutil.GetPtr("original ipxe"),
		Status:     cdbm.OperatingSystemStatusPending,
		Type:       cdbm.OperatingSystemTypeIPXE,
		CreatedBy:  uuid.New(),
	}

	existingImageBasedOSWithFSLabel := &cdbm.OperatingSystem{
		ID:          uuid.New(),
		Name:        "abc",
		ImageURL:    cutil.GetPtr("https://oldimagepath.iso"),
		ImageSHA:    cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"),
		RootFsLabel: cutil.GetPtr("10886660c5b2746ff48224646c5094ebcf88c889"),
		Status:      cdbm.OperatingSystemStatusPending,
		Type:        cdbm.OperatingSystemTypeImage,
		CreatedBy:   uuid.New(),
	}
	tests := []struct {
		desc       string
		obj        APIOperatingSystemUpdateRequest
		existingOS *cdbm.OperatingSystem
		expectErr  bool
	}{
		{
			desc:       "error when userData exceeds max length",
			obj:        APIOperatingSystemUpdateRequest{UserData: cutil.GetPtr(strings.Repeat("a", util.MaxUserDataBytes+1))},
			existingOS: existingIpxeBasedOS,
			expectErr:  true,
		},
		{
			desc:      "ok when Name is not provided",
			obj:       APIOperatingSystemUpdateRequest{Description: cutil.GetPtr("ab")},
			expectErr: false,
		},
		{
			desc:      "ok when Description is not provided",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("ab")},
			expectErr: false,
		},
		{
			desc:      "error when Name is provided but is empty",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr(""), Description: cutil.GetPtr("ab")},
			expectErr: true,
		},
		{
			desc:      "error when Name is no valid string",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("a"), Description: cutil.GetPtr("ab")},
			expectErr: true,
		},
		{
			desc:      "error when imageUrl is changed",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), ImageURL: cutil.GetPtr("https://newimagepath.iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980")},
			expectErr: true,
		},
		{
			desc:      "error when imageSha is changed",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), ImageURL: cutil.GetPtr("https://oldimagepath.iso"), ImageSHA: cutil.GetPtr("b2efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7981")},
			expectErr: true,
		},
		{
			desc:      "ok when imageUrl and imageSha are re-sent unchanged",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), ImageURL: cutil.GetPtr("https://oldimagepath.iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980")},
			expectErr: false,
		},
		{
			desc:      "ok when imageAuthType and imageAuthToken are updated without imageUrl",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), ImageAuthType: cutil.GetPtr("Bearer"), ImageAuthToken: cutil.GetPtr("rotated-token")},
			expectErr: false,
		},
		{
			desc:      "error when imageAuthType is invalid",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), ImageAuthType: cutil.GetPtr("VAPID"), ImageAuthToken: cutil.GetPtr("rsa")},
			expectErr: true,
		},
		{
			desc:       "error when imageAuthToken is updated on an iPXE based Operating System",
			obj:        APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), ImageAuthType: cutil.GetPtr("Bearer"), ImageAuthToken: cutil.GetPtr("rsa")},
			existingOS: existingIpxeBasedOS,
			expectErr:  true,
		},
		{
			desc:      "error when imageURL and ipxeScript both specified",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), ImageURL: cutil.GetPtr("http://image.net/iso"), IpxeScript: cutil.GetPtr("ipxe")},
			expectErr: true,
		},
		{
			desc:      "error when both RootFsID and RootFsLabel are populated",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), RootFsLabel: cutil.GetPtr("test-label")},
			expectErr: true,
		},
		{
			desc:      "error when os created with rootFsID but request to update rootFsLabel",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), RootFsLabel: cutil.GetPtr("test-label")},
			expectErr: true,
		},
		{
			desc:      "error when os created with rootFsID and try to clear it without specifying rootFsLabel",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), RootFsID: cutil.GetPtr("")},
			expectErr: true,
		},
		{
			desc:       "error when os created with rootFsLabel but request to update rootFsID",
			obj:        APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e")},
			existingOS: existingImageBasedOSWithFSLabel,
			expectErr:  true,
		},
		{
			desc:       "error when os created with rootFsLabel and try to clear it without specifying rootFsID",
			obj:        APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), RootFsLabel: cutil.GetPtr("")},
			existingOS: existingImageBasedOSWithFSLabel,
			expectErr:  true,
		},
		{
			desc:       "error when ipxeScript is empty",
			obj:        APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("abc"), IpxeScript: cutil.GetPtr("")},
			existingOS: existingIpxeBasedOS,
			expectErr:  true,
		},
		{
			desc:      "ok when description is not valid with empty",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("ab"), Description: cutil.GetPtr("")},
			expectErr: false,
		},
		{
			desc:       "ok when all valid fields for ipxe are provided",
			obj:        APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("ab"), Description: cutil.GetPtr("test"), IpxeScript: cutil.GetPtr("ipxe"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), IsCloudInit: cutil.GetPtr(false), AllowOverride: cutil.GetPtr(true)},
			existingOS: existingIpxeBasedOS,
			expectErr:  false,
		},
		{
			desc:      "ok when all valid image fields provided",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("ab"), ImageURL: cutil.GetPtr("https://oldimagepath.iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e")},
			expectErr: false,
		},
		{
			desc:      "ok when ImageDisk selects the smallest disk",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("ab"), ImageDisk: cutil.GetPtr("smallest")},
			expectErr: false,
		},
		{
			desc:      "ok when optional image fields are empty",
			obj:       APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("ab"), ImageURL: cutil.GetPtr("https://oldimagepath.iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), ImageDisk: cutil.GetPtr(""), ImageAuthType: cutil.GetPtr(""), ImageAuthToken: cutil.GetPtr("")},
			expectErr: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			var os *cdbm.OperatingSystem
			if tc.existingOS != nil {
				os = tc.existingOS
			} else {
				os = existingImageBasedOS
			}
			err := tc.obj.Validate(os)
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPIOperatingSystemCreateRequest_ValidateAndSetUserData(t *testing.T) {
	type fields struct {
		Name              string
		Description       *string
		TenantID          *string
		OperatingSystemID *string
		IpxeScript        *string
		UserData          *string
		PhoneHomeEnabled  *bool
	}
	tests := []struct {
		name         string
		fields       fields
		phoneHomeUrl *string
		wantErr      bool
	}{
		{
			name: "test valid Operating System PhoneHome enabled create request when userData is nil",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      false,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "test valid Operating System PhoneHome enabled create request when userData is empty",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr(""),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      false,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is invalid",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr("test"),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      true,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "error when effective userData exceeds max length after phone home insertion",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr("a: " + strings.Repeat("b", util.MaxUserDataBytes-10)),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      true,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is valid YAML but invalid cloud-config",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr("#test"),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      true,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is valid YAML and valid cloud-config but comments only",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr("#cloud-config"),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      true,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "test valid Instance PhoneHome disabled create request when userData is invalid",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr("test"),
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:      false,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "test valid Instance PhoneHome nil create request when userData is invalid",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr("test"),
				PhoneHomeEnabled:  nil,
			},
			wantErr:      false,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
		{
			name: "test valid Instance PhoneHome enabled create request when userData is valid",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				TenantID:          cutil.GetPtr(uuid.NewString()),
				OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				UserData:          cutil.GetPtr(util.TestCommonCloudInit),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      false,
			phoneHomeUrl: cutil.GetPtr("http://localhost/local"),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			icr := APIOperatingSystemCreateRequest{
				Name:             tt.fields.Name,
				Description:      tt.fields.Description,
				TenantID:         tt.fields.TenantID,
				IpxeScript:       tt.fields.IpxeScript,
				UserData:         tt.fields.UserData,
				PhoneHomeEnabled: tt.fields.PhoneHomeEnabled,
			}

			err := icr.ValidateAndSetUserData(*tt.phoneHomeUrl)
			if (err != nil) != tt.wantErr {
				marshalledErr, _ := json.Marshal(err)
				t.Errorf("APIOperatingSystemCreateRequest.ValidateAndSetUserData() error = %v, wantErr %v", string(marshalledErr), tt.wantErr)
			}

			if err != nil {
				return
			}

			if icr.PhoneHomeEnabled != nil && *icr.PhoneHomeEnabled {
				assert.NotNil(t, icr.UserData)
				assert.Contains(t, *icr.UserData, *tt.phoneHomeUrl)
				// split string into new line and make sure always first line is #cloud-config
				lines := strings.Split(*icr.UserData, "\n")
				assert.Equal(t, util.SiteCloudConfig, lines[0])
				assert.NotEqual(t, util.SiteCloudConfig, lines[1])
			}
		})
	}
}

func TestAPIOperatingSystemUpdateRequest_ValidateAndSetUserData(t *testing.T) {
	type fields struct {
		Name              string
		Description       *string
		OperatingSystemID *string
		UserData          *string
		PhoneHomeEnabled  *bool
	}

	existingPhoneHomeEnabledOS := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr(cdmu.TestCommonPhoneHomeCloudInit),
		PhoneHomeEnabled: true,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	// Phone-home is enabled and the stored user-data has a phone-home URL,
	// but it is not the default one.
	existingPhoneHomeEnabledOSStaleURL := &cdbm.OperatingSystem{
		ID:         uuid.New(),
		Name:       "ab",
		IpxeScript: cutil.GetPtr("original ipxe"),
		UserData: cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
  url: http://169.254.169.254:7777/latest/meta-data/phone_home
  post: all`),
		PhoneHomeEnabled: true,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	existingPhoneHomeEnabledOSUserdataNil := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         nil,
		PhoneHomeEnabled: true,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	existingPhoneHomeDisabledOS := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr(cdmu.TestCommonCloudInit),
		PhoneHomeEnabled: false,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	existingPhoneHomeDisabledOSNilUserData := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         nil,
		PhoneHomeEnabled: false,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	existingOSWithPhoneHomeOnlyUserData := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr(util.TestCommonPhoneHomeOnlyCloudInit),
		PhoneHomeEnabled: false,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	existingOSWithXMLUserData := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         cutil.GetPtr(util.TestCommonXMLUserData),
		PhoneHomeEnabled: false,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	// Phone-home was enabled and the stored user-data carries a stale URL,
	// but the request replaces user-data with a phone-home block of its own.
	callerPhoneHomeUserData := cutil.GetPtr(`#cloud-config
package_update: true
phone_home:
    post: all
    url: https://collector.example.com/hook
`)

	// Phone-home was never enabled, so the stored phone-home block is the
	// caller's, not NICo's.
	existingForeignPhoneHomeOS := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             "ab",
		IpxeScript:       cutil.GetPtr("original ipxe"),
		UserData:         callerPhoneHomeUserData,
		PhoneHomeEnabled: false,
		Status:           cdbm.OperatingSystemStatusReady,
		Type:             cdbm.OperatingSystemTypeIPXE,
		CreatedBy:        uuid.New(),
	}

	tests := []struct {
		name                     string
		fields                   fields
		phoneHomeUrl             string
		userDataSearches         []string
		userDataNegativeSearches []string
		wantErr                  bool
		existingOS               *cdbm.OperatingSystem
	}{
		{
			name: "test valid Operating System PhoneHome disabled update request when userData is nil and existing OS has enabled",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingPhoneHomeEnabledOS.ID.String()),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:      false,
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingPhoneHomeEnabledOS,
		},
		{
			name: "error when merged userData from existing OS exceeds max length after phone home insertion",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("Test description"),
				UserData:         nil,
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			wantErr:      true,
			phoneHomeUrl: "http://localhost/local",
			existingOS: &cdbm.OperatingSystem{
				ID:               uuid.New(),
				Name:             "ab",
				IpxeScript:       cutil.GetPtr("original ipxe"),
				UserData:         cutil.GetPtr("a: " + strings.Repeat("b", util.MaxUserDataBytes)),
				PhoneHomeEnabled: false,
				Status:           cdbm.OperatingSystemStatusReady,
				Type:             cdbm.OperatingSystemTypeIPXE,
				CreatedBy:        uuid.New(),
			},
		},
		{
			name: "test valid Operating System PhoneHome disabled update request when existing OS has enabled and its stored phone-home URL is stale",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingPhoneHomeEnabledOSStaleURL.ID.String()),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:      false,
			phoneHomeUrl: config.DefaultSitePhoneHomeUrl,
			// NICo authored the block, so it is removed by key without
			// matching the URL: neither the key nor the stale URL survives,
			// while the rest of the document is preserved.
			userDataSearches:         []string{"package_update"},
			userDataNegativeSearches: []string{"phone_home", "169.254.169.254:7777"},
			existingOS:               existingPhoneHomeEnabledOSStaleURL,
		},
		{
			name: "test valid PhoneHome disabled update request with caller-supplied userData when existing OS has enabled with a stale stored URL",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingPhoneHomeEnabledOSStaleURL.ID.String()),
				UserData:          callerPhoneHomeUserData,
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:      false,
			phoneHomeUrl: config.DefaultSitePhoneHomeUrl,
			// The stored blob is NICo's, but the document being edited is the
			// caller's, so removal stays URL-matched and their hook survives.
			userDataSearches:         []string{"collector.example.com", "package_update"},
			userDataNegativeSearches: []string{"169.254.169.254"},
			existingOS:               existingPhoneHomeEnabledOSStaleURL,
		},
		{
			name: "test valid PhoneHome disabled update request with caller-supplied userData when existing OS never had phone-home enabled",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingForeignPhoneHomeOS.ID.String()),
				UserData:          callerPhoneHomeUserData,
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:      false,
			phoneHomeUrl: config.DefaultSitePhoneHomeUrl,
			// Nothing here is NICo's, so the URL filter applies both before
			// and after the ownership fix and the caller's block survives.
			userDataSearches: []string{"collector.example.com", "package_update"},
			existingOS:       existingForeignPhoneHomeOS,
		},
		{
			name: "test valid PhoneHome enabled, request userData is nil, existing OS userdata is nil, and existing OS has phonehome enabled",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingPhoneHomeEnabledOS.ID.String()),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:      false,
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingPhoneHomeEnabledOSUserdataNil,
		},
		{
			name: "test valid Operating System PhoneHome enabled update request when userData is nil and existing OS has disabled",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingPhoneHomeDisabledOS.ID.String()),
				UserData:          nil,
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      false,
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingPhoneHomeDisabledOS,
		},
		{
			name: "test valid Operating System PhoneHome enabled update request when userData is empty and existing OS has disabled",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingPhoneHomeDisabledOS.ID.String()),
				UserData:          cutil.GetPtr(""),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      false,
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingPhoneHomeDisabledOS,
		},
		{
			name: "test valid Operating System PhoneHome enabled update request when userData is present and existing OS has disabled",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingPhoneHomeDisabledOSNilUserData.ID.String()),
				UserData:          &cdmu.TestCommonCloudInit,
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      false,
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingPhoneHomeDisabledOSNilUserData,
		},
		{
			name: "test valid Operating System PhoneHome disabled update request when userData is present with only phone home section",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingOSWithPhoneHomeOnlyUserData.ID.String()),
				PhoneHomeEnabled:  cutil.GetPtr(false),
			},
			wantErr:      false,
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingOSWithPhoneHomeOnlyUserData,
		},
		{
			name: "test invalid Operating System PhoneHome enabled update request when userData is XML",
			fields: fields{
				Name:              "test-name",
				Description:       cutil.GetPtr("Test description"),
				OperatingSystemID: cutil.GetPtr(existingOSWithXMLUserData.ID.String()),
				PhoneHomeEnabled:  cutil.GetPtr(true),
			},
			wantErr:      true,
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingOSWithXMLUserData,
		},
		{
			name: "success ipxe request arbitrary user-data, OS phone-home disabled, request phonehome nil",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr("random:\narbitrary\n"),
				PhoneHomeEnabled: nil,
			},
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingPhoneHomeDisabledOSNilUserData,
			wantErr:      false,
		},
		{
			name: "success ipxe request arbitrary user-data, OS phone-home disabled, request phonehome enabled",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr("random:\narbitrary\n"),
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			existingOS: existingPhoneHomeDisabledOSNilUserData,
			wantErr:    true,
		},
		{
			name: "fail ipxe request invalid user-data, OS phone-home disabled, request phonehome enabled",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr("random:\narbitrary\n"),
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			existingOS: existingPhoneHomeDisabledOSNilUserData,
			wantErr:    true,
		},
		{
			name: "success ipxe request invalid user-data, OS phone-home disabled, request phonehome disabled",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr("random:\narbitrary\n"),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			phoneHomeUrl: "http://localhost/local",
			existingOS:   existingPhoneHomeDisabledOSNilUserData,
			wantErr:      false,
		},
		{
			name: "success ipxe request invalid user-data, OS phone-home disabled, request phonehome enabled, OS user-data nil, request user-data empty",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr(""),
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			existingOS: existingPhoneHomeDisabledOSNilUserData,
			wantErr:    false,
		},
		{
			name: "success ipxe, request valid user-data, OS phone-home disabled, request phonehome disabled, OS user-data nil, request user-data has phone-home only",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr(cdmu.TestCommonPhoneHomeSegment),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			phoneHomeUrl: "http://localhost",
			existingOS:   existingPhoneHomeDisabledOSNilUserData,
			wantErr:      false,
		},
		{
			name: "success ipxe, request valid user-data, OS phone-home disabled, request phonehome disabled, OS user-data nil, request user-data has phone-home multiple times",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr(cdmu.TestCommonPhoneHomeSegment + cdmu.TestCommonPhoneHomeSegment + cdmu.TestCommonPhoneHomeSegment),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			phoneHomeUrl: "http://localhost",
			existingOS:   existingPhoneHomeDisabledOSNilUserData,
			wantErr:      false,
		},
		{
			name: "success ipxe, request valid user-data, OS phone-home disabled, request phonehome enabled, OS user-data nil, request user-data has phone-home already",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				UserData:         cutil.GetPtr(cdmu.TestCommonCloudInit + "\n" + cdmu.TestCommonPhoneHomeOnlyCloudInit),
				PhoneHomeEnabled: cutil.GetPtr(true),
			},
			phoneHomeUrl:             "http://localhost",
			existingOS:               existingPhoneHomeDisabledOSNilUserData,
			userDataNegativeSearches: []string{"TestCommonPhoneHomeOnlyCloudInit"}, // It's looking for a comment in the TestCommonPhoneHomeOnlyCloudInit value.
			wantErr:                  false,
		},
		// The two cases below reach the early returns that skip the rewrite,
		// where the stored blob is what the Site receives. Every other
		// oversized case here is caught after phone-home insertion instead.
		{
			name: "fail unrelated update when stored user-data is over max length and phone-home was never enabled",
			fields: fields{
				Name:        "renamed",
				Description: cutil.GetPtr("test"),
			},
			phoneHomeUrl: "http://localhost/local",
			existingOS: &cdbm.OperatingSystem{
				ID:               uuid.New(),
				Name:             "ab",
				IpxeScript:       cutil.GetPtr("original ipxe"),
				UserData:         cutil.GetPtr("a: " + strings.Repeat("b", util.MaxUserDataBytes)),
				PhoneHomeEnabled: false,
				Status:           cdbm.OperatingSystemStatusReady,
				Type:             cdbm.OperatingSystemTypeIPXE,
				CreatedBy:        uuid.New(),
			},
			wantErr: true,
		},
		{
			name: "fail phonehome disabled request when stored user-data is over max length and is not valid YAML",
			fields: fields{
				Name:             "test-name",
				Description:      cutil.GetPtr("test"),
				PhoneHomeEnabled: cutil.GetPtr(false),
			},
			phoneHomeUrl: "http://localhost/local",
			existingOS: &cdbm.OperatingSystem{
				ID:               uuid.New(),
				Name:             "ab",
				IpxeScript:       cutil.GetPtr("original ipxe"),
				UserData:         cutil.GetPtr(util.TestCommonXMLUserData + strings.Repeat("<!-- pad -->", util.MaxUserDataBytes/12)),
				PhoneHomeEnabled: false,
				Status:           cdbm.OperatingSystemStatusReady,
				Type:             cdbm.OperatingSystemTypeIPXE,
				CreatedBy:        uuid.New(),
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			osur := APIOperatingSystemUpdateRequest{
				Name:             &tt.fields.Name,
				Description:      tt.fields.Description,
				UserData:         tt.fields.UserData,
				PhoneHomeEnabled: tt.fields.PhoneHomeEnabled,
			}

			err := osur.ValidateAndSetUserData(tt.phoneHomeUrl, tt.existingOS)
			if tt.wantErr {
				require.Error(t, err)
				return
			} else {
				require.NoError(t, err)
			}

			if len(tt.userDataSearches) > 0 {
				require.NotNil(t, osur.UserData)
				for _, search := range tt.userDataSearches {
					assert.Contains(t, *osur.UserData, search)
				}
			}

			if len(tt.userDataNegativeSearches) > 0 {
				// If user-data is nil, then it certainly won't
				// have things we don't want it to have.
				if osur.UserData != nil {
					for _, search := range tt.userDataNegativeSearches {
						assert.NotContains(t, *osur.UserData, search)
					}
				}
			}

			if osur.PhoneHomeEnabled != nil && *osur.PhoneHomeEnabled {
				assert.NotNil(t, osur.UserData)
				assert.Contains(t, *osur.UserData, tt.phoneHomeUrl)
				// split string into new line and make sure always first line is #cloud-config
				lines := strings.Split(*osur.UserData, "\n")
				assert.Equal(t, util.SiteCloudConfig, lines[0])
				assert.Equal(t, util.SiteCloudConfig, lines[0])
				assert.NotEqual(t, util.SiteCloudConfig, lines[1])
			}

			if osur.PhoneHomeEnabled != nil && !*osur.PhoneHomeEnabled {
				// If phone-home is disabled, user-data can validly be nil.
				if osur.UserData != nil {
					assert.NotContains(t, *osur.UserData, tt.phoneHomeUrl)
				}
			}
		})
	}
}

func TestIsCloudInitFromUserData(t *testing.T) {
	tests := []struct {
		desc     string
		userData *string
		want     bool
	}{
		{
			desc:     "nil user data",
			userData: nil,
			want:     false,
		},
		{
			desc:     "empty user data",
			userData: cutil.GetPtr(""),
			want:     false,
		},
		{
			desc:     "non-empty user data",
			userData: cutil.GetPtr("#cloud-config"),
			want:     true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			assert.Equal(t, tc.want, IsCloudInitFromUserData(tc.userData))
		})
	}
}

func TestAPIOperatingSystemNew(t *testing.T) {
	dbOS := &cdbm.OperatingSystem{
		ID:                       uuid.New(),
		Name:                     "test",
		Description:              cutil.GetPtr("test"),
		Org:                      "test",
		InfrastructureProviderID: cutil.GetPtr(uuid.New()),
		TenantID:                 cutil.GetPtr(uuid.New()),
		IpxeScript:               cutil.GetPtr("ipxe"),
		UserData:                 cutil.GetPtr("ud"),
		AllowOverride:            false,
		Status:                   cdbm.OperatingSystemStatusPending,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
	}
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbOS.ID.String(),
			Status:   cdbm.OperatingSystemStatusPending,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}
	dbossa := []cdbm.OperatingSystemSiteAssociation{
		{ID: uuid.New(), OperatingSystemID: dbOS.ID, SiteID: uuid.New(), Version: cutil.GetPtr("1233"), Status: cdbm.OperatingSystemSiteAssociationStatusSyncing},
	}
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{
		dbossa[0].SiteID: {
			ID:                  uuid.New(),
			TenantID:            *dbOS.TenantID,
			TenantOrg:           "test",
			SiteID:              dbossa[0].SiteID,
			EnableSerialConsole: true,
			Config:              cdbm.TenantSiteConfig{},
			Created:             cdb.GetCurTime(),
			Updated:             cdb.GetCurTime(),
		},
	}
	tests := []struct {
		desc    string
		dbObj   *cdbm.OperatingSystem
		sdObj   []cdbm.StatusDetail
		osas    []cdbm.OperatingSystemSiteAssociation
		sttsmap map[uuid.UUID]*cdbm.TenantSite
	}{
		{
			desc:    "test creating API OperatingSystem",
			dbObj:   dbOS,
			sdObj:   dbsds,
			osas:    dbossa,
			sttsmap: sttsmap,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIOperatingSystem(tc.dbObj, tc.sdObj, tc.osas, tc.sttsmap)
			assert.Equal(t, tc.dbObj.ID.String(), got.ID)
			assert.Equal(t, *tc.dbObj.Description, *got.Description)
			assert.Equal(t, IsCloudInitFromUserData(tc.dbObj.UserData), got.IsCloudInit)
		})
	}
}

func TestAPIOperatingSystemCreateRequest_ToImageProto(t *testing.T) {
	id := uuid.New()
	url := "https://image"
	sha := "deadbeef"
	rootFsID := "fs-1"
	os := &cdbm.OperatingSystem{
		ID:                 id,
		Name:               "ubuntu",
		ImageURL:           &url,
		ImageSHA:           &sha,
		RootFsID:           &rootFsID,
		EnableBlockStorage: false,
	}
	t.Run("delegates to ToImageAttributesProto with tenantOrg", func(t *testing.T) {
		req := APIOperatingSystemCreateRequest{}
		got := req.ToImageProto(os, "org-1")
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		require.NotNil(t, got.Name)
		assert.Equal(t, "ubuntu", *got.Name)
		assert.Equal(t, "org-1", got.TenantOrganizationId)
		assert.Equal(t, "https://image", got.SourceUrl)
		assert.Equal(t, "deadbeef", got.Digest)
		require.NotNil(t, got.RootfsId)
		assert.Equal(t, "fs-1", *got.RootfsId)
	})
	t.Run("uses ControllerOperatingSystemID when set", func(t *testing.T) {
		ctrlID := uuid.New()
		osWithCtrl := &cdbm.OperatingSystem{
			ID:                          id,
			ControllerOperatingSystemID: &ctrlID,
			Name:                        "ubuntu",
			ImageURL:                    &url,
			ImageSHA:                    &sha,
			RootFsID:                    &rootFsID,
		}
		req := APIOperatingSystemCreateRequest{}
		got := req.ToImageProto(osWithCtrl, "org-1")
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, ctrlID.String(), got.Id.Value)
	})
}

func TestAPIOperatingSystemUpdateRequest_ToImageProto(t *testing.T) {
	id := uuid.New()
	url := "https://image-new"
	sha := "cafebabe"
	rootFsLabel := "lbl"
	uos := &cdbm.OperatingSystem{
		ID:                 id,
		Name:               "ubuntu-22",
		ImageURL:           &url,
		ImageSHA:           &sha,
		RootFsLabel:        &rootFsLabel,
		EnableBlockStorage: true,
	}
	t.Run("delegates to ToImageAttributesProto with tenantOrg", func(t *testing.T) {
		req := APIOperatingSystemUpdateRequest{}
		got := req.ToImageProto(uos, "org-2")
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		require.NotNil(t, got.Name)
		assert.Equal(t, "ubuntu-22", *got.Name)
		assert.Equal(t, "org-2", got.TenantOrganizationId)
		assert.Equal(t, "https://image-new", got.SourceUrl)
		assert.Equal(t, "cafebabe", got.Digest)
		assert.True(t, got.CreateVolume)
		require.NotNil(t, got.RootfsLabel)
		assert.Equal(t, "lbl", *got.RootfsLabel)
	})
}
