// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"net/url"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIDhcpOptions is the set of DHCP values for one IP family
type APIDhcpOptions struct {
	// BootURL is the boot file URL sent to hosts; it must use http or https
	BootURL *string `json:"bootUrl"`
	// DNSServers is the list of DNS server addresses sent to hosts
	DNSServers []string `json:"dnsServers"`
}

// validate checks the values, using ip to check each DNS server address
func (o *APIDhcpOptions) validate(ip validation.Rule) error {
	if o == nil {
		return nil
	}
	return validation.ValidateStruct(o,
		validation.Field(&o.BootURL, validation.By(validateBootURL)),
		validation.Field(&o.DNSServers, validation.Each(validation.Required, ip)),
	)
}

// isSet reports whether any value is set
func (o *APIDhcpOptions) isSet() bool {
	return o != nil && ((o.BootURL != nil && *o.BootURL != "") || len(o.DNSServers) > 0)
}

// merge returns the stored values with these values applied; an omitted value keeps the stored one and an empty one clears it
func (o *APIDhcpOptions) merge(bootURL *string, dnsServers []string) (*string, []string) {
	if o == nil {
		return bootURL, dnsServers
	}
	if o.BootURL != nil {
		bootURL = nil
		if *o.BootURL != "" {
			bootURL = o.BootURL
		}
	}
	if o.DNSServers != nil {
		dnsServers = o.DNSServers
	}
	return bootURL, dnsServers
}

// applyToV4 returns the stored DHCPv4 values with these values applied
func (o *APIDhcpOptions) applyToV4(current *corev1.DhcpOptionsV4) *corev1.DhcpOptionsV4 {
	if o == nil {
		return current
	}
	v4 := &corev1.DhcpOptionsV4{}
	if current != nil {
		v4.BootUrl, v4.DnsServers = current.BootUrl, current.DnsServers
	}
	v4.BootUrl, v4.DnsServers = o.merge(v4.BootUrl, v4.DnsServers)
	return v4
}

// applyToV6 returns the stored DHCPv6 values with these values applied
func (o *APIDhcpOptions) applyToV6(current *corev1.DhcpOptionsV6) *corev1.DhcpOptionsV6 {
	if o == nil {
		return current
	}
	v6 := &corev1.DhcpOptionsV6{}
	if current != nil {
		v6.BootUrl, v6.DnsServers = current.BootUrl, current.DnsServers
	}
	v6.BootUrl, v6.DnsServers = o.merge(v6.BootUrl, v6.DnsServers)
	return v6
}

// fromProto populates the values from their Core representation
func (o *APIDhcpOptions) fromProto(bootURL string, dnsServers []string) {
	o.BootURL = nil
	if bootURL != "" {
		o.BootURL = &bootURL
	}
	o.DNSServers = append([]string{}, dnsServers...)
}

// validateBootURL accepts an unset or empty boot URL, otherwise an http or https URL
func validateBootURL(value interface{}) error {
	var bootURL string
	switch v := value.(type) {
	case string:
		bootURL = v
	case *string:
		if v == nil {
			return nil
		}
		bootURL = *v
	default:
		return errors.New("boot URL must be a string type")
	}
	if bootURL == "" {
		return nil
	}
	u, err := url.Parse(bootURL)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" {
		return errors.New("must be a valid http or https URL")
	}
	// Hosts receive the boot URL in cleartext DHCP replies, so it must not carry credentials.
	if u.User != nil {
		return errors.New("must not contain a username or password")
	}
	return nil
}

