// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

var (
	ErrMsgNotConfigurableByProvider = "value is not configurable by Provider"
	ErrMsgNotConfigurableByTenant   = "value is not configurable by Tenant"

	MachineStatsAllocatedInUse    = "allocatedInUse"
	MachineStatsAllocatedNotInUse = "allocatedNotInUse"
	MachineStatsUnallocated       = "unallocated"
)

// APISiteCreateRequest captures the request data for creating a new site
type APISiteCreateRequest struct {
	// Name is the name of the site
	Name string `json:"name"`
	// Description is the description of the site
	Description *string `json:"description"`
	// SerialConsoleHostname is the serial console hostname of the site
	SerialConsoleHostname *string `json:"serialConsoleHostname"`
	// Location identifies site location
	Location *APISiteLocation `json:"location"`
	// Contact identifies site contact
	Contact *APISiteContact `json:"contact"`
}

// Validate validates Site create request data
func (ascr APISiteCreateRequest) Validate() error {
	err := validation.ValidateStruct(&ascr,
		validation.Field(&ascr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&ascr.SerialConsoleHostname, validationis.Host.Error(validationErrorInvalidHostname)),
	)

	if err != nil {
		return err
	}

	return nil
}

type APISiteCapabilitiesUpdateRequest struct {
	NativeNetworking          *bool `json:"nativeNetworking"`
	NetworkSecurityGroup      *bool `json:"networkSecurityGroup"`
	NVLinkPartition           *bool `json:"nvLinkPartition"`
	ImageBasedOperatingSystem *bool `json:"imageBasedOperatingSystem"`
	DPSPowerManagement        *bool `json:"dpsPowerManagement"`
	// Flow and VpcSlaac are accepted by the binder so update attempts can be rejected
	// explicitly. Site config inventory is the only writer for these fields.
	Flow     *bool `json:"flow"`
	VpcSlaac *bool `json:"vpcSlaac"`
}

func (ascur APISiteCapabilitiesUpdateRequest) ToSiteConfig(existing *cdbm.SiteConfig) *cdbm.SiteConfig {
	cfg := existing
	if cfg == nil {
		cfg = &cdbm.SiteConfig{}
	}

	if ascur.NativeNetworking != nil {
		cfg.NativeNetworking = *ascur.NativeNetworking
	}

	if ascur.NetworkSecurityGroup != nil {
		cfg.NetworkSecurityGroup = *ascur.NetworkSecurityGroup
	}

	if ascur.NVLinkPartition != nil {
		cfg.NVLinkPartition = *ascur.NVLinkPartition
	}

	if ascur.ImageBasedOperatingSystem != nil {
		cfg.ImageBasedOperatingSystem = *ascur.ImageBasedOperatingSystem
	}

	if ascur.DPSPowerManagement != nil {
		cfg.DPSPowerManagement = *ascur.DPSPowerManagement
	}

	return cfg
}

// APISiteUpdateRequest captures the request data for updating a new site
type APISiteUpdateRequest struct {
	// Name is the name of the site
	Name *string `json:"name"`
	// Description is the description of the site
	Description *string `json:"description"`
	// RenewRegistrationToken is a flag to renew the registration token
	RenewRegistrationToken *bool `json:"renewRegistrationToken"`
	// SerialConsoleHostname is the serial console hostname of the site
	SerialConsoleHostname *string `json:"serialConsoleHostname"`
	// IsSerialConsoleEnabled is a flag to indicate if serial console is enabled
	IsSerialConsoleEnabled *bool `json:"isSerialConsoleEnabled"`
	// SerialConsoleIdleTimeout is the idle timeout for a serial console session
	SerialConsoleIdleTimeout *int `json:"serialConsoleIdleTimeout"`
	// SerialConsoleMaxSessionLength is the maximum session length for a serial console session
	SerialConsoleMaxSessionLength *int `json:"serialConsoleMaxSessionLength"`
	// IsSerialConsoleSSHKeysEnabled indicates if Tenant has enabled/disabled serial console access
	IsSerialConsoleSSHKeysEnabled *bool `json:"isSerialConsoleSSHKeysEnabled"`
	// Location updates location of the site
	Location *APISiteLocation `json:"location"`
	// Contact updates contact for the site
	Contact *APISiteContact `json:"contact"`
	// Capabilities updates capabilities for the site
	Capabilities *APISiteCapabilitiesUpdateRequest `json:"capabilities"`
}

