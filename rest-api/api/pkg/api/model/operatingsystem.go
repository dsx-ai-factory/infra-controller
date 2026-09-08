// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	validationErrorInfrastructureProviderIDExpectNil = "Specifying InfrastructureProviderID is currently not supported"
	errMsgInvalidImageSHA                            = "not a valid SHA hash"
	errMsgExactlyOneRootFsField                      = "exactly one of 'rootFsId' and 'rootFsLabel' must be specified"
	errMsgOnlyOneRootFsField                         = "only one of 'rootFsId' and 'rootFsLabel' may be specified"
	errMsgNotEmpty                                   = "cannot be empty"
)

// IsCloudInitFromUserData reports whether non-empty user data is present.
func IsCloudInitFromUserData(userData *string) bool {
	return userData != nil && *userData != ""
}

// APIOperatingSystemIpxeParameter is the API representation of a single iPXE
// template name/value parameter. It is API-owned so the REST contract does not
// depend on the persistence model.
type APIOperatingSystemIpxeParameter struct {
	// Name is the parameter name (used as a variable in the template).
	Name string `json:"name"`
	// Value is the parameter value.
	Value string `json:"value"`
}

// toDBModel converts the receiver to the persistence model.
func (p APIOperatingSystemIpxeParameter) toDBModel() cdbm.OperatingSystemIpxeParameter {
	return cdbm.OperatingSystemIpxeParameter{Name: p.Name, Value: p.Value}
}

// APIOperatingSystemIpxeParameters is a typed list of iPXE parameters carrying
// list-level validation and conversion helpers.
type APIOperatingSystemIpxeParameters []APIOperatingSystemIpxeParameter

// Validate checks every parameter in the list.
func (ps APIOperatingSystemIpxeParameters) Validate() error {
	for i, p := range ps {
		if strings.TrimSpace(p.Name) == "" {
			return validation.Errors{"ipxeTemplateParameters": fmt.Errorf("entry %d: name is required", i)}
		}
	}
	return nil
}

// ToDBModel converts the list to the persistence model, preserving nil.
func (ps APIOperatingSystemIpxeParameters) ToDBModel() []cdbm.OperatingSystemIpxeParameter {
	if ps == nil {
		return nil
	}
	out := make([]cdbm.OperatingSystemIpxeParameter, len(ps))
	for i := range ps {
		out[i] = ps[i].toDBModel()
	}
	return out
}

// ToDBModelPtr converts an optional (pointer) list to the pointer persistence
// model used by update inputs, preserving a nil pointer (field not provided).
func (ps *APIOperatingSystemIpxeParameters) ToDBModelPtr() *[]cdbm.OperatingSystemIpxeParameter {
	if ps == nil {
		return nil
	}
	out := ps.ToDBModel()
	return &out
}

// APIOperatingSystemIpxeArtifact is the API (request) representation of a single
// iPXE artifact (kernel, initrd, ISO, ...). AuthToken is accepted on input but is
// never echoed back: responses use APIOperatingSystemIpxeArtifactResponse, which
// has no AuthToken field.
type APIOperatingSystemIpxeArtifact struct {
	// Name is the artifact name.
	Name string `json:"name"`
	// URL is the original URL for the artifact.
	URL string `json:"url"`
	// SHA is an optional SHA256 checksum.
	SHA *string `json:"sha"`
	// AuthType is an optional auth type (Basic or Bearer).
	AuthType *string `json:"authType"`
	// AuthToken is an optional auth token, only accepted on input.
	AuthToken *string `json:"authToken"`
	// CacheStrategy controls how the artifact is cached on-site.
	CacheStrategy string `json:"cacheStrategy"`
}

// toDBModel converts the receiver to the persistence model.
func (a APIOperatingSystemIpxeArtifact) toDBModel() cdbm.OperatingSystemIpxeArtifact {
	return cdbm.OperatingSystemIpxeArtifact{
		Name:          a.Name,
		URL:           a.URL,
		SHA:           a.SHA,
		AuthType:      a.AuthType,
		AuthToken:     a.AuthToken,
		CacheStrategy: a.CacheStrategy,
	}
}

// APIOperatingSystemIpxeArtifacts is a typed list of iPXE artifacts carrying
// list-level validation and conversion helpers.
type APIOperatingSystemIpxeArtifacts []APIOperatingSystemIpxeArtifact

// Validate checks every artifact in the list.
func (as APIOperatingSystemIpxeArtifacts) Validate() error {
	for i, a := range as {
		if strings.TrimSpace(a.Name) == "" {
			return validation.Errors{"ipxeTemplateArtifacts": fmt.Errorf("entry %d: name is required", i)}
		}
		if strings.TrimSpace(a.URL) == "" {
			return validation.Errors{"ipxeTemplateArtifacts": fmt.Errorf("entry %d (%s): url is required", i, a.Name)}
		}
		if err := validation.Validate(a.URL, is.URL); err != nil {
			return validation.Errors{"ipxeTemplateArtifacts": fmt.Errorf("entry %d (%s): url is not valid: %w", i, a.Name, err)}
		}
		if _, ok := validCacheStrategies[a.CacheStrategy]; !ok {
			return validation.Errors{"ipxeTemplateArtifacts": fmt.Errorf("entry %d (%s): cacheStrategy must be one of CacheAsNeeded, LocalOnly, CachedOnly, RemoteOnly", i, a.Name)}
		}
		if a.AuthType != nil && *a.AuthType != "" {
			at := *a.AuthType
			if at != cdbm.OperatingSystemAuthTypeBasic && at != cdbm.OperatingSystemAuthTypeBearer {
				return validation.Errors{"ipxeTemplateArtifacts": fmt.Errorf("entry %d (%s): authType must be Basic or Bearer", i, a.Name)}
			}
			if a.AuthToken == nil || *a.AuthToken == "" {
				return validation.Errors{"ipxeTemplateArtifacts": fmt.Errorf("entry %d (%s): authToken is required when authType is specified", i, a.Name)}
			}
		}
		if a.AuthToken != nil && *a.AuthToken != "" && (a.AuthType == nil || *a.AuthType == "") {
			return validation.Errors{"ipxeTemplateArtifacts": fmt.Errorf("entry %d (%s): authType must be specified when authToken is provided", i, a.Name)}
		}
	}
	return nil
}

// ToDBModel converts the list to the persistence model, preserving nil.
func (as APIOperatingSystemIpxeArtifacts) ToDBModel() []cdbm.OperatingSystemIpxeArtifact {
	if as == nil {
		return nil
	}
	out := make([]cdbm.OperatingSystemIpxeArtifact, len(as))
	for i := range as {
		out[i] = as[i].toDBModel()
	}
	return out
}

