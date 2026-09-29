//! Descriptor-selected Surface projection, before pattern and recursive lowering.

#[cfg(test)]
mod tests;

use crate::ast::*;
use crate::error::Error;
use crate::pass::surface_registry::{BlockDeclaration, PackageBlockScope};
use crate::span::Span;

pub(crate) fn project_module(
    module: &mut Module<Surface>,
    scope: &PackageBlockScope,
) -> Result<(), Error> {
    let module_path = module.path.segments.join("/");
    let mut projection = Projection {
        scope,
        module: &module_path,
    };
    for item in &mut module.items {
        match item {
            Item::FnDef(def) => projection.expr(&mut def.body)?,
            Item::RecGroup(group, _) => {
                for def in &mut group.members {
                    projection.expr(&mut def.body)?;
                }
            }
            Item::Equiv(equiv, _) => {
                for term in &mut equiv.terms {
                    projection.expr(&mut term.body)?;
                }
            }
            Item::TypeAlias(_)
            | Item::LiteralAlias(_, _)
            | Item::Newtype(_)
            | Item::Labels(_, _)
            | Item::LabelForward(_, _)
            | Item::Elaborator(_, _)
            | Item::TypeRecGroup(_)
            | Item::HostType(_)
            | Item::HostFn(_)
            | Item::Op(_, _)
            | Item::VariadicOperator(_, _) => {}
        }
    }
    Ok(())
}

struct Projection<'a> {
    scope: &'a PackageBlockScope,
    module: &'a str,
}

impl Projection<'_> {
    fn expr(&mut self, expr: &mut Expr<Surface>) -> Result<(), Error> {
        if let Expr::BlockCall { .. } = expr {
            let span = expr.span();
            let Expr::BlockCall {
                id,
                head,
                prefix,
                blocks,
                meta,
                ..
            } = std::mem::replace(expr, unit(span))
            else {
                unreachable!()
            };
            let declaration = self
                .scope
                .declaration(self.module, head.as_str())
                .ok_or_else(|| {
                    Error::name_res(
                        head.span,
                        format!("elaborator `{}` is not in scope", head.as_str()),
                    )
                    .with_help(format!(
                        "import it with `import <module>({});`",
                        head.as_str()
                    ))
                })?;
            if declaration.module == self.module && declaration.span.start > head.span.start {
                return Err(Error::name_res(
                    head.span,
                    format!("elaborator `{}` is not yet in scope", head.as_str()),
                ));
            }
            if declaration.blocks.is_empty() {
                return Err(block_error(
                    declaration,
                    head.span,
                    "this elaborator declares no trailing blocks",
                ));
            }
            if blocks.len() != declaration.blocks.len() {
                let mut error = block_error(
                    declaration,
                    span,
                    format!(
                        "expected {} trailing block(s), found {}",
                        declaration.blocks.len(),
                        blocks.len()
                    ),
                );
                if blocks.len() < declaration.blocks.len()
                    && blocks
                        .iter()
                        .zip(&declaration.blocks)
                        .all(|(block, descriptor)| {
                            block.label.as_ref().map(PathSegment::as_str)
                                == descriptor.label.as_ref().map(PathSegment::as_str)
                        })
                {
                    let mut text = String::new();
                    for descriptor in &declaration.blocks[blocks.len()..] {
                        if let Some(label) = &descriptor.label {
                            text.push(' ');
                            text.push_str(label.as_str());
                        }
                        text.push_str(" { }");
                    }
                    error = error.with_fix(crate::error::Fix::scaffold(
                        "Add missing trailing block scaffolds",
                        vec![crate::error::FixEdit::new(
                            Span::new(span.end, span.end),
                            text,
                        )],
                    ));
                }
                return Err(error);
            }
            let mut args: Vec<_> = prefix.into_iter().map(CallArg::Value).collect();
            for (index, (block, descriptor)) in
                blocks.into_iter().zip(&declaration.blocks).enumerate()
            {
                if block.label.as_ref().map(PathSegment::as_str)
                    != descriptor.label.as_ref().map(PathSegment::as_str)
                {
                    let mut error = block_error(
                        declaration,
                        block.label.as_ref().map_or(block.open, |label| label.span),
                        match &descriptor.label {
                            Some(label) => {
                                format!("expected trailing block label `{}`", label.as_str())
                            }
                            None => "the first trailing block is unlabeled".to_owned(),
                        },
                    );
                    if let (Some(written), Some(expected)) = (&block.label, &descriptor.label) {
                        error = error.with_fix(crate::error::Fix::machine_applicable(
                            format!("Use trailing block label `{}`", expected.as_str()),
                            vec![crate::error::FixEdit::new(written.span, expected.as_str())],
                        ));
                    }
                    return Err(error);
                }
                args.push(CallArg::Value(project_block(
                    block,
                    descriptor.exposure,
                    declaration,
                    id,
                    index,
                )?));
            }
            *expr = Expr::UserElaborator {
                occurrence: Default::default(),
                name: head.name,
                args,
                form: UserElaboratorCallForm::TrailingBlocks,
                meta,
                ext: id,
            };
        }
        match expr {
            Expr::BlockCall { .. } => unreachable!("block projection replaced the current node"),
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::StrLit { .. }
            | Expr::BoolLit { .. } => {}
            Expr::Call { callee, args, .. } => {
                self.expr(callee)?;
                self.args(args)?;
            }
            Expr::RecCall { args, .. } | Expr::UserElaborator { args, .. } => self.args(args)?,
            Expr::Ufcs { receiver, args, .. } => {
                self.expr(receiver)?;
                self.args(args)?;
            }
            Expr::FnExpr { body, .. } | Expr::FnPlaceholder { body, .. } => self.expr(body)?,
            Expr::Let { value, body, .. }
            | Expr::RowLet { value, body, .. }
            | Expr::Seq { value, body, .. } => {
                self.expr(value)?;
                self.expr(body)?;
            }
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item)?;
                }
            }
            Expr::LabelValue { labels, .. } => {
                for label in labels {
                    self.expr(&mut label.value)?;
                }
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => self.expr(receiver)?,
                ElaboratorCall::FieldUpdate { receiver, updates } => {
                    self.expr(receiver)?;
                    for update in updates {
                        self.expr(&mut update.value)?;
                    }
                }
            },
            Expr::OpChain { .. } => unreachable!("operator folding precedes block projection"),
            Expr::RecOrder { ext, .. }
            | Expr::RecQuote { ext, .. }
            | Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedRecord { ext, .. }
            | Expr::EnrichedFieldGet { ext, .. }
            | Expr::LowHostCall { ext, .. }
            | Expr::LowModuleCall { ext, .. }
            | Expr::LowQualifiedModuleCall { ext, .. }
            | Expr::LowQualifiedNewtypeMember { ext, .. }
            | Expr::LowNewtypeCtor { ext, .. }
            | Expr::LowNewtypeProj { ext, .. }
            | Expr::LowClosureCall { ext, .. }
            | Expr::LowIndirectCall { ext, .. }
            | Expr::LowTypeApplication { ext, .. }
            | Expr::LowAbsurdCall { ext, .. }
            | Expr::LowCpsProjectorApply { ext, .. }
            | Expr::LowBoundRef { ext, .. }
            | Expr::LowHostFnValueRef { ext, .. }
            | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
        }
        Ok(())
    }

    fn args(&mut self, args: &mut [CallArg<Surface>]) -> Result<(), Error> {
        for arg in args {
            if let CallArg::Value(value) = arg {
                self.expr(value)?;
            }
        }
        Ok(())
    }
}

