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
use std::fmt;
use std::fmt::{Display, Formatter};

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::trace;

/// Represents the individual components of a container image name.
/// e.g.
/// nvcr.io/nvidia/doca/doca_hbn:1.5.0-doca2.2.0
///
/// repository - nvcr.io/nvidia/doca, name - doca_hbn, version - 1.5.0-doca2.2.0
#[derive(Clone, Default, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ImageNameComponent {
    /// The repository of the container image
    pub repository: String,
    /// The name of the container image
    pub name: String,
    /// The version of the container image
    pub version: String,
}

/// A container image present on the system
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub id: String,
    #[serde(rename = "repoTags")]
    #[serde(deserialize_with = "container_image_name_to_component")]
    pub names: Vec<ImageNameComponent>,
}

impl ImageNameComponent {
    pub fn repository(&self) -> String {
        self.repository.to_string()
    }

    pub fn name(&self) -> String {
        self.name.to_string()
    }

    pub fn version(&self) -> String {
        self.version.to_string()
    }
}

impl Display for ImageNameComponent {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}:{}", self.repository, self.name, self.version)
    }
}

/// When deserializing an `Image`, split the name into its components and return
/// `ImageComponentName` in place of the string
pub fn container_image_name_to_component<'de, D>(
    deserializer: D,
) -> Result<Vec<ImageNameComponent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let vec: Vec<String> = Vec::deserialize(deserializer)?;
    let initial: Vec<ImageNameComponent> = Vec::new();

    trace!(image_name_components = ?vec, "Container name component");
    vec.iter().try_fold(initial, |mut accum, value| {
        let re = Regex::new(r#"(.+)\/(.+):(.+)"#).unwrap();
        re.captures(value.as_str())
            .map(|components| {
                let image_name_component = ImageNameComponent {
                    repository: components[1].to_string(),
                    name: components[2].to_string(),
                    version: components[3].to_string(),
                };
                accum.push(image_name_component);
                accum
            })
            .ok_or(serde::de::Error::custom(
                "Could not parse image name into components",
            ))
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    /// Builds the `ImageNameComponent` a well-formed `repository/name:version`
    /// string should split into.
    fn component(repository: &str, name: &str, version: &str) -> ImageNameComponent {
        ImageNameComponent {
            repository: repository.to_string(),
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    /// Splits the `repoTags` of an `Image` through `container_image_name_to_component`
    /// by deserializing a minimal `Image` JSON, returning either the parsed
    /// components or the deserialization error message.
    fn split_repo_tags(repo_tags: &[&str]) -> Result<Vec<ImageNameComponent>, String> {
        let json = serde_json::json!({ "id": "img-1", "repoTags": repo_tags });
        serde_json::from_value::<Image>(json)
            .map(|image| image.names)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn test_container_image_name_to_component() {
        scenarios!(run = |tags: &[&str]| split_repo_tags(tags);
            "well-formed names split into repository, name, and version" {
                ["nvcr.io/nvidia/doca/doca_hbn:1.5.0-doca2.2.0"].as_slice()
                    => Yields(vec![component("nvcr.io/nvidia/doca", "doca_hbn", "1.5.0-doca2.2.0")]),
                // The repository greedily absorbs every leading path segment.
                ["docker.io/library/busybox:latest"].as_slice()
                    => Yields(vec![component("docker.io/library", "busybox", "latest")]),
                ["a/b:c", "x/y/z:1"].as_slice()
                    => Yields(vec![component("a", "b", "c"), component("x/y", "z", "1")]),
                [].as_slice() => Yields(vec![]),
            }

            "names missing a component are rejected" {
                // No version (no colon), and no repository (no slash).
                ["nvcr.io/nvidia/doca_hbn"].as_slice() => Fails,
                ["doca_hbn:1.5.0"].as_slice() => Fails,
                // One bad entry fails the whole list.
                ["a/b:c", "bogus"].as_slice() => Fails,
            }
        );
    }
}
