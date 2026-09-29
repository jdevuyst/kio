use std::time::Instant;

use crate::ast::{
    CallArg, Expr, HostFnParam, Item, LitAnnotationExt, Module, OpChainKind, Phase, Signature,
    SignatureParam, TargetBlock, Type,
};
use crate::pass::resolve::Package;

pub(crate) fn enabled() -> bool {
    crate::timing::build_enabled()
}

pub(crate) fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

pub(crate) fn package_label<P: Phase>(package: &Package<P>) -> String {
    package
        .package_file()
        .map(|entry| entry.package_name.clone())
        .unwrap_or_else(|| "<no-package-file>".to_owned())
}

pub(crate) fn log_workspace(
    package: &str,
    selected_targets: usize,
    typecheck_ms: f64,
    dispatch_ms: f64,
    total_ms: f64,
) {
    if !enabled() {
        return;
    }
    eprintln!(
        "build-workspace-timing: package={} selected_targets={} typecheck_ms={:.3} target_dispatch_ms={:.3} total_ms={:.3}",
        package, selected_targets, typecheck_ms, dispatch_ms, total_ms
    );
}

pub(crate) fn log_target(
    package: &str,
    target: &TargetBlock,
    backend: &str,
    artifact_hit: bool,
    fields: &[(&str, f64)],
    total_ms: f64,
) {
    if !enabled() {
        return;
    }
    let mut line = format!(
        "build-target-timing: package={} target={} backend={} artifact_hit={} total_ms={:.3}",
        package,
        target.id,
        backend,
        usize::from(artifact_hit),
        total_ms
    );
    for (name, value) in fields {
        line.push_str(&format!(" {name}_ms={value:.3}"));
    }
    eprintln!("{line}");
}

/// Timing breakdown of the backend-agnostic recover → optimize → route →
/// annotate prefix, emitted once per build (see
/// [`crate::cmd::build::SharedRoutedPackage`]) rather than once per target.
pub(crate) fn log_shared_prefix(package: &str, fields: &[(&str, f64)]) {
    if !enabled() {
        return;
    }
    let mut line = format!("build-shared-prefix-timing: package={package}");
    for (name, value) in fields {
        line.push_str(&format!(" {name}_ms={value:.3}"));
    }
    eprintln!("{line}");
}

/// Shape of the shared (backend-agnostic) prefix package at one phase,
/// emitted once per build. The target-keyed [`log_package_shape`] still
/// covers the per-target subjects (`prime` for kio-prime); the shared
/// prefix's `prime` / `enriched` / `routed` shapes are logged here.
pub(crate) fn log_shared_package_shape<P: Phase>(package: &str, phase: &str, tree: &Package<P>) {
    if !enabled() {
        return;
    }
    let stats = BuildShapeStats::from_package(tree);
    eprintln!(
        "build-shared-shape: package={} phase={} modules={} uses={} items={} functions={} rec_groups={} type_rec_groups={} aliases={} newtypes={} labels={} elaborator_defs={} equivs={} ops={} export_items={} host_fns={} export_fns={} expr_nodes={} type_nodes={} calls={} rec_calls={} fn_exprs={} lets={} seqs={} field_syntax_calls={} user_elab_calls={} enriched_nodes={} low_nodes={} max_expr_depth={} max_type_depth={}",
        package,
        phase,
        stats.modules,
        stats.imports,
        stats.items,
        stats.functions,
        stats.rec_groups,
        stats.type_rec_groups,
        stats.aliases,
        stats.newtypes,
        stats.labels,
        stats.elaborator_defs,
        stats.equivs,
        stats.ops,
        stats.export_items,
        stats.host_fns,
        stats.export_fns,
        stats.expr_nodes,
        stats.type_nodes,
        stats.calls,
        stats.rec_calls,
        stats.fn_exprs,
        stats.lets,
        stats.seqs,
        stats.field_syntax_calls,
        stats.user_elab_calls,
        stats.enriched_nodes,
        stats.low_nodes,
        stats.max_expr_depth,
        stats.max_type_depth
    );
}

