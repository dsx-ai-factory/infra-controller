// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"math/big"
	"net/http"
	"net/netip"
	"os"
	"reflect"
	"regexp"
	"sort"
	"strconv"
	"strings"

	urfavecli "github.com/urfave/cli/v2"
	"gopkg.in/yaml.v3"
)

const (
	bootstrapPageSize = 100
	bootstrapMaxPages = 1000
)

var (
	bootstrapAliasPattern = regexp.MustCompile(`^[A-Za-z][A-Za-z0-9_-]*$`)
	bootstrapRefPattern   = regexp.MustCompile(`\$\{([^{}]+)\}`)

	errInvalidBootstrapManifest  = errors.New("invalid site prerequisite manifest")
	errInvalidBootstrapResource  = errors.New("invalid site prerequisite resource")
	errBootstrapReference        = errors.New("invalid site prerequisite reference")
	errBootstrapResponse         = errors.New("invalid site prerequisite API response")
	errBootstrapDrift            = errors.New("site prerequisite resource drift")
	errBootstrapClientRequired   = errors.New("REST client is required")
	errBootstrapManifestRequired = errors.New("manifest is required")
	errBootstrapSpecRequired     = errors.New("OpenAPI spec is required")
)

// sitePrerequisiteManifest is a declarative, replayable site bring-up plan.
// Managed resource requests are passed through to their existing REST
// operations after ${...} references have been resolved. Site IP Blocks are
// selected read-only after the Site reports its fabric-prefix inventory.
type sitePrerequisiteManifest struct {
	Provider      bootstrapOrganization         `yaml:"provider"`
	Tenant        bootstrapOrganization         `yaml:"tenant"`
	Site          *bootstrapResource            `yaml:"site"`
	SiteIPBlocks  *bootstrapExistingResource    `yaml:"siteIpBlocks,omitempty"`
	InstanceTypes map[string]*bootstrapResource `yaml:"instanceTypes,omitempty"`
	Allocations   map[string]*bootstrapResource `yaml:"allocations,omitempty"`
	VPCs          map[string]*bootstrapResource `yaml:"vpcs,omitempty"`
	VPCPrefixes   map[string]*bootstrapResource `yaml:"vpcPrefixes,omitempty"`
	Instances     map[string]*bootstrapResource `yaml:"instances,omitempty"`
}

type bootstrapOrganization struct {
	Org string `yaml:"org"`
	ID  string `yaml:"id,omitempty"`
}

type bootstrapResource struct {
	ID      string         `yaml:"id,omitempty"`
	Request map[string]any `yaml:"request"`
}

type bootstrapExistingResource struct {
	ID    string         `yaml:"id,omitempty"`
	Match map[string]any `yaml:"match,omitempty"`
}

type bootstrapResourceAPI struct {
	category       string
	displayName    string
	providerScoped bool
	list           resolvedOp
	create         resolvedOp
	get            resolvedOp
	itemIDParam    string
}

type bootstrapResourceGroup struct {
	api       bootstrapResourceAPI
	resources map[string]*bootstrapResource
}

type siteBootstrapOperations struct {
	serviceAccount resolvedOp
	provider       resolvedOp
	tenant         resolvedOp
	site           bootstrapResourceAPI
	siteIPBlock    bootstrapResourceAPI
	instanceType   bootstrapResourceAPI
	allocation     bootstrapResourceAPI
	vpc            bootstrapResourceAPI
	vpcPrefix      bootstrapResourceAPI
	instance       bootstrapResourceAPI
}

type siteBootstrap struct {
	client     *Client
	manifest   *sitePrerequisiteManifest
	progress   io.Writer
	operations *siteBootstrapOperations
	references bootstrapReferences
}

type bootstrapReferences map[string]any

type bootstrapServiceAccount struct {
	Enabled                  bool    `json:"enabled"`
	InfrastructureProviderID *string `json:"infrastructureProviderId"`
	TenantID                 *string `json:"tenantId"`
}

func addSiteBootstrapCommand(commands []*urfavecli.Command, spec *Spec) []*urfavecli.Command {
	for _, command := range commands {
		if command.Name != "site" {
			continue
		}
		command.Subcommands = append(command.Subcommands, siteBootstrapCommand(spec))
		sort.Slice(command.Subcommands, func(i, j int) bool {
			return command.Subcommands[i].Name < command.Subcommands[j].Name
		})
		return commands
	}

	return append(commands, &urfavecli.Command{
		Name:        "site",
		Usage:       "site operations",
		Subcommands: []*urfavecli.Command{siteBootstrapCommand(spec)},
	})
}

