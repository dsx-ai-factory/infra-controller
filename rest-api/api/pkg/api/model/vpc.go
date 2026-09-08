// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"math"
	"net/netip"
	"regexp"
	"slices"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"
)

var (
	vpcRoutingProfileStartsWithLetterRegexp = regexp.MustCompile(`^[A-Za-z]`)
	vpcRoutingProfileAllowedCharsRegexp     = regexp.MustCompile(`^[A-Za-z0-9-]+$`)
)

const (
	APIVpcRoutingProfileExternal           = "external"
	APIVpcRoutingProfileInternal           = "internal"
	APIVpcRoutingProfilePrivilegedInternal = "privileged-internal"

	apiVpcRoutingProfileSiteExternal           = "EXTERNAL"
	apiVpcRoutingProfileSiteInternal           = "INTERNAL"
	apiVpcRoutingProfileSitePrivilegedInternal = "PRIVILEGED_INTERNAL"
)

var apiVpcRoutingProfileToSiteMap = map[string]string{
	APIVpcRoutingProfileExternal:           apiVpcRoutingProfileSiteExternal,
	APIVpcRoutingProfileInternal:           apiVpcRoutingProfileSiteInternal,
	APIVpcRoutingProfilePrivilegedInternal: apiVpcRoutingProfileSitePrivilegedInternal,
}

var apiVpcRoutingProfileFromSiteMap = map[string]string{
	apiVpcRoutingProfileSiteExternal:           APIVpcRoutingProfileExternal,
	apiVpcRoutingProfileSiteInternal:           APIVpcRoutingProfileInternal,
	apiVpcRoutingProfileSitePrivilegedInternal: APIVpcRoutingProfilePrivilegedInternal,
}

// NormalizeAPIVpcRoutingProfileForSite converts REST routing profile values to the
// current site-controller wire format when a known mapping exists.
func NormalizeAPIVpcRoutingProfileForSite(routingProfile string) string {
	if mapped, ok := apiVpcRoutingProfileToSiteMap[routingProfile]; ok {
		return mapped
	}
	return routingProfile
}

// NormalizeAPIVpcRoutingProfileFromSite converts known site-controller routing
// profile values to the REST API spelling.
func NormalizeAPIVpcRoutingProfileFromSite(routingProfile string) string {
	if mapped, ok := apiVpcRoutingProfileFromSiteMap[routingProfile]; ok {
		return mapped
	}
	return routingProfile
}

// APIVpcRouteTarget identifies a BGP route target by ASN and VNI.
type APIVpcRouteTarget struct {
	ASN uint32 `json:"asn"`
	VNI uint32 `json:"vni"`
}

// ToDBModel converts an API route target to its persisted representation.
func (target APIVpcRouteTarget) ToDBModel() cdbm.VpcRouteTarget {
	return cdbm.VpcRouteTarget{ASN: target.ASN, VNI: target.VNI}
}

// FromDBModel populates an API route target from its persisted representation.
func (target *APIVpcRouteTarget) FromDBModel(dbTarget cdbm.VpcRouteTarget) {
	*target = APIVpcRouteTarget{ASN: dbTarget.ASN, VNI: dbTarget.VNI}
}

// APIVpcRouteTargets is a collection of API route targets with DB conversion behavior.
type APIVpcRouteTargets []APIVpcRouteTarget

// ToDBModel converts route targets to their persisted representation.
// Nil input is normalized to an allocated empty slice.
func (targets APIVpcRouteTargets) ToDBModel() []cdbm.VpcRouteTarget {
	dbTargets := make([]cdbm.VpcRouteTarget, 0, len(targets))
	for _, target := range targets {
		dbTargets = append(dbTargets, target.ToDBModel())
	}
	return dbTargets
}

// FromDBModel populates route targets from their persisted representation.
// Nil input is normalized to an allocated empty slice.
func (targets *APIVpcRouteTargets) FromDBModel(dbTargets []cdbm.VpcRouteTarget) {
	*targets = make(APIVpcRouteTargets, 0, len(dbTargets))
	for _, dbTarget := range dbTargets {
		target := APIVpcRouteTarget{}
		target.FromDBModel(dbTarget)
		*targets = append(*targets, target)
	}
}

