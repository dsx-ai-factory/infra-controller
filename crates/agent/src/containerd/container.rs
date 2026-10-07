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
use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::error;

use crate::containerd::image::{Image, ImageNameComponent};
use crate::containerd::{BashCommand, Command};

/// A containers metadata
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerMetadata {
    pub name: String,
    pub attempt: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerState {
    #[serde(rename(deserialize = "CONTAINER_RUNNING"))]
    Running,
    #[serde(rename(deserialize = "CONTAINER_EXITED"))]
    Exited,
    #[serde(rename(deserialize = "CONTAINER_UNKNOWN"))]
    Unknown,
    #[serde(rename(deserialize = "CONTAINER_CREATED"))]
    Created,
}

/// An image associated with a container that is running or has terminated
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerImage {
    #[serde(rename = "image")]
    pub id: String,
    pub annotations: HashMap<String, String>,
}

/// A container deserialized with additional information for convenience
/// The container needs to be running or have terminated
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerSummary {
    pub id: String,
    #[serde(rename = "podSandboxId")]
    pub sandbox_id: String,
    pub metadata: ContainerMetadata,
    pub image: ContainerImage,
    #[serde(rename = "imageRef")]
    #[serde(default, skip_deserializing)]
    pub image_ref: Vec<ImageNameComponent>,
    // We skip this during deserialize because we will populate it later
    pub state: ContainerState,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub labels: HashMap<String, String>,
    pub annotations: HashMap<String, String>,
}

/// A list of Container Images that are present on a system
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Images {
    pub images: Vec<Image>,
}

/// A list of running or terminated containers on a system
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Containers {
    pub containers: Vec<ContainerSummary>,
}

impl FromIterator<Image> for Images {
    fn from_iter<I: IntoIterator<Item = Image>>(iter: I) -> Self {
        Images {
            images: iter.into_iter().collect(),
        }
    }
}

impl Images {
    pub async fn list() -> eyre::Result<Images> {
        let data = get_container_images().await?;
        serde_json::from_str::<Images>(&data).map_err(|err| eyre::eyre!(err))
    }

    pub fn filter_by_name<T>(self, name: T) -> eyre::Result<Images>
    where
        T: AsRef<str>,
    {
        let filtered = self
            .images
            .into_iter()
            .filter(|x| x.names.iter().any(|y| y.name.contains(name.as_ref())))
            .collect();
        Ok(filtered)
    }

    pub fn find_by_name<T>(self, name: T) -> eyre::Result<Image>
    where
        T: AsRef<str>,
    {
        self.images
            .into_iter()
            .find(|x| x.names.iter().any(|y| y.name.contains(name.as_ref())))
            .ok_or_else(|| eyre::eyre!("could not find container image for name {}", name.as_ref()))
    }

    pub fn find_by_id<T>(self, container_id: T) -> eyre::Result<Image>
    where
        T: AsRef<str>,
        T: PartialEq,
    {
        self.images
            .into_iter()
            .find(|x| x.id == container_id.as_ref())
            .ok_or_else(|| {
                eyre::eyre!(
                    "could not find container image for id {}",
                    container_id.as_ref()
                )
            })
    }
}

impl Containers {
    pub async fn list() -> eyre::Result<Self> {
        let data = get_containers().await?;

        let containers = serde_json::from_str::<Containers>(&data)
            .map_err(|e| eyre::eyre!(e))?
            .containers;

        let images = Images::list().await?;

        let containers: Vec<_> = containers
            .into_iter()
            .map(|mut c| {
                c.image_ref = images
                    .images
                    .iter()
                    .filter(|i| i.id == c.image.id)
                    .flat_map(|i| i.names.clone())
                    .collect();
                c
            })
            .collect();

        Ok(Containers { containers })
    }

    pub async fn list_pod(pod_id: &str) -> eyre::Result<Self> {
        let data = get_pod_containers(pod_id).await?;

        let containers = serde_json::from_str::<Containers>(&data)
            .map_err(|e| eyre::eyre!(e))?
            .containers;

        let images = Images::list().await?;

        let containers: Vec<_> = containers
            .into_iter()
            .map(|mut c| {
                c.image_ref = images
                    .images
                    .iter()
                    .filter(|i| i.id == c.image.id)
                    .flat_map(|i| i.names.clone())
                    .collect();
                c
            })
            .collect();

        Ok(Containers { containers })
    }

    /// Keep only the latest attempt for each container name.
    ///
    /// `crictl ps -a` returns historical attempts; this function only keeps latest attempt.
    pub fn filter_by_latest_attempt(self) -> Self {
        let mut latest_by_name: HashMap<String, ContainerSummary> = HashMap::new();

        for container in self.containers {
            let name = container.metadata.name.clone();
            if let Some(existing) = latest_by_name.get(&name)
                && container.metadata.attempt <= existing.metadata.attempt
            {
                continue;
            }
            latest_by_name.insert(name, container);
        }

        let mut containers: Vec<_> = latest_by_name.into_values().collect();
        containers.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));

        Containers { containers }
    }

    pub fn find_by_name<T>(self, name: T) -> eyre::Result<ContainerSummary>
    where
        T: AsRef<str>,
        T: PartialEq,
    {
        self.containers
            .into_iter()
            .find(|x| x.metadata.name == name.as_ref())
            .ok_or_else(|| eyre::eyre!("could not find container for name {}", name.as_ref()))
    }
}

