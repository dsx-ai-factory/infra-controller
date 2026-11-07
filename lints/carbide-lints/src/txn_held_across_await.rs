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
use rustc_ast::UnOp;
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_errors::DiagDecorator;
use rustc_hir as hir;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{
    Body, Expr, ExprKind, HirId, ImplItem, ImplItemKind, Item, ItemKind, LetStmt, Node, Param,
    PatKind, Path, PathSegment, QPath, YieldSource,
};
use rustc_lint_defs::{Lint, LintPass, LintVec};
use rustc_middle::hir::nested_filter;
use rustc_middle::hir::place::{Place as HirPlace, PlaceBase};
use rustc_middle::mir::{Location, ProjectionElem};
use rustc_middle::ty::{Adt, ParamEnv, Ty as MiddleTy, TyCtxt, TypeckResults, UpvarId, UpvarPath};
use rustc_middle::{bug, mir};
use rustc_mir_dataflow::Analysis;
use rustc_mir_dataflow::impls::{MaybeInitializedPlaces, MaybeUninitializedPlaces};
use rustc_mir_dataflow::move_paths::{LookupResult, MoveData};
use rustc_span::def_id::DefId;
use rustc_span::{Span, Symbol};
use rustc_type_ir::inherent::SliceLike;

///  ### What it does
///
///  Finds cases where a `sqlx::Transaction` is held while awaiting non-database work.
///
///  ### Why is this bad?
///
///  Transactions should be used to group database writes and reads together, but should not be
///  held open while doing other work. Open transactions consume resources on the PostgreSQL server
/// and cause increased memory usage, as well as the possibility of locking database rows,
/// preventing other queries from running.
///
///  ### Notes
///
///  Database work and non-database work are determined by whether a reference to the transaction
///  is actually passed to the function being awaited. It's considered "unrelated" work if the
///  async function you're calling doesn't accept your transaction as a parameter.
///
///  ### Example
///
///  ```
///  use sqlx::PgPool;
///
///  async fn get_items(pool: PgPool) -> sqlx::Result<()> {
///      let mut txn = pool.begin()
///      db::fetch_items(&mut txn).await?; // GOOD: the txn can be held while awaiting database work
///      do_http_request().await; // BAD: txn is held open while unrelated work is happening
///  }
///
///  async fn do_http_request() {
///  }
///
///  mod db {
///      use sqlx::PgTransaction;
///
///      pub async fn fetch_items(txn: &mut PgTransaction) -> sqlx::Result<()> {
///          // ...
///          Ok(())
///      }
///  }
///  ```
///
///  Instead, finish the transaction before doing unrelated work:
///
///  ```rust
///  use sqlx::PgPool;
///
///  async fn get_items(pool: PgPool) -> sqlx::Result<()> {
///      {
///          let mut txn = pool.begin()
///          db::fetch_items(&mut txn).await?; // GOOD: the txn can be held while awaiting database work
///      }
///      do_http_request().await; // GOOD: txn is dropped before doing the http request
///  }
///
///  async fn do_http_request() {
///  }
///
///  mod db {
///      use sqlx::PgTransaction;
///
///      pub async fn fetch_items(txn: &mut PgTransaction) -> sqlx::Result<()> {
///          // ...
///          Ok(())
///      }
///  }
///  ```
pub(crate) static TXN_HELD_ACROSS_AWAIT: &Lint = &Lint {
    name: "txn_held_across_await",
    default_level: ::rustc_lint_defs::Warn,
    desc: "Do not do expensive non-database work while holding open a database transaction",
    is_externally_loaded: false,
    ..Lint::default_fields_for_macro()
};

#[derive(Clone)]
pub(crate) struct TxnHeldAcrossAwait {
    txn_symbol_paths: Vec<Vec<Symbol>>,
    txn_self_methods: Vec<Symbol>,
}