// APIVpcRoutingProfileOverrides contains presence-aware routing properties set on a VPC.
// Nil fields inherit from the named routing profile, while present empty lists
// explicitly replace the corresponding base-profile lists.
type APIVpcRoutingProfileOverrides struct {
	RouteTargetImports             *APIVpcRouteTargets `json:"routeTargetImports"`
	RouteTargetsOnExports          *APIVpcRouteTargets `json:"routeTargetsOnExports"`
	LeakDefaultRouteFromUnderlay   *bool               `json:"leakDefaultRouteFromUnderlay"`
	LeakTenantHostRoutesToUnderlay *bool               `json:"leakTenantHostRoutesToUnderlay"`
	TenantLeakCommunitiesAccepted  *bool               `json:"tenantLeakCommunitiesAccepted"`
	AcceptedLeaksFromUnderlay      *[]string           `json:"acceptedLeaksFromUnderlay"`
	AllowedAnycastPrefixes         *[]string           `json:"allowedAnycastPrefixes"`
}

// validateVpcRoutingProfilePrefix ensures a routing-policy prefix is valid IPv4 or IPv6 CIDR.
func validateVpcRoutingProfilePrefix(value any) error {
	prefix, ok := value.(string)
	if !ok {
		return nil
	}
	if _, err := netip.ParsePrefix(prefix); err != nil {
		return fmt.Errorf("invalid prefix `%s`", prefix)
	}
	return nil
}

// validateVpcRoutingProfilePrefixes validates every prefix while preserving empty-list support.
func validateVpcRoutingProfilePrefixes(value any) error {
	prefixes, ok := value.(*[]string)
	if !ok || prefixes == nil {
		return nil
	}
	return validation.Validate(*prefixes,
		validation.Each(validation.By(validateVpcRoutingProfilePrefix)),
	)
}

// Validate ensures every supplied override can be represented by Core.
func (profile *APIVpcRoutingProfileOverrides) Validate() error {
	if profile == nil {
		return nil
	}
	return validation.ValidateStruct(profile,
		validation.Field(&profile.RouteTargetImports),
		validation.Field(&profile.RouteTargetsOnExports),
		validation.Field(&profile.AcceptedLeaksFromUnderlay,
			validation.By(validateVpcRoutingProfilePrefixes)),
		validation.Field(&profile.AllowedAnycastPrefixes,
			validation.By(validateVpcRoutingProfilePrefixes)),
	)
}

// ToDB converts API routing-profile overrides to their persisted representation.
func (profile *APIVpcRoutingProfileOverrides) ToDB() *cdbm.VpcRoutingProfileOverrides {
	if profile == nil {
		return nil
	}

	dbProfile := &cdbm.VpcRoutingProfileOverrides{
		LeakDefaultRouteFromUnderlay:   profile.LeakDefaultRouteFromUnderlay,
		LeakTenantHostRoutesToUnderlay: profile.LeakTenantHostRoutesToUnderlay,
		TenantLeakCommunitiesAccepted:  profile.TenantLeakCommunitiesAccepted,
	}
	if profile.RouteTargetImports != nil {
		targets := profile.RouteTargetImports.ToDBModel()
		dbProfile.RouteTargetImports = &targets
	}
	if profile.RouteTargetsOnExports != nil {
		targets := profile.RouteTargetsOnExports.ToDBModel()
		dbProfile.RouteTargetsOnExports = &targets
	}
	if profile.AcceptedLeaksFromUnderlay != nil {
		prefixes := slices.Clone(*profile.AcceptedLeaksFromUnderlay)
		dbProfile.AcceptedLeaksFromUnderlay = &prefixes
	}
	if profile.AllowedAnycastPrefixes != nil {
		prefixes := slices.Clone(*profile.AllowedAnycastPrefixes)
		dbProfile.AllowedAnycastPrefixes = &prefixes
	}

	return dbProfile
}

