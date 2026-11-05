// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cluster

import (
	"context"
	"net/http"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
)

// DeleteLease removes a Lease only while its UID is unchanged, for dev-cluster
// test cleanup. It is test-only: the production client never deletes.
func DeleteLease(k *Kube, ctx context.Context, name, uid string) error {
	id := types.UID(uid)
	opts := metav1.DeleteOptions{Preconditions: &metav1.Preconditions{UID: &id}}
	_, err := k.call(ctx, http.MethodDelete, k.url(leases, k.ns, "leases", name), opts, &metav1.Status{}, k.deadline())
	return err
}
