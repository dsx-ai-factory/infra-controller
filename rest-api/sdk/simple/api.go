// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"golang.org/x/mod/semver"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// StatusClientClosedRequest is the HTTP status code (499) used when the client closes the connection before the server responds.
// It is a non-standard code used by nginx and gRPC for cancelled requests.
const StatusClientClosedRequest = 499

type ApiMetadata struct {
	// Version of the API we are talking to
	apiVersion string
	// Organization is the organization ID for the service using this SDK
	Organization string
	// SiteId is ID of the Site all SDK requests are scoped to
	SiteID string
	// SiteName is the Name of the selected Site
	SiteName string
	// ProviderId is the provider ID for the service using this SDK
	ProviderID string
	// TenantID is the tenant ID for the service using this SDK
	TenantID string
	// VpcId is the ID of the default VPC for the Site
	VpcID string
	// VpcName is the Name of default VPC for the Site
	VpcName string
	// VpcNetworkVirtualizationType is the network virtualization type of the default VPC
	VpcNetworkVirtualizationType string
	// VpcPrefix is the prefix of the default VPC for the Site
	VpcPrefixID string
	// VpcPrefixName is the Prefix Name of the default VPC for the Site
	VpcPrefixName string
	// SubnetID is the ID of the default Subnet for the Site (used when FNN is not supported)
	SubnetID string
	// SubnetName is the Name of the default Subnet for the Site (used when FNN is not supported)
	SubnetName string
}

// IsMinimumAPIVersion returns the API version and whether it meets the required minimum.
// Returns false if the current API version is empty or does not start with "v", as semver.Compare
// treats two invalid versions as equal (returning 0), which would otherwise produce a false positive.
func (am *ApiMetadata) IsMinimumAPIVersion(requiredVersion string) (string, bool) {
	if !strings.HasPrefix(am.apiVersion, "v") {
		return am.apiVersion, false
	}
	return am.apiVersion, semver.Compare(am.apiVersion, requiredVersion) >= 0
}

// SetDefaultSite try to use the preset SiteId if provided and exists, otherwise fetch and set a default Site for the Organization.
func (am *ApiMetadata) SetDefaultSite(ctx context.Context, apiClient *standard.APIClient) *ApiError {
	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Fetching Sites for org: %s", am.Organization)

	sites, resp, err := apiClient.SiteAPI.GetAllSite(ctx, am.Organization).PageSize(100).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return apiErr
	}

	// reset values
	am.SiteName = ""

	// if asked for a specific site verify it exists otherwise clear it
	if am.SiteID != "" {
		found := false
		for _, site := range sites {
			if site.Id == nil || *site.Id != am.SiteID {
				continue
			}
			found = true
			if site.Name != nil {
				am.SiteName = *site.Name
			}
			logger.Info().Msgf("Default Site %s (%s) has been set for Instance creation.", am.SiteName, am.SiteID)
			break
		}
		if !found {
			logger.Warn().Msgf("Preset Site ID %s was not found.", am.SiteID)
			am.SiteID = ""
		}
	}

	// set a default site if not already set
	if am.SiteID == "" {
		if len(sites) == 0 {
			return &ApiError{
				Code:    http.StatusNotFound,
				Message: fmt.Sprintf("No Sites configured for org: %s. Most operations will not be available.", am.Organization),
			}
		}
		if sites[0].Id == nil {
			return &ApiError{
				Code:    http.StatusInternalServerError,
				Message: fmt.Sprintf("API returned unexpected payload: Site at index 0 has no ID for org: %s", am.Organization),
			}
		}
		am.SiteID = *sites[0].Id
		if sites[0].Name != nil {
			am.SiteName = *sites[0].Name
		}
		if len(sites) > 1 {
			logger.Warn().Msgf("Multiple Sites configured for org: %s. Will default to Site: %s (%s) for Instance creation.", am.Organization, am.SiteName, am.SiteID)
		} else {
			logger.Info().Msgf("Default Site: %s (%s) has been set for all operations.", am.SiteName, am.SiteID)
		}
	}

	// if site changes then we need to reset VPC and VPC Prefix or Subnet
	apiErr = am.SetDefaultVPC(ctx, apiClient)
	if apiErr != nil {
		if apiErr.Code == http.StatusForbidden {
			logger.Warn().Msgf("Unable to fetch VPCs for Site: %s due to insufficient permissions. Instance creation will not be available.", am.SiteID)
			return nil
		}
		return apiErr
	}

	// Skip VPC Prefix/Subnet when no VPCs are configured (Instance creation won't be available)
	if am.VpcID == "" {
		return nil
	}
	// Use NetworkVirtualizationType from VPC to determine whether to fetch VPC Prefix or Subnet
	// If NetworkVirtualizationType is "FNN", fetch VPC Prefix otherwise fetch Subnet (for sites that don't support FNN)
	if am.VpcNetworkVirtualizationType == FNNVirtualizationType {
		return am.SetDefaultVPCPrefix(ctx, apiClient)
	}
	return am.SetDefaultSubnet(ctx, apiClient)
}