func siteBootstrapCommand(spec *Spec) *urfavecli.Command {
	return &urfavecli.Command{
		Name:      "bootstrap",
		Usage:     "Create or verify site prerequisite resources from a manifest",
		UsageText: binaryName + " site bootstrap --file <site-prerequisites.yaml> [--output-file <resolved.yaml>]",
		Flags: []urfavecli.Flag{
			&urfavecli.StringFlag{
				Name:     "file",
				Aliases:  []string{"f"},
				Usage:    "Input YAML manifest path (use - for stdin)",
				Required: true,
			},
			&urfavecli.StringFlag{
				Name:  "output-file",
				Usage: "Write the replayable manifest with resolved resource IDs to this path (use - for stdout)",
				Value: "-",
			},
		},
		Action: func(c *urfavecli.Context) error {
			stdin := io.Reader(os.Stdin)
			stdout := io.Writer(os.Stdout)
			stderr := io.Writer(os.Stderr)
			if c.App.Reader != nil {
				stdin = c.App.Reader
			}
			if c.App.Writer != nil {
				stdout = c.App.Writer
			}
			if c.App.ErrWriter != nil {
				stderr = c.App.ErrWriter
			}

			manifest, err := readSitePrerequisiteManifest(c.String("file"), stdin)
			if err != nil {
				return err
			}

			if manifest.Provider.Org != "" && c.String("org") == "" {
				if err := c.Set("org", manifest.Provider.Org); err != nil {
					return fmt.Errorf("using provider.org as the CLI organization: %w", err)
				}
			}

			client, err := clientFromContext(c)
			if err != nil {
				return err
			}
			if manifest.Provider.Org == "" {
				manifest.Provider.Org = client.Org
			}

			bootstrap, err := newSiteBootstrap(spec, client, manifest, stderr)
			if err != nil {
				return fmt.Errorf("preparing site prerequisite bootstrap: %w", err)
			}
			if err := bootstrap.apply(); err != nil {
				return fmt.Errorf("bootstrapping site prerequisites: %w", err)
			}

			if err := writeSitePrerequisiteManifest(c.String("output-file"), stdout, manifest); err != nil {
				return err
			}
			return nil
		},
	}
}

func newSiteBootstrap(spec *Spec, client *Client, manifest *sitePrerequisiteManifest, progress io.Writer) (*siteBootstrap, error) {
	operations, err := newSiteBootstrapOperations(spec)
	if err != nil {
		return nil, err
	}
	if progress == nil {
		progress = io.Discard
	}
	return &siteBootstrap{
		client:     client,
		manifest:   manifest,
		progress:   progress,
		operations: operations,
		references: bootstrapReferences{},
	}, nil
}

func newSiteBootstrapOperations(spec *Spec) (*siteBootstrapOperations, error) {
	if spec == nil {
		return nil, errBootstrapSpecRequired
	}
	index := newOperationIndex(spec)

	serviceAccount, err := index.require("get-current-service-account")
	if err != nil {
		return nil, err
	}
	provider, err := index.require("get-current-infrastructure-provider")
	if err != nil {
		return nil, err
	}
	tenant, err := index.require("get-current-tenant")
	if err != nil {
		return nil, err
	}

	operations := new(siteBootstrapOperations)
	operations.serviceAccount = serviceAccount
	operations.provider = provider
	operations.tenant = tenant
	resources := []struct {
		target         *bootstrapResourceAPI
		category       string
		displayName    string
		listID         string
		createID       string
		getID          string
		providerScoped bool
	}{
		{target: &operations.site, category: "site", displayName: "site", listID: "get-all-site", createID: "", getID: "get-site", providerScoped: true},
		{target: &operations.siteIPBlock, category: "siteIpBlocks", displayName: "site IP block", listID: "get-all-ipblock", createID: "", getID: "get-ipblock", providerScoped: true},
		{target: &operations.instanceType, category: "instanceTypes", displayName: "instance type", listID: "get-all-instance-type", createID: "create-instance-type", getID: "get-instance-type", providerScoped: true},
		{target: &operations.allocation, category: "allocations", displayName: "allocation", listID: "get-all-allocation", createID: "create-allocation", getID: "get-allocation", providerScoped: true},
		{target: &operations.vpc, category: "vpcs", displayName: "VPC", listID: "get-all-vpc", createID: "create-vpc", getID: "get-vpc", providerScoped: false},
		{target: &operations.vpcPrefix, category: "vpcPrefixes", displayName: "VPC prefix", listID: "get-all-vpc-prefix", createID: "create-vpc-prefix", getID: "get-vpc-prefix", providerScoped: false},
		{target: &operations.instance, category: "instances", displayName: "instance", listID: "get-all-instance", createID: "create-instance", getID: "get-instance", providerScoped: false},
	}
	for _, resource := range resources {
		*resource.target, err = index.bootstrapResourceAPI(
			resource.category,
			resource.displayName,
			resource.listID,
			resource.createID,
			resource.getID,
			resource.providerScoped,
		)
		if err != nil {
			return nil, err
		}
	}
	return operations, nil
}

func (index operationIndex) bootstrapResourceAPI(category, displayName, listID, createID, getID string, providerScoped bool) (bootstrapResourceAPI, error) {
	list, err := index.require(listID)
	if err != nil {
		return bootstrapResourceAPI{}, err
	}
	get, err := index.require(getID)
	if err != nil {
		return bootstrapResourceAPI{}, err
	}
	itemIDParam, err := get.resourceIDParameter()
	if err != nil {
		return bootstrapResourceAPI{}, err
	}
	var create resolvedOp
	if createID != "" {
		create, err = index.require(createID)
		if err != nil {
			return bootstrapResourceAPI{}, err
		}
	}
	return bootstrapResourceAPI{
		category:       category,
		displayName:    displayName,
		providerScoped: providerScoped,
		list:           list,
		create:         create,
		get:            get,
		itemIDParam:    itemIDParam,
	}, nil
}