impl Default for TxnHeldAcrossAwait {
    fn default() -> Self {
        let txn_symbol_paths = vec![
            "sqlx_core::transaction::Transaction",
            "sqlx_core::pool::connection::PoolConnection",
            "sqlx_postgres::PgTransaction",
            "db::Transaction",
            "db::db_read::DbReader",
            "sqlx_postgres::connection::PgConnection",
            "sqlx_core::pool::connection::PoolConnection",
        ]
        .into_iter()
        .map(|txn| {
            txn.split("::")
                .filter_map(|s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(Symbol::intern(s))
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();
        let txn_self_methods = vec!["deref_mut", "as_pgconn", "as_mut"]
            .into_iter()
            .map(|s| Symbol::intern(s))
            .collect();

        Self {
            txn_symbol_paths,
            txn_self_methods,
        }
    }
}

impl LintPass for TxnHeldAcrossAwait {
    fn name(&self) -> &'static str {
        "TxnHeldAcrossAwait"
    }
    fn get_lints(&self) -> LintVec {
        [TXN_HELD_ACROSS_AWAIT].to_vec()
    }
}

impl TxnHeldAcrossAwait {
    /// Perform the TxnHeldAcrossAwait lint on any locals which are "interior" to a closure. For
    /// desugared `async fn` bodies, this checks both locals declared in the function, and any of
    /// its input parameters. For `async move { ... }` closures, it can only check locals defined
    /// inside the closure, and does *not* check variables referenced from the parent scope.
    pub(crate) fn check_closure_locals<'tcx>(
        &mut self,
        tcx: TyCtxt<'tcx>,
        closure: &'tcx hir::Closure<'tcx>,
    ) {
        let mir_body = tcx.mir_promoted(closure.def_id).0.borrow();
        // In async rust, functions are turned into a coroutine closure, capturing what is
        // essentially an enum, with variants corresponding to each chunk of code between await
        // points. These are what we're checking.
        let Some(coroutine) = tcx.mir_coroutine_witnesses(closure.def_id) else {
            return;
        };
        // Get the user-visible function that desugared into this coroutine
        let outer_fn = tcx.parent_hir_node(tcx.local_def_id_to_hir_id(closure.def_id));
        let param_env = self.param_env_for_owner(tcx, outer_fn, closure.def_id);

        // Get any txns passed as input parameters (which are sometimes not included in coroutine
        // variants)
        let borrowed_txn_input_params = self.find_txn_input_params(tcx, param_env, outer_fn);

        // Get the HIR (high-level representation) body for this coroutine's owner (the function
        // itself)
        let hir_node = tcx.hir_node_by_def_id(closure.def_id);
        let hir_body_id = hir_node.body_id().expect("no HIR body");
        let hir_body = tcx.hir_body(hir_body_id);

        // Gather typeck results, needed to get type info for locals
        let typeck_results = tcx.typeck_body(hir_body_id);
        let txn_lineage = TxnLineage::gather(self, hir_body, typeck_results, tcx, param_env);

        // Gather all move data in the MIR body (to analyze when txn's are moved out of scope)
        let move_data = MoveData::gather_moves(&mir_body, tcx, |_| true);

        // Now we go through each variant of the coroutine (which correspond to await points), and
        // find any transactions being held across them.
        for (variant, source_info) in coroutine.variant_source_info.iter_enumerated() {
            if source_info.span.is_empty() {
                continue;
            }

            // Are any Transactions being held across this await?
            let mut txn_local_spans: Vec<Span> = Vec::new();

            // First check the fields in this coroutine variant:
            for field in &coroutine.variant_fields[variant] {
                let ty_cause = &coroutine.field_tys[field.clone()];
                if self.is_sqlx_transaction_ty(tcx, param_env, &ty_cause.ty) {
                    txn_local_spans.push(ty_cause.source_info.span)
                }
            }

            // Next find any txn locals we're borrowing as part of the function's parameters. These
            // are sometimes not used in a variant, leading to a false negative, because we are
            // still "logically" holding a transaction if we're borrowing it, even if the coroutine
            // variant no longer needs the local.
            for param in &borrowed_txn_input_params {
                // Skip params that already being warned about, to avoid duplicate warnings.
                if !txn_local_spans
                    .iter()
                    .any(|variant_span| variant_span.overlaps(param.pat.span))
                {
                    txn_local_spans.push(param.pat.span)
                }
            }

            // Now we have spans for each txn local in scope across this await point
            for local_span in txn_local_spans {
                // Find the MIR (mid-level representation) local for this span
                let Some((local, local_decl)) = self.find_mir_local(&mir_body, local_span) else {
                    bug!("could not find mir local corresponding to local span");
                };

                // Don't warn if the local is not alive (ie. has been moved via a .commit() or
                // something)
                if is_local_dead_at_span(local, source_info.span, &move_data, &mir_body, tcx) {
                    continue;
                }

                // If this await is from a function call where we're passing this transaction local,
                // don't warn.
                //
                // Note: We pass the local_decl.source_info.span we got from the MIR local, and not
                // the local_span we got from coroutine.variant_source_info, because the MIR local
                // is the "real" definition of the variable, and the span it returns is the one that
                // will actually match the HIR ID we're looking for. See comments in
                // [`TxnHeldAcrossAwait::find_mir_local`] for more info.
                if DbAwaitFinder::await_is_passing_local_as_param(
                    &self,
                    DbAwaitSearchParams {
                        await_span: source_info.span,
                        txn_local: SpanOrHirId::Span(local_decl.source_info.span),
                    },
                    &txn_lineage,
                    hir_body,
                    tcx,
                ) {
                    continue;
                }

                tcx.emit_node_span_lint(
                    TXN_HELD_ACROSS_AWAIT,
                    hir_body.id().hir_id,
                    source_info.span,
                    DiagDecorator(|diag| {
                        diag.primary_message(
                            "A sqlx::Transaction is being held across this 'await' point",
                        );
                        diag.span_note(local_span, "Transaction declared here");
                    }),
                );
            }
        }
    }

    /// Perform the TxnHeldAcrossAwait lint on an explicit async closure (ie. async move { ... }),
    /// checking for transactions captured as "upvars" (variables captured from the surrounding
    /// scope.) This uses different logic for finding alive variables from the sort of closures
    /// generated by `async fn` desugaring, so it gets a separate function.
    pub(crate) fn check_closure_upvars<'tcx>(
        &mut self,
        tcx: TyCtxt<'tcx>,
        closure: &'tcx hir::Closure<'tcx>,
    ) {
        let mir_body = tcx.mir_promoted(closure.def_id).0.borrow();
        let Some(coroutine) = tcx.mir_coroutine_witnesses(closure.def_id) else {
            return;
        };
        let outer_fn = tcx.parent_hir_node(tcx.local_def_id_to_hir_id(closure.def_id));
        let param_env = self.param_env_for_owner(tcx, outer_fn, closure.def_id);

        // Gather typeck results, needed to get type info for locals
        let typeck_results = tcx.typeck_body(closure.body);
        let hir_body = tcx.hir_body(closure.body);
        let txn_lineage = TxnLineage::gather(self, hir_body, typeck_results, tcx, param_env);

        // Gather all move data in the MIR body (to analyze when txn's are moved out of scope)
        let move_data = MoveData::gather_moves(&mir_body, tcx, |_| true);

        // For upvars, we use `tcx.closure_captures()` to discover which values are captured. For
        // liveness checking we need a MIR `Place` that refers to the storage of that capture. In
        // closure/coroutine MIR, the first argument is the closure/coroutine environment (`self`).
        // Captured upvars are stored as fields in that environment, in the same order as
        // `closure_captures()`. So the `capture_idx`th capture corresponds to `self.<field capture_idx>`
        // (or `(*self).<field capture_idx>` when `self` is by reference).
        let txn_local_places = tcx
            .closure_captures(closure.def_id)
            .iter()
            .enumerate()
            .filter_map(|(capture_idx, place)| {
                if !self.is_sqlx_transaction_ty(tcx, param_env, &place.place.base_ty) {
                    return None;
                }

                // In closure/coroutine MIR, the first argument is the environment (`self`) that
                // stores captured upvars. It’s an Adt, so a struct with anonymous/numbered fields
                // that correspond to the captures (in the same order that `closure_captures`
                // returns)
                let Some(self_local) = mir_body.args_iter().next() else {
                    bug!("Closure/coroutine MIR body should have a first self argument");
                };
                let mut proj = vec![];

                // Get the MIR LocalDecl for the self arg
                let self_ty = mir_body.local_decls[self_local].ty;

                // If the env argument is by reference, MIR represents access to captures as
                // (*self).field, so we add a Deref projection before selecting the capture field
                if self_ty.is_ref() {
                    proj.push(ProjectionElem::Deref);
                }

                // Another projection is the `capture_idx`th field within the self Adt, which is
                // the actual upvar we want.
                let field = rustc_abi::FieldIdx::from_usize(capture_idx);
                proj.push(ProjectionElem::Field(field, place.place.ty()));

                let mir_place = mir::Place {
                    local: self_local,
                    projection: tcx.mk_place_elems(&proj),
                };
                Some((mir_place, *place))
            })
            .collect::<Vec<_>>();

        // Now we go through each variant of the coroutine (which correspond to await points), and
        // find any transactions being held across them.
        for source_info in coroutine.variant_source_info.iter() {
            if source_info.span.is_empty() {
                continue;
            }

            // Are any Transactions being held across this await?
            // Now we have "place" information for each txn local in scope across this await point
            for (mir_local_place, hir_local_place) in txn_local_places.iter() {
                let Some(txn_local_hir_id) = hir_local_place.place.hir_id() else {
                    bug!(
                        "No hir_id for txn local at {:?}",
                        hir_local_place.var_ident.span
                    );
                };

                // Don't warn if the local is not alive (ie. has been moved via a .commit() or
                // something)
                if is_local_dead_at_place(
                    &mir_local_place,
                    source_info.span,
                    &move_data,
                    &mir_body,
                    tcx,
                ) {
                    continue;
                }

                // If this await is from a function call where we're passing this transaction local,
                // don't warn.
                if DbAwaitFinder::await_is_passing_local_as_param(
                    &self,
                    DbAwaitSearchParams {
                        await_span: source_info.span,
                        txn_local: SpanOrHirId::HirId(txn_local_hir_id),
                    },
                    &txn_lineage,
                    hir_body,
                    tcx,
                ) {
                    continue;
                }

                tcx.emit_node_span_lint(
                    TXN_HELD_ACROSS_AWAIT,
                    closure.body.hir_id,
                    source_info.span,
                    DiagDecorator(|diag| {
                        diag.primary_message(
                            "A sqlx::Transaction is being held across this 'await' point",
                        );
                        diag.span_note(hir_local_place.var_ident.span, "Transaction declared here");
                    }),
                );
            }
        }
    }

