/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
// Code for configuring the host platform that the agent is running on.

use std::fs::File;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use eyre::{WrapErr, eyre};
use tempfile::NamedTempFile;
use tracing;

const DOCA_POD_CONFIG_PATH: &str = "/etc/kubelet.d";
const DOCA_POD_CONFIG_SOURCE_BASE: &str = "/opt/forge/doca_container_configs/configs";
const HBN_POD_FILENAME: &str = "doca_hbn.yaml";

pub async fn ensure_doca_containers() -> eyre::Result<()> {
    let client = containerd::ContainerdClient::default();

    client.wait_for_image("doca_hbn").await?;
    ensure_doca_hbn_pod()?;

    Ok(())
}

pub fn ensure_doca_hbn_pod() -> eyre::Result<()> {
    let source_path = PathBuf::from_iter([DOCA_POD_CONFIG_SOURCE_BASE, HBN_POD_FILENAME]);
    let destination_path = PathBuf::from_iter([DOCA_POD_CONFIG_PATH, HBN_POD_FILENAME]);
    let doca_hbn_pod_file = ManagedFile::new(destination_path);
    doca_hbn_pod_file.copy_if_not_present(source_path.as_path())
}

/// A representation of a file at `path` that we manage at runtime.
pub struct ManagedFile {
    path: PathBuf,
}

impl ManagedFile {
    pub fn new(path: PathBuf) -> Self {
        ManagedFile { path }
    }

    /// Checks if the file exists, and if not copies it from source_path. No
    /// comparison of timestamps or contents is performed.
    pub fn copy_if_not_present(self, source_path: &Path) -> eyre::Result<()> {
        let destination_exists = self.path.try_exists().context(format!(
            "couldn't check existence of destination file {f}",
            f = self.path.display()
        ))?;
        if !destination_exists {
            safe_copy(self.path.as_path(), source_path).inspect(|_| {
                tracing::info!(
                    source_path = %source_path.display(),
                    destination_path = %self.path.display(),
                    "Copied file contents"
                );
            })
        } else {
            tracing::debug!(
                source_path = %source_path.display(),
                destination_path = %self.path.display(),
                "File already exists, will not be updated"
            );
            Ok(())
        }
    }

    pub fn ensure_contents(&mut self, contents: &[u8]) -> eyre::Result<bool> {
        let destination_exists = self.path.try_exists().wrap_err_with(|| {
            format!(
                "couldn't check existence of destination file {f}",
                f = self.path.display()
            )
        })?;

        let update_needed = match destination_exists {
            false => true,
            true => {
                let current_contents = std::fs::read(self.path.as_path()).wrap_err_with(|| {
                    format!(
                        "couldn't read current file contents of {f}",
                        f = self.path.display()
                    )
                })?;
                current_contents.as_slice() != contents
            }
        };

        match update_needed {
            true => safe_write(self.path.as_path(), contents).map(|_| true),
            false => Ok(false),
        }
    }
}

/// Copies a file from one destination to another using reasonably safe
/// semantics. The source file is copied to a temporary file in the same
/// directory as the destination file, and then it's renamed into place and
/// fsync()-ed.
fn safe_copy(destination_path: &Path, source_path: &Path) -> eyre::Result<()> {
    let destination_dirname = destination_path.parent().ok_or_else(|| {
        eyre!(
            "couldn't determine directory name of destination file {d}",
            d = destination_path.display()
        )
    })?;
    let mut source_file = File::open(source_path)
        .with_context(|| format!("couldn't open source file {f}", f = source_path.display()))?;
    let mut tmp_destination = NamedTempFile::with_suffix_in(".tmp", destination_dirname)
        .with_context(|| {
            format!(
                "couldn't create temporary file in destination directory {d}",
                d = destination_dirname.display()
            )
        })?;
    std::io::copy(&mut source_file, &mut tmp_destination).with_context(|| {
        format!(
            "couldn't copy file contents from {s} to {d}",
            s = source_path.display(),
            d = tmp_destination.path().display()
        )
    })?;
    let destination_file = tmp_destination.persist(destination_path).with_context(|| {
        format!(
            "couldn't persist contents to destination file {d}",
            d = destination_path.display()
        )
    })?;
    destination_file.sync_all().with_context(|| {
        format!(
            "couldn't sync file data of destination file {d}",
            d = destination_path.display()
        )
    })?;

    Ok(())
}