// SetDefaultVPC try to use the preset VpcId if provided and exists, otherwise fetch and set a default VPC for the Site.
func (am *ApiMetadata) SetDefaultVPC(ctx context.Context, apiClient *standard.APIClient) *ApiError {
	logger := LoggerFromContext(ctx)
	// This requires being a TENANT_ADMIN: will fail for PROVIDER_*-only roles
	// Fetch and set default VPC, filter by Site ID
	logger.Info().Msgf("Fetching VPCs for org: %s", am.Organization)

	// reset values
	am.VpcName = ""
	am.VpcNetworkVirtualizationType = ""

	vpcs, resp, err := apiClient.VPCAPI.GetAllVpc(ctx, am.Organization).PageSize(100).SiteId(am.SiteID).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return apiErr
	}

	// if asked for a specific vpc verify it exists
	if am.VpcID != "" {
		found := false
		for _, vpc := range vpcs {
			if vpc.Id == nil || *vpc.Id != am.VpcID {
				continue
			}
			found = true
			if vpc.Name != nil {
				am.VpcName = *vpc.Name
			}
			if vpc.NetworkVirtualizationType.IsSet() {
				am.VpcNetworkVirtualizationType = *vpc.NetworkVirtualizationType.Get()
			}
			logger.Info().Msgf("Default VPC %s (%s) with NetworkVirtualizationType '%s' has been set for Instance creation.", am.VpcName, am.VpcID, am.VpcNetworkVirtualizationType)
			break
		}
		if !found {
			logger.Warn().Msgf("Preset VPC ID %s was not found.", am.VpcID)
			am.VpcID = ""
		}
	}

	// set a default VPC if not already set
	if am.VpcID == "" {
		if len(vpcs) == 0 {
			logger.Warn().Msgf("No VPCs configured for Site: %s. Instance creation will not be available.", am.SiteID)
		} else if vpcs[0].Id == nil {
			return &ApiError{
				Code:    http.StatusInternalServerError,
				Message: fmt.Sprintf("API returned unexpected payload: VPC at index 0 has no ID for org: %s", am.Organization),
			}
		} else {
			am.VpcID = *vpcs[0].Id
			if vpcs[0].Name != nil {
				am.VpcName = *vpcs[0].Name
			}
			if vpcs[0].NetworkVirtualizationType.IsSet() {
				am.VpcNetworkVirtualizationType = *vpcs[0].NetworkVirtualizationType.Get()
			}
			if len(vpcs) > 1 {
				logger.Warn().Msgf("Multiple VPCs configured for Site: %s. Will default to VPC: %s (%s) with NetworkVirtualizationType '%s' for Instance creation.", am.SiteName, am.VpcName, am.VpcID, am.VpcNetworkVirtualizationType)
			} else {
				logger.Info().Msgf("Default VPC %s (%s) with NetworkVirtualizationType '%s' has been set for Instance creation.", am.VpcName, am.VpcID, am.VpcNetworkVirtualizationType)
			}
		}
	}
	return nil
}