    /// Check if the given DefId resolves to a sqlx::Transaction
    fn is_sqlx_transaction(&self, tcx: TyCtxt<'_>, def_id: DefId) -> bool {
        let path = tcx.def_path(def_id);
        let path_as_syms = path
            .data
            .iter()
            .map(|i| i.as_sym(false))
            .collect::<Vec<_>>();
        let result = self
            .txn_symbol_paths
            .iter()
            // TODO: ignoring crates via skip(1), we shouldn't do that
            .any(|txn_path| txn_path.iter().skip(1).eq(path_as_syms.iter()));

        result
    }

    /// Check if the given rustc_middle::Ty (ie. output from typeck) resolves to a sqlx::Transaction
    fn is_sqlx_transaction_ty<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
        param_env: ParamEnv<'tcx>,
        ty: &MiddleTy<'tcx>,
    ) -> bool {
        let peeled_ty = ty.peel_refs();
        let def_id = match peeled_ty.kind() {
            Adt(adt, _) => Some(adt.did()),
            rustc_middle::ty::Dynamic(predicates, _) => predicates.principal_def_id(),
            _ => None,
        };

        if let Some(def_id) = def_id {
            if self.is_sqlx_transaction(tcx, def_id) {
                return true;
            }
        }

        // Fall back to checking trait bounds (ie. DbReader) when the type itself isn't an ADT we recognize.
        self.ty_has_txn_bound(tcx, param_env, *ty)
            || self.ty_has_txn_bound(tcx, param_env, peeled_ty)
    }

    fn ty_has_txn_bound<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
        param_env: ParamEnv<'tcx>,
        ty: MiddleTy<'tcx>,
    ) -> bool {
        param_env.caller_bounds().iter().any(|predicate| {
            let Some(trait_pred) = predicate.as_trait_clause() else {
                return false;
            };
            if !self.is_sqlx_transaction(tcx, trait_pred.def_id()) {
                return false;
            }

            let self_ty = trait_pred.skip_binder().trait_ref.self_ty().peel_refs();
            self_ty == ty.peel_refs()
        })
    }

    /// Given a function Node, get any input parameters of type sqlx::Transaction
    fn find_txn_input_params<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
        param_env: ParamEnv<'tcx>,
        fn_node: Node,
    ) -> Vec<Param<'tcx>> {
        if let Node::Item(Item {
            kind:
                ItemKind::Fn {
                    sig,
                    body: fn_body_id,
                    ..
                },
            ..
        })
        | Node::ImplItem(ImplItem {
            kind: ImplItemKind::Fn(sig, fn_body_id),
            ..
        }) = fn_node
        {
            let body = tcx.hir_body(*fn_body_id);
            let typeck_results = tcx.typeck_body(*fn_body_id);
            sig.decl
                .inputs
                .iter()
                .zip(body.params.iter())
                .filter_map(|(_decl, param)| {
                    let ty = typeck_results.node_type_opt(param.pat.hir_id)?;
                    if self.is_sqlx_transaction_ty(tcx, param_env, &ty) {
                        Some(*param)
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            vec![]
        }
    }

    /// The closure coroutine's def_id does not always carry all the trait bounds from the user
    /// facing function. Grab the parent's param_env when we can so trait-based detection (DbReader)
    /// works.
    fn param_env_for_owner<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
        owner: Node<'tcx>,
        fallback: rustc_hir::def_id::LocalDefId,
    ) -> ParamEnv<'tcx> {
        match owner {
            Node::Item(item) => tcx.param_env(item.owner_id.def_id),
            Node::ImplItem(item) => tcx.param_env(item.owner_id.def_id),
            _ => tcx.param_env(fallback),
        }
    }

    /// Check if the expression contained in `txn_local_span` is being passed as an argument to the expression `awaited`.
    ///
    /// In the snippet:
    ///
    /// ```ignore
    /// async fn db_wrapper(txn: &mut sqlx::Transaction) {
    ///     sqlx::query("SELECT true").execute(txn).await.unwrap()
    /// }
    /// ```
    ///
    /// - `txn_local_span` would cover `txn` in db_wrapper's parameter list, since that's where the
    ///   local is defined.
    /// - `awaited` would cover `execute(txn)`, since it's the expression that's actually triggering an
    ///   await.
    ///
    /// In this example, we would return true, since the `txn` binding is being passed as an argument to
    /// `execute`.
    ///
    /// Additionally, these would all qualify as passing txn:
    ///
    /// - `txn.prepare("SELECT true") // txn is the receiver`
    /// - `some_fn(&mut txn) // any level of borrows are ok`
    /// - `some_fn(txn.deref_mut()) // deref_mut() is a special case`
    fn is_passing_txn<'tcx>(
        &self,
        txn_local_hir_ids: &FxHashSet<HirId>,
        awaited: &Expr<'tcx>,
    ) -> bool {
        let args = match awaited.peel_borrows().kind {
            // e.g. `db::machine::get(db)`
            ExprKind::Call(_, args) => args,
            // e.g. `some_future.await` where the future came from a method call
            ExprKind::MethodCall(_, recv, args, _) => {
                // Short-circuit: Is the transaction the receiver? txn.commit().await,
                // txn.prepare(...).await(), etc count as passing txn (as the self param)
                if let ExprKind::Path(qpath) = recv.kind {
                    if let Some(Res::Local(hir_id)) = qpath_res(&qpath)
                        && txn_local_hir_ids.contains(&hir_id)
                    {
                        return true;
                    }
                }
                if local_hir_id(recv).is_some_and(|hir_id| txn_local_hir_ids.contains(&hir_id)) {
                    return true;
                }
                args
            }
            _ => return false,
        };

        args.iter().any(|arg| {
            // Is the arg either the txn, or txn.deref_mut()? Either of these count as "doing database work".
            let txn_param_res = match arg.peel_borrows().peel_derefs().kind {
                // The arg is a simple resolved path, optionally with a deref (*) before it.
                ExprKind::Path(QPath::Resolved(_, Path { res, .. })) => Some(res),
                // The arg is an inline method call (ie. `txn.foo()` in `some_func(txn.foo()))`)
                ExprKind::MethodCall(
                    PathSegment {
                        ident: method_ident,
                        ..
                    },
                    receiver,
                    ..,
                ) => {
                    // We only bother checking for simple "txn.foo()", nothing like
                    // "opt_txn.map(|t| t.foo())" or anything complicated.
                    if let ExprKind::Path(QPath::Resolved(
                        _,
                        Path {
                            segments: [PathSegment { res, .. }],
                            ..
                        },
                    )) = receiver.kind
                    {
                        // We only support calling "deref_mut" on the txn to qualify as "doing db work"
                        if self.txn_self_methods.contains(&method_ident.name) {
                            Some(res)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };

            txn_param_res.is_some_and(|res| {
                if let Res::Local(hir_id) = res {
                    txn_local_hir_ids.contains(hir_id)
                } else {
                    false
                }
            })
        })
    }

    fn find_mir_local<'a, 'tcx>(
        &self,
        mir_body: &'a mir::Body<'tcx>,
        txn_local_span: Span,
    ) -> Option<(mir::Local, &'a mir::LocalDecl<'tcx>)> {
        for (local, decl) in mir_body.local_decls.iter_enumerated() {
            // NOTE: We need to use source_equal here (and other places in this file) because we're
            // comparing HIR vs MIR representations of the code, and spans there don't always
            // compare equal, even if they're the same source location.
            if txn_local_span.source_equal(decl.source_info.span) {
                return Some((local, decl));
            }
        }
        None
    }
}

#[derive(Default)]
struct TxnLineage {
    children: FxHashMap<HirId, Vec<HirId>>,
}

impl TxnLineage {
    fn gather<'tcx>(
        lint: &TxnHeldAcrossAwait,
        body: &'tcx Body<'tcx>,
        typeck_results: &'tcx TypeckResults<'tcx>,
        tcx: TyCtxt<'tcx>,
        param_env: ParamEnv<'tcx>,
    ) -> Self {
        let mut finder = TxnLineageFinder {
            lint,
            typeck_results,
            tcx,
            param_env,
            lineage: Self::default(),
        };
        finder.visit_body(body);
        finder.lineage
    }

    fn descendants_including(&self, local: HirId) -> FxHashSet<HirId> {
        let mut descendants = FxHashSet::default();
        let mut pending = vec![local];
        while let Some(local) = pending.pop() {
            if descendants.insert(local)
                && let Some(children) = self.children.get(&local)
            {
                pending.extend(children);
            }
        }
        descendants
    }
}

struct TxnLineageFinder<'a, 'tcx> {
    lint: &'a TxnHeldAcrossAwait,
    typeck_results: &'tcx TypeckResults<'tcx>,
    tcx: TyCtxt<'tcx>,
    param_env: ParamEnv<'tcx>,
    lineage: TxnLineage,
}

impl<'tcx> Visitor<'tcx> for TxnLineageFinder<'_, 'tcx> {
    // TypeckResults is scoped to one HIR owner. Nested items and closures have their own results
    // and are checked independently, so crossing into them would query this owner's table with
    // foreign HirIds and trigger an internal compiler error.
    type NestedFilter = rustc_hir::intravisit::nested_filter::None;

    fn visit_local(&mut self, let_stmt: &'tcx LetStmt<'tcx>) {
        if let Some(init) = let_stmt.init
            && let PatKind::Binding(_, child, ..) = let_stmt.pat.kind
            && self.typeck_results.node_type_opt(child).is_some_and(|ty| {
                self.lint
                    .is_sqlx_transaction_ty(self.tcx, self.param_env, &ty)
            })
            && let Some(parent) = self.nested_transaction_parent(init)
        {
            self.lineage.children.entry(parent).or_default().push(child);
        }

        intravisit::walk_local(self, let_stmt);
    }
}

impl<'tcx> TxnLineageFinder<'_, 'tcx> {
    fn nested_transaction_parent(&self, init: &'tcx Expr<'tcx>) -> Option<HirId> {
        let mut call_finder = NestedTransactionCallFinder {
            lint: self.lint,
            typeck_results: self.typeck_results,
            tcx: self.tcx,
            param_env: self.param_env,
            parent: None,
        };
        call_finder.visit_expr(init);
        call_finder.parent
    }
}

