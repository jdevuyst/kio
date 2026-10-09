//! Static Go indices over resolved facade uses. Conversion and type-stage
//! execution remain in the existing live facade; these markers carry no values.

use super::*;
use crate::ast::Kind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GoIndexType {
    pub(super) ty: GoType,
    pub(super) kind: Kind,
}

impl Ord for GoIndexType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.ty, kind_key(&self.kind)).cmp(&(&other.ty, kind_key(&other.kind)))
    }
}

impl PartialOrd for GoIndexType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct GoIndexDeclarations {
    declarations: BTreeMap<GoIdentifier, String>,
    nominal_provenance: BTreeMap<GoIdentifier, GoDeclarationProvenance>,
    pub(super) parameters: BTreeMap<GoType, GoIndexType>,
}

impl GoIndexDeclarations {
    fn insert(&mut self, name: GoIdentifier, source: String) -> Result<(), GoFacadeError> {
        if let Some(previous) = self.declarations.insert(name, source.clone())
            && previous != source
        {
            return Err(GoFacadeError::new(
                "one static index identity has two declarations",
            ));
        }
        Ok(())
    }

    pub(super) fn merge(&mut self, other: Self) -> Result<(), GoFacadeError> {
        for (name, provenance) in other.nominal_provenance {
            record_declaration_provenance(&mut self.nominal_provenance, &name, provenance);
        }
        for (name, source) in other.declarations {
            self.insert(name, source)?;
        }
        Ok(())
    }

    pub(super) fn claims(&self) -> impl Iterator<Item = (&GoIdentifier, &str)> {
        self.declarations.keys().map(|name| (name, name.as_str()))
    }

    pub(super) fn render(&self, out: &mut String) {
        for (name, source) in &self.declarations {
            if let Some(provenance) = self.nominal_provenance.get(name) {
                render_deprecation_comment(out, provenance.removed_at_version());
            }
            out.push_str(source);
        }
    }

    pub(super) fn nominal(
        &mut self,
        name: &QualifiedTypeName,
        kind: Kind,
        native: Option<&GoType>,
        provenance: GoDeclarationProvenance,
    ) -> Result<GoIndexType, GoFacadeError> {
        let marker = self.marker(
            format!("KioIndexNominal_{}", encode_qualified_type_frame(name)),
            &native.map(|_| vec![None]).unwrap_or_default(),
            &kind,
        )?;
        record_declaration_provenance(&mut self.nominal_provenance, &marker, provenance);
        let readable = crate::backends::public_names::readable_public_identity(
            name.module_segments()
                .iter()
                .map(String::as_str)
                .chain([name.name()]),
        );
        let alias = go_identifier(format!("Index_{readable}"))?;
        let (parameters, arguments) = if native.is_some() {
            ("[Native any]", "[Native]")
        } else {
            ("", "")
        };
        self.insert(
            alias.clone(),
            format!("type {alias}{parameters} = {marker}{arguments}\n\n"),
        )?;
        record_declaration_provenance(&mut self.nominal_provenance, &alias, provenance);
        Ok(GoIndexType {
            ty: GoType::new(match native {
                Some(native) => format!("{marker}[{native}]"),
                None => marker.to_string(),
            }),
            kind,
        })
    }

    pub(super) fn kind(&mut self, kind: &Kind) -> Result<String, GoFacadeError> {
        let key = kind_key(kind);
        let name = go_identifier(format!("KioIndexKind_{key}"))?;
        self.insert(
            name.clone(),
            format!("type {name} interface {{ kioIndexKind_{key}() }}\n\n"),
        )?;
        Ok(name.to_string())
    }

    pub(super) fn erased(&mut self, kind: &Kind) -> Result<GoType, GoFacadeError> {
        let marker = self.marker(format!("KioErasedIndex_{}", kind_key(kind)), &[], kind)?;
        Ok(GoType::new(marker.to_string()))
    }

    pub(super) fn product(
        &mut self,
        shell: &GoFacadeShell,
        indices: &[GoType],
        provenance: GoDeclarationProvenance,
    ) -> Result<GoType, GoFacadeError> {
        let marker = self.marker(
            format!("KioIndex_{}", shell.name),
            &vec![Some(Kind::Star); indices.len()],
            &Kind::Star,
        )?;
        record_declaration_provenance(&mut self.nominal_provenance, &marker, provenance);
        Ok(GoType::new(format!(
            "{marker}{}",
            bracketed(&indices.iter().map(ToString::to_string).collect::<Vec<_>>())
        )))
    }