// Validate validates Site update request data
func (asur APISiteUpdateRequest) Validate(isProvider bool, isTenant bool) error {
	var err error

	if isProvider {
		// Validate fields that can only be set by Provider
		err = validation.ValidateStruct(&asur,
			validation.Field(&asur.Name,
				validation.NilOrNotEmpty.Error(validationErrorStringLength),
				validation.When(asur.Name != nil, validation.By(util.ValidateNameCharacters)),
				validation.When(asur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
			validation.Field(&asur.SerialConsoleHostname, validationis.Host.Error(validationErrorInvalidHostname)),
			validation.Field(&asur.SerialConsoleIdleTimeout, validation.Min(1).Error("value must be greater than 0")),
			validation.Field(&asur.SerialConsoleMaxSessionLength, validation.Min(1).Error("value must be greater than 0")),
		)
		if err == nil && asur.Capabilities != nil {
			err = validation.ValidateStruct(asur.Capabilities,
				validation.Field(&asur.Capabilities.Flow, validation.Nil.Error(ErrMsgNotConfigurableByProvider)),
				validation.Field(&asur.Capabilities.VpcSlaac, validation.Nil.Error(ErrMsgNotConfigurableByProvider)),
			)
		}
	} else {
		// Request is not from a user with Provider role, reject updates to fields that can only be set by Provider
		err = validation.ValidateStruct(&asur,
			validation.Field(&asur.Name, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
			validation.Field(&asur.Description, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
			validation.Field(&asur.RenewRegistrationToken, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
			validation.Field(&asur.IsSerialConsoleEnabled, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
			validation.Field(&asur.SerialConsoleHostname, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
			validation.Field(&asur.SerialConsoleIdleTimeout, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
			validation.Field(&asur.SerialConsoleMaxSessionLength, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
			validation.Field(&asur.Capabilities, validation.Nil.Error(ErrMsgNotConfigurableByTenant)),
		)
	}

	if err != nil {
		return err
	}

	if isTenant {
		// Validate fields that can only be set by Tenant
		err = validation.ValidateStruct(&asur,
			validation.Field(&asur.IsSerialConsoleSSHKeysEnabled, validation.Nil.Error("configuring this value is no longer supported, update SSH Key Groups to remove Site instead")),
		)
	} else {
		// Request is not from a user with Tenant role, reject updates to fields that can only be set by Tenant
		err = validation.ValidateStruct(&asur,
			validation.Field(&asur.IsSerialConsoleSSHKeysEnabled, validation.Nil.Error(ErrMsgNotConfigurableByProvider)),
		)
	}

	return err
}

type APISiteMachineStats struct {
	Total                  int                       `json:"total"`
	TotalByStatus          map[string]int            `json:"totalByStatus"`
	TotalByHealth          map[string]int            `json:"totalByHealth"`
	TotalByStatusAndHealth map[string]map[string]int `json:"totalByStatusAndHealth"`
	TotalByAllocation      map[string]int            `json:"totalByAllocation"`
}

// NewAPISiteMachineStats creates and returns a new APISiteMachineStats object
func NewAPISiteMachineStats() *APISiteMachineStats {
	return &APISiteMachineStats{
		Total:                  0,
		TotalByStatus:          map[string]int{},
		TotalByHealth:          map[string]int{},
		TotalByStatusAndHealth: map[string]map[string]int{},
		TotalByAllocation: map[string]int{
			MachineStatsAllocatedInUse:    0,
			MachineStatsAllocatedNotInUse: 0,
			MachineStatsUnallocated:       0,
		},
	}
}

// APISite is a data structure to capture information about site at the API layer
type APISite struct {
	// ID is the unique UUID v4 identifier of the site in NICo Cloud
	ID string `json:"id"`
	// Name is the name of the site
	Name string `json:"name"`
	// Description is the description of the site
	Description *string `json:"description"`
	// Org is the NGC organization ID of the infrastructure provider and the org the site belongs to
	Org string `json:"org"`
	// InfrastructureProviderID is the ID of the infrastructure provider who owns the site
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// SiteControllerVersion is the version of the site controller
	SiteControllerVersion *string `json:"siteControllerVersion"`
	// SiteAgentVersion is the version of the site agent
	SiteAgentVersion *string `json:"siteAgentVersion"`
	// RegistrationToken is the registration token to pair the site with the site controller
	RegistrationToken *string `json:"registrationToken"`
	// RegistrationTokenExpiration is the ISO datetime string for when the registration token expires
	RegistrationTokenExpiration *time.Time `json:"registrationTokenExpiration"`
	// SerialConsoleHostname is the serial console hostname of the site controller
	SerialConsoleHostname *string `json:"serialConsoleHostname"`
	// IsSerialConsoleEnabled is a flag to indicate if serial console is enabled
	IsSerialConsoleEnabled bool `json:"isSerialConsoleEnabled"`
	// SerialConsoleIdleTimeout is the idle timeout for a serial console session
	SerialConsoleIdleTimeout *int `json:"serialConsoleIdleTimeout"`
	// SerialConsoleMaxSessionLength is the maximum session length for a serial console session
	SerialConsoleMaxSessionLength *int `json:"serialConsoleMaxSessionLength"`
	// IsSerialConsoleSSHKeysEnabled indicates if Tenant has enabled serial console access using SSH Keys
	IsSerialConsoleSSHKeysEnabled *bool `json:"isSerialConsoleSSHKeysEnabled,omitempty"`
	// IsOnline is the connection status attribute for Site
	IsOnline bool `json:"isOnline"`
	// Status is the status of the site
	Status string `json:"status"`
	// StatusHistory is the status detail records for the site over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created indicates the ISO datetime string for when the site was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the site was last updated
	Updated time.Time `json:"updated"`
	// Location information about site location
	Location *APISiteLocation `json:"location"`
	// Contact information about site contact
	Contact *APISiteContact `json:"contact"`
	// MachineStats holds machine counts by status for a site
	MachineStats *APISiteMachineStats `json:"machineStats"`
	// GpuStats holds GPU summary stats grouped by GPU type for a site
	GpuStats *[]APIMachineGPUStats `json:"gpuStats"`
	// Capabilities holds the capabilities, currently for use
	// as site-level feature flagging.
	Capabilities *APISiteCapabilities `json:"capabilities"`
}

// APISiteLocation information about site address
type APISiteLocation struct {
	City    string `json:"city"`
	State   string `json:"state"`
	Country string `json:"country"`
}

// APISiteContact information about site contact
type APISiteContact struct {
	Email string `json:"email"`
}

// NewAPISite creates and returns a new APISite object. If TenantSite is not nil, we skip settings attributes that Tenant does not have access to
func NewAPISite(dbs cdbm.Site, dbsds []cdbm.StatusDetail, ts *cdbm.TenantSite) APISite {
	apiSite := APISite{
		ID:                            dbs.ID.String(),
		Name:                          dbs.Name,
		Description:                   dbs.Description,
		Org:                           dbs.Org,
		InfrastructureProviderID:      dbs.InfrastructureProviderID.String(),
		SiteControllerVersion:         dbs.SiteControllerVersion,
		SiteAgentVersion:              dbs.SiteAgentVersion,
		SerialConsoleHostname:         dbs.SerialConsoleHostname,
		IsSerialConsoleEnabled:        dbs.IsSerialConsoleEnabled,
		SerialConsoleIdleTimeout:      dbs.SerialConsoleIdleTimeout,
		SerialConsoleMaxSessionLength: dbs.SerialConsoleMaxSessionLength,
		Capabilities:                  siteConfigToAPISiteCapabilities(dbs.Config),
		IsOnline:                      dbs.Status == cdbm.SiteStatusRegistered,
		Status:                        dbs.Status,
		Created:                       dbs.Created,
		Updated:                       dbs.Updated,
	}

	if dbs.Location != nil {
		apiSite.Location = &APISiteLocation{
			City:    dbs.Location.City,
			State:   dbs.Location.State,
			Country: dbs.Location.Country,
		}
	}

	if dbs.Contact != nil {
		apiSite.Contact = &APISiteContact{
			Email: dbs.Contact.Email,
		}
	}

	if ts == nil {
		// Return Provider specific information
		apiSite.RegistrationToken = dbs.RegistrationToken
		apiSite.RegistrationTokenExpiration = dbs.RegistrationTokenExpiration
	} else {
		// Return Tenant specific information
		apiSite.IsSerialConsoleSSHKeysEnabled = cutil.GetPtr(ts.EnableSerialConsole)
	}

	// Expand relation if available
	if dbs.InfrastructureProvider != nil {
		apiSite.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbs.InfrastructureProvider)
	}

	apiSite.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiSite.StatusHistory = append(apiSite.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	return apiSite
}

// APISiteCapabilities holds the model of site capabilities
type APISiteCapabilities struct {
	NativeNetworking          bool `json:"nativeNetworking"`
	NetworkSecurityGroup      bool `json:"networkSecurityGroup"`
	NVLinkPartition           bool `json:"nvLinkPartition"`
	Flow                      bool `json:"flow"`
	ImageBasedOperatingSystem bool `json:"imageBasedOperatingSystem"`
	VpcSlaac                  bool `json:"vpcSlaac"`
	DPSPowerManagement        bool `json:"dpsPowerManagement"`
}

func siteConfigToAPISiteCapabilities(cfg *cdbm.SiteConfig) *APISiteCapabilities {
	apiCaps := &APISiteCapabilities{}

	if cfg != nil {
		apiCaps.NativeNetworking = cfg.NativeNetworking
		apiCaps.NetworkSecurityGroup = cfg.NetworkSecurityGroup
		apiCaps.NVLinkPartition = cfg.NVLinkPartition
		apiCaps.Flow = cfg.Flow
		apiCaps.ImageBasedOperatingSystem = cfg.ImageBasedOperatingSystem
		apiCaps.VpcSlaac = cfg.VpcSlaac
		apiCaps.DPSPowerManagement = cfg.DPSPowerManagement
	}

	return apiCaps
}

// APISiteSummary is the data structure to capture API summary of a Site
type APISiteSummary struct {
	// ID is the unique UUID v4 identifier for the Site
	ID string `json:"id"`
	// Name of the Site, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// InfrastructureProviderID is the ID of the infrastructure provider who owns the site
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// IsSerialConsoleEnabled is a flag to indicate if serial console is enabled
	IsSerialConsoleEnabled bool `json:"isSerialConsoleEnabled"`
	// IsOnline is the connection status attribute for Site
	IsOnline bool `json:"isOnline"`
	// Status is the status of the site
	Status string `json:"status"`
	// Capabilities holds the capabilities, currently for use as site-level feature flagging
	Capabilities *APISiteCapabilities `json:"capabilities"`
}

// NewAPISiteSummary accepts a DB layer Site object returns an API layer object
func NewAPISiteSummary(dbst *cdbm.Site) *APISiteSummary {
	ast := APISiteSummary{
		ID:                       dbst.ID.String(),
		Name:                     dbst.Name,
		InfrastructureProviderID: dbst.InfrastructureProviderID.String(),
		IsSerialConsoleEnabled:   dbst.IsSerialConsoleEnabled,
		IsOnline:                 dbst.Status == cdbm.SiteStatusRegistered,
		Status:                   dbst.Status,
		Capabilities:             siteConfigToAPISiteCapabilities(dbst.Config),
	}

	// Expand relation if available
	if dbst.InfrastructureProvider != nil {
		ast.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbst.InfrastructureProvider)
	}

	return &ast
}