// SetDefaultVPCPrefix try to use the preset VpcPrefixId if provided and exists, otherwise fetch and set a default VPC Prefix for the VPC.
func (am *ApiMetadata) SetDefaultVPCPrefix(ctx context.Context, apiClient *standard.APIClient) *ApiError {
	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Fetching VPC Prefixes for Organization, Site and VPC: %s/%s/%s", am.Organization, am.SiteName, am.VpcName)

	vpcPrefixes, resp, err := apiClient.VPCPrefixAPI.GetAllVpcPrefix(ctx, am.Organization).PageSize(100).SiteId(am.SiteID).VpcId(am.VpcID).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return apiErr
	}

	// reset values
	am.VpcPrefixName = ""

	// if asked for a specific prefix verify it exists
	if am.VpcPrefixID != "" {
		found := false
		for _, vpcPrefix := range vpcPrefixes {
			if vpcPrefix.Id == nil || *vpcPrefix.Id != am.VpcPrefixID {
				continue
			}
			found = true
			if vpcPrefix.Name != nil {
				am.VpcPrefixName = *vpcPrefix.Name
			}
			logger.Info().Msgf("Default VPC Prefix ID: %s (%s) has been set for Instance creation.", am.VpcPrefixID, am.VpcPrefixName)
			break
		}
		if !found {
			logger.Warn().Msgf("Preset VPC Prefix ID %s was not found.", am.VpcPrefixID)
			am.VpcPrefixID = ""
		}
	}

	// otherwise set default prefix
	if am.VpcPrefixID == "" {
		if len(vpcPrefixes) == 0 {
			logger.Warn().Msgf("No VPC Prefixes configured for Site: %s. Instance creation will not be available.", am.SiteName)
		} else if vpcPrefixes[0].Id == nil {
			return &ApiError{
				Code:    http.StatusInternalServerError,
				Message: fmt.Sprintf("API returned unexpected payload: VPC Prefix at index 0 has no ID for org: %s", am.Organization),
			}
		} else {
			am.VpcPrefixID = *vpcPrefixes[0].Id
			if vpcPrefixes[0].Name != nil {
				am.VpcPrefixName = *vpcPrefixes[0].Name
			}
			if len(vpcPrefixes) > 1 {
				logger.Warn().Msgf("Multiple VPC prefixes configured for Site: %s. Will default to VPC prefix: %s (%s) for Instance creation.", am.SiteName, am.VpcPrefixName, am.VpcPrefixID)
			} else {
				logger.Info().Msgf("Default VPC prefix: %s has been set for Instance creation.", am.VpcPrefixName)
			}
		}
	}
	return nil
}

