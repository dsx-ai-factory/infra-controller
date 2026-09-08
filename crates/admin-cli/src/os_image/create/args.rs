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

use clap::Parser;
use rpc::forge as forgerpc;

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::os_image::common::str_to_rpc_uuid;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Create an OS image entry in a tenant's catalog:
    $ nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 \
    --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a

Create one with a name/description and a Bearer auth token for the image URL:
    $ nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 \
    --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a \
    --name ubuntu-22.04 --description \"Ubuntu 22.04 base\" --auth-type Bearer --auth-token ZXhhbXBsZS1pbWFnZS10b2tlbg==

Overwrite the smallest whole disk, preferring an EFI disk to break a size tie:
    $ nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 \
    --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a \
    --boot-disk smallest

Overwrite a disk selected by its stable Linux identifier:
    $ nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 \
    --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a \
    --boot-disk /dev/disk/by-id/nvme-Dell_BOSS-N1_VNOWW56VFCV0055601UT

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "uuid of the OS image to create.")]
    id: String,
    #[clap(short = 'u', long, help = "url of the OS image qcow file.")]
    url: String,
    #[clap(
        short = 'm',
        long,
        help = "Digest of the OS image file, typically a SHA-256."
    )]
    digest: String,
    #[clap(
        short = 't',
        long,
        help = "Tenant organization identifier for the OS catalog to create this in."
    )]
    tenant_org_id: String,
    #[clap(
        short = 'v',
        long,
        help = "Create a source volume for block storage use."
    )]
    create_volume: Option<bool>,
    #[clap(
        short = 's',
        long,
        help = "Size of the OS image source volume to create."
    )]
    capacity: Option<u64>,
    #[clap(short = 'n', long, help = "Name of the OS image entry.")]
    name: Option<String>,
    #[clap(short = 'd', long, help = "Description of the OS image entry.")]
    description: Option<String>,
    #[clap(short = 'y', long, help = "Authentication type, usually Bearer.")]
    auth_type: Option<String>,
    #[clap(short = 'p', long, help = "Authentication token, usually in base64.")]
    auth_token: Option<String>,
    #[clap(
        short = 'f',
        long,
        help = "uuid of the root filesystem of the OS image."
    )]
    rootfs_id: Option<String>,
    #[clap(
        short = 'l',
        long,
        help = "Label of the root filesystem of the OS image."
    )]
    rootfs_label: Option<String>,
    #[clap(
        short = 'b',
        long,
        help = "Whole-disk target that the image overwrites. Accepts 'smallest', /dev/nvme<controller>n<namespace>, /dev/sd<letters>, or /dev/disk/by-id/<identifier>. If omitted or empty, selection prefers a disk with an EFI partition, then /dev/nvme0n1 or /dev/sda."
    )]
    boot_disk: Option<String>,
    #[clap(long, help = "UUID of the image boot filesystem (/boot)")]
    bootfs_id: Option<String>,
    #[clap(long, help = "UUID of the image EFI filesystem (/boot/efi)")]
    efifs_id: Option<String>,
}

impl TryFrom<Args> for forgerpc::OsImageAttributes {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let id = str_to_rpc_uuid(&args.id)?;
        Ok(forgerpc::OsImageAttributes {
            id: Some(id),
            source_url: args.url,
            digest: args.digest,
            tenant_organization_id: args.tenant_org_id,
            create_volume: args.create_volume.unwrap_or(false),
            name: args.name,
            description: args.description,
            auth_type: args.auth_type,
            auth_token: args.auth_token,
            rootfs_id: args.rootfs_id,
            rootfs_label: args.rootfs_label,
            boot_disk: args.boot_disk,
            capacity: args.capacity,
            bootfs_id: args.bootfs_id,
            efifs_id: args.efifs_id,
        })
    }
}