func readSitePrerequisiteManifest(filename string, stdin io.Reader) (*sitePrerequisiteManifest, error) {
	var data []byte
	var err error
	if filename == "-" {
		data, err = io.ReadAll(stdin)
	} else {
		data, err = os.ReadFile(filename)
	}
	if err != nil {
		return nil, fmt.Errorf("reading site prerequisite manifest: %w", err)
	}

	decoder := yaml.NewDecoder(bytes.NewReader(data))
	decoder.KnownFields(true)
	manifest := &sitePrerequisiteManifest{}
	if err := decoder.Decode(manifest); err != nil {
		return nil, fmt.Errorf("parsing site prerequisite manifest: %w", err)
	}
	var extra any
	if err := decoder.Decode(&extra); !errors.Is(err, io.EOF) {
		if err == nil {
			return nil, fmt.Errorf("parsing site prerequisite manifest: %w: multiple YAML documents are not supported", errInvalidBootstrapManifest)
		}
		return nil, fmt.Errorf("parsing site prerequisite manifest: %w", err)
	}
	if err := manifest.validate(); err != nil {
		return nil, fmt.Errorf("validating site prerequisite manifest: %w", err)
	}
	return manifest, nil
}

func writeSitePrerequisiteManifest(filename string, stdout io.Writer, manifest *sitePrerequisiteManifest) error {
	data, err := yaml.Marshal(manifest)
	if err != nil {
		return fmt.Errorf("encoding resolved site prerequisite manifest: %w", err)
	}
	if filename == "-" {
		if _, err := stdout.Write(data); err != nil {
			return fmt.Errorf("writing resolved site prerequisite manifest: %w", err)
		}
		return nil
	}
	if err := os.WriteFile(filename, data, 0o600); err != nil {
		return fmt.Errorf("writing resolved site prerequisite manifest: %w", err)
	}
	return nil
}

func (manifest *sitePrerequisiteManifest) validate() error {
	if manifest.Tenant.Org == "" {
		return fmt.Errorf("%w: tenant.org is required", errInvalidBootstrapManifest)
	}
	if manifest.Site == nil {
		return fmt.Errorf("%w: site is required", errInvalidBootstrapManifest)
	}
	if err := manifest.Site.validate("site"); err != nil {
		return err
	}
	if manifest.SiteIPBlocks != nil {
		if err := manifest.SiteIPBlocks.validate("siteIpBlocks"); err != nil {
			return err
		}
	}

	groups := []struct {
		name      string
		resources map[string]*bootstrapResource
	}{
		{name: "instanceTypes", resources: manifest.InstanceTypes},
		{name: "allocations", resources: manifest.Allocations},
		{name: "vpcs", resources: manifest.VPCs},
		{name: "vpcPrefixes", resources: manifest.VPCPrefixes},
		{name: "instances", resources: manifest.Instances},
	}
	for _, group := range groups {
		for alias, resource := range group.resources {
			if !bootstrapAliasPattern.MatchString(alias) {
				return fmt.Errorf("%w: %s alias %q must start with a letter and contain only letters, digits, underscores, or hyphens", errInvalidBootstrapManifest, group.name, alias)
			}
			if err := resource.validate(group.name + "." + alias); err != nil {
				return err
			}
		}
	}
	return nil
}

func (resource *bootstrapResource) validate(path string) error {
	if resource == nil {
		return fmt.Errorf("%w: %s must not be null", errInvalidBootstrapResource, path)
	}
	if len(resource.Request) == 0 {
		return fmt.Errorf("%w: %s.request is required", errInvalidBootstrapResource, path)
	}
	name, ok := resource.Request["name"].(string)
	if !ok || strings.TrimSpace(name) == "" {
		return fmt.Errorf("%w: %s.request.name is required", errInvalidBootstrapResource, path)
	}
	return nil
}

func (resource *bootstrapExistingResource) validate(path string) error {
	if resource == nil {
		return fmt.Errorf("%w: %s must not be null", errInvalidBootstrapResource, path)
	}
	if resource.ID == "" && len(resource.Match) == 0 {
		return fmt.Errorf("%w: %s.id or %s.match is required", errInvalidBootstrapResource, path, path)
	}
	return nil
}