// FromDB populates API routing-profile overrides from their persisted representation.
func (profile *APIVpcRoutingProfileOverrides) FromDB(dbProfile *cdbm.VpcRoutingProfileOverrides) {
	*profile = APIVpcRoutingProfileOverrides{}
	if dbProfile == nil {
		return
	}

	profile.LeakDefaultRouteFromUnderlay = dbProfile.LeakDefaultRouteFromUnderlay
	profile.LeakTenantHostRoutesToUnderlay = dbProfile.LeakTenantHostRoutesToUnderlay
	profile.TenantLeakCommunitiesAccepted = dbProfile.TenantLeakCommunitiesAccepted
	if dbProfile.RouteTargetImports != nil {
		targets := APIVpcRouteTargets{}
		targets.FromDBModel(*dbProfile.RouteTargetImports)
		profile.RouteTargetImports = &targets
	}
	if dbProfile.RouteTargetsOnExports != nil {
		targets := APIVpcRouteTargets{}
		targets.FromDBModel(*dbProfile.RouteTargetsOnExports)
		profile.RouteTargetsOnExports = &targets
	}
	if dbProfile.AcceptedLeaksFromUnderlay != nil {
		prefixes := slices.Clone(*dbProfile.AcceptedLeaksFromUnderlay)
		profile.AcceptedLeaksFromUnderlay = &prefixes
	}
	if dbProfile.AllowedAnycastPrefixes != nil {
		prefixes := slices.Clone(*dbProfile.AllowedAnycastPrefixes)
		profile.AllowedAnycastPrefixes = &prefixes
	}
}

// APIVpcEffectiveRoutingProfile is the fully resolved routing policy reported by Core.
// It does not preserve override presence semantics, and its list fields are
// exposed as non-nil arrays.
type APIVpcEffectiveRoutingProfile struct {
	RouteTargetImports             APIVpcRouteTargets `json:"routeTargetImports"`
	RouteTargetsOnExports          APIVpcRouteTargets `json:"routeTargetsOnExports"`
	LeakDefaultRouteFromUnderlay   bool               `json:"leakDefaultRouteFromUnderlay"`
	LeakTenantHostRoutesToUnderlay bool               `json:"leakTenantHostRoutesToUnderlay"`
	TenantLeakCommunitiesAccepted  bool               `json:"tenantLeakCommunitiesAccepted"`
	AcceptedLeaksFromUnderlay      []string           `json:"acceptedLeaksFromUnderlay"`
	AllowedAnycastPrefixes         []string           `json:"allowedAnycastPrefixes"`
	Internal                       bool               `json:"internal"`
	AccessTier                     uint32             `json:"accessTier"`
}

// FromDB populates an API effective routing profile from the last Core-reported value.
func (profile *APIVpcEffectiveRoutingProfile) FromDB(dbProfile *cdbm.VpcEffectiveRoutingProfile) {
	*profile = APIVpcEffectiveRoutingProfile{}
	if dbProfile == nil {
		return
	}

	profile.RouteTargetImports.FromDBModel(dbProfile.RouteTargetImports)
	profile.RouteTargetsOnExports.FromDBModel(dbProfile.RouteTargetsOnExports)
	profile.LeakDefaultRouteFromUnderlay = dbProfile.LeakDefaultRouteFromUnderlay
	profile.LeakTenantHostRoutesToUnderlay = dbProfile.LeakTenantHostRoutesToUnderlay
	profile.TenantLeakCommunitiesAccepted = dbProfile.TenantLeakCommunitiesAccepted
	profile.AcceptedLeaksFromUnderlay = slices.Clone(dbProfile.AcceptedLeaksFromUnderlay)
	if profile.AcceptedLeaksFromUnderlay == nil {
		profile.AcceptedLeaksFromUnderlay = []string{}
	}
	profile.AllowedAnycastPrefixes = slices.Clone(dbProfile.AllowedAnycastPrefixes)
	if profile.AllowedAnycastPrefixes == nil {
		profile.AllowedAnycastPrefixes = []string{}
	}
	profile.Internal = dbProfile.Internal
	profile.AccessTier = dbProfile.AccessTier
}