/// Return a list of all container images in JSON format.
async fn get_container_images() -> eyre::Result<String> {
    if cfg!(test) || std::env::var("NO_DPU_CONTAINERS").is_ok() {
        let test_data_dir = PathBuf::from(TEST_DATA_DIR);

        std::fs::read_to_string(test_data_dir.join("container_images.json")).map_err(|e| {
            error!(error = %e, "Could not read container_images.json");
            eyre::eyre!("could not read container_images.json: {}", e)
        })
    } else {
        let result = BashCommand::new("bash")
            .args(vec!["-c", "crictl images -o json"])
            .run()
            .await
            .map_err(|e| {
                error!(error = %e, "Could not read container_images.json");
                eyre::eyre!("could not read container_images.json: {}", e)
            })?;
        Ok(result)
    }
}

/// Returns a list of all containers on a host in JSON format.
async fn get_containers() -> eyre::Result<String> {
    if cfg!(test) || std::env::var("NO_DPU_CONTAINERS").is_ok() {
        let test_data_dir = PathBuf::from(TEST_DATA_DIR);

        println!("Path: {}", test_data_dir.join("containers.json").display());

        std::fs::read_to_string(test_data_dir.join("containers.json")).map_err(|e| {
            error!(error = %e, "Could not read containers.json");
            eyre::eyre!("could not read containers.json: {}", e)
        })
    } else {
        let result = BashCommand::new("bash")
            .args(vec!["-c", "crictl ps -o json"])
            .run()
            .await
            .map_err(|e| {
                error!(error = %e, "Could not read containers.json");
                eyre::eyre!("could not read containers.json: {}", e)
            })?;
        Ok(result)
    }
}

/// Returns a list of all containers on a host in JSON format.
async fn get_pod_containers(pod_id: &str) -> eyre::Result<String> {
    if cfg!(test) || std::env::var("NO_DPU_CONTAINERS").is_ok() {
        let test_data_dir = PathBuf::from(TEST_DATA_DIR);

        println!("Path: {}", test_data_dir.join("containers.json").display());

        std::fs::read_to_string(test_data_dir.join("containers.json")).map_err(|e| {
            error!(error = %e, "Could not read containers.json");
            eyre::eyre!("could not read containers.json: {}", e)
        })
    } else {
        let cmd = format!("crictl ps -a --pod {} -o json", pod_id);
        let result = BashCommand::new("bash")
            .args(vec!["-c", cmd.as_str()])
            .run()
            .await
            .map_err(|e| {
                error!(error = %e, "Could not read containers.json");
                eyre::eyre!("could not read containers.json: {}", e)
            })?;
        Ok(result)
    }
}