func (bootstrap *siteBootstrap) apply() error {
	if bootstrap.client == nil {
		return errBootstrapClientRequired
	}
	if bootstrap.manifest == nil {
		return errBootstrapManifestRequired
	}
	if bootstrap.manifest.Provider.Org == "" {
		return fmt.Errorf("%w: provider.org is required when applying a manifest", errInvalidBootstrapManifest)
	}

	originalOrg := bootstrap.client.Org
	defer func() {
		bootstrap.client.Org = originalOrg
	}()

	if err := bootstrap.initializeOrganizations(); err != nil {
		return err
	}

	site, err := bootstrap.ensureResource(bootstrap.operations.site, "site", bootstrap.manifest.Site)
	if err != nil {
		return err
	}
	bootstrap.references["site"] = site

	if bootstrap.manifest.SiteIPBlocks != nil {
		siteIPBlock, err := bootstrap.discoverExistingResource(bootstrap.operations.siteIPBlock, "siteIpBlocks", bootstrap.manifest.SiteIPBlocks)
		if err != nil {
			return err
		}
		bootstrap.references["siteIpBlocks"] = siteIPBlock
	}
	for _, group := range bootstrap.operations.managedGroups(bootstrap.manifest) {
		if err := bootstrap.ensureResources(group.api, group.resources); err != nil {
			return err
		}
	}
	return nil
}

func (bootstrap *siteBootstrap) initializeOrganizations() error {
	manifest := bootstrap.manifest
	initialized, err := bootstrap.initializeServiceAccount(manifest.Provider.Org)
	if err != nil {
		return err
	}
	if initialized {
		return nil
	}

	provider, err := bootstrap.resolveCurrentOrganization(bootstrap.operations.provider, manifest.Provider.Org, "provider")
	if err != nil {
		return err
	}
	manifest.Provider.ID, err = bootstrapResponseID(provider)
	if err != nil {
		return fmt.Errorf("resolving provider: %w", err)
	}
	bootstrap.references["provider"] = provider

	tenant, err := bootstrap.resolveCurrentOrganization(bootstrap.operations.tenant, manifest.Tenant.Org, "tenant")
	if err != nil {
		return err
	}
	manifest.Tenant.ID, err = bootstrapResponseID(tenant)
	if err != nil {
		return fmt.Errorf("resolving tenant: %w", err)
	}
	bootstrap.references["tenant"] = tenant
	return nil
}

func (bootstrap *siteBootstrap) initializeServiceAccount(org string) (bool, error) {
	bootstrap.client.Org = org
	body, _, err := bootstrap.operations.serviceAccount.execute(bootstrap.client, nil, nil, nil)
	if err != nil {
		return false, fmt.Errorf("resolving service account for org %q: %w", org, err)
	}
	var status bootstrapServiceAccount
	if err := json.Unmarshal(body, &status); err != nil {
		return false, fmt.Errorf("decoding service account for org %q: %w", org, err)
	}
	if !status.Enabled {
		fmt.Fprintf(bootstrap.progress, "service account mode is not enabled for %s; resolving provider and tenant separately\n", org)
		return false, nil
	}
	if bootstrap.manifest.Provider.Org != bootstrap.manifest.Tenant.Org {
		return false, fmt.Errorf("%w: service account mode requires provider.org and tenant.org to match", errInvalidBootstrapManifest)
	}
	if status.InfrastructureProviderID == nil || *status.InfrastructureProviderID == "" || status.TenantID == nil || *status.TenantID == "" {
		return false, fmt.Errorf("%w: enabled service account response for org %q is missing provider or tenant ID", errBootstrapResponse, org)
	}

	provider := map[string]any{"id": *status.InfrastructureProviderID, "org": org}
	tenant := map[string]any{"id": *status.TenantID, "org": org}
	bootstrap.manifest.Provider.ID = *status.InfrastructureProviderID
	bootstrap.manifest.Tenant.ID = *status.TenantID
	bootstrap.references["serviceAccount"] = map[string]any{
		"enabled":                  true,
		"infrastructureProviderId": *status.InfrastructureProviderID,
		"tenantId":                 *status.TenantID,
	}
	bootstrap.references["provider"] = provider
	bootstrap.references["tenant"] = tenant
	fmt.Fprintf(bootstrap.progress, "resolved service account %s (provider %s, tenant %s)\n", org, *status.InfrastructureProviderID, *status.TenantID)
	return true, nil
}

func (bootstrap *siteBootstrap) resolveCurrentOrganization(operation resolvedOp, org, displayName string) (map[string]any, error) {
	bootstrap.client.Org = org
	body, _, err := operation.execute(bootstrap.client, nil, nil, nil)
	if err != nil {
		return nil, fmt.Errorf("resolving %s for org %q: %w", displayName, org, err)
	}
	response, err := decodeBootstrapObject(body)
	if err != nil {
		return nil, fmt.Errorf("decoding %s for org %q: %w", displayName, org, err)
	}
	id, err := bootstrapResponseID(response)
	if err != nil {
		return nil, fmt.Errorf("resolving %s for org %q: %w", displayName, org, err)
	}
	fmt.Fprintf(bootstrap.progress, "resolved %s %s (%s)\n", displayName, org, id)
	return response, nil
}

func (operations *siteBootstrapOperations) managedGroups(manifest *sitePrerequisiteManifest) []bootstrapResourceGroup {
	return []bootstrapResourceGroup{
		{api: operations.instanceType, resources: manifest.InstanceTypes},
		{api: operations.allocation, resources: manifest.Allocations},
		{api: operations.vpc, resources: manifest.VPCs},
		{api: operations.vpcPrefix, resources: manifest.VPCPrefixes},
		{api: operations.instance, resources: manifest.Instances},
	}
}

