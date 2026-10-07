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
use std::sync::{Arc, LazyLock, RwLock};

use rustc_data_structures::fx::FxIndexMap;
use rustc_hir::intravisit::Visitor;
use rustc_hir::{
    BodyId, Closure, ClosureKind, CoroutineDesugaring, CoroutineKind, CoroutineSource, Expr,
    ExprKind, intravisit,
};
use rustc_middle::hir::nested_filter;
use rustc_middle::ty::{DefinitionSiteHiddenType, TyCtxt};
use rustc_span::ErrorGuaranteed;
use rustc_span::def_id::LocalDefId;

use crate::txn_held_across_await::TxnHeldAcrossAwait;
use crate::txn_without_commit::TxnWithoutCommit;

/// The type for the mir_borrowck query in rustc, which we're overriding.
type BorrowckQueryFn = Box<
    dyn Fn(
            TyCtxt<'_>,
            LocalDefId,
        )
            -> Result<&FxIndexMap<LocalDefId, DefinitionSiteHiddenType<'_>>, ErrorGuaranteed>
        + Send
        + Sync,
>;

/// This is set by CarbideLints::config when starting up, once all default queries are populated.
pub(crate) static ORIG_BORROWCK_QUERY: LazyLock<Arc<RwLock<Option<BorrowckQueryFn>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(None)));

pub(crate) struct BorrowckShim;

impl BorrowckShim {
    pub(crate) fn new() -> Self {
        BorrowckShim {}
    }

    /// Entrypoint to our lint(s) for a given LocalDefId: First call the default mir_borrowck, then
    /// before returning, if LocalDefId refers to an async function, run TxnHeldAcrossAwait on it.
    pub(crate) fn mir_borrowck<'tcx>(
        &mut self,
        tcx: TyCtxt<'tcx>,
        def_id: LocalDefId,
    ) -> Result<&'tcx FxIndexMap<LocalDefId, DefinitionSiteHiddenType<'tcx>>, ErrorGuaranteed> {
        let orig_borrowck_guard = ORIG_BORROWCK_QUERY.read().expect("lock poisoned");
        let orig_borrowck = orig_borrowck_guard
            .as_ref()
            .expect("no mir_borrowck query set, shim not configured");
        let result = orig_borrowck(tcx, def_id);

        let Some((_owner_def_id, body_id)) = tcx.hir_node_by_def_id(def_id).associated_body()
        else {
            // We only check nodes with associated bodies (functions)
            return result;
        };

        // We care about HIR bodies which are async closure blocks, which is how async functions are
        // represented (desugared), as well as `async move { ... }` closures.
        let results = AsyncClosureFinder::find_async_closures(body_id, tcx);

        // For any function body, ensure transactions we own are eventually committed or moved out.
        TxnWithoutCommit::default().check_fn_body(tcx, def_id);

        for (closure, coroutine_source) in results {
            TxnWithoutCommit::default().check_fn_body(tcx, closure.def_id);

            // Check locals defined in the closure
            TxnHeldAcrossAwait::default().check_closure_locals(tcx, closure);

            if matches!(coroutine_source, CoroutineSource::Block) {
                // For `async move { ... }` blocks, check any "upvars" (locals defined in the
                // parent scope and referenced inside the closure.) This requires distinct logic
                // and so is a separate function.
                TxnHeldAcrossAwait::default().check_closure_upvars(tcx, closure);
            }
        }

        result
    }
}

struct AsyncClosureFinder<'tcx> {
    tcx: TyCtxt<'tcx>,
    async_closures: Vec<(&'tcx Closure<'tcx>, CoroutineSource)>,
}

impl<'tcx> Visitor<'tcx> for AsyncClosureFinder<'tcx> {
    type NestedFilter = nested_filter::All;

    fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
        self.tcx
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        match expr.kind {
            ExprKind::Closure(
                closure @ Closure {
                    kind:
                        ClosureKind::Coroutine(CoroutineKind::Desugared(
                            CoroutineDesugaring::Async,
                            source,
                        )),
                    ..
                },
            ) => {
                self.async_closures.push((closure, *source));
            }
            _ => {}
        }

        intravisit::walk_expr(self, expr);
    }
}

impl<'tcx> AsyncClosureFinder<'tcx> {
    fn find_async_closures(
        body_id: BodyId,
        tcx: TyCtxt<'tcx>,
    ) -> Vec<(&'tcx Closure<'tcx>, CoroutineSource)> {
        let mut finder = Self {
            tcx,
            async_closures: Default::default(),
        };

        let body = tcx.hir_body(body_id);
        finder.visit_body(body);
        finder.async_closures
    }
}