struct NestedTransactionCallFinder<'a, 'tcx> {
    lint: &'a TxnHeldAcrossAwait,
    typeck_results: &'tcx TypeckResults<'tcx>,
    tcx: TyCtxt<'tcx>,
    param_env: ParamEnv<'tcx>,
    parent: Option<HirId>,
}

impl<'tcx> Visitor<'tcx> for NestedTransactionCallFinder<'_, 'tcx> {
    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        if self.parent.is_some() {
            return;
        }

        self.parent = match expr.kind {
            ExprKind::MethodCall(_, receiver, _, _) => {
                let def_id = self.typeck_results.type_dependent_def_id(expr.hir_id);
                if self.source_is_transaction(receiver)
                    && def_id.is_some_and(|def_id| {
                        self.tcx.item_name(def_id).as_str() == "begin"
                            && self
                                .tcx
                                .def_path_str(def_id)
                                .ends_with("sqlx::Acquire::begin")
                    })
                {
                    local_hir_id(receiver)
                } else {
                    None
                }
            }
            ExprKind::Call(callee, [first, ..]) => {
                let def_id = match callee.kind {
                    ExprKind::Path(ref qpath) => self
                        .typeck_results
                        .qpath_res(qpath, callee.hir_id)
                        .opt_def_id(),
                    _ => None,
                };
                if self.source_is_transaction(first)
                    && def_id.is_some_and(|def_id| {
                        let parent = self.tcx.parent(def_id);
                        if !matches!(self.tcx.def_kind(parent), DefKind::Impl { .. }) {
                            return false;
                        }
                        let parent_ty = self
                            .tcx
                            .type_of(parent)
                            .instantiate_identity()
                            .skip_norm_wip();
                        self.tcx.item_name(def_id).as_str() == "begin_inner"
                            && (self.lint.is_sqlx_transaction_ty(
                                self.tcx,
                                self.param_env,
                                &parent_ty,
                            ) || self
                                .tcx
                                .def_path_str(def_id)
                                .ends_with("db::Transaction::begin_inner"))
                    })
                {
                    local_hir_id(first)
                } else {
                    None
                }
            }
            _ => None,
        };