func (bootstrap *siteBootstrap) ensureResources(api bootstrapResourceAPI, resources map[string]*bootstrapResource) error {
	resolved := map[string]any{}
	bootstrap.references[api.category] = resolved
	for _, alias := range sortedBootstrapAliases(resources) {
		response, err := bootstrap.ensureResource(api, alias, resources[alias])
		if err != nil {
			return err
		}
		resolved[alias] = response
	}
	return nil
}

func (bootstrap *siteBootstrap) ensureResource(api bootstrapResourceAPI, alias string, resource *bootstrapResource) (map[string]any, error) {
	resolvedValue, err := bootstrap.references.resolve(resource.Request)
	if err != nil {
		return nil, fmt.Errorf("resolving %s %q request: %w", api.displayName, alias, err)
	}
	request, ok := resolvedValue.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("resolving %s %q request: %w: expected an object", api.displayName, alias, errInvalidBootstrapResource)
	}
	if err := (&bootstrapResource{Request: request}).validate(api.manifestPath(alias)); err != nil {
		return nil, fmt.Errorf("resolving %s %q request: %w", api.displayName, alias, err)
	}
	name, _ := request["name"].(string)

	bootstrap.client.Org = api.organization(bootstrap.manifest)

	candidateID := resource.ID
	if candidateID == "" {
		candidateID, _ = request["id"].(string)
	}
	if candidateID != "" {
		response, err := api.getResource(bootstrap.client, candidateID)
		if err == nil {
			if err := api.verify(alias, request, response); err != nil {
				return nil, err
			}
			resource.ID = candidateID
			fmt.Fprintf(bootstrap.progress, "reused %s %s (%s)\n", api.displayName, name, candidateID)
			return response, nil
		}
		if !isBootstrapNotFound(err) {
			return nil, fmt.Errorf("retrieving %s %q by ID %q: %w", api.displayName, alias, candidateID, err)
		}
	}

	response, found, err := api.findByName(bootstrap.client, request)
	if err != nil {
		return nil, fmt.Errorf("finding %s %q: %w", api.displayName, alias, err)
	}
	if found {
		if err := api.verify(alias, request, response); err != nil {
			return nil, err
		}
		resource.ID, err = bootstrapResponseID(response)
		if err != nil {
			return nil, fmt.Errorf("finding %s %q: %w", api.displayName, alias, err)
		}
		fmt.Fprintf(bootstrap.progress, "reused %s %s (%s)\n", api.displayName, name, resource.ID)
		return response, nil
	}
	if api.create.op == nil {
		return nil, fmt.Errorf("%w: required %s %q was not found", errInvalidBootstrapResource, api.displayName, name)
	}

	requestBody, err := json.Marshal(request)
	if err != nil {
		return nil, fmt.Errorf("encoding %s %q request: %w", api.displayName, alias, err)
	}
	body, _, err := api.create.execute(bootstrap.client, nil, nil, requestBody)
	if err != nil {
		var apiErr *APIError
		if errors.As(err, &apiErr) && apiErr.StatusCode == http.StatusConflict {
			response, found, findErr := api.findByName(bootstrap.client, request)
			if findErr == nil && found {
				if verifyErr := api.verify(alias, request, response); verifyErr != nil {
					return nil, verifyErr
				}
				resource.ID, findErr = bootstrapResponseID(response)
				if findErr == nil {
					fmt.Fprintf(bootstrap.progress, "reused %s %s (%s) after a concurrent create\n", api.displayName, name, resource.ID)
					return response, nil
				}
			}
		}
		return nil, fmt.Errorf("creating %s %q: %w", api.displayName, alias, err)
	}
	response, err = decodeBootstrapObject(body)
	if err != nil {
		return nil, fmt.Errorf("decoding created %s %q: %w", api.displayName, alias, err)
	}
	resource.ID, err = bootstrapResponseID(response)
	if err != nil {
		return nil, fmt.Errorf("decoding created %s %q: %w", api.displayName, alias, err)
	}
	fmt.Fprintf(bootstrap.progress, "created %s %s (%s)\n", api.displayName, name, resource.ID)
	return response, nil
}