// SetDefaultSubnet try to use the preset SubnetId if provided and exists, otherwise fetch and set a default Subnet for the VPC.
func (am *ApiMetadata) SetDefaultSubnet(ctx context.Context, apiClient *standard.APIClient) *ApiError {
	logger := LoggerFromContext(ctx)
	logger.Info().Msgf("Fetching Subnets for Organization, Site and VPC: %s/%s/%s", am.Organization, am.SiteName, am.VpcName)

	// reset values
	am.SubnetName = ""

	// if asked for a specific subnet verify it exists
	if am.SubnetID != "" {
		subnet, resp, err := apiClient.SubnetAPI.GetSubnet(ctx, am.Organization, am.SubnetID).Execute()
		apiErr := HandleResponseError(resp, err)
		if apiErr != nil {
			return apiErr
		}
		if subnet.SiteId != "" && subnet.SiteId != am.SiteID {
			logger.Warn().Msgf("Preset Subnet ID %s does not belong to Site ID %s.", am.SubnetID, am.SiteID)
			am.SubnetID = ""
		} else if subnet.VpcId != "" && subnet.VpcId != am.VpcID {
			logger.Warn().Msgf("Preset Subnet ID %s does not belong to VPC ID %s.", am.SubnetID, am.VpcID)
			am.SubnetID = ""
		} else {
			am.SubnetName = subnet.Name
			logger.Info().Msgf("Default Subnet ID: %s (%s) has been set for Instance creation.", am.SubnetID, am.SubnetName)
			return nil
		}
	}

	// if not provided a specific subnet or if it was invalid fetch all subnets for the site and VPC and pick the first one
	if am.SubnetID == "" {
		subnets, resp, err := apiClient.SubnetAPI.GetAllSubnet(ctx, am.Organization).SiteId(am.SiteID).VpcId(am.VpcID).Execute()
		apiErr := HandleResponseError(resp, err)
		if apiErr != nil {
			return apiErr
		}
		if len(subnets) == 0 {
			logger.Warn().Msgf("No Subnets configured for Site: %s. Instance creation will not be available.", am.SiteName)
		} else if subnets[0].Id == "" {
			return &ApiError{
				Code:    http.StatusInternalServerError,
				Message: fmt.Sprintf("API returned unexpected payload: Subnet at index 0 has no ID for org: %s", am.Organization),
			}
		} else {
			am.SubnetID = subnets[0].Id
			am.SubnetName = subnets[0].Name
			if len(subnets) > 1 {
				logger.Warn().Msgf("Multiple Subnets configured for Site: %s. Will default to Subnet: %s (%s) for Instance creation.", am.SiteName, am.SubnetName, am.SubnetID)
			} else {
				logger.Info().Msgf("Default Subnet: %s has been set for Instance creation.", am.SubnetName)
			}
		}
	}
	return nil
}

// Initialize initializes the ApiMetadata struct. It will try to use preset values if provided, otherwise it will fetch and set the values from the API.
func (am *ApiMetadata) Initialize(ctx context.Context, org string, apiClient *standard.APIClient) *ApiError {
	logger := LoggerFromContext(ctx)
	// NOTE: These variables are for internal use only, not to be used by the SDK user
	am.Organization = org

	// Fetch API version:
	logger.Info().Msgf("Fetching API version for org: %s", org)
	metadata, resp, err := apiClient.MetadataAPI.GetMetadata(ctx, am.Organization).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		logger.Warn().Msgf("Could not retrieve API version for org: %s. Error: %s", org, apiErr.Message)
		return apiErr
	}
	if metadata.Version == nil {
		logger.Warn().Msgf("Retrieve empty API version for org: %s", org)
		return &ApiError{
			Code:    http.StatusInternalServerError,
			Message: fmt.Sprintf("Unable to retrieve API version for org %s.", org),
		}
	}
	am.apiVersion = *metadata.Version
	if !strings.HasPrefix(am.apiVersion, "v") {
		am.apiVersion = "v" + am.apiVersion
	}

	// Fetch Infrastructure Provider
	logger.Info().Msgf("Fetching Infrastructure Provider for org: %s", org)
	provider, resp, err := apiClient.InfrastructureProviderAPI.GetCurrentInfrastructureProvider(ctx, am.Organization).Execute()
	apiErr = HandleResponseError(resp, err)
	if apiErr != nil {
		if apiErr.Code == http.StatusForbidden {
			logger.Warn().Msgf("Could not retrieve Infrastructure Provider for org: %s. Certain operations will not be available. Error: %s", org, apiErr.Message)
		} else {
			return apiErr
		}
	} else if provider != nil && provider.Id != nil {
		am.ProviderID = *provider.Id
		logger.Info().Msg("Infrastructure Provider has been successfully set")
	} else if provider != nil {
		return &ApiError{
			Code:    http.StatusInternalServerError,
			Message: fmt.Sprintf("API returned unexpected payload: Infrastructure Provider has no ID for org: %s", org),
		}
	}

	// Fetch Tenant
	logger.Info().Msgf("Fetching Tenant for org: %s", org)
	tenant, resp, err := apiClient.TenantAPI.GetCurrentTenant(ctx, am.Organization).Execute()
	apiErr = HandleResponseError(resp, err)
	if apiErr != nil {
		if apiErr.Code == http.StatusForbidden {
			logger.Warn().Msgf("Could not retrieve Tenant for org: %s. Certain operations will not be available. Error: %s", org, apiErr.Message)
		} else {
			return apiErr
		}
	} else if tenant != nil && tenant.Id != nil {
		am.TenantID = *tenant.Id
		logger.Info().Msg("Tenant has been successfully set")
	} else if tenant != nil {
		return &ApiError{
			Code:    http.StatusInternalServerError,
			Message: fmt.Sprintf("API returned unexpected payload: Tenant has no ID for org: %s", org),
		}
	}

	if am.TenantID == "" && am.ProviderID == "" {
		return &ApiError{
			Code:    http.StatusNotFound,
			Message: fmt.Sprintf("Could not fetch Provider or Tenant for org: %s. Ensure your JWT has required permissions.", org),
		}
	}

	// Fetch and set default Site, VPC and VPC Prefix
	return am.SetDefaultSite(ctx, apiClient)
}

