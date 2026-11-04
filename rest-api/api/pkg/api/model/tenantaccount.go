// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"slices"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

const (
	// ErrTenantIDOrOrgRequired is returned when no tenant ID or tenant org is provided
	validationErrorTenantIDOrOrgRequired = "Either Tenant ID or Tenant Org must be specified"

	validationErrorDuplicateAccountCapability = "only one siteCapabilities entry with empty siteIds is allowed"
	validationErrorMissingAccountCapability   = "exactly one siteCapabilities entry with empty siteIds is required"
	validationErrorDuplicateSiteID            = "duplicate siteIds are not allowed across siteCapabilities entries"
	validationErrorMissingCapability          = "at least one capability must be specified"
	validationErrorCapabilityMissingDefault   = "capability is not present in the account-default entry"
)

var (
	// Time when the AccountNumber, SubscriptionID, and SubscriptionTier attributes will be deprecated
	accountNumberSubscriptionIDTierDeprecationTime = time.Date(2026, time.September, 10, 0, 0, 0, 0, time.UTC)

	tenantAccountDeprecations = []DeprecatedEntity{
		{
			OldValue:     "accountNumber",
			Type:         DeprecationTypeAttribute,
			TakeActionBy: accountNumberSubscriptionIDTierDeprecationTime,
		},
		{
			OldValue:     "subscriptionId",
			Type:         DeprecationTypeAttribute,
			TakeActionBy: accountNumberSubscriptionIDTierDeprecationTime,
		},
		{
			OldValue:     "subscriptionTier",
			Type:         DeprecationTypeAttribute,
			TakeActionBy: accountNumberSubscriptionIDTierDeprecationTime,
		},
	}
)

// APITenantAccountCreateRequest is the data structure to capture user request to create a new Tenant
type APITenantAccountCreateRequest struct {
	// InfrastructureProviderID is the ID of the infrastructureProvider in the org
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// TenantID is the ID of the tenant
	TenantID *string `json:"tenantId"`
	// TenantOrg is the org of the tenant
	TenantOrg *string `json:"tenantOrg"`
}