func (bootstrap *siteBootstrap) discoverExistingResource(api bootstrapResourceAPI, alias string, resource *bootstrapExistingResource) (map[string]any, error) {
	bootstrap.client.Org = api.organization(bootstrap.manifest)

	if resource.ID != "" {
		response, err := api.getResource(bootstrap.client, resource.ID)
		if err == nil {
			if len(resource.Match) > 0 {
				match, matchErr := bootstrap.resolveExistingResourceMatch(api, alias, resource.Match)
				if matchErr != nil {
					return nil, matchErr
				}
				if !bootstrapValueMatches(match, api.normalizePrefix(response)) {
					return nil, fmt.Errorf("%w: existing %s %q does not match the manifest selector", errBootstrapDrift, api.displayName, alias)
				}
			}
			fmt.Fprintf(bootstrap.progress, "resolved %s %s (%s)\n", api.displayName, alias, resource.ID)
			return response, nil
		}
		if !isBootstrapNotFound(err) || len(resource.Match) == 0 {
			return nil, fmt.Errorf("retrieving %s %q by ID %q: %w", api.displayName, alias, resource.ID, err)
		}
	}

	match, err := bootstrap.resolveExistingResourceMatch(api, alias, resource.Match)
	if err != nil {
		return nil, err
	}
	response, found, err := api.findMatching(bootstrap.client, match)
	if err != nil {
		return nil, fmt.Errorf("finding %s %q: %w", api.displayName, alias, err)
	}
	if !found {
		return nil, fmt.Errorf("%w: %s %q is not available yet; wait for Site fabric-prefix inventory and rerun", errInvalidBootstrapResource, api.displayName, alias)
	}
	resource.ID, err = bootstrapResponseID(response)
	if err != nil {
		return nil, fmt.Errorf("finding %s %q: %w", api.displayName, alias, err)
	}
	fmt.Fprintf(bootstrap.progress, "resolved %s %s (%s)\n", api.displayName, alias, resource.ID)
	return response, nil
}

func (bootstrap *siteBootstrap) resolveExistingResourceMatch(api bootstrapResourceAPI, alias string, selector map[string]any) (map[string]any, error) {
	resolvedValue, err := bootstrap.references.resolve(selector)
	if err != nil {
		return nil, fmt.Errorf("resolving %s %q match: %w", api.displayName, alias, err)
	}
	match, ok := resolvedValue.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("resolving %s %q match: %w: expected an object", api.displayName, alias, errInvalidBootstrapResource)
	}
	return api.normalizePrefix(match), nil
}

func (api bootstrapResourceAPI) organization(manifest *sitePrerequisiteManifest) string {
	if api.providerScoped {
		return manifest.Provider.Org
	}
	return manifest.Tenant.Org
}

func (api bootstrapResourceAPI) manifestPath(alias string) string {
	if alias == api.category {
		return api.category
	}
	return api.category + "." + alias
}

func (api bootstrapResourceAPI) getResource(client *Client, id string) (map[string]any, error) {
	body, _, err := api.get.execute(client, map[string]string{api.itemIDParam: id}, nil, nil)
	if err != nil {
		return nil, err
	}
	return decodeBootstrapObject(body)
}

func (api bootstrapResourceAPI) findByName(client *Client, request map[string]any) (map[string]any, bool, error) {
	name, _ := request["name"].(string)
	var matches []map[string]any
	for page := 1; page <= bootstrapMaxPages; page++ {
		query := map[string]string{
			"query":      name,
			"pageNumber": strconv.Itoa(page),
			"pageSize":   strconv.Itoa(bootstrapPageSize),
		}
		body, _, err := api.list.execute(client, nil, query, nil)
		if err != nil {
			return nil, false, err
		}
		items, err := decodeBootstrapList(body)
		if err != nil {
			return nil, false, err
		}
		for _, item := range items {
			if itemName, _ := item["name"].(string); itemName == name && bootstrapIdentityMatches(request, item) {
				matches = append(matches, item)
			}
		}
		if len(items) < bootstrapPageSize {
			break
		}
	}
	if len(matches) == 0 {
		return nil, false, nil
	}
	if len(matches) > 1 {
		return nil, false, fmt.Errorf("%w: multiple resources named %q matched the manifest scope", errInvalidBootstrapResource, name)
	}
	return matches[0], true, nil
}

func (api bootstrapResourceAPI) findMatching(client *Client, match map[string]any) (map[string]any, bool, error) {
	query := api.queryFromMatch(match)
	var matches []map[string]any
	for page := 1; page <= bootstrapMaxPages; page++ {
		query["pageNumber"] = strconv.Itoa(page)
		query["pageSize"] = strconv.Itoa(bootstrapPageSize)
		body, _, err := api.list.execute(client, nil, query, nil)
		if err != nil {
			return nil, false, err
		}
		items, err := decodeBootstrapList(body)
		if err != nil {
			return nil, false, err
		}
		for _, item := range items {
			if bootstrapValueMatches(match, api.normalizePrefix(item)) {
				matches = append(matches, item)
			}
		}
		if len(items) < bootstrapPageSize {
			break
		}
	}
	if len(matches) == 0 {
		return nil, false, nil
	}
	if len(matches) > 1 {
		return nil, false, fmt.Errorf("%w: multiple resources matched %s selector", errInvalidBootstrapResource, api.displayName)
	}
	return matches[0], true, nil
}

func (api bootstrapResourceAPI) queryFromMatch(match map[string]any) map[string]string {
	query := map[string]string{}
	for _, parameter := range api.list.parameters() {
		if parameter.In != "query" {
			continue
		}
		value, ok := match[parameter.Name]
		if !ok {
			continue
		}
		switch value.(type) {
		case string, bool, json.Number, int, int8, int16, int32, int64, uint, uint8, uint16, uint32, uint64, float32, float64:
			query[parameter.Name] = fmt.Sprint(value)
		}
	}
	return query
}

