/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import "context"

// Storage is a interface to store ipam objects.
type Storage interface {
	Name() string
	CreatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error)
	ReadPrefix(ctx context.Context, prefix string, namespace string) (Prefix, error)
	DeleteAllPrefixes(ctx context.Context, namespace string) error
	ReadAllPrefixes(ctx context.Context, namespace string) (Prefixes, error)
	ReadAllPrefixCidrs(ctx context.Context, namespace string) ([]string, error)
	UpdatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error)
	DeletePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error)
	CreateNamespace(ctx context.Context, namespace string) error
	ListNamespaces(ctx context.Context) ([]string, error)
	DeleteNamespace(ctx context.Context, namespace string) error
}