// APIVpcCreateRequest captures the request data for creating a new VPC
type APIVpcCreateRequest struct {
	// ID is the user-specified UUID of the VPC.
	ID *uuid.UUID `json:"id"`
	// Name is the name of the VPC
	Name string `json:"name"`
	// Description is the description of the VPC
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// NetworkVirtualizationType is a VPC virtualization type
	NetworkVirtualizationType *string `json:"networkVirtualizationType"`
	// SlaacEnabled selects SLAAC allocation mode for instance IPv6 interfaces.
	SlaacEnabled *bool `json:"slaacEnabled"`
	// Labels is a key value objects
	Labels map[string]string `json:"labels"`
	// NetworkSecurityGroupID is the ID if a desired
	// NSG to attach to the VPC
	NetworkSecurityGroupID *string `json:"networkSecurityGroupId"`
	// NVLinkLogicalPartitionID is the ID of the NVLinkLogicalPartition
	NVLinkLogicalPartitionID *string `json:"nvLinkLogicalPartitionId"`
	// Vni is an optional, explicitly requested VPC VNI.
	// The request will be rejected by the site if the VNI
	// is not within a VNI range allowed for explicit requests.
	Vni *int `json:"vni"`
	// RoutingProfile specifies the routing profile for the VPC.
	// This is only supported when `networkVirtualizationType` is `FNN`, or when
	// `networkVirtualizationType` is omitted and the Site has native networking enabled.
	// This requires the Tenant to have elevated privileges. The selected value must
	// be one of the Site-configured profiles returned for the Tenant.
	RoutingProfile *string `json:"routingProfile"`
	// RoutingProfileOverrides replaces selected properties from the VPC's named routing profile.
	RoutingProfileOverrides *APIVpcRoutingProfileOverrides `json:"routingProfileOverrides"`
	// PowerResourceGroup is the external power provisioning resource group associated with the VPC.
	PowerResourceGroup *string `json:"powerResourceGroup"`
}