fn block_error(declaration: &BlockDeclaration, span: Span, message: impl Into<String>) -> Error {
    Error::type_(span, message).with_secondary_in_file(
        &declaration.file,
        declaration.span,
        "trailing blocks selected from this elaborator declaration",
    )
}

fn unit(span: Span) -> Expr<Surface> {
    Expr::Unit {
        occurrence: Default::default(),
        meta: Meta::new(span),
    }
}

fn path(name: &str, span: Span) -> Expr<Surface> {
    Expr::Path {
        occurrence: Default::default(),
        segments: vec![PathSegment {
            name: name.to_owned(),
            span,
        }],
        meta: Meta::new(span),
        ext: (),
    }
}

fn lambda(params: Vec<SignatureParam<Surface>>, body: Expr<Surface>, span: Span) -> Expr<Surface> {
    let sig = if params.is_empty() {
        Signature::from_groups(vec![SignatureGroup::Value(Vec::new())])
    } else {
        Signature::new(params)
    };
    Expr::FnExpr {
        occurrence: Default::default(),
        sig,
        ret_ty: None,
        body: Box::new(body),
        meta: Meta::new(span),
        caps: (),
    }
}

fn call(callee: Expr<Surface>, args: Vec<CallArg<Surface>>, span: Span) -> Expr<Surface> {
    Expr::Call {
        occurrence: Default::default(),
        callee: Box::new(callee),
        args,
        meta: Meta::new(span),
        ext: (),
    }
}