// ToDBModelPtr converts an optional (pointer) list to the pointer persistence
// model used by update inputs, preserving a nil pointer (field not provided).
func (as *APIOperatingSystemIpxeArtifacts) ToDBModelPtr() *[]cdbm.OperatingSystemIpxeArtifact {
	if as == nil {
		return nil
	}
	out := as.ToDBModel()
	return &out
}

// APIOperatingSystemIpxeArtifactResponse is the API response representation of an
// iPXE artifact. It deliberately has no AuthToken field so stored secrets cannot
// be serialized back to clients (structural redaction).
type APIOperatingSystemIpxeArtifactResponse struct {
	// Name is the artifact name.
	Name string `json:"name"`
	// URL is the original URL for the artifact.
	URL string `json:"url"`
	// SHA is an optional SHA256 checksum.
	SHA *string `json:"sha"`
	// AuthType is an optional auth type (Basic or Bearer).
	AuthType *string `json:"authType"`
	// CacheStrategy controls how the artifact is cached on-site.
	CacheStrategy string `json:"cacheStrategy"`
}

// newAPIIpxeParametersFromDB converts persisted parameters to the API response
// representation, preserving nil.
func newAPIIpxeParametersFromDB(params []cdbm.OperatingSystemIpxeParameter) []APIOperatingSystemIpxeParameter {
	if params == nil {
		return nil
	}
	out := make([]APIOperatingSystemIpxeParameter, len(params))
	for i, p := range params {
		out[i] = APIOperatingSystemIpxeParameter{Name: p.Name, Value: p.Value}
	}
	return out
}

// newAPIIpxeArtifactResponsesFromDB converts persisted artifacts to the API
// response representation, preserving nil. AuthToken is dropped structurally: the
// response type has no such field.
func newAPIIpxeArtifactResponsesFromDB(artifacts []cdbm.OperatingSystemIpxeArtifact) []APIOperatingSystemIpxeArtifactResponse {
	if artifacts == nil {
		return nil
	}
	out := make([]APIOperatingSystemIpxeArtifactResponse, len(artifacts))
	for i, a := range artifacts {
		out[i] = APIOperatingSystemIpxeArtifactResponse{
			Name:          a.Name,
			URL:           a.URL,
			SHA:           a.SHA,
			AuthType:      a.AuthType,
			CacheStrategy: a.CacheStrategy,
		}
	}
	return out
}