    fn marker(
        &mut self,
        name: String,
        parameters: &[Option<Kind>],
        result: &Kind,
    ) -> Result<GoIdentifier, GoFacadeError> {
        let name = go_identifier(name)?;
        let mut declaration = Vec::new();
        let mut arguments = Vec::new();
        for (index, kind) in parameters.iter().enumerate() {
            let constraint = match kind {
                Some(kind) => self.kind(kind)?,
                None => "any".to_owned(),
            };
            declaration.push(format!("I{index} {constraint}"));
            arguments.push(format!("I{index}"));
        }
        self.kind(result)?;
        let declaration = bracketed(&declaration);
        let arguments = bracketed(&arguments);
        let mut source = format!("type {name}{declaration} struct {{\n");
        if parameters.is_empty() {
            writeln!(source, "\tkioIndexIdentity_{name} [0]struct{{}}")
                .expect("writing to String cannot fail");
        } else {
            for index in 0..parameters.len() {
                writeln!(source, "\tkioIndexIdentity_{name}_{index} [0]*I{index}")
                    .expect("writing to String cannot fail");
            }
        }
        writeln!(
            source,
            "}}\nfunc ({name}{arguments}) kioIndexKind_{}() {{}}\n",
            kind_key(result)
        )
        .expect("writing to String cannot fail");
        self.insert(name.clone(), source)?;
        Ok(name)
    }

    pub(super) fn apply(
        &mut self,
        constructor: GoIndexType,
        argument: GoIndexType,
    ) -> Result<GoIndexType, GoFacadeError> {
        let Kind::Arrow(domain, result) = &constructor.kind else {
            return Err(GoFacadeError::new(
                "a prepared index application has a saturated head",
            ));
        };
        if domain.as_ref() != &argument.kind {
            return Err(GoFacadeError::new(
                "a prepared index application has the wrong argument kind",
            ));
        }
        let name = self.marker(
            format!("KioIndexApply_{}", kind_key(&constructor.kind)),
            &[Some(constructor.kind.clone()), Some(argument.kind)],
            result,
        )?;
        Ok(GoIndexType {
            ty: GoType::new(format!("{name}[{}, {}]", constructor.ty, argument.ty)),
            kind: result.as_ref().clone(),
        })
    }
}

pub(super) fn bracketed(values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!("[{}]", values.join(", "))
    }
}

pub(super) fn kind_key(kind: &Kind) -> String {
    match kind {
        Kind::Star => "S".to_owned(),
        Kind::Arrow(domain, result) => format!("A{}{}", kind_key(domain), kind_key(result)),
    }
}