fn project_block(
    block: NeutralBlock<Surface>,
    exposure: BlockExposure,
    declaration: &BlockDeclaration,
    id: NodeId,
    index: usize,
) -> Result<Expr<Surface>, Error> {
    let span = Span::new(block.open.start, block.close.end);
    let step_span = block.open;
    let mut items = block.items;
    if exposure == BlockExposure::Product {
        let values = items
            .into_iter()
            .map(|item| match item {
                NeutralItem::Expression { value, .. } => Ok(value),
                NeutralItem::Binding { meta, .. }
                | NeutralItem::RowBinding { meta, .. }
                | NeutralItem::ExistentialBinding { meta, .. } => Err(block_error(
                    declaration,
                    meta.span,
                    "a product block contains independent expressions, not bindings",
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(match values.len() {
            0 => unit(span),
            1 => values.into_iter().next().unwrap(),
            _ => Expr::Tuple {
                occurrence: Default::default(),
                items: values,
                meta: Meta::new(span),
                ext: (),
            },
        });
    }
    let sequence = exposure == BlockExposure::Sequence;
    let step = format!("__block_step_n{}_n{index}__", id.0);
    let mut body = match items.pop() {
        Some(NeutralItem::Expression { value, .. }) => value,
        None if !sequence => unit(span),
        _ => {
            return Err(block_error(
                declaration,
                block.close,
                if sequence {
                    "a sequence block requires a final expression"
                } else {
                    "a nonempty thunk block requires a final expression"
                },
            ));
        }
    };
    for (position, item) in items.into_iter().enumerate().rev() {
        body = match item {
            NeutralItem::Expression { value, .. } if sequence => call(
                path(&step, step_span),
                vec![
                    CallArg::Type(Type::Unit {
                        meta: Meta::new(value.span()),
                    }),
                    CallArg::Value(value),
                    CallArg::Value(lambda(Vec::new(), body, span)),
                ],
                span,
            ),
            NeutralItem::Expression { value, .. } => Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(value),
                body: Box::new(body),
                meta: Meta::new(span),
            },
            NeutralItem::Binding {
                name,
                name_span,
                ty,
                pattern,
                value,
                bind: false,
                meta,
            } => Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty,
                pattern,
                value: Box::new(value),
                body: Box::new(body),
                meta,
            },
            NeutralItem::Binding {
                name,
                name_span,
                ty,
                pattern,
                value,
                bind: true,
                meta,
            } => {
                if !sequence {
                    return Err(block_error(
                        declaration,
                        meta.span,
                        "`<-` is admitted only in a sequence block",
                    ));
                }
                let payload = ty
                    .clone()
                    .or_else(|| pattern.as_ref().map(ParamPattern::outer_type));
                let complete =
                    payload.filter(|ty| !crate::pass::typecheck_core::type_contains_infer(ty));
                let continuation = lambda(
                    vec![SignatureParam::Value(Param {
                        name,
                        ty,
                        pattern,
                        meta: Meta::new(name_span),
                    })],
                    body,
                    meta.span,
                );
                if let Some(ty) = complete {
                    call(
                        path(&step, step_span),
                        vec![
                            CallArg::Type(ty),
                            CallArg::Value(value),
                            CallArg::Value(continuation),
                        ],
                        meta.span,
                    )
                } else {
                    let source = format!("__block_source_n{}_n{index}_n{position}__", id.0);
                    let application = call(
                        path(&step, step_span),
                        vec![
                            CallArg::Value(path(&source, meta.span)),
                            CallArg::Value(continuation),
                        ],
                        meta.span,
                    );
                    Expr::Let {
                        occurrence: Default::default(),
                        name: source,
                        name_span: meta.span,
                        ty: None,
                        pattern: None,
                        value: Box::new(value),
                        body: Box::new(application),
                        meta,
                    }
                }
            }
            NeutralItem::RowBinding {
                entries,
                value,
                meta,
            } => Expr::RowLet {
                occurrence: Default::default(),
                entries,
                value: Box::new(value),
                body: Box::new(body),
                temp_name: format!("__block_row_n{}_n{index}_n{position}__", id.0),
                meta,
                ext: (),
            },
            NeutralItem::ExistentialBinding {
                type_params,
                name,
                name_span,
                pattern,
                value,
                meta,
            } => {
                let mut params: Vec<_> =
                    type_params.into_iter().map(SignatureParam::Type).collect();
                params.push(SignatureParam::Value(Param {
                    name,
                    ty: None,
                    pattern,
                    meta: Meta::new(name_span),
                }));
                call(
                    value,
                    vec![
                        CallArg::Type(Type::Infer {
                            meta: Meta::new(meta.span),
                            ext: (),
                        }),
                        CallArg::Value(lambda(params, body, meta.span)),
                    ],
                    meta.span,
                )
            }
        };
    }
    let params = if sequence {
        vec![SignatureParam::Value(Param {
            name: step,
            ty: None,
            pattern: None,
            meta: Meta::new(step_span),
        })]
    } else {
        Vec::new()
    };
    Ok(lambda(params, body, span))
}