func (api bootstrapResourceAPI) verify(alias string, request, actual map[string]any) error {
	differences := bootstrapSubsetDifferences(api.normalizePrefix(request), api.normalizePrefix(actual), "")
	if len(differences) == 0 {
		return nil
	}
	return fmt.Errorf("%w: existing %s %q does not match the manifest request: %s", errBootstrapDrift, api.displayName, alias, strings.Join(differences, "; "))
}

// normalizePrefix compares address text in the format returned by the API
// without rewriting the manifest.
func (api bootstrapResourceAPI) normalizePrefix(resource map[string]any) map[string]any {
	prefix, ok := resource["prefix"].(string)
	if !ok {
		return resource
	}

	var normalized string
	switch api.category {
	case "vpcPrefixes":
		parsed, err := netip.ParsePrefix(prefix)
		if err != nil {
			return resource
		}
		// Keep host bits: the API rejects unaligned prefixes instead of masking them.
		normalized = parsed.String()
	case "siteIpBlocks":
		parsed, err := netip.ParseAddr(prefix)
		if err != nil {
			return resource
		}
		normalized = parsed.String()
	default:
		return resource
	}

	fields := maps.Clone(resource)
	fields["prefix"] = normalized
	return fields
}

func sortedBootstrapAliases[T any](resources map[string]T) []string {
	aliases := make([]string, 0, len(resources))
	for alias := range resources {
		aliases = append(aliases, alias)
	}
	sort.Strings(aliases)
	return aliases
}

func bootstrapIdentityMatches(request, actual map[string]any) bool {
	for _, field := range []string{"siteId", "tenantId", "vpcId"} {
		expected, requested := request[field]
		if !requested {
			continue
		}
		observed, present := actual[field]
		if !present || !bootstrapScalarEqual(expected, observed) {
			return false
		}
	}
	return true
}

func bootstrapValueMatches(expected, actual any) bool {
	switch expectedValue := expected.(type) {
	case map[string]any:
		actualValue, ok := actual.(map[string]any)
		if !ok {
			return false
		}
		for key, value := range expectedValue {
			observed, present := actualValue[key]
			if !present || !bootstrapValueMatches(value, observed) {
				return false
			}
		}
		return true
	case []any:
		actualValue, ok := actual.([]any)
		if !ok || len(expectedValue) != len(actualValue) {
			return false
		}
		for index, value := range expectedValue {
			if !bootstrapValueMatches(value, actualValue[index]) {
				return false
			}
		}
		return true
	default:
		return bootstrapScalarEqual(expected, actual)
	}
}

