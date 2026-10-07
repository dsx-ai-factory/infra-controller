// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"context"
	"fmt"
	"net/http"
	"slices"
	"sort"
	"strings"
	"time"

	"golang.org/x/crypto/ssh"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// GetSSHKeyFingerprint generates the fingerprint for a given SSH public key
func GetSSHKeyFingerprint(publicKey string) (*string, error) {
	parsedKey, _, _, _, err := ssh.ParseAuthorizedKey([]byte(publicKey))
	if err != nil {
		return nil, err
	}
	fp := ssh.FingerprintSHA256(parsedKey)
	trimmed := strings.TrimPrefix(fp, "SHA256:")
	if trimmed == fp {
		return nil, fmt.Errorf("unexpected SSH fingerprint format (missing \"SHA256:\" prefix): %s", fp)
	}
	return &trimmed, nil
}

// GetInstanceSshKeyGroupName returns the name of the SSH Key Group for an Instance
func GetInstanceSshKeyGroupName(instanceName string) string {
	return fmt.Sprintf("%s-ssh-key-group", instanceName)
}

// SshKeyGroupManager manages SSH Key Group operations
type SshKeyGroupManager struct {
	client *Client
}

// NewSshKeyGroupManager creates a new SshKeyGroupManager
func NewSshKeyGroupManager(client *Client) SshKeyGroupManager {
	return SshKeyGroupManager{client: client}
}

// CreateSshKey creates a new SSH Key (uses fingerprint as name)
func (skm SshKeyGroupManager) CreateSshKey(ctx context.Context, sshPublicKey string) (*standard.SshKey, *ApiError) {
	ctx = WithLogger(ctx, skm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, skm.client.Config.Token)

	fingerprint, err := GetSSHKeyFingerprint(sshPublicKey)
	if err != nil {
		return nil, &ApiError{Code: http.StatusBadRequest, Message: "failed to generate fingerprint: " + err.Error()}
	}
	apiSk, resp, err := skm.client.apiClient.SSHKeyAPI.CreateSshKey(ctx, skm.client.apiMetadata.Organization).
		SshKeyCreateRequest(standard.SshKeyCreateRequest{Name: *fingerprint, PublicKey: sshPublicKey}).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	return apiSk, nil
}

// IsSSHKeyGroupMatching checks if an existing group matches the wanted SSH key IDs
func (skm SshKeyGroupManager) IsSSHKeyGroupMatching(existingGroup *standard.SshKeyGroup, wantedSshKeyIDs []string) bool {
	if existingGroup.Org != nil && *existingGroup.Org != skm.client.apiMetadata.Organization {
		return false
	}
	if existingGroup.TenantId != nil && *existingGroup.TenantId != skm.client.apiMetadata.TenantID {
		return false
	}
	if len(existingGroup.SiteAssociations) != 1 {
		return false
	}
	if existingGroup.SiteAssociations[0].Site == nil || existingGroup.SiteAssociations[0].Site.Id == nil ||
		*existingGroup.SiteAssociations[0].Site.Id != skm.client.apiMetadata.SiteID {
		return false
	}
	wantedCopy := make([]string, len(wantedSshKeyIDs))
	copy(wantedCopy, wantedSshKeyIDs)
	sort.Strings(wantedCopy)
	existingSSHKeys := make([]string, 0, len(existingGroup.SshKeys))
	for _, sk := range existingGroup.SshKeys {
		if sk.Id != nil {
			existingSSHKeys = append(existingSSHKeys, *sk.Id)
		}
	}
	sort.Strings(existingSSHKeys)
	return slices.Equal(existingSSHKeys, wantedCopy)
}