// Validate ensure the values passed in request are acceptable
func (tacr APITenantAccountCreateRequest) Validate() error {
	return validation.ValidateStruct(&tacr,
		validation.Field(&tacr.InfrastructureProviderID,
			validation.When(tacr.InfrastructureProviderID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&tacr.TenantID,
			validation.When(tacr.TenantOrg == nil, validation.Required.Error(validationErrorTenantIDOrOrgRequired)),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&tacr.TenantOrg,
			validation.When(tacr.TenantID == nil, validation.Required.Error(validationErrorTenantIDOrOrgRequired)),
			validation.Length(2, 256).Error(validationErrorStringLength)),
	)
}

// APITenantAccountUpdateRequest is the data structure to capture user request to update a TenantAccount
type APITenantAccountUpdateRequest struct {
	// TenantContactID is the ID of the requesting user
	TenantContactID *string `json:"tenantContactId"`
	// SiteCapabilities replaces the provider-scoped capability configuration for the
	// TenantAccount. It is a pointer so an omitted payload (nil) is distinguishable
	// from a supplied-but-empty payload (non-nil, zero length), which is rejected.
	SiteCapabilities *APITenantAccountSiteCapabilitiesUpdateRequest `json:"siteCapabilities"`
}

// Validate ensure the values passed in request are acceptable
func (taur APITenantAccountUpdateRequest) Validate() error {
	return validation.ValidateStruct(&taur,
		validation.Field(&taur.TenantContactID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// HasSiteCapabilities reports whether the request supplied a siteCapabilities replace
// payload at all. A supplied payload (including an explicit empty array) is considered
// present; only an omitted or JSON-null field is treated as absent.
func (taur APITenantAccountUpdateRequest) HasSiteCapabilities() bool {
	return taur.SiteCapabilities != nil
}

// APITenantAccountSiteCapability describes effective capability values for
// either the TenantAccount default or an explicit site list.
type APITenantAccountSiteCapability struct {
	SiteIDs                      []string `json:"siteIds"`
	TargetedInstanceCreation     bool     `json:"targetedInstanceCreation"`
	CrossTenantInterfaceCreation bool     `json:"crossTenantInterfaceCreation"`
}

// APITenantAccountSiteCapabilityUpdate is the replace payload entry for Provider Admin
// capability updates. Each capability is optional so callers can replace one
// capability without changing the other.
type APITenantAccountSiteCapabilityUpdate struct {
	SiteIDs                      []string `json:"siteIds"`
	TargetedInstanceCreation     *bool    `json:"targetedInstanceCreation"`
	CrossTenantInterfaceCreation *bool    `json:"crossTenantInterfaceCreation"`
}

// APITenantAccountSiteCapabilitiesUpdateRequest is the replace payload for Provider Admin
// capability updates on a TenantAccount.
type APITenantAccountSiteCapabilitiesUpdateRequest []APITenantAccountSiteCapabilityUpdate

// Validate ensures the replace payload is structurally valid.
func (caps APITenantAccountSiteCapabilitiesUpdateRequest) Validate() error {
	if len(caps) == 0 {
		return validation.Errors{"siteCapabilities": errors.New("siteCapabilities must contain at least one entry")}
	}

	accountCapabilityCount := 0
	accountCapabilityIndex := -1
	seenSiteIDs := map[string]struct{}{}

	for i, cap := range caps {
		prefix := fmt.Sprintf("[%d]", i)
		err := validation.ValidateStruct(&cap,
			validation.Field(&cap.SiteIDs,
				validation.Each(validationis.UUID.Error(validationErrorInvalidUUID)),
			),
		)
		if err != nil {
			return validation.Errors{prefix: err}
		}
		if cap.TargetedInstanceCreation == nil && cap.CrossTenantInterfaceCreation == nil {
			return validation.Errors{prefix: errors.New(validationErrorMissingCapability)}
		}

		if len(cap.SiteIDs) == 0 {
			accountCapabilityCount++
			accountCapabilityIndex = i
		}

		for _, siteID := range cap.SiteIDs {
			// Normalize to the canonical UUID string so differently formatted
			// representations of the same Site (case, urn prefix) dedupe. The
			// values are already validated as UUIDs above, so parsing succeeds.
			parsed, perr := uuid.Parse(siteID)
			if perr != nil {
				return validation.Errors{prefix: errors.New(validationErrorInvalidUUID)}
			}
			key := parsed.String()
			if _, ok := seenSiteIDs[key]; ok {
				return validation.Errors{"siteCapabilities": errors.New(validationErrorDuplicateSiteID)}
			}
			seenSiteIDs[key] = struct{}{}
		}
	}

	if accountCapabilityCount == 0 {
		return validation.Errors{"siteCapabilities": errors.New(validationErrorMissingAccountCapability)}
	}
	if accountCapabilityCount > 1 {
		return validation.Errors{"siteCapabilities": errors.New(validationErrorDuplicateAccountCapability)}
	}

	accountCapability := caps[accountCapabilityIndex]
	for i, cap := range caps {
		if len(cap.SiteIDs) == 0 {
			continue
		}
		prefix := fmt.Sprintf("[%d]", i)
		if cap.TargetedInstanceCreation != nil && accountCapability.TargetedInstanceCreation == nil {
			return validation.Errors{prefix: fmt.Errorf("%s: targetedInstanceCreation", validationErrorCapabilityMissingDefault)}
		}
		if cap.CrossTenantInterfaceCreation != nil && accountCapability.CrossTenantInterfaceCreation == nil {
			return validation.Errors{prefix: fmt.Errorf("%s: crossTenantInterfaceCreation", validationErrorCapabilityMissingDefault)}
		}
	}

	return nil
}

// APITenantAccount is the data structure to capture API representation of a TenantAccount
type APITenantAccount struct {
	// ID is the unique UUID v4 identifier for the TenantAccount
	ID string `json:"id"`
	// AccountNumber is the account number of the TenantAccount
	AccountNumberDeprecated *string `json:"accountNumber,omitempty"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// InfrastructureProviderOrg is the org of the InfrastructureProvider
	InfrastructureProviderOrg string `json:"infrastructureProviderOrg"`
	// SubscriptionID is the ID of the subscription
	SubscriptionIDDeprecated *string `json:"subscriptionId,omitempty"`
	// SubscriptionTier is the tier of the subscription
	SubscriptionTierDeprecated *string `json:"subscriptionTier,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID *string `json:"tenantId"`
	// Tenant is the summary of the Tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// TenantOrg is the org of the Tenant
	TenantOrg string `json:"tenantOrg"`
	// TenantContact is the the contact user for the tenant
	TenantContact *APIUser `json:"tenantContact"`
	// AllocationCount is the number of allocations for the TenantAccount
	AllocationCount int `json:"allocationCount"`
	// Status is the status of the TenantAccount
	Status string `json:"status"`
	// StatusHistory is the history of statuses for the TenantAccount
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
	// Deprecations is the list of deprecations for the TenantAccount
	Deprecations []APIDeprecation `json:"deprecations"`
	// SiteCapabilities describes provider-scoped TargetedInstanceCreation settings
	SiteCapabilities []APITenantAccountSiteCapability `json:"siteCapabilities"`
}

// APITenantAccountStats is a data structure to capture information about a TenantAccount stats at the API layer
type APITenantAccountStats struct {
	// Total is the total number of the TenantAccount object in NICo Cloud
	Total int `json:"total"`
	// Pending is the total number of pending TenantAccount object in NICo Cloud
	Pending int `json:"pending"`
	// Invited is the total number of provisioning TenantAccount object in NICo Cloud
	Invited int `json:"invited"`
	// Ready is the total number of ready TenantAccount object in NICo Cloud
	Ready int `json:"ready"`
	// Error is the total number of error TenantAccount object in NICo Cloud
	Error int `json:"error"`
}

// NewAPITenantAccount accepts a DB layer TenantAccount object returns an API layer object
func NewAPITenantAccount(dbta *cdbm.TenantAccount, dbsds []cdbm.StatusDetail, allocationCount int, tenantSites []cdbm.TenantSite) *APITenantAccount {
	apiTenantAccount := APITenantAccount{
		ID:                        dbta.ID.String(),
		InfrastructureProviderID:  dbta.InfrastructureProviderID.String(),
		InfrastructureProviderOrg: dbta.InfrastructureProviderOrg,
		TenantOrg:                 dbta.TenantOrg,
		AllocationCount:           allocationCount,
		Status:                    dbta.Status,
		Created:                   dbta.Created,
		Updated:                   dbta.Updated,
	}

	if dbta.TenantID != nil {
		apiTenantAccount.TenantID = cutil.GetPtr(dbta.TenantID.String())
	}

	if dbta.TenantContact != nil {
		apiTenantAccount.TenantContact = NewAPIUserFromDBUser(*dbta.TenantContact)
	}

	if dbta.InfrastructureProvider != nil {
		apiTenantAccount.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbta.InfrastructureProvider)
	}

	if dbta.Tenant != nil {
		apiTenantAccount.Tenant = NewAPITenantSummary(dbta.Tenant)
	}

	apiTenantAccount.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiTenantAccount.StatusHistory = append(apiTenantAccount.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	if time.Now().Before(accountNumberSubscriptionIDTierDeprecationTime) {
		apiTenantAccount.AccountNumberDeprecated = cutil.GetPtr(dbta.AccountNumber)
		apiTenantAccount.SubscriptionIDDeprecated = dbta.SubscriptionID
		apiTenantAccount.SubscriptionTierDeprecated = dbta.SubscriptionTier
	}

	for _, deprecation := range tenantAccountDeprecations {
		apiTenantAccount.Deprecations = append(apiTenantAccount.Deprecations, NewAPIDeprecation(deprecation))
	}

	accountDefault := dbta.Config
	caps := []APITenantAccountSiteCapability{
		{
			SiteIDs:                      []string{},
			TargetedInstanceCreation:     accountDefault.TargetedInstanceCreation,
			CrossTenantInterfaceCreation: accountDefault.CrossTenantInterfaceCreation,
		},
	}

	if dbta.TenantID != nil {
		groupedSiteIDs := map[cdbm.TenantAccountConfig][]string{}

		for _, ts := range tenantSites {
			// Fail closed: a TenantSite with a missing Site relation cannot be
			// confirmed to belong to this account's provider, so it is excluded.
			if ts.Site == nil || ts.Site.InfrastructureProviderID != dbta.InfrastructureProviderID {
				continue
			}
			effective := accountDefault
			if ts.Config.TargetedInstanceCreation != nil {
				effective.TargetedInstanceCreation = *ts.Config.TargetedInstanceCreation
			}
			if ts.Config.CrossTenantInterfaceCreation != nil {
				effective.CrossTenantInterfaceCreation = *ts.Config.CrossTenantInterfaceCreation
			}
			if effective == accountDefault {
				continue
			}

			groupedSiteIDs[effective] = append(groupedSiteIDs[effective], ts.SiteID.String())
		}

		vectors := make([]cdbm.TenantAccountConfig, 0, len(groupedSiteIDs))
		for vector := range groupedSiteIDs {
			vectors = append(vectors, vector)
		}
		slices.SortFunc(vectors, func(a, b cdbm.TenantAccountConfig) int {
			if a.TargetedInstanceCreation != b.TargetedInstanceCreation {
				if !a.TargetedInstanceCreation {
					return -1
				}
				return 1
			}
			if a.CrossTenantInterfaceCreation != b.CrossTenantInterfaceCreation {
				if !a.CrossTenantInterfaceCreation {
					return -1
				}
				return 1
			}
			return 0
		})

		for _, vector := range vectors {
			siteIDs := groupedSiteIDs[vector]
			slices.Sort(siteIDs)
			caps = append(caps, APITenantAccountSiteCapability{
				SiteIDs:                      siteIDs,
				TargetedInstanceCreation:     vector.TargetedInstanceCreation,
				CrossTenantInterfaceCreation: vector.CrossTenantInterfaceCreation,
			})
		}
	}

	apiTenantAccount.SiteCapabilities = caps

	return &apiTenantAccount
}
