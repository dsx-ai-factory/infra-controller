// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
)

// RealK8sServiceClient implements K8sServiceClient using the real Kubernetes API.
type RealK8sServiceClient struct {
	clientset kubernetes.Interface
}

// NewRealK8sServiceClient creates a new RealK8sServiceClient.
func NewRealK8sServiceClient(clientset kubernetes.Interface) *RealK8sServiceClient {
	return &RealK8sServiceClient{clientset: clientset}
}

// List returns all Services matching the label selector.
func (c *RealK8sServiceClient) List(ctx context.Context, namespace string, labelSelector string) ([]*corev1.Service, error) {
	list, err := c.clientset.CoreV1().Services(namespace).List(ctx, metav1.ListOptions{
		LabelSelector: labelSelector,
	})
	if err != nil {
		return nil, err
	}

	result := make([]*corev1.Service, len(list.Items))
	for i := range list.Items {
		result[i] = &list.Items[i]
	}
	return result, nil
}

// Create creates a new Service.
func (c *RealK8sServiceClient) Create(ctx context.Context, svc *corev1.Service) error {
	_, err := c.clientset.CoreV1().Services(svc.Namespace).Create(ctx, svc, metav1.CreateOptions{})
	return err
}

// Update updates an existing Service.
func (c *RealK8sServiceClient) Update(ctx context.Context, svc *corev1.Service) error {
	_, err := c.clientset.CoreV1().Services(svc.Namespace).Update(ctx, svc, metav1.UpdateOptions{})
	return err
}

// Delete deletes a Service by name.
func (c *RealK8sServiceClient) Delete(ctx context.Context, namespace, name string) error {
	return c.clientset.CoreV1().Services(namespace).Delete(ctx, name, metav1.DeleteOptions{})
}
