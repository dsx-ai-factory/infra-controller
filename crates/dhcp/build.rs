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
    use std::path::PathBuf;

    println!("cargo:rerun-if-env-changed=KEA_INCLUDE_PATH");
    println!("cargo:rerun-if-env-changed=KEA_LIB_PATH");

    let kea_include_path =
        env::var("KEA_INCLUDE_PATH").unwrap_or_else(|_| "/usr/include/kea".to_string());

    #[cfg(target_arch = "x86_64")]
    let default_kea_lib_path = PathBuf::from("/usr/lib/x86_64-linux-gnu");
    #[cfg(target_arch = "aarch64")]
    let default_kea_lib_path = PathBuf::from("/usr/lib/aarch64-linux-gnu");

    let kea_lib_paths = env::var_os("KEA_LIB_PATH")
        .map(|path| vec![PathBuf::from(path)])
        .unwrap_or_else(|| {
            // pkg-config variables are host paths and do not apply the cross-compilation sysroot.
            let metadata_path = if env::var_os("HOST") == env::var_os("TARGET") {
                pkg_config::get_variable("kea", "libdir")
                    .ok()
                    .filter(|path| !path.is_empty())
            } else {
                None
            };
            metadata_path
                .map(PathBuf::from)
                .into_iter()
                .chain([default_kea_lib_path.join("kea"), default_kea_lib_path])
                .collect()
        });
    let (kea_lib_path, kea_dhcp_lib) = build_support::find_kea_dhcp_library(&kea_lib_paths)
        .unwrap_or_else(|error| panic!("{error}"));

    let kea_shim_root = format!("{}/src/kea", env!("CARGO_MANIFEST_DIR"));

    cbindgen::Builder::new()
        .with_crate(env!("CARGO_MANIFEST_DIR"))
        .with_config(cbindgen::Config::from_file("cbindgen.toml").expect("Config file missing"))
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(format!("{kea_shim_root}/carbide_rust.h"));

    #[cfg(target_os = "linux")]
    if env::var_os("CARGO_FEATURE_TEST_SUPPORT").is_some() {
        let source = "tests/common/lease_io_fault.c";
        let library = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("lease_io_fault.so");
        let status = cc::Build::new()
            .get_compiler()
            .to_command()
            .args(["-shared", "-fPIC", source, "-ldl", "-pthread", "-o"])
            .arg(&library)
            .status()
            .expect("compile lease I/O test helper");
        assert!(status.success(), "lease I/O test helper compilation failed");
        println!(
            "cargo:rustc-env=KEA_TEST_LEASE_IO_LIBRARY={}",
            library.display()
        );
        println!("cargo:rerun-if-changed={source}");
    }

    #[cfg(target_os = "linux")]
    if env::var_os("CARGO_FEATURE_TEST_SUPPORT").is_some() {
        let source = "tests/common/lease_read_barrier.cc";
        let library = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("lease_read_barrier.so");
        let status = cc::Build::new()
            .cpp(true)
            .get_compiler()
            .to_command()
            .args(["-shared", "-fPIC", source, "-I"])
            .arg(&kea_include_path)
            .arg("-L")
            .arg(kea_lib_path)
            .args(["-lkea-hooks", "-o"])
            .arg(&library)
            .status()
            .expect("compile lease read barrier");
        assert!(status.success(), "lease read barrier compilation failed");
        println!(
            "cargo:rustc-env=KEA_TEST_LEASE_READ_LIBRARY={}",
            library.display()
        );
        println!("cargo:rerun-if-changed={source}");
    }

    cc::Build::new()
        .cpp(true)
        .file(format!("{kea_shim_root}/logger.cc"))
        .file(format!("{kea_shim_root}/loader.cc"))
        .file(format!("{kea_shim_root}/callouts.cc"))
        .file(format!("{kea_shim_root}/carbide_logger.cc"))
        .include(kea_include_path)
        .pic(true)
        .compile("keashim");

    println!("cargo:rerun-if-changed=src/kea/callouts.cc");
    println!("cargo:rerun-if-changed=src/kea/callouts.h");
    println!("cargo:rerun-if-changed=src/kea/loader.cc");
    println!("cargo:rerun-if-changed=src/kea/logger.cc");
    println!("cargo:rerun-if-changed=src/kea/carbide_rust.h");
    println!("cargo:rerun-if-changed=src/kea/carbide_logger.cc");
    println!("cargo:rerun-if-changed=src/kea/carbide_logger.h");

    println!("cargo:rustc-link-search=native={}", kea_lib_path.display());
    println!("cargo:rustc-link-lib=keashim");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-link-lib=dl");
    println!("cargo:rustc-link-lib=kea-asiolink");
    println!("cargo:rustc-link-lib=kea-dhcpsrv");
    println!("cargo:rustc-link-lib={kea_dhcp_lib}");
    println!("cargo:rustc-link-lib=kea-hooks");
    println!("cargo:rustc-link-lib=kea-log");
    println!("cargo:rustc-link-lib=kea-util");
    println!("cargo:rustc-link-lib=kea-exceptions");
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod build_support {
    use std::path::{Path, PathBuf};

    pub(crate) fn find_kea_dhcp_library(
        lib_paths: &[PathBuf],
    ) -> Result<(&Path, &'static str), String> {
        for lib_path in lib_paths {
            // Autotools calls this library kea-dhcp++; Meson calls it kea-dhcp.
            for name in ["kea-dhcp++", "kea-dhcp"] {
                if ["so", "a"]
                    .iter()
                    .any(|extension| lib_path.join(format!("lib{name}.{extension}")).is_file())
                {
                    return Ok((lib_path, name));
                }
            }
        }

        Err(format!(
            "No linkable libkea-dhcp++ or libkea-dhcp found in [{}]. \
         Install the Kea development package or set KEA_LIB_PATH to its library directory.",
            lib_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn main() {}