/// Writes the contents of a file using reasonably safe semantics. A new
/// temporary file is written in the same directory, after which it's renamed
/// into place and fsync()-ed.
fn safe_write(destination_path: &Path, contents: &[u8]) -> eyre::Result<()> {
    let destination_dirname = destination_path.parent().ok_or_else(|| {
        eyre!(
            "couldn't determine directory name of destination file {d}",
            d = destination_path.display()
        )
    })?;
    let mut tmp_destination = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o644))
        .suffix(".tmp")
        .tempfile_in(destination_dirname)
        .with_context(|| {
            format!(
                "couldn't create temporary file in destination directory {d}",
                d = destination_dirname.display()
            )
        })?;
    tmp_destination.write_all(contents).with_context(|| {
        format!(
            "couldn't write file contents to {d}",
            d = destination_path.display()
        )
    })?;
    let destination_file = tmp_destination.persist(destination_path).with_context(|| {
        format!(
            "couldn't persist contents to destination file {d}",
            d = destination_path.display()
        )
    })?;
    destination_file.sync_all().with_context(|| {
        format!(
            "couldn't sync file data of destination file {d}",
            d = destination_path.display()
        )
    })?;

    Ok(())
}

pub mod containerd {

    use std::time::Duration;

    use containerd_client::services::v1::images_client::ImagesClient;
    use containerd_client::services::v1::{Image, ListImagesRequest};
    use containerd_client::with_namespace;
    use eyre::Context;
    use tokio::sync::OnceCell;

    const CONTAINERD_SOCKET_PATH: &str = "/run/containerd/containerd.sock";
    const CONTAINERD_K8S_HEADER_VALUE: &str = "k8s.io";

    pub struct ContainerdClient {
        socket_path: &'static str,
        // We lazily connect the channel so that a consumer of this API can
        // construct the client without having to do error handling.
        connection_channel: OnceCell<containerd_client::tonic::transport::Channel>,
    }

    impl ContainerdClient {
        pub fn new(socket_path: &'static str) -> Self {
            let connection_channel = OnceCell::new();
            Self {
                socket_path,
                connection_channel,
            }
        }

        async fn get_connection_channel(
            &self,
        ) -> Result<
            containerd_client::tonic::transport::Channel,
            containerd_client::tonic::transport::Error,
        > {
            self.connection_channel
                .get_or_try_init(async || containerd_client::connect(self.socket_path).await)
                .await
                .cloned()
        }

        pub async fn list_images(&self) -> eyre::Result<Vec<Image>> {
            let c = self
                .get_connection_channel()
                .await
                .context("can't connect to containerd socket")?;

            let mut images_client = ImagesClient::new(c);

            let request = ListImagesRequest { filters: vec![] };
            let request = {
                // with_namespace is a poorly-written macro and we have to import
                // this on its behalf.
                use containerd_client::tonic::Request;
                with_namespace!(request, CONTAINERD_K8S_HEADER_VALUE)
            };
            let response = images_client
                .list(request)
                .await
                .context("can't list images")?
                .into_inner();
            Ok(response.images)
        }

        /// Wait for the named image (which should be specified as a base name
        /// with no prefix or tag) to show up in the containerd images. Once
        /// found, the full image name is returned.
        pub async fn wait_for_image(&self, image_base_name: &str) -> eyre::Result<String> {
            // FIXME: It would be better to parse the image names, but this
            // seems to work okay for now.
            let image_base_name_pattern = format!("/{image_base_name}:");

            loop {
                let images = self.list_images().await?;
                let matching_image = images.iter().find_map(|image| {
                    let name = image.name.as_str();
                    name.contains(image_base_name_pattern.as_str())
                        .then_some(name.to_owned())
                });
                if let Some(image_name) = matching_image {
                    return Ok(image_name);
                }
                tokio::time::sleep(Duration::from_secs(1)).await
            }
        }
    }

    impl Default for ContainerdClient {
        fn default() -> Self {
            Self::new(CONTAINERD_SOCKET_PATH)
        }
    }
}