// APIOperatingSystemCreateRequest is the data structure to capture user request to create a new OperatingSystem
type APIOperatingSystemCreateRequest struct {
	// Name is the name of the OperatingSystem
	Name string `json:"name"`
	// Description is the description of the OperatingSystem
	Description *string `json:"description"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider creating the Operating System
	InfrastructureProviderID *string `json:"infrastructureProviderId"`
	// SiteIDs is a list of Site objects
	SiteIDs []string `json:"siteIds"`
	// TenantID is the ID of the Tenant creating the Operating System
	TenantID *string `json:"tenantId"`
	// IpxeScript is the iPXE script for the Operating System
	IpxeScript *string `json:"ipxeScript"`
	// ImageURL is the image path for the Operating System
	ImageURL *string `json:"imageUrl"`
	// ImageSHA is SHA for the Operating System image type
	ImageSHA *string `json:"imageSha"`
	// ImageAuthType is auth type for the Operating System type
	ImageAuthType *string `json:"imageAuthType"`
	// ImageAuthToken is auth token for for the Operating System image type
	ImageAuthToken *string `json:"imageAuthToken"`
	// ImageDisk is disk for the Operating System image type
	ImageDisk *string `json:"imageDisk"`
	// RootFsID is root fs id for the Operating System image type
	RootFsID *string `json:"rootFsId"`
	// RootFsLabel is root fs label for the Operating System image type
	RootFsLabel *string `json:"rootFsLabel"`
	// PhoneHomeEnabled is the flag to allow enable phone home
	PhoneHomeEnabled *bool `json:"phoneHomeEnabled"`
	// UserData is the user data for the Operating System
	UserData *string `json:"userData"`
	// IsCloudInit is deprecated and ignored; derived from value of userData.
	IsCloudInit bool `json:"isCloudInit"`
	// AllowOverride indicates if overrides are allowed
	AllowOverride bool `json:"allowOverride"`
	// EnableBlockStorage indicates whether the Operating System image will be stored remotely via block storage
	EnableBlockStorage bool `json:"enableBlockStorage"`
	// IpxeTemplateId is the ID of the iPXE template to use (alternative to a raw ipxeScript)
	IpxeTemplateId *string `json:"ipxeTemplateId"`
	// IpxeTemplateParameters are the parameters to pass to the iPXE template
	IpxeTemplateParameters APIOperatingSystemIpxeParameters `json:"ipxeTemplateParameters"`
	// IpxeTemplateArtifacts are the artifacts (kernel, initrd, ISO, ...) for the iPXE OS definition
	IpxeTemplateArtifacts APIOperatingSystemIpxeArtifacts `json:"ipxeTemplateArtifacts"`
}

// GetOperatingSystemType returns the OperatingSystem type inferred from the
// create request's source fields (`IpxeScript`, `IpxeTemplateId`, or neither).
func (oscr *APIOperatingSystemCreateRequest) GetOperatingSystemType() string {
	if oscr.IpxeScript != nil {
		return cdbm.OperatingSystemTypeIPXE
	}
	if oscr.IpxeTemplateId != nil {
		return cdbm.OperatingSystemTypeTemplatedIPXE
	}
	return cdbm.OperatingSystemTypeImage
}

// Validate ensure the values passed in request are acceptable
func (oscr *APIOperatingSystemCreateRequest) Validate() error {
	var err error
	err = validation.ValidateStruct(oscr,
		validation.Field(&oscr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&oscr.InfrastructureProviderID,
			// infrastructure provider id must be nil
			validation.Nil.Error(validationErrorInfrastructureProviderIDExpectNil)),
		validation.Field(&oscr.UserData,
			validation.When(oscr.UserData != nil,
				validation.Length(0, util.MaxUserDataBytes).Error(validationErrorUserDataLength)),
		),
	)
	if err != nil {
		return err
	}

	if oscr.IpxeTemplateId != nil {
		if strings.TrimSpace(*oscr.IpxeTemplateId) == "" {
			return validation.Errors{
				"ipxeTemplateId": errors.New("must not be empty"),
			}
		}
		if _, err := uuid.Parse(*oscr.IpxeTemplateId); err != nil {
			return validation.Errors{
				"ipxeTemplateId": errors.New("must be a valid UUID"),
			}
		}
	}

	if oscr.IpxeScript != nil && oscr.IpxeTemplateId != nil {
		return validation.Errors{
			"ipxeTemplateId": errors.New("ipxeScript and ipxeTemplateId are mutually exclusive"),
		}
	}

	// Make sure siteIds only required in case of image is OS based
	if oscr.IpxeScript != nil && len(oscr.SiteIDs) > 0 {
		return validation.Errors{
			"siteIds": errors.New("cannot be specified for iPXE based Operating Systems"),
		}
	}

	if (oscr.IpxeScript != nil || oscr.IpxeTemplateId != nil) && oscr.ImageURL != nil {
		return validation.Errors{
			"imageUrl": errors.New("cannot be specified for iPXE based Operating Systems"),
		}
	} else if oscr.IpxeScript == nil && oscr.IpxeTemplateId == nil && oscr.ImageURL == nil {
		return validation.Errors{
			validationCommonErrorField: errors.New("one of imageURL, ipxeScript, or ipxeTemplateId must be specified"),
		}
	}

	if oscr.EnableBlockStorage {
		return validation.Errors{
			"enableBlockStorage": errors.New("Enabling block storage is not supported at this time"),
		}
	}

	// iPXE template definition fields are only valid for Templated iPXE Operating Systems.
	if oscr.IpxeTemplateId == nil {
		if len(oscr.IpxeTemplateParameters) > 0 {
			return validation.Errors{
				"ipxeTemplateParameters": errors.New("can only be specified for Templated iPXE Operating Systems"),
			}
		}
		if len(oscr.IpxeTemplateArtifacts) > 0 {
			return validation.Errors{
				"ipxeTemplateArtifacts": errors.New("can only be specified for Templated iPXE Operating Systems"),
			}
		}
	}

	// Templated iPXE is validated in full by validateTemplatedIpxeOS (including its
	// own image-field/site-id rules), so it returns early and never falls through to
	// the image checks below. Raw iPXE and Image types have no further type-specific
	// pre-checks here.
	if oscr.IpxeTemplateId != nil {
		return oscr.validateTemplatedIpxeOS()
	}

	if oscr.ImageURL != nil {
		err = validation.ValidateStruct(oscr,
			validation.Field(&oscr.ImageURL, is.URL),
			validation.Field(&oscr.ImageSHA,
				validation.Required.Error(validationErrorValueRequired),
				validation.When(oscr.ImageSHA != nil, validation.Match(util.ShaHashRegex).Error(errMsgInvalidImageSHA))),
			validation.Field(&oscr.ImageAuthType,
				validation.When(!(util.IsNilOrEmptyStrPtr(oscr.ImageAuthType)) && util.IsNilOrEmptyStrPtr(oscr.ImageAuthToken),
					validation.Required.Error("imageAuthType cannot be specified if imageAuthToken is not specified")),
				validation.When(!(util.IsNilOrEmptyStrPtr(oscr.ImageAuthType)),
					validation.In(cdbm.OperatingSystemAuthTypeBasic, cdbm.OperatingSystemAuthTypeBearer).Error("imageAuthType must be Basic or Bearer")),
			),
			validation.Field(&oscr.ImageAuthToken,
				validation.When(!(util.IsNilOrEmptyStrPtr(oscr.ImageAuthToken)) && util.IsNilOrEmptyStrPtr(oscr.ImageAuthType), validation.Required.Error("imageAuthType must be specified when imageAuthToken is specified"))),
			validation.Field(&oscr.ImageDisk,
				validation.When(!(util.IsNilOrEmptyStrPtr(oscr.ImageDisk)), validation.By(util.ValidateDiskImagePath))),
			validation.Field(&oscr.RootFsID,
				validation.When(util.IsNilOrEmptyStrPtr(oscr.RootFsLabel), validation.Required.Error(errMsgExactlyOneRootFsField)),
				validation.When(!(util.IsNilOrEmptyStrPtr(oscr.RootFsLabel)), validation.Empty.Error(errMsgExactlyOneRootFsField))),
			validation.Field(&oscr.RootFsLabel,
				validation.When(util.IsNilOrEmptyStrPtr(oscr.RootFsID), validation.Required.Error(errMsgExactlyOneRootFsField)),
				validation.When(!(util.IsNilOrEmptyStrPtr(oscr.RootFsID)), validation.Empty.Error(errMsgExactlyOneRootFsField))),
		)
		if len(oscr.SiteIDs) == 0 {
			return validation.Errors{
				"siteIds": errors.New("must be specified for image based Operating Systems"),
			}
		} else if len(oscr.SiteIDs) > 1 {
			return validation.Errors{
				"siteIds": errors.New("must specify a single Site ID. Creating Image based Operating System on more than one Site is not supported"),
			}
		}
	} else {
		err = validation.ValidateStruct(oscr,
			validation.Field(&oscr.SiteIDs,
				validation.Nil.Error("siteIds cannot be specified if imageURL is not specified")),
			validation.Field(&oscr.ImageSHA,
				validation.Nil.Error("imageSHA cannot be specified if imageURL is not specified")),
			validation.Field(&oscr.ImageAuthType,
				validation.Nil.Error("imageAuthType cannot be specified if imageURL is not specified")),
			validation.Field(&oscr.ImageAuthToken,
				validation.Nil.Error("imageAuthToken cannot be specified if imageURL is not specified")),
			validation.Field(&oscr.ImageDisk,
				validation.Nil.Error("imageDisk cannot be specified if imageURL is not specified")),
			validation.Field(&oscr.RootFsID,
				validation.Nil.Error("rootFsID cannot be specified if imageURL is not specified")),
			validation.Field(&oscr.RootFsLabel,
				validation.Nil.Error("rootFsLabel cannot be specified if imageURL is not specified")),
		)
	}

	if oscr.IpxeScript != nil {
		err = validation.ValidateStruct(oscr,
			validation.Field(&oscr.IpxeScript,
				validation.Required.Error(validationErrorValueRequired)),
			validation.Field(&oscr.EnableBlockStorage,
				validation.Empty.Error("enableBlockStorage must be false if ipxeScript is specified")),
		)
	}

	return err
}

func (oscr *APIOperatingSystemCreateRequest) ValidateAndSetUserData(phonehomeUrl string) error {
	// This is a create.  If phone-home is unspecified or false,
	// then any user-data content is acceptable, so do nothing and return.
	if oscr.PhoneHomeEnabled == nil || !*oscr.PhoneHomeEnabled {
		return nil
	}

	// At this point, we know phone-home has been requested,
	// so default to empty user-data if nothing was passed in
	if oscr.UserData == nil || *oscr.UserData == "" {
		oscr.UserData = cutil.GetPtr("{}")
	}

	userDataMap := &yaml.Node{}

	var documentRoot *yaml.Node

	isUserDataValidYAML := false

	err := yaml.Unmarshal([]byte(*oscr.UserData), userDataMap)
	if err == nil {

		// We have a slightly more restrictive view of what
		// counts as valid YAML.
		if len(userDataMap.Content) > 0 {
			documentRoot = userDataMap.Content[0]
			if documentRoot.Kind == yaml.MappingNode {
				isUserDataValidYAML = true
			}
		}
	}

	if !isUserDataValidYAML {
		return validation.Errors{
			"userData": errors.New("userData specified in request must be valid cloud-init YAML to enable phone home"),
		}
	}

	if err := util.InsertPhoneHomeIntoUserData(documentRoot, phonehomeUrl); err != nil {
		return validation.Errors{
			"userData": errors.New("failed to update userData with phone home config"),
		}
	}

	byteUserData, err := util.MarshalUserData(userDataMap)
	if err != nil {
		return validation.Errors{
			"userData": errors.New("failed to re-construct userData after processing phone home config"),
		}
	}

	// Render it back out.
	oscr.UserData = cutil.GetPtr(string(byteUserData))

	return util.ValidateEffectiveUserData(oscr.UserData)
}

// ToImageProto builds the workflow request that asks a Site to create the
// OS image for this API request. `os` is the just-persisted DB record;
// its `ToImageAttributesProto(tenantOrg)` is the source of every wire
// field because the handler has already merged the request fields into
// the entity via the DAO before this method runs. `tenantOrg` is a
// side input — it lives on the request's resolved Tenant rather than
// on the entity, and the handler passes it through.
//
// The method trusts that the request has already been Validated (and
// that ValidateAndSetUserData has run) and that the handler has
// performed the cross-context checks Validate cannot see — most
// importantly that the OS is image-typed, since
// `ToImageAttributesProto` dereferences `ImageURL` and `ImageSHA`.
// For iPXE-typed records there is no Site-side image workflow, so
// this method should not be called.
func (oscr *APIOperatingSystemCreateRequest) ToImageProto(os *cdbm.OperatingSystem, tenantOrg string) *corev1.OsImageAttributes {
	return os.ToImageAttributesProto(tenantOrg)
}

// APIOperatingSystemUpdateRequest is the data structure to capture user request to update an OperatingSystem
type APIOperatingSystemUpdateRequest struct {
	// Name is the name of the OperatingSystem
	Name *string `json:"name"`
	// Description is the description of the Operating System
	Description *string `json:"description"`
	// IpxeScript is the ipxe script for the Operating System
	IpxeScript *string `json:"ipxeScript"`
	// ImageURL is the image path for the Operating System
	ImageURL *string `json:"imageUrl"`
	// ImageSHA is SHA for the Operating System image type
	ImageSHA *string `json:"imageSha"`
	// ImageAuthType is auth type for the Operating System type
	ImageAuthType *string `json:"imageAuthType"`
	// ImageAuthToken is auth token for for the Operating System image type
	ImageAuthToken *string `json:"imageAuthToken"`
	// ImageDisk is disk for the Operating System image type
	ImageDisk *string `json:"imageDisk"`
	// RootFsID is root fs id for the Operating System image type
	RootFsID *string `json:"rootFsId"`
	// RootFsLabel is root fs label for the Operating System image type
	RootFsLabel *string `json:"rootFsLabel"`
	// PhoneHomeEnabled is the flag to allow enable phone home
	PhoneHomeEnabled *bool `json:"phoneHomeEnabled"`
	// UserData is the user data for the Operating System
	UserData *string `json:"userData"`
	// IsCloudInit is deprecated and ignored; derived from value of userData.
	IsCloudInit *bool `json:"isCloudInit"`
	// AllowOverride indicates if overrides are allowed
	AllowOverride *bool `json:"allowOverride"`
	// IsActive indicates if the Operating System is active
	IsActive *bool `json:"isActive"`
	// DeactivationNote is the deactivation note if any
	DeactivationNote *string `json:"deactivationNote"`
	// IpxeTemplateId is the ID of the iPXE template to use (alternative to a raw ipxeScript)
	IpxeTemplateId *string `json:"ipxeTemplateId"`
	// IpxeTemplateParameters are the parameters to pass to the iPXE template
	IpxeTemplateParameters *APIOperatingSystemIpxeParameters `json:"ipxeTemplateParameters"`
	// IpxeTemplateArtifacts are the artifacts (kernel, initrd, ISO, ...) for the iPXE OS definition
	IpxeTemplateArtifacts *APIOperatingSystemIpxeArtifacts `json:"ipxeTemplateArtifacts"`
}

// Validate ensure the values passed in request are acceptable
func (osur *APIOperatingSystemUpdateRequest) Validate(existingOS *cdbm.OperatingSystem) error {
	err := validation.ValidateStruct(osur,
		validation.Field(&osur.Name,
			validation.When(osur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(osur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(osur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&osur.UserData,
			validation.When(osur.UserData != nil,
				validation.Length(0, util.MaxUserDataBytes).Error(validationErrorUserDataLength)),
		),
	)
	if err != nil {
		return err
	}

	// reject attempts to change active status if already in desired state:
	if osur.IsActive != nil {
		if *osur.IsActive && existingOS.IsActive {
			return validation.Errors{
				"isActive": errors.New("Operating System is already active"),
			}
		} else if !*osur.IsActive && !existingOS.IsActive {
			return validation.Errors{
				"isActive": errors.New("Operating System is already deactivated"),
			}
		} else if *osur.IsActive && osur.DeactivationNote != nil {
			return validation.Errors{
				"deactivationNote": errors.New("cannot provide Deactivation Note when activating Operating System"),
			}
		}
	} else if existingOS.IsActive && osur.DeactivationNote != nil {
		return validation.Errors{
			"deactivationNote": errors.New("cannot change Deactivation Note on an active Operating System"),
		}
	}

	// iPXE script and template are mutually exclusive in a single request.
	if osur.IpxeScript != nil && osur.IpxeTemplateId != nil {
		return validation.Errors{
			"ipxeTemplateId": errors.New("ipxeScript and ipxeTemplateId are mutually exclusive"),
		}
	}
	if osur.IpxeTemplateId != nil && strings.TrimSpace(*osur.IpxeTemplateId) == "" {
		return validation.Errors{
			"ipxeTemplateId": errors.New("must not be empty"),
		}
	}
	if osur.IpxeTemplateId != nil && osur.ImageURL != nil {
		return validation.Errors{
			"imageUrl": errors.New("cannot be specified for iPXE based Operating Systems"),
		}
	}

	// Reject cross-type field assignments based on the existing OS type and
	// validate iPXE template definition fields (Templated iPXE only).
	switch existingOS.Type {
	case cdbm.OperatingSystemTypeImage:
		if osur.IpxeTemplateId != nil {
			return validation.Errors{"ipxeTemplateId": errors.New("unable to set iPXE template for image based Operating System")}
		}
	case cdbm.OperatingSystemTypeIPXE:
		if osur.IpxeTemplateId != nil {
			return validation.Errors{"ipxeTemplateId": errors.New("unable to set iPXE template for raw iPXE Operating System")}
		}
	case cdbm.OperatingSystemTypeTemplatedIPXE:
		if osur.IpxeScript != nil {
			return validation.Errors{"ipxeScript": errors.New("unable to set iPXE script for templated iPXE Operating System")}
		}
		if osur.ImageURL != nil {
			return validation.Errors{"imageUrl": errors.New("unable to set image URL for iPXE based Operating System")}
		}
		if osur.IpxeTemplateId != nil {
			if strings.TrimSpace(*osur.IpxeTemplateId) == "" {
				return validation.Errors{"ipxeTemplateId": errors.New("must not be empty")}
			}
			if _, err := uuid.Parse(*osur.IpxeTemplateId); err != nil {
				return validation.Errors{"ipxeTemplateId": errors.New("must be a valid UUID")}
			}
		}
	}
	if existingOS.Type == cdbm.OperatingSystemTypeTemplatedIPXE {
		if osur.IpxeTemplateParameters != nil {
			if verr := osur.IpxeTemplateParameters.Validate(); verr != nil {
				return verr
			}
		}
		if osur.IpxeTemplateArtifacts != nil {
			if verr := osur.IpxeTemplateArtifacts.Validate(); verr != nil {
				return verr
			}
		}
	} else {
		if osur.IpxeTemplateParameters != nil {
			return validation.Errors{"ipxeTemplateParameters": errors.New("can only be specified for Templated iPXE Operating Systems")}
		}
		if osur.IpxeTemplateArtifacts != nil {
			return validation.Errors{"ipxeTemplateArtifacts": errors.New("can only be specified for Templated iPXE Operating Systems")}
		}
	}

	if osur.IpxeScript != nil && osur.ImageURL != nil {
		return validation.Errors{
			"imageUrl": errors.New("cannot be specified for iPXE based Operating Systems"),
		}
	}

	isImageBased := existingOS.Type == cdbm.OperatingSystemTypeImage

	// verify if os was not created as image-based, reject the update if imageURL provided
	if !isImageBased && osur.ImageURL != nil {
		return validation.Errors{
			"imageUrl": errors.New("unable to set image URL for non-image based Operating System"),
		}
	} else if isImageBased && osur.IpxeScript != nil {
		return validation.Errors{
			"ipxeScript": errors.New("unable to set iPXE script for image based Operating System"),
		}
	}

	if !util.IsNilOrEmptyStrPtr(osur.RootFsID) && osur.RootFsLabel == nil && !util.IsNilOrEmptyStrPtr(existingOS.RootFsLabel) {
		return validation.Errors{
			"rootFsId": errors.New("unable to set root filesystem id for Operating System with root filesystem label specified"),
		}
	} else if isImageBased && util.IsEmptyStrPtr(osur.RootFsID) && ((osur.RootFsLabel == nil && util.IsNilOrEmptyStrPtr(existingOS.RootFsLabel)) || util.IsEmptyStrPtr(osur.RootFsLabel)) {
		return validation.Errors{
			"rootFsId": errors.New("unable to clear root filesystem id for Operating System without specifying root filesystem label"),
		}
	} else if isImageBased && util.IsEmptyStrPtr(osur.RootFsLabel) && util.IsNilOrEmptyStrPtr(existingOS.RootFsID) && osur.RootFsID == nil {
		return validation.Errors{
			"rootFsLabel": errors.New("unable to clear root filesystem label for Operating System without specifying root filesystem id"),
		}
	} else if osur.RootFsID == nil && !util.IsNilOrEmptyStrPtr(osur.RootFsLabel) && !util.IsNilOrEmptyStrPtr(existingOS.RootFsID) {
		return validation.Errors{
			"rootFsLabel": errors.New("unable to set root filesystem label for Operating System with root filesystem id specified"),
		}
	}

	// imageUrl and imageSha identify the underlying image content and are
	// immutable after creation. The Site treats source_url/digest as
	// read-only and rejects any change during sync with
	// "os_image update read-only attributes changed" (see api-core
	// update_os_image); rejecting the change here gives the caller a clear,
	// actionable error up front instead of a cryptic site-sync failure.
	// Re-sending the current value is accepted as a no-op so clients that
	// echo back the full resource still succeed.
	if osur.ImageURL != nil && !util.IsNilOrEmptyStrPtr(existingOS.ImageURL) && *osur.ImageURL != *existingOS.ImageURL {
		return validation.Errors{
			"imageUrl": errors.New("imageUrl cannot be changed after creation; create a new Operating System to use a different image"),
		}
	}
	if osur.ImageSHA != nil && !util.IsNilOrEmptyStrPtr(existingOS.ImageSHA) && *osur.ImageSHA != *existingOS.ImageSHA {
		return validation.Errors{
			"imageSha": errors.New("imageSha cannot be changed after creation; create a new Operating System to use a different image"),
		}
	}

	if isImageBased {
		// Image auth credentials can be updated on their own — the caller
		// does not have to re-send the immutable imageUrl/imageSha to
		// rotate a token. Those fields, when present, are validated for
		// format only; the immutability guard above has already rejected
		// any attempt to change them.
		//
		// TODO: rootFsId/rootFsLabel are also read-only on the Site (see
		// api-core update_os_image), so changing them still fails at sync.
		// Left as-is to keep this fix scoped to the reported
		// imageUrl/imageSha/imageAuthToken behavior.
		err = validation.ValidateStruct(osur,
			validation.Field(&osur.ImageURL,
				validation.When(osur.ImageURL != nil, is.URL)),
			validation.Field(&osur.ImageSHA,
				validation.When(osur.ImageSHA != nil, validation.Match(util.ShaHashRegex).Error(errMsgInvalidImageSHA))),
			validation.Field(&osur.ImageAuthType,
				validation.When(!(util.IsNilOrEmptyStrPtr(osur.ImageAuthType)) && util.IsNilOrEmptyStrPtr(osur.ImageAuthToken), validation.Required.Error("imageAuthType cannot be specified if imageAuthToken is not specified")),
				validation.When(!(util.IsNilOrEmptyStrPtr(osur.ImageAuthType)),
					validation.In(cdbm.OperatingSystemAuthTypeBasic, cdbm.OperatingSystemAuthTypeBearer).Error("imageAuthType must be Basic or Bearer")),
			),
			validation.Field(&osur.ImageAuthToken,
				validation.When(!(util.IsNilOrEmptyStrPtr(osur.ImageAuthToken)) && util.IsNilOrEmptyStrPtr(osur.ImageAuthType), validation.Required.Error("imageAuthType must be specified when imageAuthToken is specified"))),
			validation.Field(&osur.ImageDisk,
				validation.When(!(util.IsEmptyStrPtr(osur.ImageDisk)), validation.By(util.ValidateDiskImagePath))),
			validation.Field(&osur.RootFsID,
				validation.When(!(util.IsNilOrEmptyStrPtr(osur.RootFsLabel)), validation.Empty.Error(errMsgOnlyOneRootFsField))),
			validation.Field(&osur.RootFsLabel,
				validation.When(!(util.IsNilOrEmptyStrPtr(osur.RootFsID)), validation.Empty.Error(errMsgOnlyOneRootFsField))),
		)
	} else {
		err = validation.ValidateStruct(osur,
			validation.Field(&osur.ImageSHA,
				validation.Nil.Error("imageSHA cannot be specified if imageURL is not specified")),
			validation.Field(&osur.ImageAuthType,
				validation.Nil.Error("imageAuthType cannot be specified if imageURL is not specified")),
			validation.Field(&osur.ImageAuthToken,
				validation.Nil.Error("imageAuthToken cannot be specified if imageURL is not specified")),
			validation.Field(&osur.ImageDisk,
				validation.Nil.Error("imageDisk cannot be specified if imageURL is not specified")),
			validation.Field(&osur.RootFsID,
				validation.Nil.Error("rootFsID cannot be specified if imageURL is not specified")),
			validation.Field(&osur.RootFsLabel,
				validation.Nil.Error("rootFsLabel cannot be specified if imageURL is not specified")),
		)
	}

	if osur.IpxeScript != nil {
		err = validation.ValidateStruct(osur,
			validation.Field(&osur.IpxeScript,
				validation.Required.Error(validationErrorValueRequired)),
		)
	}
	return err
}

func (osur *APIOperatingSystemUpdateRequest) ValidateAndSetUserData(phonehomeUrl string, existingOS *cdbm.OperatingSystem) error {

	mergedPhoneHomeEnabled := osur.PhoneHomeEnabled
	mergedUserData := osur.UserData

	if mergedUserData == nil {
		mergedUserData = existingOS.UserData
	}

	// If phone-home was enabled, it can be removed by key instead of by URL.
	//
	// Ownership is a property of the stored blob. A request that supplies its own
	// user-data replaces that blob outright, so the flag says nothing about the
	// document actually being edited and removal there stays URL-matched.
	nicoAuthoredPhoneHome := existingOS.PhoneHomeEnabled && osur.UserData == nil

	if mergedPhoneHomeEnabled == nil {
		mergedPhoneHomeEnabled = &existingOS.PhoneHomeEnabled

		// If phone-home has never been enabled, then
		// any user-data content was always acceptable,
		// so there is nothing to rewrite. The stored blob still reaches the
		// Site, so its size is checked before returning.
		if !*mergedPhoneHomeEnabled {
			return util.ValidateEffectiveUserData(mergedUserData)
		}
	}

	// If phone-home is being disabled, but there
	// isn't any user-data to begin with, there's nothing to do.
	if !*mergedPhoneHomeEnabled && (mergedUserData == nil || *mergedUserData == "") {
		return nil
	}

	if mergedUserData == nil || *mergedUserData == "" {
		// A request to disable that had no user-data would
		// have returned already; so, If we're here, then we
		// have a request to enable that is totally missing
		// user data, so default it.
		mergedUserData = cutil.GetPtr("{}")
	}

	userDataMap := &yaml.Node{}

	var documentRoot *yaml.Node

	isUserDataValidYAML := false

	err := yaml.Unmarshal([]byte(*mergedUserData), userDataMap)
	if err == nil {

		// We have a slightly more restrictive view of what
		// counts as valid YAML.
		if len(userDataMap.Content) > 0 {
			documentRoot = userDataMap.Content[0]
			if documentRoot.Kind == yaml.MappingNode {
				isUserDataValidYAML = true
			}
		}
	}

	if *mergedPhoneHomeEnabled {
		if !isUserDataValidYAML {
			return validation.Errors{
				"userData": errors.New("userData specified in request must be valid cloud-init YAML to enable phone home"),
			}
		}

		// If some user-data was sent in,
		// insert our phone-home block into the
		// existing data.
		if err := util.InsertPhoneHomeIntoUserData(documentRoot, phonehomeUrl); err != nil {
			return validation.Errors{
				"userData": errors.New("failed to update userData with phone home config"),
			}
		}
	} else if isUserDataValidYAML {
		// If phone-home is being disabled,
		// We still have to make sure we don't try to remove from invalid yaml,
		// but the UI will always send false if phone-home is unchecked,
		// so we want to do this check silently and not alert people who
		// are using non-YAML user-data.

		// Our own block is removed by key, because the URL frozen into it may
		// predate a change to site.phoneHomeUrl.
		var phoneHomeURLFilter *string = nil
		if !nicoAuthoredPhoneHome {
			phoneHomeURLFilter = &phonehomeUrl
		}

		if err := util.RemovePhoneHomeFromUserData(documentRoot, phoneHomeURLFilter); err != nil {
			return validation.Errors{
				"userData": errors.New("failed to remove phone home config from userData"),
			}
		}
	} else {
		// If we've arrived here, then phone-home is being disabled,
		// and the user-data is NOT valid YAML,
		// but we don't care, so don't touch user-data.
		// Its size still applies, valid YAML or not.
		return util.ValidateEffectiveUserData(mergedUserData)
	}

	if len(documentRoot.Content) == 0 {
		// If we've arrived here, then the original user-data
		// was valid, but phone-home has been disabled, and the
		// phone-home block was the only thing in the original YAML,
		// so just blank the DB field.
		osur.UserData = cutil.GetPtr("")
		return nil
	}

	// Render any data that still exists.
	byteUserData, err := util.MarshalUserData(userDataMap)
	if err != nil {
		return validation.Errors{
			"userData": errors.New("failed to re-construct userData after processing phone home config"),
		}
	}

	// Set it in the request.
	osur.UserData = cutil.GetPtr(string(byteUserData))

	return util.ValidateEffectiveUserData(osur.UserData)
}

// ToImageProto builds the workflow request that asks a Site to update the
// OS image for this API request. `uos` is the post-update DB record;
// its `ToImageAttributesProto(tenantOrg)` is the source of every wire
// field, so unchanged fields stay populated and updated fields reflect
// the just-persisted state. `tenantOrg` is a side input — it lives on
// the request's resolved Tenant rather than on the entity, and the
// handler passes it through.
//
// The same `OsImageAttributes` proto is used for both create and
// update workflows on the Site side, so this method delegates to the
// entity-level method rather than building a distinct wire shape. The
// request-level method exists so call sites stay uniform with the
// rest of the layered convention.
//
// As with the create variant, the method trusts that the request has
// been Validated (Validate + ValidateAndSetUserData) and that the
// handler has confirmed the OS is image-typed before this is called;
// `ToImageAttributesProto` dereferences `ImageURL` and `ImageSHA`.
func (osur *APIOperatingSystemUpdateRequest) ToImageProto(uos *cdbm.OperatingSystem, tenantOrg string) *corev1.OsImageAttributes {
	return uos.ToImageAttributesProto(tenantOrg)
}

// APIOperatingSystem is the data structure to capture API representation of an OS
type APIOperatingSystem struct {
	// ID is the unique UUID v4 identifier for the Operating System
	ID string `json:"id"`
	// Name is the name of the Operating System
	Name string `json:"name"`
	// Description is the description of the Operating System
	Description *string `json:"description"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider creating the OS
	InfrastructureProviderID *string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// TenantID is the ID of the tenant creating the Operating System
	TenantID *string `json:"tenantId"`
	// Tenant is the summary of the Tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// Type is which type of Operating System
	Type *string `json:"type"`
	// ImageUrl is url path for the Operating System
	ImageURL *string `json:"imageUrl"`
	// ImageSHA is SHA for the Operating System image type
	ImageSHA *string `json:"imageSha"`
	// ImageAuthType is auth type for the Operating System type
	ImageAuthType *string `json:"imageAuthType"`
	// ImageAuthToken is auth token for for the Operating System image type
	ImageAuthToken *string `json:"imageAuthToken"`
	// ImageDisk is disk for the Operating System image type
	ImageDisk *string `json:"imageDisk"`
	// RootFsID is root fs id for the Operating System image type
	RootFsID *string `json:"rootFsId"`
	// RootFsLabel is root fs id for the Operating System image type
	RootFsLabel *string `json:"rootFsLabel"`
	// IpxeScript is the ipxe ocript for the Operating System
	IpxeScript *string `json:"ipxeScript"`
	// IpxeTemplateId is the ID of the iPXE template used by this Operating System
	IpxeTemplateId *string `json:"ipxeTemplateId"`
	// IpxeTemplateParameters are the parameters passed to the iPXE template
	IpxeTemplateParameters []APIOperatingSystemIpxeParameter `json:"ipxeTemplateParameters"`
	// IpxeTemplateArtifacts are the artifacts (kernel, initrd, ISO, ...) for the iPXE OS definition.
	// The response artifact type has no authToken field, so stored secrets are never echoed back.
	IpxeTemplateArtifacts []APIOperatingSystemIpxeArtifactResponse `json:"ipxeTemplateArtifacts"`
	// PhoneHomeEnabled is an attribute which is specified by user if Operating System needs to be enabled for phone home or not
	PhoneHomeEnabled bool `json:"phoneHomeEnabled"`
	// UserData is the user data for the Operating System
	UserData *string `json:"userData"`
	// IsCloudInit indicates if the Operating System is cloud-init based -- convenience field that is only returned in API response
	// and is derived from value of userData
	IsCloudInit bool `json:"isCloudInit"`
	// AllowOverride indicates if overrides are allowed
	AllowOverride bool `json:"allowOverride"`
	// EnableBlockStorage indicates whether the Operating System image will be stored remotely via block storage
	EnableBlockStorage bool `json:"enableBlockStorage"`
	// IsActive indicates if the Operating System is active
	IsActive bool `json:"isActive"`
	// DeactivationNote is the deactivation note if any
	DeactivationNote *string `json:"deactivationNote"`
	// Status is the status of the Operating System
	Status string `json:"status"`
	// StatusHistory is the history of statuses for the Operating System
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// SiteAssociations is the list of Sites associated with the Operating System
	SiteAssociations []APIOperatingSystemSiteAssociation `json:"siteAssociations"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIOperatingSystem accepts a DB layer object and returns an API layer object
func NewAPIOperatingSystem(dbOS *cdbm.OperatingSystem, dbsds []cdbm.StatusDetail, ossas []cdbm.OperatingSystemSiteAssociation, sttsmap map[uuid.UUID]*cdbm.TenantSite) *APIOperatingSystem {
	apiOperatingSystem := APIOperatingSystem{
		ID:                 dbOS.ID.String(),
		Name:               dbOS.Name,
		Description:        dbOS.Description,
		Type:               &dbOS.Type,
		ImageURL:           dbOS.ImageURL,
		ImageSHA:           dbOS.ImageSHA,
		ImageAuthType:      dbOS.ImageAuthType,
		ImageAuthToken:     dbOS.ImageAuthToken,
		ImageDisk:          dbOS.ImageDisk,
		RootFsID:           dbOS.RootFsID,
		RootFsLabel:        dbOS.RootFsLabel,
		IpxeScript:         dbOS.IpxeScript,
		IpxeTemplateId:     dbOS.IpxeTemplateId,
		PhoneHomeEnabled:   dbOS.PhoneHomeEnabled,
		UserData:           dbOS.UserData,
		IsCloudInit:        IsCloudInitFromUserData(dbOS.UserData),
		AllowOverride:      dbOS.AllowOverride,
		EnableBlockStorage: dbOS.EnableBlockStorage,
		IsActive:           dbOS.IsActive,
		DeactivationNote:   dbOS.DeactivationNote,
		Status:             dbOS.Status,
		Created:            dbOS.Created,
		Updated:            dbOS.Updated,
	}
	apiOperatingSystem.IpxeTemplateParameters = newAPIIpxeParametersFromDB(dbOS.IpxeTemplateParameters)
	// The response artifact type has no AuthToken field, so stored secrets are
	// dropped structurally rather than by manual redaction.
	apiOperatingSystem.IpxeTemplateArtifacts = newAPIIpxeArtifactResponsesFromDB(dbOS.IpxeTemplateArtifacts)
	if dbOS.InfrastructureProviderID != nil {
		apiOperatingSystem.InfrastructureProviderID = cutil.GetPtr(dbOS.InfrastructureProviderID.String())
	}
	if dbOS.TenantID != nil {
		apiOperatingSystem.TenantID = cutil.GetPtr(dbOS.TenantID.String())
	}
	if dbOS.InfrastructureProvider != nil {
		apiOperatingSystem.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbOS.InfrastructureProvider)
	}
	if dbOS.Tenant != nil {
		apiOperatingSystem.Tenant = NewAPITenantSummary(dbOS.Tenant)
	}
	apiOperatingSystem.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiOperatingSystem.StatusHistory = append(apiOperatingSystem.StatusHistory, NewAPIStatusDetail(dbsd))
	}
	apiOperatingSystem.SiteAssociations = []APIOperatingSystemSiteAssociation{}
	for _, ossa := range ossas {
		ts := sttsmap[ossa.SiteID]
		curVal := ossa
		apiOperatingSystem.SiteAssociations = append(apiOperatingSystem.SiteAssociations, *NewAPIOperatingSystemSiteAssociation(&curVal, ts))
	}

	return &apiOperatingSystem
}

// APIOperatingSystemSummary is the data structure to capture API summary of an OperatingSystem
type APIOperatingSystemSummary struct {
	// ID of the OperatingSystem
	ID string `json:"id"`
	// Name of the OperatingSystem, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// Type is which type of Operating System
	Type *string `json:"type"`
	// Status is the status of the Operating System
	Status string `json:"status"`
}

// NewAPIOperatingSystemSummary accepts a DB layer OperatingSystem object returns an API layer object
func NewAPIOperatingSystemSummary(dbos *cdbm.OperatingSystem) *APIOperatingSystemSummary {
	aos := APIOperatingSystemSummary{
		ID:     dbos.ID.String(),
		Name:   dbos.Name,
		Type:   &dbos.Type,
		Status: dbos.Status,
	}

	return &aos
}

// validateTemplatedIpxeOS fully validates a Templated iPXE create request: image
// fields must be absent, exactly one target site must be specified (the site list
// is fixed at creation and is immutable thereafter), and the template
// parameters/artifacts must be well-formed.
func (oscr *APIOperatingSystemCreateRequest) validateTemplatedIpxeOS() error {
	if err := validation.ValidateStruct(oscr,
		validation.Field(&oscr.ImageSHA, validation.Nil.Error("imageSHA cannot be specified for Templated iPXE Operating Systems")),
		validation.Field(&oscr.ImageAuthType, validation.Nil.Error("imageAuthType cannot be specified for Templated iPXE Operating Systems")),
		validation.Field(&oscr.ImageAuthToken, validation.Nil.Error("imageAuthToken cannot be specified for Templated iPXE Operating Systems")),
		validation.Field(&oscr.ImageDisk, validation.Nil.Error("imageDisk cannot be specified for Templated iPXE Operating Systems")),
		validation.Field(&oscr.RootFsID, validation.Nil.Error("rootFsId cannot be specified for Templated iPXE Operating Systems")),
		validation.Field(&oscr.RootFsLabel, validation.Nil.Error("rootFsLabel cannot be specified for Templated iPXE Operating Systems")),
	); err != nil {
		return err
	}

	if len(oscr.SiteIDs) != 1 {
		return validation.Errors{"siteIds": errors.New("exactly one siteId must be specified for Templated iPXE Operating Systems")}
	}
	for _, siteID := range oscr.SiteIDs {
		if _, err := uuid.Parse(siteID); err != nil {
			return validation.Errors{"siteIds": fmt.Errorf("siteId %q is not a valid UUID", siteID)}
		}
	}

	if err := oscr.IpxeTemplateParameters.Validate(); err != nil {
		return err
	}
	if err := oscr.IpxeTemplateArtifacts.Validate(); err != nil {
		return err
	}
	return nil
}

// validCacheStrategies is the set of accepted artifact CacheStrategy string values.
// It is derived from the DB model's strategy map so the API and persistence layers
// agree on the canonical (friendly) strategy names.
var validCacheStrategies = func() map[string]struct{} {
	m := make(map[string]struct{}, len(cdbm.OperatingSystemIpxeArtifactCacheStrategyToProtoMap))
	for name := range cdbm.OperatingSystemIpxeArtifactCacheStrategyToProtoMap {
		m[name] = struct{}{}
	}
	return m
}()

// ToProto builds the forge.Forge CreateOperatingSystem request from the
// persisted entity. The handler has already validated and merged this API
// request into os, whose ToProto method supplies the canonical wire fields.
//
// Note: artifact authTokens are nested inside the repeated artifacts message and
// are therefore carried as-is (the proxy cannot redact nested fields).
func (oscr *APIOperatingSystemCreateRequest) ToProto(os *cdbm.OperatingSystem) *corev1.CreateOperatingSystemRequest {
	protoOS := os.ToProto()
	return &corev1.CreateOperatingSystemRequest{
		Id:                     protoOS.Id,
		Name:                   protoOS.Name,
		Description:            protoOS.Description,
		TenantOrganizationId:   protoOS.TenantOrganizationId,
		IsActive:               protoOS.IsActive,
		AllowOverride:          protoOS.AllowOverride,
		PhoneHomeEnabled:       protoOS.PhoneHomeEnabled,
		UserData:               protoOS.UserData,
		IpxeScript:             protoOS.IpxeScript,
		IpxeTemplateId:         protoOS.IpxeTemplateId,
		IpxeTemplateParameters: protoOS.IpxeTemplateParameters,
		IpxeTemplateArtifacts:  protoOS.IpxeTemplateArtifacts,
	}
}

// ToProto builds the forge.Forge UpdateOperatingSystem request from the
// post-update entity. The entity conversion keeps unchanged and updated fields
// populated consistently.
func (osur *APIOperatingSystemUpdateRequest) ToProto(os *cdbm.OperatingSystem) *corev1.UpdateOperatingSystemRequest {
	protoOS := os.ToProto()
	return &corev1.UpdateOperatingSystemRequest{
		Id:                         protoOS.Id,
		Name:                       &protoOS.Name,
		Description:                protoOS.Description,
		IsActive:                   &protoOS.IsActive,
		AllowOverride:              &protoOS.AllowOverride,
		PhoneHomeEnabled:           &protoOS.PhoneHomeEnabled,
		UserData:                   protoOS.UserData,
		IpxeScript:                 protoOS.IpxeScript,
		IpxeTemplateId:             protoOS.IpxeTemplateId,
		IpxeTemplateParameters:     &corev1.IpxeTemplateParameters{Items: protoOS.IpxeTemplateParameters},
		IpxeTemplateArtifacts:      &corev1.IpxeTemplateArtifacts{Items: protoOS.IpxeTemplateArtifacts},
		IpxeTemplateDefinitionHash: protoOS.IpxeTemplateDefinitionHash,
	}
}