pub(crate) fn log_package_shape<P: Phase>(
    package: &str,
    target: &TargetBlock,
    backend: &str,
    phase: &str,
    subject: &str,
    tree: &Package<P>,
) {
    if !enabled() {
        return;
    }
    let stats = BuildShapeStats::from_package(tree);
    eprintln!(
        "build-shape: package={} target={} backend={} phase={} subject={} modules={} uses={} items={} functions={} rec_groups={} type_rec_groups={} aliases={} newtypes={} labels={} elaborator_defs={} equivs={} ops={} export_items={} host_fns={} export_fns={} expr_nodes={} type_nodes={} calls={} rec_calls={} fn_exprs={} lets={} seqs={} field_syntax_calls={} user_elab_calls={} enriched_nodes={} low_nodes={} max_expr_depth={} max_type_depth={}",
        package,
        target.id,
        backend,
        phase,
        subject,
        stats.modules,
        stats.imports,
        stats.items,
        stats.functions,
        stats.rec_groups,
        stats.type_rec_groups,
        stats.aliases,
        stats.newtypes,
        stats.labels,
        stats.elaborator_defs,
        stats.equivs,
        stats.ops,
        stats.export_items,
        stats.host_fns,
        stats.export_fns,
        stats.expr_nodes,
        stats.type_nodes,
        stats.calls,
        stats.rec_calls,
        stats.fn_exprs,
        stats.lets,
        stats.seqs,
        stats.field_syntax_calls,
        stats.user_elab_calls,
        stats.enriched_nodes,
        stats.low_nodes,
        stats.max_expr_depth,
        stats.max_type_depth
    );
}

#[derive(Default)]
struct BuildShapeStats {
    modules: usize,
    imports: usize,
    items: usize,
    functions: usize,
    rec_groups: usize,
    type_rec_groups: usize,
    aliases: usize,
    newtypes: usize,
    labels: usize,
    elaborator_defs: usize,
    equivs: usize,
    ops: usize,
    export_items: usize,
    host_fns: usize,
    export_fns: usize,
    expr_nodes: usize,
    type_nodes: usize,
    calls: usize,
    rec_calls: usize,
    fn_exprs: usize,
    lets: usize,
    seqs: usize,
    field_syntax_calls: usize,
    user_elab_calls: usize,
    enriched_nodes: usize,
    low_nodes: usize,
    max_expr_depth: usize,
    max_type_depth: usize,
}

impl BuildShapeStats {
    fn from_package<P: Phase>(package: &Package<P>) -> Self {
        let mut stats = Self::default();
        for (_, entry) in package.modules() {
            stats.visit_module(&entry.module);
        }
        // The package file carries only the `bridge` glob list — no
        // items to visit; host items are counted as ordinary module
        // items in `visit_module`.
        stats
    }

    fn visit_module<P: Phase>(&mut self, module: &Module<P>) {
        self.modules += 1;
        self.imports += module.imports.len();
        self.items += module.items.len();
        for item in &module.items {
            self.visit_item(item);
        }
    }

