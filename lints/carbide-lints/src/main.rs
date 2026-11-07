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
#![deny(warnings)]
#![feature(rustc_private)]

mod borrowck_shim;
mod txn_held_across_await;
mod txn_without_commit;

extern crate rustc_abi;
extern crate rustc_arena;
extern crate rustc_ast;
extern crate rustc_ast_pretty;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_hir_pretty;
extern crate rustc_index;
extern crate rustc_infer;
extern crate rustc_interface;
extern crate rustc_lexer;
extern crate rustc_lint;
extern crate rustc_lint_defs;
extern crate rustc_middle;
extern crate rustc_mir_build;
extern crate rustc_mir_dataflow;
extern crate rustc_mir_transform;
extern crate rustc_parse;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_target;
extern crate rustc_trait_selection;
extern crate rustc_type_ir;

use std::path::Path;

use rustc_driver::Callbacks;
use rustc_errors::ColorConfig;
use rustc_errors::emitter::HumanReadableErrorType;
use rustc_interface::interface;
use rustc_session::EarlyDiagCtxt;
use rustc_session::config::ErrorOutputType;

use crate::borrowck_shim::BorrowckShim;

/// Default callbacks: Passing this to run_compiler just runs the compiler as norm
struct DefaultCallbacks;
impl Callbacks for DefaultCallbacks {}

/// Callbacks for our lints: Passing this to run_compiler will add our lints to the compilation
struct CarbideLints;
impl Callbacks for CarbideLints {
    fn config(&mut self, config: &mut interface::Config) {
        // Register the lints themselves, so that `#[allow(txn_held_across_await)]` works properly
        config.register_lints = Some(Box::new(|_session, lints| {
            lints.register_lints(&[
                txn_held_across_await::TXN_HELD_ACROSS_AWAIT,
                txn_without_commit::TXN_WITHOUT_COMMIT,
            ]);
        }));

        // Override the `mir_borrowck` query from rustc. This query has the right information
        // available, namely both type-checker results and borrow-checker results.
        config.override_queries = Some(|_session, queries| {
            let orig_borrowck_query = Box::new(queries.queries.mir_borrowck);
            // Keep the original query in a global so we can call it.
            borrowck_shim::ORIG_BORROWCK_QUERY
                .write()
                .unwrap()
                .replace(orig_borrowck_query);
            queries.queries.mir_borrowck = |tcx, def_id| {
                let mut shim = BorrowckShim::new();
                let result = shim.mir_borrowck(tcx, def_id);
                result
            }
        });
    }
}

fn main() -> std::process::ExitCode {
    let early_dcx = EarlyDiagCtxt::new(ErrorOutputType::default());
    rustc_driver::install_ice_hook("https://github.com/NVIDIA/carbide/issues/new", |_| ());
    let handler = EarlyDiagCtxt::new(ErrorOutputType::HumanReadable {
        kind: HumanReadableErrorType {
            short: false,
            unicode: false,
        },
        color_config: ColorConfig::Auto,
    });
    rustc_driver::init_rustc_env_logger(&handler);
    rustc_driver::catch_with_exit_code(move || {
        let mut orig_args = rustc_driver::args::raw_args(&early_dcx);

        // Taken from clippy's driver: Support the `--sysroot` rustc arg.
        let has_sysroot_arg = |args: &mut [String]| -> bool {
            if has_arg(args, "--sysroot") {
                return true;
            }
            // https://doc.rust-lang.org/rustc/command-line-arguments.html#path-load-command-line-flags-from-a-path
            // Beside checking for existence of `--sysroot` on the command line, we need to
            // check for the arg files that are prefixed with @ as well to be consistent with rustc
            for arg in args.iter() {
                if let Some(arg_file_path) = arg.strip_prefix('@')
                    && let Ok(arg_file) = std::fs::read_to_string(arg_file_path)
                {
                    let split_arg_file: Vec<String> =
                        arg_file.lines().map(ToString::to_string).collect();
                    if has_arg(&split_arg_file, "--sysroot") {
                        return true;
                    }
                }
            }
            false
        };

        let sys_root_env = std::env::var("SYSROOT").ok();
        let pass_sysroot_env_if_given = |args: &mut Vec<String>, sys_root_env| {
            if let Some(sys_root) = sys_root_env
                && !has_sysroot_arg(args)
            {
                args.extend(vec!["--sysroot".into(), sys_root]);
            }
        };

        // Setting RUSTC_WRAPPER causes Cargo to pass 'rustc' as the first argument.
        // We're invoking the compiler programmatically, so we ignore this
        let wrapper_mode =
            orig_args.get(1).map(Path::new).and_then(Path::file_stem) == Some("rustc".as_ref());

        if wrapper_mode {
            // we still want to be able to invoke it normally though
            orig_args.remove(1);
        }

        let mut args: Vec<String> = orig_args.clone();
        pass_sysroot_env_if_given(&mut args, sys_root_env);

        // We only run our lints for our packages, not dependencies. If CARGO_PRIMARY_PACKAGE is
        // set, we're checking a package in this repo, otherwise just run rustc as-is.
        if std::env::var("CARGO_PRIMARY_PACKAGE").is_ok() {
            let mut driver = CarbideLints;
            rustc_driver::run_compiler(&args, &mut driver);
        } else {
            let mut driver = DefaultCallbacks;
            rustc_driver::run_compiler(&args, &mut driver);
        }
    })
}

fn has_arg(args: &[String], find_arg: &str) -> bool {
    args.iter()
        .any(|arg| find_arg == arg.split('=').next().unwrap())
}