// APIDhcpOptionSetCreateRequest is the data structure to capture a user request to create a new DhcpOptionSet
type APIDhcpOptionSetCreateRequest struct {
	// Name is the name of the DhcpOptionSet
	Name string `json:"name"`
	// Description is the description of the DhcpOptionSet
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// IPv4 holds the DHCPv4 values
	IPv4 *APIDhcpOptions `json:"ipv4"`
	// IPv6 holds the DHCPv6 values
	IPv6 *APIDhcpOptions `json:"ipv6"`
	// Labels is the labels of the DhcpOptionSet
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (r *APIDhcpOptionSetCreateRequest) Validate() error {
	err := validation.ValidateStruct(r,
		validation.Field(&r.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&r.Description,
			validation.When(r.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
	if err != nil {
		return err
	}

	err = r.IPv4.validate(validationis.IPv4.Error(validationErrorInvalidIPv4Address))
	if err != nil {
		return validation.Errors{"ipv4": err}
	}
	err = r.IPv6.validate(validationis.IPv6.Error(validationErrorInvalidIPv6Address))
	if err != nil {
		return validation.Errors{"ipv6": err}
	}
	if !r.IPv4.isSet() && !r.IPv6.isSet() {
		return validation.Errors{validationCommonErrorField: errors.New("at least one boot URL or DNS server must be specified")}
	}

	return util.ValidateLabels(r.Labels)
}

// ToProto builds the Core request that creates the DhcpOptionSet with ID `dosID` for the Tenant in `org`
func (r *APIDhcpOptionSetCreateRequest) ToProto(dosID uuid.UUID, org string) *corev1.CreateDhcpOptionSetRequest {
	metadata := &corev1.Metadata{
		Name:   r.Name,
		Labels: cdbm.Labels(r.Labels).ToProto(),
	}
	if r.Description != nil {
		metadata.Description = *r.Description
	}
	return &corev1.CreateDhcpOptionSetRequest{
		Id:                   &corev1.DhcpOptionSetId{Value: dosID.String()},
		TenantOrganizationId: org,
		Metadata:             metadata,
		Options: &corev1.DhcpOptions{
			V4: r.IPv4.applyToV4(nil),
			V6: r.IPv6.applyToV6(nil),
		},
	}
}

// APIDhcpOptionSetGetAllRequest is the data structure to capture the query parameters of a request to list DhcpOptionSets
type APIDhcpOptionSetGetAllRequest struct {
	// SiteID filters by Site; repeat it to filter on more than one Site
	SiteID []string `query:"siteId"`
	// Status filters by Status; repeat it to match more than one Status
	Status []string `query:"status"`
	// Query is the search query
	Query *string `query:"query"`
	// IncludeRelation lists the related entities to include in the response
	IncludeRelation []string `query:"includeRelation"`
}

// APIDhcpOptionSetGetRequest is the data structure to capture the query parameters of a request to retrieve a DhcpOptionSet
type APIDhcpOptionSetGetRequest struct {
	// IncludeRelation lists the related entities to include in the response
	IncludeRelation []string `query:"includeRelation"`
}

// APIDhcpOptionSetUpdateRequest is the data structure to capture a user request to update a DhcpOptionSet
type APIDhcpOptionSetUpdateRequest struct {
	// Name is the name of the DhcpOptionSet
	Name *string `json:"name"`
	// Description is the description of the DhcpOptionSet
	Description *string `json:"description"`
	// IPv4 holds the DHCPv4 values to change
	IPv4 *APIDhcpOptions `json:"ipv4"`
	// IPv6 holds the DHCPv6 values to change
	IPv6 *APIDhcpOptions `json:"ipv6"`
	// Labels is the labels of the DhcpOptionSet
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (r *APIDhcpOptionSetUpdateRequest) Validate() error {
	err := validation.ValidateStruct(r,
		validation.Field(&r.Name,
			validation.When(r.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(r.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(r.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&r.Description,
			validation.When(r.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
	)
	if err != nil {
		return err
	}

	err = r.IPv4.validate(validationis.IPv4.Error(validationErrorInvalidIPv4Address))
	if err != nil {
		return validation.Errors{"ipv4": err}
	}
	err = r.IPv6.validate(validationis.IPv6.Error(validationErrorInvalidIPv6Address))
	if err != nil {
		return validation.Errors{"ipv6": err}
	}

	return util.ValidateLabels(r.Labels)
}

// mergeOptions returns the stored options with this request's values applied
func (r *APIDhcpOptionSetUpdateRequest) mergeOptions(current *cdbm.DhcpOptions) *cdbm.DhcpOptions {
	var stored *corev1.DhcpOptions
	if current != nil {
		stored = current.DhcpOptions
	}
	return &cdbm.DhcpOptions{DhcpOptions: &corev1.DhcpOptions{
		V4: r.IPv4.applyToV4(stored.GetV4()),
		V6: r.IPv6.applyToV6(stored.GetV6()),
	}}
}

// ToProto builds the Core request that applies this update to `dos`, the current DB record;
// its version guards against overwriting a change made on the Site
func (r *APIDhcpOptionSetUpdateRequest) ToProto(dos *cdbm.DhcpOptionSet) *corev1.UpdateDhcpOptionSetRequest {
	dosProto := dos.ToProto()
	metadata := dosProto.Metadata
	if r.Name != nil {
		metadata.Name = *r.Name
	}
	if r.Description != nil {
		metadata.Description = *r.Description
	}
	if r.Labels != nil {
		metadata.Labels = cdbm.Labels(r.Labels).ToProto()
	}
	req := &corev1.UpdateDhcpOptionSetRequest{
		Id:                   dosProto.Id,
		TenantOrganizationId: dosProto.TenantOrganizationId,
		Metadata:             metadata,
		Options:              r.mergeOptions(dos.Options).DhcpOptions,
	}
	if dos.Version != "" {
		req.IfVersionMatch = &dos.Version
	}
	return req
}

// APIDhcpOptionSet is the data structure to capture API representation of a DhcpOptionSet
type APIDhcpOptionSet struct {
	// ID is the unique UUID v4 identifier for the DhcpOptionSet
	ID string `json:"id"`
	// Name is the name of the DhcpOptionSet
	Name string `json:"name"`
	// Description is the description of the DhcpOptionSet
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the Tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// IPv4 holds the DHCPv4 values
	IPv4 APIDhcpOptions `json:"ipv4"`
	// IPv6 holds the DHCPv6 values
	IPv6 APIDhcpOptions `json:"ipv6"`
	// Labels is the labels of the DhcpOptionSet
	Labels APILabels `json:"labels"`
	// Status is the status of the DhcpOptionSet
	Status cdbm.DhcpOptionSetStatus `json:"status"`
	// StatusHistory is the status detail records for the DhcpOptionSet over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created indicates the ISO datetime string for when the DhcpOptionSet was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the DhcpOptionSet was last updated
	Updated time.Time `json:"updated"`
}

// FromDBModel populates this APIDhcpOptionSet from its DB layer representation and the
// supplied status detail history. A nil DB model is a no-op.
func (a *APIDhcpOptionSet) FromDBModel(dos *cdbm.DhcpOptionSet, dbsds []cdbm.StatusDetail) {
	if dos == nil {
		return
	}

	a.ID = dos.ID.String()
	a.Name = dos.Name
	a.Description = dos.Description
	a.SiteID = dos.SiteID.String()
	a.TenantID = dos.TenantID.String()
	a.Labels = APILabels(dos.Labels)
	a.Status = dos.Status
	a.Created = dos.Created
	a.Updated = dos.Updated

	var options *corev1.DhcpOptions
	if dos.Options != nil {
		options = dos.Options.DhcpOptions
	}
	a.IPv4.fromProto(options.GetV4().GetBootUrl(), options.GetV4().GetDnsServers())
	a.IPv6.fromProto(options.GetV6().GetBootUrl(), options.GetV6().GetDnsServers())

	a.Site = nil
	if dos.Site != nil {
		a.Site = NewAPISiteSummary(dos.Site)
	}

	a.Tenant = nil
	if dos.Tenant != nil {
		a.Tenant = NewAPITenantSummary(dos.Tenant)
	}

	a.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		a.StatusHistory = append(a.StatusHistory, NewAPIStatusDetail(dbsd))
	}
}