impl GoFacadeSite<'_> {
    pub(super) fn index_type(
        &self,
        traversal: &GoFacadeTraversal,
        declarations: &mut GoIndexDeclarations,
    ) -> Result<Option<GoIndexType>, GoFacadeError> {
        self.index_context(traversal, declarations, &BTreeMap::new())
    }

    fn index_context(
        &self,
        traversal: &GoFacadeTraversal,
        declarations: &mut GoIndexDeclarations,
        binders: &BTreeMap<(GoFacadeArenaId, FacadeBinderId), GoIndexType>,
    ) -> Result<Option<GoIndexType>, GoFacadeError> {
        let source = match traversal.context.term.as_ref() {
            GoFacadeTerm::Use { source, .. } => source,
            GoFacadeTerm::Index(index) => {
                declarations.kind(&index.kind)?;
                declarations
                    .parameters
                    .insert(index.ty.clone(), index.clone());
                return Ok(Some(index.clone()));
            }
            GoFacadeTerm::Erased(_) => return Ok(None),
        };
        let node = self.use_at(&traversal.context)?;
        let (name, arguments) = match node {
            FacadeUse::Bound { binder, .. } => {
                if let Some(index) = binders.get(&(source.arena.clone(), *binder)) {
                    return Ok(Some(index.clone()));
                }
                let resolved = self.resolve_bound_traversal(traversal, *binder)?;
                return self.index_context(&resolved, declarations, binders);
            }
            FacadeUse::Nominal { name, .. } => {
                let declaration = self.prepared.nominals().declaration(name).ok_or_else(|| {
                    GoFacadeError::new("index nominal is absent from its exact snapshot")
                })?;
                let kind = declaration.kind();
                let native = match self.nominals.get(name) {
                    Some(GoNominalPlan::Host {
                        exact_boundary_type: Some(native),
                        ..
                    }) => Some(native),
                    _ => None,
                };
                return Ok(Some(declarations.nominal(
                    name,
                    kind,
                    native,
                    self.provenance(),
                )?));
            }
            FacadeUse::Apply {
                constructor, args, ..
            } => {
                let child = self.child_traversal(traversal, *constructor)?;
                let mut constructor = self.index_context(&child, declarations, binders)?;
                for argument in args {
                    let child = self.child_traversal(traversal, *argument)?;
                    let argument = self.index_context(&child, declarations, binders)?;
                    constructor = match (constructor, argument) {
                        (Some(constructor), Some(argument)) => {
                            Some(declarations.apply(constructor, argument)?)
                        }
                        _ => None,
                    };
                }
                return Ok(constructor);
            }
            FacadeUse::Unit { .. } => ("KioIndexUnit".to_owned(), Vec::new()),
            FacadeUse::Bottom { .. } => ("KioIndexBottom".to_owned(), Vec::new()),
            FacadeUse::Product { shell, args, .. } | FacadeUse::Sum { shell, args, .. } => (
                format!("KioIndex_{}", GoFacadeShell::new(shell)?.name),
                args.clone(),
            ),
            FacadeUse::Function { slots, result, .. } => {
                let mut arguments = slots.clone();
                arguments.push(*result);
                (format!("KioIndexFunction_{}", slots.len()), arguments)
            }
            FacadeUse::Forall { binder, result, .. } => {
                let kind = self.arena_plan(&source.arena)?.binder(*binder).kind.clone();
                let bound = declarations.marker(
                    format!("KioIndexBound_{}_{}", binders.len(), kind_key(&kind)),
                    &[],
                    &kind,
                )?;
                let mut nested = binders.clone();
                nested.insert(
                    (source.arena.clone(), *binder),
                    GoIndexType {
                        ty: GoType::new(bound.to_string()),
                        kind: kind.clone(),
                    },
                );
                let child = self.child_traversal(traversal, *result)?;
                let Some(body) = self.index_context(&child, declarations, &nested)? else {
                    return Ok(None);
                };
                let marker = declarations.marker(
                    format!("KioIndexForall_{}", kind_key(&kind)),
                    &[Some(Kind::Star)],
                    &Kind::Star,
                )?;
                return Ok(Some(GoIndexType {
                    ty: GoType::new(format!("{marker}[{}]", body.ty)),
                    kind: Kind::Star,
                }));
            }
        };
        let mut indices = Vec::with_capacity(arguments.len());
        let mut complete = true;
        for argument in arguments {
            let child = self.child_traversal(traversal, argument)?;
            match self.index_context(&child, declarations, binders)? {
                Some(index) => indices.push(index),
                None => complete = false,
            }
        }
        if !complete {
            return Ok(None);
        }
        let marker =
            declarations.marker(name, &vec![Some(Kind::Star); indices.len()], &Kind::Star)?;
        if matches!(node, FacadeUse::Product { .. } | FacadeUse::Sum { .. }) {
            record_declaration_provenance(
                &mut declarations.nominal_provenance,
                &marker,
                self.provenance(),
            );
        }
        Ok(Some(GoIndexType {
            ty: GoType::new(format!(
                "{marker}{}",
                bracketed(
                    &indices
                        .iter()
                        .map(|index| index.ty.to_string())
                        .collect::<Vec<_>>()
                )
            )),
            kind: Kind::Star,
        }))
    }
}

impl GoProductUse<'_, '_> {
    pub(crate) fn index_type(&self) -> Result<Option<GoType>, GoFacadeError> {
        self.index_type_with_declarations(&mut GoIndexDeclarations::default())
    }

    pub(super) fn index_type_with_declarations(
        &self,
        declarations: &mut GoIndexDeclarations,
    ) -> Result<Option<GoType>, GoFacadeError> {
        let mut indices = Vec::new();
        for field in &self.fields {
            let cursor = &field.payload;
            let Some(index) = cursor.site.index_type(&cursor.traversal, declarations)? else {
                return Ok(None);
            };
            indices.push(index.ty);
        }
        let provenance = self
            .fields
            .first()
            .expect("a grouped product has fields")
            .payload
            .site
            .provenance();
        Ok(Some(declarations.product(
            &self.shell,
            &indices,
            provenance,
        )?))
    }
}