const TEST_DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/docker-env");

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_container_images() {
        let container_images = get_container_images().await.unwrap();
        let json = serde_json::from_str::<Images>(&container_images).unwrap();
        assert_eq!(json.images.len(), 3);
    }

    #[tokio::test]
    async fn test_all_containers() {
        let containers = get_containers().await.unwrap();
        let json = serde_json::from_str::<Containers>(&containers).unwrap();
        assert_eq!(json.containers.len(), 5);
    }

    #[tokio::test]
    async fn test_container_image_list() {
        let container_images = Images::list().await.unwrap();
        assert_eq!(container_images.images.len(), 3);
    }

    #[tokio::test]
    async fn test_filter_container_images_by_name() {
        let container_images = Images::list().await.unwrap();
        let filtered = container_images.filter_by_name("doca_").unwrap();
        assert_eq!(filtered.images.len(), 2);
        assert_eq!(
            filtered.images[0].names[0],
            ImageNameComponent {
                repository: "nvcr.io/nvidia/doca".to_string(),
                name: "doca_hbn".to_string(),
                version: "2.3.0-doca2.8.0".to_string(),
            }
        );
        assert_eq!(
            filtered.images[1].names[0],
            ImageNameComponent {
                repository: "nvcr.io/nvidia/doca".to_string(),
                name: "doca_telemetry".to_string(),
                version: "1.14.2-doca2.2.0".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn test_find_container_by_name() {
        let containers = Containers::list().await.expect("Could not get containers");
        tracing::info!(?containers, "Listed containers");
        let container = containers.find_by_name("doca-hbn").unwrap();
        tracing::info!(?container, "Found container by name");
        assert_eq!(container.metadata.name, "doca-hbn");
        assert_eq!(container.state, ContainerState::Running);
    }

    #[tokio::test]
    async fn test_filter_and_image_version() {
        let container_images = Images::list().await.unwrap();
        let filtered = container_images.filter_by_name("doca_hbn").unwrap();
        assert_eq!(filtered.images.len(), 1);
        assert_eq!(
            filtered.images[0].names[0].version(),
            "2.3.0-doca2.8.0".to_string()
        );
    }

    #[tokio::test]
    async fn test_find_and_image_version() {
        let container_images = Images::list().await.unwrap();
        let filtered = container_images.find_by_name("doca_hbn").unwrap();
        assert_eq!(filtered.names[0].version(), "2.3.0-doca2.8.0".to_string());
    }

    #[test]
    fn test_filter_container_by_latest_attempt() {
        let old_exited = ContainerSummary {
            id: "old".to_string(),
            sandbox_id: "pod1".to_string(),
            metadata: ContainerMetadata {
                name: "svc-a".to_string(),
                attempt: 0,
            },
            image: ContainerImage {
                id: "img".to_string(),
                annotations: HashMap::new(),
            },
            image_ref: Vec::new(),
            state: ContainerState::Exited,
            created_at: "1".to_string(),
            labels: HashMap::new(),
            annotations: HashMap::new(),
        };

        let new_running = ContainerSummary {
            id: "new".to_string(),
            sandbox_id: "pod1".to_string(),
            metadata: ContainerMetadata {
                name: "svc-a".to_string(),
                attempt: 1,
            },
            image: ContainerImage {
                id: "img".to_string(),
                annotations: HashMap::new(),
            },
            image_ref: Vec::new(),
            state: ContainerState::Running,
            created_at: "2".to_string(),
            labels: HashMap::new(),
            annotations: HashMap::new(),
        };

        let unaffected = ContainerSummary {
            id: "other".to_string(),
            sandbox_id: "pod1".to_string(),
            metadata: ContainerMetadata {
                name: "svc-b".to_string(),
                attempt: 0,
            },
            image: ContainerImage {
                id: "img2".to_string(),
                annotations: HashMap::new(),
            },
            image_ref: Vec::new(),
            state: ContainerState::Running,
            created_at: "3".to_string(),
            labels: HashMap::new(),
            annotations: HashMap::new(),
        };

        let got = Containers {
            containers: vec![old_exited, new_running, unaffected],
        }
        .filter_by_latest_attempt();

        assert_eq!(got.containers.len(), 2);
        assert!(
            got.containers
                .iter()
                .any(|c| c.metadata.name == "svc-a" && c.metadata.attempt == 1)
        );
        assert!(
            got.containers
                .iter()
                .any(|c| c.metadata.name == "svc-b" && c.metadata.attempt == 0)
        );
    }
}