// PaginationFilter is a struct that encapsulates supported query parameters for pagination
type PaginationFilter struct {
	// PageNumber is the page number to retrieve
	PageNumber *int
	// PageSize is the number of items per page
	PageSize *int
	// OrderBy is the field to order the results by
	OrderBy *string
}

// ApiError is a struct that encapsulates the error response from the API
type ApiError struct {
	Code    int                    `json:"code"`
	Message string                 `json:"message"`
	Data    map[string]interface{} `json:"data"`
}

func (ae *ApiError) Error() string {
	return fmt.Sprintf("Code: %d, Message: %s, Data: %v", ae.Code, ae.Message, ae.Data)
}

// HandleResponseError handles the response error from the API
func HandleResponseError(resp *http.Response, err error) *ApiError {
	if resp == nil {
		apiError := &ApiError{
			Code: http.StatusInternalServerError,
		}

		if err != nil {
			apiError.Message = "Error processing API response: " + err.Error()
		} else {
			apiError.Message = "No response or error received from API"
		}
		return apiError
	}

	if resp.StatusCode < http.StatusMultipleChoices {
		return nil
	}

	var oaErr *standard.GenericOpenAPIError
	if errors.As(err, &oaErr) {
		if oaErr.Model() != nil {
			// Model can be NICoAPIError (value) or *NICoAPIError (pointer)
			switch v := oaErr.Model().(type) {
			case standard.NICoAPIError:
				apiError := &ApiError{Code: resp.StatusCode, Data: v.GetData()}
				apiError.Message = v.GetMessage()
				if apiError.Message == "" {
					apiError.Message = "Error processing API response"
				}
				return apiError
			case *standard.NICoAPIError:
				apiError := &ApiError{Code: resp.StatusCode, Data: v.GetData()}
				apiError.Message = v.GetMessage()
				if apiError.Message == "" {
					apiError.Message = "Error processing API response"
				}
				return apiError
			}
		}
		if oaErr.Body() != nil {
			apiError := &ApiError{Code: resp.StatusCode}
			var nicoErr standard.NICoAPIError
			if jerr := json.Unmarshal(oaErr.Body(), &nicoErr); jerr == nil {
				apiError.Message = nicoErr.GetMessage()
				if apiError.Message == "" {
					apiError.Message = "Error processing API response"
				}
				apiError.Data = nicoErr.GetData()
			} else {
				apiError.Message = fmt.Sprintf("Error processing API response: %v", string(oaErr.Body()))
			}
			return apiError
		}
	}
	var data map[string]interface{}
	if err != nil {
		data = map[string]interface{}{"error": err.Error()}
	}
	return &ApiError{
		Code:    resp.StatusCode,
		Message: "Error processing API response",
		Data:    data,
	}
}
