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

// dhcp package is built for x86_64 and aarch64 architectures
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn main() {
    use std::env;

    let kea_include_path =
        env::var("KEA_INCLUDE_PATH").unwrap_or_else(|_| "/usr/include/kea".to_string());

    #[cfg(target_arch = "x86_64")]
    let default_kea_lib_path = "/usr/lib/x86_64-linux-gnu/kea";
    #[cfg(target_arch = "aarch64")]
    let default_kea_lib_path = "/usr/lib/aarch64-linux-gnu/kea";

    let kea_lib_path =
        env::var("KEA_LIB_PATH").unwrap_or_else(|_| default_kea_lib_path.to_string());

    let kea_shim_root = format!("{}/src/kea", env!("CARGO_MANIFEST_DIR"));

    cbindgen::Builder::new()
        .with_crate(env!("CARGO_MANIFEST_DIR"))
        .with_config(cbindgen::Config::from_file("cbindgen.toml").expect("Config file missing"))
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(format!("{kea_shim_root}/carbide_rust.h"));

    let hooks_header = format!("{kea_include_path}/hooks/hooks.h");
    let hooks_source = std::fs::read_to_string(&hooks_header).expect("Kea hooks header missing");
    let hooks_version = hooks_source
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("const int KEA_HOOKS_VERSION = ")
                .and_then(|value| value.trim_end_matches(';').parse::<u32>().ok())
        })
        .expect("Kea hooks header has no supported version declaration");
    let hooks_version_string = hooks_version.to_string();
    let mut shim = cc::Build::new();
    shim.cpp(true)
        .file(format!("{kea_shim_root}/logger.cc"))
        .file(format!("{kea_shim_root}/loader.cc"))
        .file(format!("{kea_shim_root}/callouts.cc"))
        .file(format!("{kea_shim_root}/carbide_logger.cc"))
        .define("CARBIDE_KEA_HOOKS_VERSION", hooks_version_string.as_str())
        .include(&kea_include_path);
    shim.pic(true).compile("keashim");
    println!("cargo:rerun-if-changed={hooks_header}");
    println!("cargo:rerun-if-env-changed=KEA_INCLUDE_PATH");
    println!("cargo:rerun-if-env-changed=KEA_LIB_PATH");

    println!("cargo:rerun-if-changed=src/kea/callouts.cc");
    println!("cargo:rerun-if-changed=src/kea/callouts.h");
    println!("cargo:rerun-if-changed=src/kea/loader.cc");
    println!("cargo:rerun-if-changed=src/kea/logger.cc");
    println!("cargo:rerun-if-changed=src/kea/carbide_rust.h");
    println!("cargo:rerun-if-changed=src/kea/carbide_logger.cc");
    println!("cargo:rerun-if-changed=src/kea/carbide_logger.h");

    println!("cargo:rustc-link-search={kea_lib_path}");
    println!("cargo:rustc-link-lib=keashim");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-link-lib=kea-asiolink");
    println!("cargo:rustc-link-lib=kea-dhcpsrv");
    println!("cargo:rustc-link-lib=kea-dhcp++");
    println!("cargo:rustc-link-lib=kea-hooks");
    println!("cargo:rustc-link-lib=kea-log");
    println!("cargo:rustc-link-lib=kea-util");
    println!("cargo:rustc-link-lib=kea-exceptions");
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn main() {}