// CreateSshKeyGroupForInstance creates a new SSH Key Group for the specified Instance
func (skm SshKeyGroupManager) CreateSshKeyGroupForInstance(ctx context.Context, instanceName string, sshPublicKeys []string) (*standard.SshKeyGroup, *ApiError) {
	ctx = WithLogger(ctx, skm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, skm.client.Config.Token)

	sshKeyIds := make([]string, 0, len(sshPublicKeys))
	for _, pk := range sshPublicKeys {
		fingerprint, err := GetSSHKeyFingerprint(pk)
		if err != nil {
			return nil, &ApiError{Code: http.StatusBadRequest, Message: "failed to generate fingerprint for one or more SSH public keys: " + err.Error()}
		}
		apiSk, resp, err := skm.client.apiClient.SSHKeyAPI.CreateSshKey(ctx, skm.client.apiMetadata.Organization).
			SshKeyCreateRequest(standard.SshKeyCreateRequest{Name: *fingerprint, PublicKey: pk}).Execute()
		apiErr := HandleResponseError(resp, err)
		if apiErr != nil {
			if apiErr.Code == http.StatusConflict {
				if id, ok := apiErr.Data["id"].(string); ok && id != "" {
					sshKeyIds = append(sshKeyIds, id)
					continue
				}
				return nil, &ApiError{Code: http.StatusInternalServerError, Message: "failed to get existing SSH Key ID from conflict API response data", Data: apiErr.Data}
			}
			return nil, apiErr
		}
		if apiSk.Id != nil {
			sshKeyIds = append(sshKeyIds, *apiSk.Id)
		}
	}

	retries := 1
	for {
		apiSkg, resp, err := skm.client.apiClient.SSHKeyGroupAPI.CreateSshKeyGroup(ctx, skm.client.apiMetadata.Organization).
			SshKeyGroupCreateRequest(standard.SshKeyGroupCreateRequest{
				Name:      GetInstanceSshKeyGroupName(instanceName),
				SshKeyIds: sshKeyIds,
				SiteIds:   []string{skm.client.apiMetadata.SiteID},
			}).Execute()
		apiErr := HandleResponseError(resp, err)
		if apiErr == nil {
			return apiSkg, nil
		}
		if apiErr.Code != http.StatusConflict {
			return nil, apiErr
		}
		existingID, ok := apiErr.Data["id"].(string)
		if !ok || existingID == "" {
			return nil, &ApiError{Code: http.StatusInternalServerError, Message: "failed to get existing SSH Key Group ID from conflict API response data", Data: apiErr.Data}
		}
		existingGroup, apiErr := skm.GetSshKeyGroup(ctx, existingID)
		if apiErr != nil {
			return nil, apiErr
		}
		if skm.IsSSHKeyGroupMatching(existingGroup, sshKeyIds) {
			return existingGroup, nil
		}
		if retries <= 0 {
			return nil, apiErr
		}
		retries--
		if delErr := skm.DeleteSshKeyGroup(ctx, existingID); delErr != nil {
			return nil, delErr
		}
		// Context-aware wait: respect cancellation/deadlines instead of blocking for full 30s
		select {
		case <-ctx.Done():
			return nil, &ApiError{Code: StatusClientClosedRequest, Message: "context cancelled while waiting to retry: " + ctx.Err().Error()}
		case <-time.After(30 * time.Second):
			// continue to retry
		}
	}
}

// GetSshKeyGroup returns an SSH Key Group by ID
func (skm SshKeyGroupManager) GetSshKeyGroup(ctx context.Context, sshKeyGroupID string) (*standard.SshKeyGroup, *ApiError) {
	ctx = WithLogger(ctx, skm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, skm.client.Config.Token)

	apiSkg, resp, err := skm.client.apiClient.SSHKeyGroupAPI.GetSshKeyGroup(ctx, skm.client.apiMetadata.Organization, sshKeyGroupID).Execute()
	apiErr := HandleResponseError(resp, err)
	if apiErr != nil {
		return nil, apiErr
	}
	return apiSkg, nil
}

// DeleteSshKeyGroup deletes an SSH Key Group
func (skm SshKeyGroupManager) DeleteSshKeyGroup(ctx context.Context, sshKeyGroupID string) *ApiError {
	ctx = WithLogger(ctx, skm.client.Logger)
	ctx = context.WithValue(ctx, standard.ContextAccessToken, skm.client.Config.Token)

	_, resp, err := skm.client.apiClient.SSHKeyGroupAPI.DeleteSshKeyGroup(ctx, skm.client.apiMetadata.Organization, sshKeyGroupID).Execute()
	return HandleResponseError(resp, err)
}
