// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package result

import (
	"context"
	"errors"
	"fmt"
	"maps"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

const (
	ResultKey = "result.json" // the result ConfigMap data key the summary is written to
	// OwnerAnnotation names the attempt that owns a result ConfigMap, as "<executionId>/<token>". Its text is never
	// shown in an error.
	OwnerAnnotation  = "nvidia-infra-controller/scale-test-result-owner"
	resultWriteTries = 3 // bounds the compare-and-swap writes of one result
)

// ConfigMaps is the result ConfigMap port, satisfied by *cluster.Kube and a test fake. The adapter bounds every call.
type ConfigMaps interface {
	GetConfigMap(ctx context.Context, name string) (*cluster.ConfigMapObject, error)
	CreateConfigMap(ctx context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error)
	ReplaceConfigMap(ctx context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error)
}

func isKind(err error, k cluster.Kind) bool {
	var e cluster.Error
	return errors.As(err, &e) && e.Kind == k
}

func ownerOf(o cluster.ConfigMapObject) (string, bool) {
	v, ok := o.Annotations[OwnerAnnotation]
	return v, ok
}

func claimedUID(o cluster.ConfigMapObject) (string, error) {
	if o.UID == "" {
		return "", errors.New("the claimed ConfigMap has no UID")
	}
	return o.UID, nil
}

// Claim claims the result ConfigMap name for owner by a compare-and-swap on the chart's empty one, or a create when
// missing, and returns its UID, refusing one another attempt owns or an unowned result; a lost reply is read back and
// holds only for this exact owner, and no error carries the owner.
func Claim(ctx context.Context, cm ConfigMaps, name, owner string) (string, error) {
	current, err := cm.GetConfigMap(ctx, name)
	if err != nil {
		return "", fmt.Errorf("not read: %w", err)
	}
	var written cluster.ConfigMapObject
	if current == nil {
		written, err = cm.CreateConfigMap(ctx, cluster.ConfigMapObject{
			Name: name, Annotations: map[string]string{OwnerAnnotation: owner},
		})
	} else {
		switch other, ok := ownerOf(*current); {
		case ok && other == owner:
			return claimedUID(*current)
		case ok:
			return "", errors.New("owned by another attempt")
		case len(current.Data) > 0:
			return "", errors.New("holds a result with no owner")
		}
		next := *current
		next.Annotations = withOwner(next.Annotations, owner)
		written, err = cm.ReplaceConfigMap(ctx, next)
	}
	if err == nil {
		return claimedUID(written)
	}
	// A conflict lost the race. Any other failure may have committed: read back.
	back, rerr := cm.GetConfigMap(ctx, name)
	switch {
	case rerr != nil:
		return "", fmt.Errorf("claim not confirmed: %w, and not read back: %w", err, rerr)
	case back == nil:
		return "", fmt.Errorf("claim not confirmed: %w", err)
	}
	if o, ok := ownerOf(*back); ok && o == owner {
		return claimedUID(*back)
	}
	if isKind(err, cluster.Conflict) || isKind(err, cluster.AlreadyExists) {
		if _, ok := ownerOf(*back); ok {
			return "", errors.New("claimed first by another attempt")
		}
		return "", fmt.Errorf("changed while it was claimed: %w", err)
	}
	return "", fmt.Errorf("claim not confirmed: %w", err)
}

func withOwner(a map[string]string, owner string) map[string]string {
	out := maps.Clone(a)
	if out == nil {
		out = map[string]string{}
	}
	out[OwnerAnnotation] = owner
	return out
}

// WriteResult writes json to the claimed result ConfigMap as a compare-and-swap while it keeps the claimed UID and this
// attempt as owner. A missing, replaced or reowned ConfigMap is refused, never made again. Only a Conflict is retried.
func WriteResult(ctx context.Context, cm ConfigMaps, name, owner, uid, json string) error {
	for range resultWriteTries {
		found, err := cm.GetConfigMap(ctx, name)
		if err != nil {
			return fmt.Errorf("not read: %w", err)
		}
		if found == nil {
			return errors.New("it is gone, and the claim with it")
		}
		if o, _ := ownerOf(*found); found.UID != uid || o != owner {
			return errors.New("it was replaced or claimed by another attempt")
		}
		next := *found
		next.Data = maps.Clone(next.Data)
		if next.Data == nil {
			next.Data = map[string]string{}
		}
		next.Data[ResultKey] = json
		switch _, err := cm.ReplaceConfigMap(ctx, next); {
		case err == nil:
			return nil
		case isKind(err, cluster.Conflict): // changed since the read: read it again
		default:
			// The reply was lost; the write may have committed. Read back under the same UID and owner fence the
			// loop uses: only our own object, still carrying this result, confirms the write. A reowner or a new
			// UID with the same bytes, or an unreadable read-back, stays a failure.
			if back, rerr := cm.GetConfigMap(ctx, name); rerr == nil && back != nil {
				if o, _ := ownerOf(*back); back.UID == uid && o == owner && back.Data[ResultKey] == json {
					return nil
				}
			}
			return err
		}
	}
	return fmt.Errorf("it changed during each of %d writes", resultWriteTries)
}