        intravisit::walk_expr(self, expr);
    }
}

impl NestedTransactionCallFinder<'_, '_> {
    fn source_is_transaction(&self, source: &Expr<'_>) -> bool {
        self.typeck_results.expr_ty_opt(source).is_some_and(|ty| {
            self.lint
                .is_sqlx_transaction_ty(self.tcx, self.param_env, &ty)
        })
    }
}

fn local_hir_id(expr: &Expr<'_>) -> Option<HirId> {
    let expr = expr.peel_borrows().peel_derefs();
    match expr.kind {
        ExprKind::Path(ref qpath) => match qpath_res(qpath) {
            Some(Res::Local(hir_id)) => Some(hir_id),
            _ => None,
        },
        ExprKind::MethodCall(ref segment, receiver, _, _)
            if matches!(
                segment.ident.name.as_str(),
                "deref_mut" | "as_pgconn" | "as_mut"
            ) =>
        {
            local_hir_id(receiver)
        }
        ExprKind::Field(base, _) => local_hir_id(base),
        _ => None,
    }
}

trait ExprExt {
    fn peel_derefs(&self) -> &Self;
}

impl ExprExt for Expr<'_> {
    fn peel_derefs(&self) -> &Self {
        let mut expr = self;
        while let ExprKind::Unary(UnOp::Deref, inner) = &expr.kind {
            expr = inner;
        }
        expr
    }
}

