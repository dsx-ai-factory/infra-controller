// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"regexp"
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// DpuExtensionServiceTypeKubernetesPod is the service type for Kubernetes Pod
	DpuExtensionServiceTypeKubernetesPod = "KubernetesPod"
	// DpuExtensionServiceMaxObservabilityConfigs is the max number of observability configs allowed per service version
	DpuExtensionServiceMaxObservabilityConfigs = 20
	// DpuExtensionServiceMaxObservabilityConfigNameLength is the max length for an observability config name
	DpuExtensionServiceMaxObservabilityConfigNameLength = 64
	// DpuExtensionServiceMaxObservabilityPropertyLength is the max length for endpoint and path properties
	DpuExtensionServiceMaxObservabilityPropertyLength = 128
)

var (
	dpuExtensionServiceObservabilityPromEndpointBadRE = regexp.MustCompile(`[^a-zA-Z0-9:\-]+`)
	dpuExtensionServiceObservabilityLogPathBadRE      = regexp.MustCompile(`[^a-zA-Z0-9\-_\/\.\@]+`)
)

// APIDpuExtensionServiceCreateRequest is the data structure to capture user request to create a new DpuExtensionService
type APIDpuExtensionServiceCreateRequest struct {
	// Name is the name of the DpuExtensionService
	Name string `json:"name"`
	// Description is the description of the DpuExtensionService
	Description *string `json:"description"`
	// ServiceType is the type of service
	ServiceType string `json:"serviceType"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Data is the deployment spec for the DPU Extension Service
	Data string `json:"data"`
	// Credentials are the credentials to download resources
	Credentials *APIDpuExtensionServiceCredentials `json:"credentials"`
	// Observability is the observability configuration for the DPU Extension Service version
	Observability *APIDpuExtensionServiceObservability `json:"observability"`
}

// Validate ensures that the values passed in request are acceptable
func (descr *APIDpuExtensionServiceCreateRequest) Validate() error {
	err := validation.ValidateStruct(descr,
		validation.Field(&descr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&descr.ServiceType,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(DpuExtensionServiceTypeKubernetesPod).Error("must be 'KubernetesPod'")),
		validation.Field(&descr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&descr.Data,
			validation.Required.Error(validationErrorValueRequired)),
	)
	if err != nil {
		return err
	}

	// Validate credentials if provided
	if descr.Credentials != nil {
		err = descr.Credentials.Validate()
		if err != nil {
			return err
		}
	}

	if descr.Observability != nil {
		err = descr.Observability.Validate()
		if err != nil {
			return err
		}
	}

	return nil
}

// APIDpuExtensionServiceUpdateRequest is the data structure to capture user request to update a DpuExtensionService
type APIDpuExtensionServiceUpdateRequest struct {
	// Name is the name of the DpuExtensionService
	Name *string `json:"name"`
	// Description is the description of the DpuExtensionService
	Description *string `json:"description"`
	// Data is the deployment spec for the DPU Extension Service
	Data *string `json:"data"`
	// Credentials are the credentials to download resources
	Credentials *APIDpuExtensionServiceCredentials `json:"credentials"`
	// Observability is the observability configuration for the DPU Extension Service version
	Observability *APIDpuExtensionServiceObservability `json:"observability"`
}

// Validate ensures that the values passed in request are acceptable
func (desur *APIDpuExtensionServiceUpdateRequest) Validate() error {
	err := validation.ValidateStruct(desur,
		validation.Field(&desur.Name,
			validation.When(desur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(desur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(desur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
	)
	if err != nil {
		return err
	}

	// Validate credentials if provided
	if desur.Credentials != nil {
		err = desur.Credentials.Validate()
		if err != nil {
			return err
		}
	}

	if desur.Observability != nil {
		err = desur.Observability.Validate()
		if err != nil {
			return err
		}
	}

	return nil
}

// APIDpuExtensionServiceCredentials is the data structure for registry credentials
type APIDpuExtensionServiceCredentials struct {
	// RegistryURL is the URL for the registry
	RegistryURL string `json:"registryUrl"`
	// Username for the registry
	Username *string `json:"username"`
	// Password for the registry
	Password *string `json:"password"`
}

// Validate ensures that the credentials are valid
func (desc APIDpuExtensionServiceCredentials) Validate() error {
	return validation.ValidateStruct(&desc,
		validation.Field(&desc.RegistryURL,
			validation.Required.Error(validationErrorValueRequired),
			validationis.URL.Error("must be a valid URL")),
		validation.Field(&desc.Username,
			validation.When(desc.RegistryURL != "", validation.Required.Error("`username` must be specified if `registryUrl` is specified"))),
		validation.Field(&desc.Password,
			validation.When(desc.RegistryURL != "", validation.Required.Error("`password` must be specified if `registryUrl` is specified"))),
	)
}

// ToProto converts the API credentials into the workflow proto
// representation. Returns nil for a nil receiver. Validation should
// have already established that Username and Password are non-nil
// when RegistryURL is non-empty; the nil guards below are defensive
// so a misuse panics with a clear shape failure rather than an opaque
// nil-deref.
func (desc *APIDpuExtensionServiceCredentials) ToProto() *corev1.DpuExtensionServiceCredential {
	if desc == nil {
		return nil
	}
	cred := &corev1.DpuExtensionServiceCredential{
		RegistryUrl: desc.RegistryURL,
	}
	if desc.Username != nil && desc.Password != nil {
		cred.Type = &corev1.DpuExtensionServiceCredential_UsernamePassword{
			UsernamePassword: &corev1.UsernamePassword{
				Username: *desc.Username,
				Password: *desc.Password,
			},
		}
	}
	return cred
}

// ToProto converts a validated APIDpuExtensionServiceCreateRequest into
// the workflow proto request. serviceID is the DB-generated ID for the
// new service and tenantOrg is the owning tenant's organization ID;
// neither is on the request body.
//
// The method trusts that the request has already been Validated. The
// nested `Credentials.ToProto()` and `Observability.ToProto()` calls
// each handle a nil receiver, so no outer guard is needed here.
func (descr *APIDpuExtensionServiceCreateRequest) ToProto(serviceID, tenantOrg string) *corev1.CreateDpuExtensionServiceRequest {
	req := &corev1.CreateDpuExtensionServiceRequest{
		ServiceId:            &serviceID,
		ServiceName:          descr.Name,
		Description:          descr.Description,
		TenantOrganizationId: tenantOrg,
		Data:                 descr.Data,
		Credential:           descr.Credentials.ToProto(),
		Observability:        descr.Observability.ToProto(),
	}
	if descr.ServiceType == DpuExtensionServiceTypeKubernetesPod {
		req.ServiceType = corev1.DpuExtensionServiceType_KUBERNETES_POD
	}
	return req
}

// ToProto converts a validated APIDpuExtensionServiceUpdateRequest into
// the workflow proto request. serviceID identifies the service being
// updated.
//
// The method trusts that the request has already been Validated. The
// nested `Credentials.ToProto()` and `Observability.ToProto()` calls
// each handle a nil receiver, so no outer guard is needed here.
func (desur *APIDpuExtensionServiceUpdateRequest) ToProto(serviceID string) *corev1.UpdateDpuExtensionServiceRequest {
	req := &corev1.UpdateDpuExtensionServiceRequest{
		ServiceId:     serviceID,
		ServiceName:   desur.Name,
		Description:   desur.Description,
		Credential:    desur.Credentials.ToProto(),
		Observability: desur.Observability.ToProto(),
	}
	if desur.Data != nil {
		req.Data = *desur.Data
	}
	return req
}

// APIDpuExtensionService is the data structure to capture API representation of a DpuExtensionService
type APIDpuExtensionService struct {
	// ID is the unique UUID v4 identifier for the DpuExtensionService
	ID string `json:"id"`
	// Name is the name of the DpuExtensionService
	Name string `json:"name"`
	// Description is the description of the DpuExtensionService
	Description *string `json:"description"`
	// ServiceType is the type of service
	ServiceType string `json:"serviceType"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the site
	Site *APISiteSummary `json:"site,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// Version is the latest version of the DPU Extension Service
	Version *string `json:"version"`
	// VersionInfo holds the details for the latest version
	VersionInfo *APIDpuExtensionServiceVersionInfo `json:"versionInfo"`
	// ActiveVersions is a list of active versions available for deployment
	ActiveVersions []string `json:"activeVersions"`
	// Status is the status of the DpuExtensionService
	Status string `json:"status"`
	// StatusHistory is the status detail records for the DpuExtensionService over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created indicates the ISO datetime string for when the DpuExtensionService was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the DpuExtensionService was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIDpuExtensionService creates and returns a new APIDpuExtensionService object
func NewAPIDpuExtensionService(dbdes *cdbm.DpuExtensionService, dbdesds []cdbm.StatusDetail) *APIDpuExtensionService {
	apiDpuExtensionService := &APIDpuExtensionService{
		ID:             dbdes.ID.String(),
		Name:           dbdes.Name,
		Description:    dbdes.Description,
		ServiceType:    dbdes.ServiceType,
		SiteID:         dbdes.SiteID.String(),
		TenantID:       dbdes.TenantID.String(),
		Version:        dbdes.Version,
		ActiveVersions: dbdes.ActiveVersions,
		Status:         dbdes.Status,
		StatusHistory:  []APIStatusDetail{},
		Created:        dbdes.Created,
		Updated:        dbdes.Updated,
	}

	if dbdes.VersionInfo != nil {
		apiDpuExtensionService.VersionInfo = NewAPIDpuExtensionServiceVersionInfo(dbdes.VersionInfo)
	}

	if dbdes.Site != nil {
		apiDpuExtensionService.Site = NewAPISiteSummary(dbdes.Site)
	}

	if dbdes.Tenant != nil {
		apiDpuExtensionService.Tenant = NewAPITenantSummary(dbdes.Tenant)
	}

	apiDpuExtensionService.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbdesds {
		apiDpuExtensionService.StatusHistory = append(apiDpuExtensionService.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	return apiDpuExtensionService
}

// APIDpuExtensionServiceSummary is the data structure to capture API summary of a DpuExtensionService
type APIDpuExtensionServiceSummary struct {
	// ID is the unique UUID v4 identifier for the DpuExtensionService
	ID string `json:"id"`
	// Name is the name of the DpuExtensionService
	Name string `json:"name"`
	// ServiceType is the type of service
	ServiceType string `json:"serviceType"`
	// LatestVersion is the latest version of the DPU Extension Service
	LatestVersion *string `json:"latestVersion"`
	// Status is the status of the DpuExtensionService
	Status string `json:"status"`
}

// NewAPIDpuExtensionServiceSummary creates and returns a new APIDpuExtensionServiceSummary object
func NewAPIDpuExtensionServiceSummary(dbdes *cdbm.DpuExtensionService) *APIDpuExtensionServiceSummary {
	return &APIDpuExtensionServiceSummary{
		ID:            dbdes.ID.String(),
		Name:          dbdes.Name,
		ServiceType:   dbdes.ServiceType,
		LatestVersion: dbdes.Version,
		Status:        dbdes.Status,
	}
}

// APIDpuExtensionServiceVersionInfo is the data structure for version information
type APIDpuExtensionServiceVersionInfo struct {
	// Version is the version identifier
	Version string `json:"version"`
	// Data is the deployment spec
	Data string `json:"data"`
	// HasCredentials indicates if this version has credentials
	HasCredentials bool `json:"hasCredentials"`
	// Created indicates when this version was created
	Created time.Time `json:"created"`
	// Observability is the observability configuration for this version
	Observability *APIDpuExtensionServiceObservability `json:"observability"`
}

// NewAPIDpuExtensionServiceVersionInfo creates and returns a new APIDpuExtensionServiceVersionInfo object
func NewAPIDpuExtensionServiceVersionInfo(dbv *cdbm.DpuExtensionServiceVersionInfo) *APIDpuExtensionServiceVersionInfo {
	return &APIDpuExtensionServiceVersionInfo{
		Version:        dbv.Version,
		Data:           dbv.Data,
		HasCredentials: dbv.HasCredentials,
		Created:        dbv.Created,
		Observability:  NewAPIDpuExtensionServiceObservability(dbv.Observability),
	}
}

// APIDpuExtensionServiceObservability is the data structure for DPU Extension Service observability
type APIDpuExtensionServiceObservability struct {
	// Configs are the observability configurations for the service version
	Configs []APIDpuExtensionServiceObservabilityConfig `json:"configs"`
}

// Validate ensures that the observability configuration is valid
func (deso APIDpuExtensionServiceObservability) Validate() error {
	err := validation.ValidateStruct(&deso,
		validation.Field(&deso.Configs,
			validation.By(func(value any) error {
				configs, ok := value.([]APIDpuExtensionServiceObservabilityConfig)
				if !ok {
					return fmt.Errorf("must be a valid list of observability configs")
				}
				if len(configs) > DpuExtensionServiceMaxObservabilityConfigs {
					return fmt.Errorf("must not contain more than %d observability configs", DpuExtensionServiceMaxObservabilityConfigs)
				}
				return nil
			})),
	)
	if err != nil {
		return err
	}

	for idx, cfg := range deso.Configs {
		if err = cfg.Validate(); err != nil {
			return fmt.Errorf("configs[%d]: %w", idx, err)
		}
	}

	return nil
}

// APIDpuExtensionServiceObservabilityConfig is the data structure for a single DPU Extension Service observability config
type APIDpuExtensionServiceObservabilityConfig struct {
	// Name is the name of the service or service component being monitored
	Name *string `json:"name"`
	// Prometheus holds prometheus scrape configuration
	Prometheus *APIDpuExtensionServiceObservabilityConfigPrometheus `json:"prometheus,omitempty"`
	// Logging holds logging configuration
	Logging *APIDpuExtensionServiceObservabilityConfigLogging `json:"logging,omitempty"`
}

// Validate ensures that the observability config is valid
func (desoc APIDpuExtensionServiceObservabilityConfig) Validate() error {
	if desoc.Name != nil {
		if strings.TrimSpace(*desoc.Name) == "" {
			return fmt.Errorf("name must be non-empty")
		}
		if len(*desoc.Name) > DpuExtensionServiceMaxObservabilityConfigNameLength {
			return fmt.Errorf("name length must not exceed %d", DpuExtensionServiceMaxObservabilityConfigNameLength)
		}
	}

	configCount := 0
	if desoc.Prometheus != nil {
		configCount++
	}
	if desoc.Logging != nil {
		configCount++
	}
	if configCount != 1 {
		return fmt.Errorf("exactly one of `prometheus` or `logging` must be specified")
	}

	switch {
	case desoc.Prometheus != nil:
		if err := desoc.Prometheus.Validate(); err != nil {
			return err
		}
	case desoc.Logging != nil:
		if err := desoc.Logging.Validate(); err != nil {
			return err
		}
	}

	return nil
}

// APIDpuExtensionServiceObservabilityConfigPrometheus is the data structure for prometheus observability config
type APIDpuExtensionServiceObservabilityConfigPrometheus struct {
	// ScrapeIntervalSeconds is how often prometheus should scrape the endpoint
	ScrapeIntervalSeconds uint32 `json:"scrapeIntervalSeconds"`
	// Endpoint is the prometheus scrape endpoint
	Endpoint string `json:"endpoint"`
}

// Validate ensures that the prometheus observability config is valid
func (desop APIDpuExtensionServiceObservabilityConfigPrometheus) Validate() error {
	err := validation.ValidateStruct(&desop,
		validation.Field(&desop.ScrapeIntervalSeconds,
			validation.Required.Error("must be greater than 0"),
			validation.Min(uint32(1)).Error("must be greater than 0")),
		validation.Field(&desop.Endpoint, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&desop.Endpoint, validation.Length(0, DpuExtensionServiceMaxObservabilityPropertyLength).Error(fmt.Sprintf("length must not exceed %d", DpuExtensionServiceMaxObservabilityPropertyLength))),
	)
	if err != nil {
		return err
	}

	if dpuExtensionServiceObservabilityPromEndpointBadRE.MatchString(desop.Endpoint) {
		return fmt.Errorf("endpoint contains invalid characters")
	}

	return nil
}

// APIDpuExtensionServiceObservabilityConfigLogging is the data structure for logging observability config
type APIDpuExtensionServiceObservabilityConfigLogging struct {
	// Path is the log path to collect
	Path string `json:"path"`
}

// Validate ensures that the logging observability config is valid
func (desol APIDpuExtensionServiceObservabilityConfigLogging) Validate() error {
	err := validation.ValidateStruct(&desol,
		validation.Field(&desol.Path, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&desol.Path, validation.Length(0, DpuExtensionServiceMaxObservabilityPropertyLength).Error(fmt.Sprintf("length must not exceed %d", DpuExtensionServiceMaxObservabilityPropertyLength))),
	)
	if err != nil {
		return err
	}

	if dpuExtensionServiceObservabilityLogPathBadRE.MatchString(desol.Path) {
		return fmt.Errorf("path contains invalid characters")
	}

	return nil
}

// ToProto converts the Prometheus sub-config to its protobuf
// representation. Trusts that `Validate` has already been run.
func (desop *APIDpuExtensionServiceObservabilityConfigPrometheus) ToProto() *corev1.DpuExtensionServiceObservabilityConfigPrometheus {
	if desop == nil {
		return nil
	}
	return &corev1.DpuExtensionServiceObservabilityConfigPrometheus{
		ScrapeIntervalSeconds: desop.ScrapeIntervalSeconds,
		Endpoint:              desop.Endpoint,
	}
}

// ToProto converts the Logging sub-config to its protobuf
// representation. Trusts that `Validate` has already been run.
func (desol *APIDpuExtensionServiceObservabilityConfigLogging) ToProto() *corev1.DpuExtensionServiceObservabilityConfigLogging {
	if desol == nil {
		return nil
	}
	return &corev1.DpuExtensionServiceObservabilityConfigLogging{
		Path: desol.Path,
	}
}

// ToProto converts a single observability config (Name plus exactly one
// of Prometheus / Logging) to its protobuf representation. The two
// sub-configs are modeled as a proto oneof; `Validate` enforces that
// exactly one is set.
func (desoc *APIDpuExtensionServiceObservabilityConfig) ToProto() *corev1.DpuExtensionServiceObservabilityConfig {
	if desoc == nil {
		return nil
	}
	proto := &corev1.DpuExtensionServiceObservabilityConfig{
		Name: desoc.Name,
	}
	switch {
	case desoc.Prometheus != nil:
		proto.Config = &corev1.DpuExtensionServiceObservabilityConfig_Prometheus{
			Prometheus: desoc.Prometheus.ToProto(),
		}
	case desoc.Logging != nil:
		proto.Config = &corev1.DpuExtensionServiceObservabilityConfig_Logging{
			Logging: desoc.Logging.ToProto(),
		}
	}
	return proto
}

// ToProto converts an API observability definition to the protobuf
// representation passed to NICo. Delegates per-config conversion to
// `APIDpuExtensionServiceObservabilityConfig.ToProto`.
func (apiObservability *APIDpuExtensionServiceObservability) ToProto() *corev1.DpuExtensionServiceObservability {
	if apiObservability == nil {
		return nil
	}
	protoObservability := &corev1.DpuExtensionServiceObservability{
		Configs: make([]*corev1.DpuExtensionServiceObservabilityConfig, 0, len(apiObservability.Configs)),
	}
	for _, cfg := range apiObservability.Configs {
		protoObservability.Configs = append(protoObservability.Configs, cfg.ToProto())
	}
	return protoObservability
}

// FromProto populates API version info from the site-agent protobuf form.
func (apiVersionInfo *APIDpuExtensionServiceVersionInfo) FromProto(protoVersionInfo *corev1.DpuExtensionServiceVersionInfo, fallbackTime time.Time) {
	if apiVersionInfo == nil || protoVersionInfo == nil {
		return
	}

	dbVersionInfo := &cdbm.DpuExtensionServiceVersionInfo{}
	dbVersionInfo.FromProto(protoVersionInfo, fallbackTime)

	*apiVersionInfo = APIDpuExtensionServiceVersionInfo{
		Version:        dbVersionInfo.Version,
		Data:           dbVersionInfo.Data,
		HasCredentials: dbVersionInfo.HasCredentials,
		Created:        dbVersionInfo.Created,
		Observability:  NewAPIDpuExtensionServiceObservability(dbVersionInfo.Observability),
	}
}

// FromProto populates API observability from the site-agent protobuf form.
func (apiObservability *APIDpuExtensionServiceObservability) FromProto(protoObservability *corev1.DpuExtensionServiceObservability) {
	if apiObservability == nil || protoObservability == nil {
		return
	}

	apiObservability.Configs = make([]APIDpuExtensionServiceObservabilityConfig, 0, len(protoObservability.Configs))

	for _, cfg := range protoObservability.Configs {
		if cfg == nil {
			continue
		}

		apiCfg := APIDpuExtensionServiceObservabilityConfig{
			Name: cfg.Name,
		}

		// A single observability config is modeled as a proto oneof,
		// so exactly one of these options should be populated.
		if cfg.GetPrometheus() != nil {
			apiCfg.Prometheus = &APIDpuExtensionServiceObservabilityConfigPrometheus{
				ScrapeIntervalSeconds: cfg.GetPrometheus().ScrapeIntervalSeconds,
				Endpoint:              cfg.GetPrometheus().Endpoint,
			}
		}

		if cfg.GetLogging() != nil {
			apiCfg.Logging = &APIDpuExtensionServiceObservabilityConfigLogging{
				Path: cfg.GetLogging().Path,
			}
		}

		apiObservability.Configs = append(apiObservability.Configs, apiCfg)
	}
}

// NewAPIDpuExtensionServiceObservability creates and returns a new APIDpuExtensionServiceObservability object
func NewAPIDpuExtensionServiceObservability(dbo *cdbm.DpuExtensionServiceObservability) *APIDpuExtensionServiceObservability {
	if dbo == nil {
		return nil
	}

	apiObservability := &APIDpuExtensionServiceObservability{}
	apiObservability.FromProto(dbo.DpuExtensionServiceObservability)
	return apiObservability
}