    fn visit_item<P: Phase>(&mut self, item: &Item<P>) {
        match item {
            Item::FnDef(def) => self.visit_fn_def(def),
            Item::RecGroup(group, _) => {
                self.rec_groups += 1;
                for member in &group.members {
                    self.visit_fn_def(member);
                }
            }
            Item::TypeRecGroup(group) => {
                self.type_rec_groups += 1;
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            self.aliases += 1;
                            self.visit_type(&alias.body, 1);
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            self.newtypes += 1;
                            self.visit_type(&newtype.payload, 1);
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            self.labels += 1;
                            for entry in &labels.entries {
                                self.visit_type(&entry.payload, 1);
                            }
                        }
                    }
                }
            }
            Item::TypeAlias(alias) => {
                self.aliases += 1;
                self.visit_type(&alias.body, 1);
            }
            Item::LiteralAlias(_, _) | Item::LabelForward(_, _) => {}
            Item::Newtype(newtype) => {
                self.newtypes += 1;
                self.visit_type(&newtype.payload, 1);
            }
            Item::Labels(labels, _) => {
                self.labels += 1;
                for entry in &labels.entries {
                    self.visit_type(&entry.payload, 1);
                }
            }
            Item::Equiv(equiv, _) => {
                self.equivs += 1;
                self.visit_signature(&equiv.sig);
                for term in &equiv.terms {
                    self.visit_expr(&term.body, 1);
                }
            }
            Item::Elaborator(def, _) => {
                self.elaborator_defs += 1;
                self.visit_type(&def.call_ty, 1);
            }
            Item::Op(op, _) => {
                self.ops += 1;
                self.visit_op_body(&op.body);
            }
            Item::VariadicOperator(_, _) => {
                self.ops += 1;
            }
            Item::HostType(_) => {}
            Item::HostFn(host_fn) => self.visit_host_fn(host_fn),
        }
    }

    fn visit_fn_def<P: Phase>(&mut self, def: &crate::ast::FnDef<P>) {
        self.functions += 1;
        self.visit_signature(&def.sig);
        self.visit_type(&def.ret, 1);
        self.visit_expr(&def.body, 1);
    }

    fn visit_signature<P: Phase>(&mut self, sig: &Signature<P>) {
        for param in &sig.params {
            if let SignatureParam::Value(value) = param
                && let Some(ty) = &value.ty
            {
                self.visit_type(ty, 1);
            }
        }
    }

    fn visit_op_body(&mut self, body: &crate::ast::OpBody) {
        match body {
            crate::ast::OpBody::Normal { .. } => {}
        }
    }

    fn visit_host_fn<P: Phase>(&mut self, host_fn: &crate::ast::HostFn<P>) {
        self.host_fns += 1;
        for param in &host_fn.params {
            if let HostFnParam::Value(value) = param {
                self.visit_type(&value.ty, 1);
            }
        }
        self.visit_type(&host_fn.ret, 1);
    }

    fn visit_call_args<P: Phase>(&mut self, args: &[CallArg<P>], expr_depth: usize) {
        for arg in args {
            match arg {
                CallArg::Type(ty) => self.visit_type(ty, 1),
                CallArg::Value(expr) => self.visit_expr(expr, expr_depth),
            }
        }
    }

    fn visit_types<P: Phase>(&mut self, types: &[Type<P>]) {
        for ty in types {
            self.visit_type(ty, 1);
        }
    }

    fn visit_types_at_depth<P: Phase>(&mut self, types: &[Type<P>], depth: usize) {
        for ty in types {
            self.visit_type(ty, depth);
        }
    }

    fn visit_exprs<P: Phase>(&mut self, exprs: &[Expr<P>], depth: usize) {
        for expr in exprs {
            self.visit_expr(expr, depth);
        }
    }

    fn visit_expr<P: Phase>(&mut self, expr: &Expr<P>, depth: usize) {
        self.expr_nodes += 1;
        self.max_expr_depth = self.max_expr_depth.max(depth);
        match expr {
            Expr::BlockCall { prefix, blocks, .. } => {
                self.visit_exprs(prefix, depth + 1);
                for block in blocks {
                    for item in &block.items {
                        self.visit_expr(item.value(), depth + 1);
                    }
                }
            }
            Expr::Path { .. } => {}
            Expr::Call { callee, args, .. } => {
                self.calls += 1;
                self.visit_expr(callee, depth + 1);
                self.visit_call_args(args, depth + 1);
            }
            Expr::RecCall { args, .. } => {
                self.rec_calls += 1;
                self.visit_call_args(args, depth + 1);
            }
            Expr::FnExpr {
                sig, ret_ty, body, ..
            } => {
                self.fn_exprs += 1;
                self.visit_signature(sig);
                if let Some(ty) = ret_ty {
                    self.visit_type(ty, 1);
                }
                self.visit_expr(body, depth + 1);
            }
            Expr::Let { value, body, .. } => {
                self.lets += 1;
                self.visit_expr(value, depth + 1);
                self.visit_expr(body, depth + 1);
            }
            Expr::RowLet { value, body, .. } => {
                self.lets += 1;
                self.visit_expr(value, depth + 1);
                self.visit_expr(body, depth + 1);
            }
            Expr::Seq { value, body, .. } => {
                self.seqs += 1;
                self.visit_expr(value, depth + 1);
                self.visit_expr(body, depth + 1);
            }
            Expr::Unit { .. } => {}
            Expr::StrLit { annotation, .. }
            | Expr::IntLit { annotation, .. }
            | Expr::FloatLit { annotation, .. }
            | Expr::BoolLit { annotation, .. } => {
                if let Some(ty) = annotation.as_type() {
                    self.visit_type(ty, 1);
                }
            }
            Expr::Tuple { items, .. } => self.visit_exprs(items, depth + 1),
            Expr::FnPlaceholder { body, .. } => self.visit_expr(body, depth + 1),
            Expr::LabelValue { labels, .. } => {
                for label in labels {
                    self.visit_expr(&label.value, depth + 1);
                }
            }
            Expr::Elaborator { call, .. } => {
                self.field_syntax_calls += 1;
                match call {
                    crate::ast::ElaboratorCall::FieldAccess { receiver, .. } => {
                        self.visit_expr(receiver, depth + 1);
                    }
                    crate::ast::ElaboratorCall::FieldUpdate { receiver, updates } => {
                        self.visit_expr(receiver, depth + 1);
                        for update in updates {
                            self.visit_expr(&update.value, depth + 1);
                        }
                    }
                }
            }
            Expr::RecQuote { plan, .. } => {
                for expression in plan.expressions() {
                    self.visit_expr(expression, depth + 1);
                }
            }
            Expr::RecOrder { plan, .. } => {
                self.visit_expr(&plan.value, depth + 1);
                self.visit_expr(&plan.body, depth + 1);
            }
            Expr::UserElaborator { args, .. } => {
                self.user_elab_calls += 1;
                self.visit_call_args(args, depth + 1);
            }
            Expr::Ufcs { receiver, args, .. } => {
                self.visit_expr(receiver, depth + 1);
                self.visit_call_args(args, depth + 1);
            }
            Expr::OpChain { kind, .. } => match kind {
                OpChainKind::Normal { slots, .. } => self.visit_exprs(slots, depth + 1),
                OpChainKind::Variadic { elements, .. } => self.visit_exprs(elements, depth + 1),
            },
            Expr::EnrichedTuple { items, .. } => {
                self.enriched_nodes += 1;
                self.visit_exprs(items, depth + 1);
            }
            Expr::EnrichedProject { target, .. } => {
                self.enriched_nodes += 1;
                self.visit_expr(target, depth + 1);
            }
            Expr::EnrichedInject { payload, .. } => {
                self.enriched_nodes += 1;
                self.visit_expr(payload, depth + 1);
            }
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => {
                self.enriched_nodes += 1;
                self.visit_expr(scrutinee, depth + 1);
                for arm in arms {
                    self.visit_expr(&arm.body, depth + 1);
                }
            }
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.enriched_nodes += 1;
                self.visit_expr(cond, depth + 1);
                self.visit_expr(then_branch, depth + 1);
                self.visit_expr(else_branch, depth + 1);
            }
            Expr::EnrichedRecord { fields, .. } => {
                self.enriched_nodes += 1;
                for field in fields {
                    self.visit_expr(&field.value, depth + 1);
                }
            }
            Expr::EnrichedFieldGet { target, .. } => {
                self.enriched_nodes += 1;
                self.visit_expr(target, depth + 1);
            }
            Expr::LowHostCall {
                type_args,
                args,
                sig,
                ret_ty,
                ..
            } => {
                self.low_nodes += 1;
                self.visit_types(type_args);
                self.visit_exprs(args, depth + 1);
                self.visit_signature(sig);
                self.visit_type(ret_ty, 1);
            }
            Expr::LowModuleCall {
                type_args,
                args,
                sig,
                ret_ty,
                ..
            }
            | Expr::LowQualifiedModuleCall {
                type_args,
                args,
                sig,
                ret_ty,
                ..
            } => {
                self.low_nodes += 1;
                self.visit_types(type_args);
                self.visit_exprs(args, depth + 1);
                self.visit_signature(sig);
                if let Some(ty) = ret_ty {
                    self.visit_type(ty, 1);
                }
            }
            Expr::LowQualifiedNewtypeMember {
                type_args, payload, ..
            }
            | Expr::LowNewtypeCtor {
                type_args, payload, ..
            } => {
                self.low_nodes += 1;
                self.visit_types(type_args);
                self.visit_expr(payload, depth + 1);
            }
            Expr::LowNewtypeProj {
                type_args, target, ..
            } => {
                self.low_nodes += 1;
                self.visit_types(type_args);
                self.visit_expr(target, depth + 1);
            }
            Expr::LowClosureCall {
                type_args, args, ..
            } => {
                self.low_nodes += 1;
                self.visit_types(type_args);
                self.visit_exprs(args, depth + 1);
            }
            Expr::LowIndirectCall {
                callee,
                type_args,
                args,
                ..
            } => {
                self.low_nodes += 1;
                self.visit_expr(callee, depth + 1);
                self.visit_types(type_args);
                self.visit_exprs(args, depth + 1);
            }
            Expr::LowTypeApplication {
                callee, type_arg, ..
            } => {
                self.low_nodes += 1;
                self.visit_expr(callee, depth + 1);
                self.visit_type(type_arg, 1);
            }
            Expr::LowAbsurdCall {
                type_arg,
                value_arg,
                ..
            } => {
                self.low_nodes += 1;
                self.visit_type(type_arg, 1);
                self.visit_expr(value_arg, depth + 1);
            }
            Expr::LowCpsProjectorApply {
                type_args,
                receiver,
                continuation,
                ..
            } => {
                self.low_nodes += 1;
                self.visit_types(type_args);
                self.visit_expr(receiver, depth + 1);
                self.visit_expr(continuation, depth + 1);
            }
            Expr::LowBoundRef { .. } => {
                self.low_nodes += 1;
            }
            Expr::LowHostFnValueRef { sig, ret_ty, .. } => {
                self.low_nodes += 1;
                self.visit_signature(sig);
                self.visit_type(ret_ty, 1);
            }
            Expr::LowModuleFnValueRef { sig, .. } => {
                self.low_nodes += 1;
                self.visit_signature(sig);
            }
        }
    }

    fn visit_type<P: Phase>(&mut self, ty: &Type<P>, depth: usize) {
        self.type_nodes += 1;
        self.max_type_depth = self.max_type_depth.max(depth);
        match ty {
            Type::Path { args, .. } | Type::Goal { args, .. } => {
                self.visit_types_at_depth(args, depth + 1)
            }
            Type::Unit { .. } | Type::Bottom { .. } => {}
            Type::Function { param, ret, .. }
            | Type::Product {
                left: param,
                right: ret,
                ..
            }
            | Type::Sum {
                left: param,
                right: ret,
                ..
            } => {
                self.visit_type(param, depth + 1);
                self.visit_type(ret, depth + 1);
            }
            Type::LabelSugar { labels, .. } => {
                for label in labels {
                    if let Some(payload) = &label.payload {
                        self.visit_type(payload, depth + 1);
                    }
                }
            }
            Type::Infer { .. } => {}
            Type::Forall { body, .. } => self.visit_type(body, depth + 1),
        }
    }
}