/// Visitor to find two things:
///
/// - The expression that `await_span` (the `.await` statement itself) is chained to, for example
///   `foo()` in `foo().await`
/// - The `HirId` of the local variable that `txn_local_span` refers to, if it's not already known.
///
/// With both of these pieces of information, we can answer the question "is this await statement
/// passing this transaction as an argument?" which is how we "allow" a transaction to be held
/// across an await.
struct DbAwaitFinder<'tcx> {
    tcx: TyCtxt<'tcx>,
    params: DbAwaitSearchParams,
    // Keep track of all expressions so we can recover the one we're awaiting
    all_exprs: FxHashMap<HirId, &'tcx Expr<'tcx>>,

    // What we're trying to find:
    found_txn_local_hir_id: Option<HirId>,
    awaited_expr_hir_id: Option<HirId>,
}

impl<'a, 'tcx> Visitor<'tcx> for DbAwaitFinder<'tcx> {
    type NestedFilter = nested_filter::All;

    fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
        self.tcx
    }

    // visit_local is here to find the hir_id of the local indicated by self.txn_local_span, and
    // assign it to self.txn_local_hir_id. This way we can correlate the locals sent to the awaited
    // call, to the txn local being held across the await.
    fn visit_local(&mut self, let_stmt: &'tcx LetStmt<'tcx>) {
        // Are we looking for the HIR id of the txn local?
        if let SpanOrHirId::Span(txn_local_span) = self.params.txn_local {
            if let PatKind::Tuple(tuple_vars, _dotdot_position) = let_stmt.pat.kind {
                // Statement is a tuple... e.g. `let (txn, stuff) = make_transaction_and_stuff()`;
                for v in tuple_vars.iter() {
                    if v.span.source_equal(txn_local_span) {
                        self.found_txn_local_hir_id = Some(v.hir_id);
                    }
                }
            } else if let_stmt.pat.span.source_equal(txn_local_span) {
                // Statement's `pat` (the place to the left of the equals sign) equals the span we're
                // looking for.
                self.found_txn_local_hir_id = Some(let_stmt.pat.hir_id);
            }
        };

        intravisit::walk_local(self, let_stmt);
    }

    // visit_expr finds the actual function call that is being `.await`ed, and stores it as self.await_expr.
    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        // Keep track of all expressions in this body: We end up visiting the await statement itself
        // after the expression it's awaiting
        self.all_exprs.insert(expr.hir_id, expr);

        // We care about the actual `.await` statements...
        if let ExprKind::Yield(
            _,
            YieldSource::Await {
                expr: Some(awaited),
            },
        ) = expr.kind
        {
            // ... particularly the one that equals `await_span`
            if expr.span.source_equal(self.params.await_span) {
                self.awaited_expr_hir_id = Some(awaited);
            }
        }
        intravisit::walk_expr(self, expr);
    }
}

