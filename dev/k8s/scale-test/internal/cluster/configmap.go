// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cluster

import (
	"context"
	"net/http"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
)

// ConfigMapObject is a ConfigMap as the result claim and publication see it. Empty strings and nil maps are unset.
// Like Lease, the resource version and UID go along on a replace, which the API refuses once either changed or when missing.
type ConfigMapObject struct {
	Name            string
	ResourceVersion string
	UID             string
	Annotations     map[string]string
	Data            map[string]string
}

func configMapTo(o ConfigMapObject) *corev1.ConfigMap {
	return &corev1.ConfigMap{
		TypeMeta:   metav1.TypeMeta{APIVersion: "v1", Kind: "ConfigMap"},
		ObjectMeta: metav1.ObjectMeta{Name: o.Name, ResourceVersion: o.ResourceVersion, UID: types.UID(o.UID), Annotations: o.Annotations},
		Data:       o.Data,
	}
}

func configMapFrom(o *corev1.ConfigMap) ConfigMapObject {
	return ConfigMapObject{
		Name: o.Name, ResourceVersion: o.ResourceVersion, UID: string(o.UID),
		Annotations: o.Annotations, Data: o.Data,
	}
}

// GetConfigMap returns nil when the ConfigMap is missing.
func (k *Kube) GetConfigMap(ctx context.Context, name string) (*ConfigMapObject, error) {
	var o corev1.ConfigMap
	if found, err := k.get(ctx, k.url(core, k.ns, "configmaps", name), &o, name); !found {
		return nil, err
	}
	c := configMapFrom(&o)
	return &c, nil
}

// CreateConfigMap fails with AlreadyExists when the ConfigMap is already there.
func (k *Kube) CreateConfigMap(ctx context.Context, o ConfigMapObject) (ConfigMapObject, error) {
	o.ResourceVersion, o.UID = "", ""
	var out corev1.ConfigMap
	_, err := k.object(ctx, http.MethodPost, k.url(core, k.ns, "configmaps", ""), configMapTo(o), &out, o.Name, k.deadline())
	return configMapFrom(&out), err
}

// ReplaceConfigMap fails with Conflict on a stale ResourceVersion, or on a UID the stored ConfigMap does not have.
func (k *Kube) ReplaceConfigMap(ctx context.Context, o ConfigMapObject) (ConfigMapObject, error) {
	if err := swappable(o.ResourceVersion); err != nil {
		return ConfigMapObject{}, err
	}
	// Without the UID the API makes the ConfigMap when it is missing.
	if o.UID == "" {
		return ConfigMapObject{}, Error{Kind: Invalid, Text: "a ConfigMap replace needs the ConfigMap UID"}
	}
	var out corev1.ConfigMap
	_, err := k.object(ctx, http.MethodPut, k.url(core, k.ns, "configmaps", o.Name), configMapTo(o), &out, o.Name, k.deadline())
	if err != nil {
		return ConfigMapObject{}, err
	}
	return configMapFrom(&out), nil
}