// bootstrapSubsetDifferences compares fields returned by the API with the
// requested fields. Write-only request fields are deliberately skipped when
// the API omits them from its response.
func bootstrapSubsetDifferences(expected, actual any, path string) []string {
	switch expectedValue := expected.(type) {
	case map[string]any:
		actualValue, ok := actual.(map[string]any)
		if !ok {
			return []string{fmt.Sprintf("%s has type %T, want object", bootstrapPath(path), actual)}
		}
		keys := make([]string, 0, len(expectedValue))
		for key := range expectedValue {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		var differences []string
		for _, key := range keys {
			observed, present := actualValue[key]
			if !present {
				continue
			}
			differences = append(differences, bootstrapSubsetDifferences(expectedValue[key], observed, bootstrapJoinPath(path, key))...)
		}
		return differences
	case []any:
		actualValue, ok := actual.([]any)
		if !ok {
			return []string{fmt.Sprintf("%s has type %T, want array", bootstrapPath(path), actual)}
		}
		if len(expectedValue) > len(actualValue) {
			return []string{fmt.Sprintf("%s has %d items, want at least %d", bootstrapPath(path), len(actualValue), len(expectedValue))}
		}
		var differences []string
		for index := range expectedValue {
			differences = append(differences, bootstrapSubsetDifferences(expectedValue[index], actualValue[index], bootstrapJoinPath(path, strconv.Itoa(index)))...)
		}
		return differences
	default:
		if bootstrapScalarEqual(expected, actual) {
			return nil
		}
		return []string{fmt.Sprintf("%s is %v, want %v", bootstrapPath(path), actual, expected)}
	}
}

func bootstrapScalarEqual(left, right any) bool {
	if left == nil {
		return right == nil
	}
	if right == nil {
		return false
	}
	leftEncoded, leftIsNumber := bootstrapNumberString(left)
	rightEncoded, rightIsNumber := bootstrapNumberString(right)
	if leftIsNumber != rightIsNumber {
		return false
	}
	if leftIsNumber {
		leftNumber, leftIsValid := new(big.Rat).SetString(leftEncoded)
		rightNumber, rightIsValid := new(big.Rat).SetString(rightEncoded)
		if !leftIsValid || !rightIsValid {
			return false
		}
		return leftNumber.Cmp(rightNumber) == 0
	}
	return reflect.TypeOf(left) == reflect.TypeOf(right) && reflect.DeepEqual(left, right)
}

func bootstrapNumberString(value any) (string, bool) {
	if number, ok := value.(json.Number); ok {
		return number.String(), true
	}

	reflected := reflect.ValueOf(value)
	kind := reflected.Kind()
	switch {
	case kind >= reflect.Int && kind <= reflect.Int64:
		return strconv.FormatInt(reflected.Int(), 10), true
	case kind >= reflect.Uint && kind <= reflect.Uintptr:
		return strconv.FormatUint(reflected.Uint(), 10), true
	case kind == reflect.Float32 || kind == reflect.Float64:
		return strconv.FormatFloat(reflected.Float(), 'g', -1, reflected.Type().Bits()), true
	default:
		return "", false
	}
}

func bootstrapJoinPath(base, element string) string {
	if base == "" {
		return element
	}
	return base + "." + element
}

func bootstrapPath(path string) string {
	if path == "" {
		return "value"
	}
	return path
}

func (references bootstrapReferences) resolve(value any) (any, error) {
	switch typed := value.(type) {
	case map[string]any:
		resolved := make(map[string]any, len(typed))
		for key, item := range typed {
			value, err := references.resolve(item)
			if err != nil {
				return nil, fmt.Errorf("%s: %w", key, err)
			}
			resolved[key] = value
		}
		return resolved, nil
	case []any:
		resolved := make([]any, len(typed))
		for index, item := range typed {
			value, err := references.resolve(item)
			if err != nil {
				return nil, fmt.Errorf("item %d: %w", index, err)
			}
			resolved[index] = value
		}
		return resolved, nil
	case string:
		matches := bootstrapRefPattern.FindAllStringSubmatchIndex(typed, -1)
		lastMatchEnd := 0
		for _, match := range matches {
			if bootstrapReferenceSyntaxMalformed(typed[lastMatchEnd:match[0]]) {
				return nil, fmt.Errorf("%w: malformed reference in %q", errBootstrapReference, typed)
			}
			lastMatchEnd = match[1]
		}
		if bootstrapReferenceSyntaxMalformed(typed[lastMatchEnd:]) {
			return nil, fmt.Errorf("%w: malformed reference in %q", errBootstrapReference, typed)
		}
		if len(matches) == 0 {
			return typed, nil
		}
		if len(matches) == 1 && matches[0][0] == 0 && matches[0][1] == len(typed) {
			return references.lookup(typed[matches[0][2]:matches[0][3]])
		}

		var result strings.Builder
		last := 0
		for _, match := range matches {
			result.WriteString(typed[last:match[0]])
			resolved, err := references.lookup(typed[match[2]:match[3]])
			if err != nil {
				return nil, err
			}
			fmt.Fprint(&result, resolved)
			last = match[1]
		}
		result.WriteString(typed[last:])
		return result.String(), nil
	default:
		return value, nil
	}
}

func bootstrapReferenceSyntaxMalformed(value string) bool {
	return strings.Contains(value, "${") || strings.Contains(value, "}")
}

func (references bootstrapReferences) lookup(reference string) (any, error) {
	parts := strings.Split(reference, ".")
	if len(parts) == 0 || parts[0] == "" {
		return nil, fmt.Errorf("%w: empty reference %q", errBootstrapReference, reference)
	}
	var current any = map[string]any(references)
	for _, part := range parts {
		switch typed := current.(type) {
		case map[string]any:
			value, ok := typed[part]
			if !ok {
				return nil, fmt.Errorf("%w: %q does not exist", errBootstrapReference, reference)
			}
			current = value
		case []any:
			index, err := strconv.Atoi(part)
			if err != nil || index < 0 || index >= len(typed) {
				return nil, fmt.Errorf("%w: %q does not exist", errBootstrapReference, reference)
			}
			current = typed[index]
		default:
			return nil, fmt.Errorf("%w: %q does not exist", errBootstrapReference, reference)
		}
	}
	if current == nil {
		return nil, fmt.Errorf("%w: %q resolved to null", errBootstrapReference, reference)
	}
	return current, nil
}

func decodeBootstrapObject(body []byte) (map[string]any, error) {
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var response map[string]any
	if err := decoder.Decode(&response); err != nil {
		return nil, fmt.Errorf("decoding object: %w", err)
	}
	if response == nil {
		return nil, fmt.Errorf("%w: API returned null", errBootstrapResponse)
	}
	return response, nil
}

func decodeBootstrapList(body []byte) ([]map[string]any, error) {
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var response []map[string]any
	if err := decoder.Decode(&response); err != nil {
		return nil, fmt.Errorf("decoding list: %w", err)
	}
	if response == nil {
		response = []map[string]any{}
	}
	return response, nil
}

func bootstrapResponseID(response map[string]any) (string, error) {
	id, ok := response["id"].(string)
	if !ok || id == "" {
		return "", fmt.Errorf("%w: response does not contain a non-empty string id", errBootstrapResponse)
	}
	return id, nil
}

func isBootstrapNotFound(err error) bool {
	var apiErr *APIError
	return errors.As(err, &apiErr) && apiErr.StatusCode == http.StatusNotFound
}