#[derive(Copy, Clone)]
struct DbAwaitSearchParams {
    // The span containing the `await` statement itself
    await_span: Span,
    // The original definition of the local variable holding the transaction (typically
    // named `txn`, etc.) We get this from the data in mir_coroutine_witnesses
    txn_local: SpanOrHirId,
}

#[derive(Copy, Clone)]
enum SpanOrHirId {
    Span(Span),
    HirId(HirId),
}

impl SpanOrHirId {
    fn hir_id(&self) -> Option<HirId> {
        match self {
            SpanOrHirId::HirId(hir_id) => Some(*hir_id),
            SpanOrHirId::Span(_) => None,
        }
    }
}

impl<'tcx> DbAwaitFinder<'tcx> {
    /// Checks if `await_span` is passing the local originally defined in `local_span` as a parameter.
    ///
    /// For example, in:
    ///
    /// ```ignore
    /// let txn = make_transaction();
    /// do_database_work(txn).await;
    /// ```
    ///
    /// - Parameter `await_span` would match `await` (that is, it's the await statement itself)
    /// - Parameter `local_span` would be `txn` in `let txn = ...` (it's the local we're checking for)
    /// - this would return `true`, since txn is being passed.
    ///
    /// To do this, this function has to:
    ///
    /// - Find the `do_database_work(txn)` awaitee expression (the one being awaited)
    /// - Find the original `let txn = make_transaction()` statement (extracting the hir_id of the
    ///   `txn` variable)
    /// - Once both are found, calls `lint.is_passing_txn()` with the awaitee expression and the txn
    ///   hir_id, and so returns true if any of the args to the expression are locals with the same
    ///   hir_id as the txn local.
    fn await_is_passing_local_as_param(
        lint: &TxnHeldAcrossAwait,
        params: DbAwaitSearchParams,
        txn_lineage: &TxnLineage,
        body: &'tcx Body,
        tcx: TyCtxt<'tcx>,
    ) -> bool {
        let mut finder = Self {
            tcx,
            params,
            all_exprs: Default::default(),
            awaited_expr_hir_id: None,
            found_txn_local_hir_id: None,
        };
        finder.visit_body(body);

        let Some(txn_local_hir_id) = finder
            .params
            .txn_local
            .hir_id()
            .or(finder.found_txn_local_hir_id)
        else {
            // Here is a good place to put a debugging eprintln if we need to expand the detection
            // of txn locals: Right now we only find simple `let txn =` or `let (txn, ..) =`
            // statements, and nothing complex like `if let Some(txn) =` or `let SomeStruct { txn,
            // ..} =` yet.
            return false;
        };

        let Some(await_expr) = finder.awaited_expr() else {
            tcx.emit_node_span_lint(
                TXN_HELD_ACROSS_AWAIT,
                body.id().hir_id,
                params.await_span,
                DiagDecorator(|diag| {
                    diag.primary_message(
                        "DbAwaitFinder could not find the awaited expression for this await span",
                    );
                }),
            );
            return false;
        };

        let accepted_locals = txn_lineage.descendants_including(txn_local_hir_id);
        lint.is_passing_txn(&accepted_locals, await_expr)
    }

    fn awaited_expr(&self) -> Option<&'tcx Expr<'tcx>> {
        self.awaited_expr_hir_id
            .and_then(|hir_id| self.all_exprs.get(&hir_id).map(|e| *e))
    }
}

