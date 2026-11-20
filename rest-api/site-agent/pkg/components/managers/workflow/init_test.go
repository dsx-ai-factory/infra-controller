// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/stretchr/testify/require"
)

// TestWatchCertFilesReloadsEveryProjectedSecretUpdate replays the kubelet
// atomic writer twice. Each update deletes the versioned directory that the
// `ca` and `client` symlinks resolved to, so only the mount root watch sees the
// second update.
func TestWatchCertFilesReloadsEveryProjectedSecretUpdate(t *testing.T) {
	root := t.TempDir()
	writeGeneration := func(generation int) string {
		dir := fmt.Sprintf("..generation_%d", generation)
		for _, file := range []string{"ca/ca.crt", "client/tls.crt", "client/tls.key"} {
			path := filepath.Join(root, dir, file)
			require.NoError(t, os.MkdirAll(filepath.Dir(path), 0o755))
			require.NoError(t, os.WriteFile(path, []byte(dir), 0o600))
		}
		return dir
	}

	current := writeGeneration(0)
	require.NoError(t, os.Symlink(current, filepath.Join(root, computils.KubernetesSecretDataSymlink)))
	for _, dir := range []string{"ca", "client"} {
		require.NoError(t, os.Symlink(filepath.Join(computils.KubernetesSecretDataSymlink, dir), filepath.Join(root, dir)))
	}

	caDir, clientDir := filepath.Join(root, "ca")+"/", filepath.Join(root, "client")+"/"
	files := map[string]bool{
		filepath.Join(root, "ca", "ca.crt"):      true,
		filepath.Join(root, "client", "tls.crt"): true,
		filepath.Join(root, "client", "tls.key"): true,
	}
	w, err := newCertWatcher(root, []string{caDir, clientDir})
	require.NoError(t, err)

	reloads := make(chan struct{}, 64)
	done := make(chan struct{})
	go func() {
		defer close(done)
		watchCertFiles(w, root, files, func() { reloads <- struct{}{} })
	}()
	t.Cleanup(func() {
		w.Close()
		<-done
	})

	for generation := 1; generation <= 2; generation++ {
		next := writeGeneration(generation)
		staging := filepath.Join(root, "..data_tmp")
		require.NoError(t, os.Symlink(next, staging))
		require.NoError(t, os.Rename(staging, filepath.Join(root, computils.KubernetesSecretDataSymlink)))
		require.NoError(t, os.RemoveAll(filepath.Join(root, current)))
		current = next

		select {
		case <-reloads:
		case <-time.After(5 * time.Second):
			t.Fatalf("update %d did not reload the Temporal certs", generation)
		}

		// Drain the remaining events of this update so they can't satisfy the next one.
		// The kqueue backend reports the `..data` rename more than once.
		for drained := false; !drained; {
			select {
			case <-reloads:
			case <-time.After(200 * time.Millisecond):
				drained = true
			}
		}
	}
}