// Validate ensure the values passed in create request are acceptable
func (ascr APIVpcCreateRequest) Validate() error {
	err := validation.ValidateStruct(&ascr,
		validation.Field(&ascr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&ascr.Description,
			validation.When(ascr.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&ascr.RoutingProfile,
			validation.When(ascr.RoutingProfile != nil,
				validation.Length(3, 64).Error("`routingProfile` must contain at least 3 characters and a maximum of 64 characters"),
				validation.Match(vpcRoutingProfileStartsWithLetterRegexp).Error("`routingProfile` must start with a letter"),
				validation.Match(vpcRoutingProfileAllowedCharsRegexp).Error("`routingProfile` may only contain letters, numbers, or dashes"),
			),
		),
		validation.Field(&ascr.RoutingProfileOverrides),
		validation.Field(&ascr.PowerResourceGroup,
			validation.When(ascr.PowerResourceGroup != nil, validation.Required.Error("`powerResourceGroup` must not be empty")),
		),
		validation.Field(&ascr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ascr.ID,
			validation.When(ascr.ID != nil, validationis.UUID.Error(validationErrorInvalidUUID))),
	)

	if err != nil {
		return err
	}

	// NetworkVirtualizationType validation
	if ascr.NetworkVirtualizationType != nil {
		if !cdbm.VpcNetworkVirtualzationTypeMap[*ascr.NetworkVirtualizationType] {
			return validation.Errors{
				"networkVirtualizationType": errors.New("ETHERNET_VIRTUALIZER, FNN, and FLAT are currently supported"),
			}
		}
	}

	if ascr.RoutingProfile != nil {
		if ascr.NetworkVirtualizationType != nil && !cdbm.VpcTypeSupportsRoutingProfile(ascr.NetworkVirtualizationType) {
			return validation.Errors{
				"routingProfile": errors.New("`routingProfile` is only supported when `networkVirtualizationType` is FNN"),
			}
		}
	}

	if ascr.RoutingProfileOverrides != nil && ascr.NetworkVirtualizationType != nil && !cdbm.VpcTypeSupportsRoutingProfile(ascr.NetworkVirtualizationType) {
		return validation.Errors{
			"routingProfileOverrides": fmt.Errorf("`routingProfileOverrides` is not supported when `networkVirtualizationType` is `%s`", *ascr.NetworkVirtualizationType),
		}
	}

	if ascr.Vni != nil && (*ascr.Vni < 0 || *ascr.Vni > math.MaxUint16) {
		return validation.Errors{
			"vni": fmt.Errorf("VNI must be an integer between 0 and %d", math.MaxUint16),
		}
	}

	if err := util.ValidateLabels(ascr.Labels); err != nil {
		return err
	}

	return err
}

// ToProto builds the workflow request that asks a Site to create a new
// VPC for this API request. `vpc` is the just-persisted DB record;
// its `ToProto()` is the source of the canonical wire fields
// (ID/Name/NSG/Labels/Description/NVLink/NetworkVirtualizationType),
// and `vpc.RoutingProfile` carries the normalised Site-facing value
// for the optional routing-profile field.
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see (e.g. resolved network-virtualization against site
// config). Specifically, the VNI cast is safe because Validate
// bounds `Vni` to `[0, MaxUint16]`.
func (ascr APIVpcCreateRequest) ToProto(vpc *cdbm.Vpc) *corev1.VpcCreationRequest {
	var vni *uint32
	if ascr.Vni != nil {
		v := uint32(*ascr.Vni)
		vni = &v
	}
	var routingProfile *string
	if ascr.RoutingProfile != nil {
		routingProfile = vpc.RoutingProfile
	}
	vpcProto := vpc.ToProto()
	config := vpcProto.GetConfig()
	return &corev1.VpcCreationRequest{
		Id:                              vpcProto.Id,
		Name:                            vpcProto.Name,
		TenantOrganizationId:            config.TenantOrganizationId,
		NetworkVirtualizationType:       config.NetworkVirtualizationType,
		SlaacEnabled:                    ascr.SlaacEnabled,
		RoutingProfileType:              routingProfile,
		RoutingProfileOverrides:         ascr.RoutingProfileOverrides.ToDB().ToProto(),
		PowerResourceGroup:              config.PowerResourceGroup,
		NetworkSecurityGroupId:          config.NetworkSecurityGroupId,
		Vni:                             vni,
		Metadata:                        vpcProto.Metadata,
		DefaultNvlinkLogicalPartitionId: config.DefaultNvlinkLogicalPartitionId,
	}
}

// APIVpcUpdateRequest captures the request data for updating a new VPC
type APIVpcUpdateRequest struct {
	// Name is the name of the VPC
	Name *string `json:"name"`
	// Description is the description of the VPC
	Description *string `json:"description"`
	// Labels is a key value objects
	Labels map[string]string `json:"labels"`
	// NetworkSecurityGroupID is the ID if a desired
	// NSG to attach to the VPC
	NetworkSecurityGroupID *string `json:"networkSecurityGroupId"`
	// NVLinkLogicalPartitionID is the ID of the NVLinkLogicalPartition
	NVLinkLogicalPartitionID *string `json:"nvLinkLogicalPartitionId"`
	// RoutingProfileOverrides replaces the VPC's current inline routing-profile definition when present.
	RoutingProfileOverrides *APIVpcRoutingProfileOverrides `json:"routingProfileOverrides"`
	// PowerResourceGroup updates the external power provisioning resource group. An empty string clears it.
	PowerResourceGroup *string `json:"powerResourceGroup"`
}

// Validate ensure the values passed in update request are acceptable
func (asur APIVpcUpdateRequest) Validate() error {
	err := validation.ValidateStruct(&asur,
		validation.Field(&asur.Name,
			validation.When(asur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(asur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(asur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&asur.Description,
			validation.When(asur.Description != nil, validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&asur.RoutingProfileOverrides),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(asur.Labels); err != nil {
		return err
	}

	return err
}

// ToProto builds the workflow request that pushes this Update's
// merged-into-DB state to a Site. The persisted `vpc` is the source of
// the wire fields because the handler has already merged the request's
// (sparse) update fields into the entity by the time this is called;
// sending the post-merge state matches the pre-existing handler
// behaviour and keeps unchanged fields populated.
//
// API-level clear intent is represented by the handler clearing the
// persisted entity before this is called. That keeps the Site/Core wire
// contract tied to persisted state: cleared associations are omitted
// instead of serialized as invalid empty IDs, and non-empty updates come
// from the validated DB value.
func (asur APIVpcUpdateRequest) ToProto(vpc *cdbm.Vpc) *corev1.VpcUpdateRequest {
	vpcProto := vpc.ToProto()
	config := vpcProto.GetConfig()
	return &corev1.VpcUpdateRequest{
		Id:                              vpcProto.Id,
		NetworkSecurityGroupId:          config.NetworkSecurityGroupId,
		DefaultNvlinkLogicalPartitionId: config.DefaultNvlinkLogicalPartitionId,
		RoutingProfileOverrides:         asur.RoutingProfileOverrides.ToDB().ToProto(),
		PowerResourceGroup:              asur.PowerResourceGroup,
		Metadata:                        vpcProto.Metadata,
	}
}

// APIVpcVirtualizationUpdateRequest captures the request data for updating virtualization type for a give VPC
type APIVpcVirtualizationUpdateRequest struct {
	// NetworkVirtualizationType is a VPC virtualization type
	NetworkVirtualizationType string `json:"networkVirtualizationType"`
}

// Validate ensure the values passed in update request are acceptable
func (avvur APIVpcVirtualizationUpdateRequest) Validate(existingVpc *cdbm.Vpc) error {
	err := validation.ValidateStruct(&avvur,
		validation.Field(&avvur.NetworkVirtualizationType,
			validation.Required.Error(validationErrorValueRequired),
		),
	)

	if err != nil {
		return err
	}

	// NetworkVirtualizationType validation
	if avvur.NetworkVirtualizationType != cdbm.VpcFNN {
		return validation.Errors{
			"networkVirtualizationType": errors.New("virtualization type can only be updated to FNN"),
		}
	}

	if existingVpc.NetworkVirtualizationType != nil && *existingVpc.NetworkVirtualizationType == cdbm.VpcFNN {
		return validation.Errors{
			"networkVirtualizationType": errors.New("VPC virtualization type is already set to FNN"),
		}
	}

	return nil
}

// APIVpc is a data structure to capture information about VPC at the API layer
type APIVpc struct {
	// ID is the unique UUID v4 identifier of the VPC in NICo Cloud
	ID string `json:"id"`
	// Name is the name of the VPC
	Name string `json:"name"`
	// Description is the description of the VPC
	Description *string `json:"description"`
	// Org is the NGC organization ID of the infrastructure provider and the org the VPC belongs to
	Org string `json:"org"`
	// InfrastructureProviderID is the ID of the infrastructure provider who owns the site
	InfrastructureProviderID *string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID *string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// SiteID is the ID of the Site
	SiteID *string `json:"siteId"`
	// Site is the summary of the site
	Site *APISiteSummary `json:"site,omitempty"`
	// NetworkVirtualizationType is a VPC virtualization type
	NetworkVirtualizationType *string `json:"networkVirtualizationType"`
	// SlaacEnabled indicates whether the VPC uses SLAAC allocation mode.
	SlaacEnabled bool `json:"slaacEnabled"`
	// ControllerVpcID is the ID of the corresponding VPC in Site Controller
	ControllerVpcID *string `json:"controllerVpcId"`
	// Labels is VPC labels specified by user
	Labels map[string]string `json:"labels"`
	// NVLinkLogicalPartitionID is the ID of the NVLinkLogicalPartition
	NVLinkLogicalPartitionID *string `json:"nvLinkLogicalPartitionId"`
	// NVLinkLogicalPartitionSummary is the summary of the NVLinkLogicalPartition
	NVLinkLogicalPartitionSummary *APINVLinkLogicalPartitionSummary `json:"nvLinkLogicalPartitionSummary,omitempty"`
	// NetworkSecurityGroupID is the ID of attached NSG, if any
	NetworkSecurityGroupID *string `json:"networkSecurityGroupId"`
	// NetworkSecurityGroup holds the summary for attached NSG, if requested via includeRelation
	NetworkSecurityGroup *APINetworkSecurityGroupSummary `json:"networkSecurityGroup,omitempty"`
	// NetworkSecurityGroupPropagationDetails is the propagation details for the attched NSG, if any
	NetworkSecurityGroupPropagationDetails *APINetworkSecurityGroupPropagationDetails `json:"networkSecurityGroupPropagationDetails"`
	// RoutingProfile is the applied routing profile for the VPC, when known.
	RoutingProfile *string `json:"routingProfile"`
	// PowerResourceGroup is the external power provisioning resource group associated with the VPC.
	PowerResourceGroup *string `json:"powerResourceGroup"`
	// RoutingProfileOverrides contains properties set directly on the VPC.
	RoutingProfileOverrides *APIVpcRoutingProfileOverrides `json:"routingProfileOverrides"`
	// EffectiveRoutingProfile is visible only to tenants with targeted instance creation permission for the Site.
	EffectiveRoutingProfile *APIVpcEffectiveRoutingProfile `json:"effectiveRoutingProfile,omitempty"`
	// RequestedVni is the explicitly requested VPC VNI at creation time _if_ one was requested.
	RequestedVni *int `json:"requestedVni"`
	// Vni is the active/actual VNI of the VPC, regardless of whether it was
	// explicitly requested or auto-allocated.
	Vni *int `json:"vni"`
	// Status is the status of the VPC
	Status string `json:"status"`
	// StatusHistory is the status detail records for the VPC over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the VPC was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIVpc converts a persisted VPC to its REST representation.
// includeEffectiveRoutingProfile controls whether cached controller-resolved
// routing state is exposed.
func NewAPIVpc(dbVpc cdbm.Vpc, dbsds []cdbm.StatusDetail, includeEffectiveRoutingProfile bool) APIVpc {
	apivpc := APIVpc{
		ID:                                     dbVpc.ID.String(),
		Name:                                   dbVpc.Name,
		Description:                            dbVpc.Description,
		Org:                                    dbVpc.Org,
		InfrastructureProviderID:               util.GetUUIDPtrToStrPtr(&dbVpc.InfrastructureProviderID),
		TenantID:                               util.GetUUIDPtrToStrPtr(&dbVpc.TenantID),
		SiteID:                                 util.GetUUIDPtrToStrPtr(&dbVpc.SiteID),
		Labels:                                 dbVpc.Labels,
		Status:                                 dbVpc.Status,
		NetworkSecurityGroupID:                 dbVpc.NetworkSecurityGroupID,
		NetworkSecurityGroupPropagationDetails: NewAPINetworkSecurityGroupPropagationDetails(dbVpc.NetworkSecurityGroupPropagationDetails),
		SlaacEnabled:                           dbVpc.SlaacEnabled,
		PowerResourceGroup:                     dbVpc.PowerResourceGroup,
		Created:                                dbVpc.Created,
		Updated:                                dbVpc.Updated,
		RequestedVni:                           dbVpc.Vni,
		Vni:                                    dbVpc.ActiveVni,
	}

	if dbVpc.NetworkVirtualizationType != nil {
		apivpc.NetworkVirtualizationType = dbVpc.NetworkVirtualizationType
	}

	if dbVpc.RoutingProfile != nil {
		routingProfile := NormalizeAPIVpcRoutingProfileFromSite(*dbVpc.RoutingProfile)
		apivpc.RoutingProfile = &routingProfile
	}

	if dbVpc.RoutingProfileOverrides != nil {
		apivpc.RoutingProfileOverrides = &APIVpcRoutingProfileOverrides{}
		apivpc.RoutingProfileOverrides.FromDB(dbVpc.RoutingProfileOverrides)
	}

	if includeEffectiveRoutingProfile && dbVpc.EffectiveRoutingProfile != nil {
		apivpc.EffectiveRoutingProfile = &APIVpcEffectiveRoutingProfile{}
		apivpc.EffectiveRoutingProfile.FromDB(dbVpc.EffectiveRoutingProfile)
	}

	if dbVpc.ControllerVpcID != nil {
		apivpc.ControllerVpcID = util.GetUUIDPtrToStrPtr(dbVpc.ControllerVpcID)
	}

	if dbVpc.NVLinkLogicalPartitionID != nil {
		apivpc.NVLinkLogicalPartitionID = util.GetUUIDPtrToStrPtr(dbVpc.NVLinkLogicalPartitionID)
	}

	if dbVpc.NVLinkLogicalPartition != nil {
		apivpc.NVLinkLogicalPartitionSummary = NewAPINVLinkLogicalPartitionSummary(dbVpc.NVLinkLogicalPartition)
	}

	apivpc.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apivpc.StatusHistory = append(apivpc.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	if dbVpc.Site != nil {
		apivpc.Site = NewAPISiteSummary(dbVpc.Site)
	}

	if dbVpc.Tenant != nil {
		apivpc.Tenant = NewAPITenantSummary(dbVpc.Tenant)
	}

	if dbVpc.InfrastructureProvider != nil {
		apivpc.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbVpc.InfrastructureProvider)
	}

	if dbVpc.NetworkSecurityGroup != nil {
		apivpc.NetworkSecurityGroup = NewAPINetworkSecurityGroupSummary(dbVpc.NetworkSecurityGroup)
	}

	return apivpc
}

// APIVpcStats is a data structure to capture information about VPC stats at the API layer
type APIVpcStats struct {
	// Total is the total number of the VPC object in NICo Cloud
	Total int `json:"total"`
	// Pending is the total number of pending VPC object in NICo Cloud
	Pending int `json:"pending"`
	// Provisioning is the total number of provisioning VPC object in NICo Cloud
	Provisioning int `json:"provisioning"`
	// Ready is the total number of ready VPC object in NICo Cloud
	Ready int `json:"ready"`
	// Deleting is the total number of deleting VPC object in NICo Cloud
	Deleting int `json:"deleting"`
	// Error is the total number of error VPC object in NICo Cloud
	Error int `json:"error"`
}

// APIVpcSummary is the data structure to capture API representation of a Vpc Summary
type APIVpcSummary struct {
	// ID is the unique UUID v4 identifier of the VPC in NICo Cloud
	ID string `json:"id"`
	// Name of the Vpc, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// ControllerVpcID is the ID of the corresponding VPC in Site Controller
	ControllerVpcID *string `json:"controllerVpcId"`
	// Network virtualization type is a VPC virtualization type
	NetworkVirtualizationType *string `json:"networkVirtualizationType"`
	// Status is the status of the VPC
	Status string `json:"status"`
}

// NewAPIVpcSummary accepts a DB layer APIVpcSummary object returns an API layer object
func NewAPIVpcSummary(dbVpc *cdbm.Vpc) *APIVpcSummary {
	apiVpcSummary := APIVpcSummary{
		ID:                        dbVpc.ID.String(),
		Name:                      dbVpc.Name,
		NetworkVirtualizationType: dbVpc.NetworkVirtualizationType,
		Status:                    dbVpc.Status,
	}

	if dbVpc.ControllerVpcID != nil {
		apiVpcSummary.ControllerVpcID = util.GetUUIDPtrToStrPtr(dbVpc.ControllerVpcID)
	}

	return &apiVpcSummary
}