/// Check if the given mir::Local is "dead" at the point of a given span, meaning it was moved out
/// of scope, for instance via a call to txn.commit() or txn.rollback().
fn is_local_dead_at_span<'a, 'tcx>(
    local: rustc_middle::mir::Local,
    span: Span,
    move_data: &'a MoveData<'tcx>,
    body: &'a mir::Body<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    // Maybe{Un,}InitializedPlaces are analyses performed by the borrow checker which can tell you
    // what "places" (storage holding locals) are maybe-initialized and maybe-initialized at a given
    // source code location. If a local is in maybe-uninitialized and not in maybe-initialized, it's
    // "dead" at that point (ie. it's been moved, usually via a call to txn.rollback() or txn.commit().)
    let mut maybe_initialized_places = MaybeInitializedPlaces::new(tcx, body, move_data)
        .iterate_to_fixpoint(tcx, body, None)
        .into_results_cursor(body);
    let mut maybe_uninitialized_places = MaybeUninitializedPlaces::new(tcx, body, move_data)
        .iterate_to_fixpoint(tcx, body, None)
        .into_results_cursor(body);

    // Iterate through each BasicBlock (chunk of code) and each Statement within, until we get to
    // the span we're looking for (ie. the `.await` call), and return whether the local (txn) we're
    // looking for is alive or dead when we get there.
    let mut is_dead = false;
    'outer: for (block, block_data) in body.basic_blocks.iter_enumerated() {
        for (statement_index, statement) in block_data.statements.iter().enumerate() {
            let loc = Location {
                block,
                statement_index,
            };

            // We got to the span we're looking for, we're done, and we can return `is_dead`.
            if statement.source_info.span.source_equal(span) {
                break 'outer;
            }

            // Check if the local is alive or dead after this statement, and store it as is_dead.
            maybe_uninitialized_places.seek_after_primary_effect(loc);
            maybe_initialized_places.seek_after_primary_effect(loc);
            let uninits = maybe_uninitialized_places.get();
            let inits = maybe_initialized_places.get();
            if let Some(move_path_index) = move_data.rev_lookup.find_local(local) {
                if uninits.contains(move_path_index) && !inits.contains(move_path_index) {
                    is_dead = true;
                } else if inits.contains(move_path_index) && !uninits.contains(move_path_index) {
                    is_dead = false;
                }
            }
        }
    }

    is_dead
}

fn qpath_res(qpath: &hir::QPath<'_>) -> Option<Res> {
    match *qpath {
        QPath::Resolved(_, path) => Some(path.res),
        _ => None,
    }
}

/// Check if the given mir::Place is "dead" at the point of a given span, meaning it was moved out
/// of scope, for instance via a call to txn.commit() or txn.rollback().
fn is_local_dead_at_place<'a, 'tcx>(
    mir_place: &mir::Place<'tcx>,
    span: Span,
    move_data: &'a MoveData<'tcx>,
    body: &'a mir::Body<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> bool {
    let lookup_result = move_data.rev_lookup.find(mir_place.as_ref());
    let move_path_index = match lookup_result {
        LookupResult::Exact(mpi) => mpi,
        _ => {
            return false;
        }
    };
    // Maybe{Un,}InitializedPlaces are analyses performed by the borrow checker which can tell you
    // what "places" (storage holding locals) are maybe-initialized and maybe-initialized at a given
    // source code location. If a local is in maybe-uninitialized and not in maybe-initialized, it's
    // "dead" at that point (ie. it's been moved, usually via a call to txn.rollback() or txn.commit().)
    let mut maybe_initialized_places = MaybeInitializedPlaces::new(tcx, body, move_data)
        .iterate_to_fixpoint(tcx, body, None)
        .into_results_cursor(body);
    let mut maybe_uninitialized_places = MaybeUninitializedPlaces::new(tcx, body, move_data)
        .iterate_to_fixpoint(tcx, body, None)
        .into_results_cursor(body);

    // Iterate through each BasicBlock (chunk of code) and each Statement within, until we get to
    // the span we're looking for (ie. the `.await` call), and return whether the local (txn) we're
    // looking for is alive or dead when we get there.
    let mut is_dead = false;
    'outer: for (block, block_data) in body.basic_blocks.iter_enumerated() {
        for (statement_index, statement) in block_data.statements.iter().enumerate() {
            let loc = Location {
                block,
                statement_index,
            };

            // We got to the span we're looking for, we're done, and we can return `is_dead`.
            if statement.source_info.span.source_equal(span) {
                break 'outer;
            }

            // Check if the local is alive or dead after this statement, and store it as is_dead.
            maybe_uninitialized_places.seek_after_primary_effect(loc);
            maybe_initialized_places.seek_after_primary_effect(loc);
            let uninits = maybe_uninitialized_places.get();
            let inits = maybe_initialized_places.get();
            if uninits.contains(move_path_index) && !inits.contains(move_path_index) {
                is_dead = true;
            } else if inits.contains(move_path_index) && !uninits.contains(move_path_index) {
                is_dead = false;
            }
        }
    }

    is_dead
}

trait PlaceHelper {
    fn hir_id(&self) -> Option<HirId>;
}

impl PlaceHelper for HirPlace<'_> {
    fn hir_id(&self) -> Option<HirId> {
        if let PlaceBase::Local(local_hir_id) = &self.base {
            Some(*local_hir_id)
        } else if let PlaceBase::Upvar(UpvarId {
            var_path: UpvarPath { hir_id },
            ..
        }) = &self.base
        {
            Some(*hir_id)
        } else {
            None
        }
    }
}
