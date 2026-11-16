//! Lexical references retained by immutable reflected type fragments.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeChild {
    Left,
    Right,
    ForallBody,
    Argument(usize),
}

#[derive(Debug)]
pub(super) struct TypeCursor {
    pub(super) parent: Option<EvalRef<TypeCursor>>,
    pub(super) child: TypeChild,
}

impl TypeChild {
    pub(super) fn select(self, ty: &EvalAstType) -> &EvalAstType {
        match (self, ty) {
            (Self::Left, Type::Function { param, .. }) => param,
            (Self::Right, Type::Function { ret, .. }) => ret,
            (Self::Left, Type::Product { left, .. } | Type::Sum { left, .. }) => left,
            (Self::Right, Type::Product { right, .. } | Type::Sum { right, .. }) => right,
            (Self::ForallBody, Type::Forall { body, .. }) => body,
            (Self::Argument(index), Type::Path { args, .. }) => &args[index],
            _ => unreachable!("reflected child cursor follows its immutable type root"),
        }
    }
}

#[derive(Debug)]
pub(super) struct TypeEnvironment {
    pub(super) parent: Option<EvalRef<TypeEnvironment>>,
    pub(super) name: String,
    pub(super) variable: ReflectedVariable,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ReflectionEnvironment(pub(super) Option<EvalRef<TypeEnvironment>>);

impl ReflectionEnvironment {
    pub(crate) fn with(&self, name: String, variable: ReflectedVariable) -> Self {
        Self(Some(EvalRef::new(TypeEnvironment {
            parent: self.0.clone(),
            name,
            variable,
        })))
    }

    pub(crate) fn get(&self, name: &str) -> Option<&ReflectedVariable> {
        let mut next = self.0.as_deref();
        while let Some(binding) = next {
            if binding.name == name {
                return Some(&binding.variable);
            }
            next = binding.parent.as_deref();
        }
        None
    }
}

/// Cold immutable lexical evidence shared by projected structural views.
#[cfg(feature = "surface")]
#[derive(Clone, Debug, Default)]
pub(crate) struct ProjectedReflection(Option<Arc<ProjectedReflectionData>>);

#[cfg(feature = "surface")]
#[derive(Clone, Debug)]
struct ProjectedReflectionData {
    value: Option<ReflectedType>,
    environment: ReflectionEnvironment,
    original: ReflectionEnvironment,
}

#[cfg(feature = "surface")]
impl ProjectedReflection {
    pub(crate) fn closed(value: Option<ReflectedType>) -> Self {
        let environment = value
            .as_ref()
            .map(|ty| ty.environment.clone())
            .unwrap_or_default();
        Self::with_environment(value, environment)
    }

    pub(crate) fn with_environment(
        value: Option<ReflectedType>,
        environment: ReflectionEnvironment,
    ) -> Self {
        if value.is_none() && environment.0.is_none() {
            return Self::default();
        }
        let value = value.map(|mut ty| {
            ty.environment = environment.clone();
            ty
        });
        Self(Some(Arc::new(ProjectedReflectionData {
            value,
            environment,
            original: ReflectionEnvironment::default(),
        })))
    }

    pub(crate) fn as_ref(&self) -> Option<&ReflectedType> {
        self.0.as_ref().and_then(|data| data.value.as_ref())
    }
    pub(crate) fn as_deref(&self) -> Option<&ReflectedType> {
        self.as_ref()
    }
    pub(crate) fn is_some(&self) -> bool {
        self.as_ref().is_some()
    }
    pub(crate) fn environment(&self) -> ReflectionEnvironment {
        self.0
            .as_ref()
            .map(|data| data.environment.clone())
            .unwrap_or_default()
    }
    fn original_environment(&self) -> ReflectionEnvironment {
        self.0
            .as_ref()
            .map(|data| data.original.clone())
            .unwrap_or_default()
    }
    fn bindings(environment: &ReflectionEnvironment) -> Vec<(String, ReflectedVariable)> {
        let mut next = environment.0.as_deref();
        let mut seen = HashSet::new();
        let mut bindings = Vec::new();
        while let Some(binding) = next {
            if seen.insert(binding.name.clone()) {
                bindings.push((binding.name.clone(), binding.variable.clone()));
            }
            next = binding.parent.as_deref();
        }
        bindings
    }
    pub(crate) fn lexical_bindings(&self) -> Vec<(String, ReflectedVariable)> {
        Self::bindings(&self.environment())
    }
    pub(crate) fn original_bindings(&self) -> Vec<(String, ReflectedVariable)> {
        Self::bindings(&self.original_environment())
    }
    pub(crate) fn variable(&self, name: &str) -> Option<ReflectedVariable> {
        self.environment().get(name).cloned()
    }
    pub(crate) fn with_original_from(mut self, source: &Self) -> Self {
        let original = source.original_environment();
        if let Some(data) = &mut self.0 {
            Arc::make_mut(data).original = original;
        } else if original.0.is_some() {
            self.0 = Some(Arc::new(ProjectedReflectionData {
                value: None,
                environment: ReflectionEnvironment::default(),
                original,
            }));
        }
        self
    }
    pub(crate) fn freeze_entry(mut self) -> Self {
        let environment = self.environment();
        for (_, variable) in Self::bindings(&environment) {
            assert!(
                matches!(
                    variable.meaning,
                    VariableMeaning::Caller(_) | VariableMeaning::Live(_)
                ),
                "entry reflection retains only caller and frozen residual identities"
            );
        }
        if let Some(data) = &mut self.0 {
            Arc::make_mut(data).original = environment;
        }
        self
    }
    pub(crate) fn with_lexical_bindings(
        self,
        bindings: impl IntoIterator<Item = (String, ReflectedVariable)>,
    ) -> Self {
        let mut environment = self.environment();
        let mut changed = false;
        for (name, variable) in bindings {
            if !environment
                .get(&name)
                .is_some_and(|old| old.same_reference(&variable))
            {
                environment = environment.with(name, variable);
                changed = true;
            }
        }
        if !changed {
            return self;
        }
        Self::with_environment(self.as_ref().cloned(), environment).with_original_from(&self)
    }
    pub(crate) fn with_scope(
        self,
        bindings: impl IntoIterator<
            Item = (
                String,
                crate::ast::Kind,
                Option<crate::pass::typecheck_core::TypeBinderId>,
            ),
        >,
    ) -> Self {
        let mut environment = self.environment();
        let mut changed = false;
        for (name, kind, identity) in bindings {
            if let Some(identity) = identity
                && environment.get(&name).is_none()
            {
                environment = environment.with(
                    name.clone(),
                    ReflectedVariable::caller(name, kind.arity(), identity),
                );
                changed = true;
            }
        }
        if !changed {
            return self;
        }
        Self::with_environment(self.as_ref().cloned(), environment).with_original_from(&self)
    }
    pub(crate) fn retaining_bindings(&self, used: &HashSet<String>) -> Self {
        let bindings = self.lexical_bindings();
        if bindings.iter().all(|(name, _)| used.contains(name)) {
            return self.clone();
        }
        let mut environment = ReflectionEnvironment::default();
        for (name, variable) in bindings.into_iter().rev() {
            if used.contains(&name) {
                environment = environment.with(name, variable);
            }
        }
        Self::with_environment(self.as_ref().cloned(), environment).with_original_from(self)
    }
    pub(crate) fn renamed(&self, names: &HashMap<String, String>) -> Self {
        if names.iter().all(|(before, after)| before == after) {
            return self.clone();
        }
        let mut environment = ReflectionEnvironment::default();
        for (name, variable) in self.lexical_bindings().into_iter().rev() {
            environment = environment.with(names.get(&name).cloned().unwrap_or(name), variable);
        }
        let value = self.as_ref().map(|ty| {
            let replacements = names
                .iter()
                .map(|(old, new)| (old.as_str(), ReflectedType::new_trusted(type_path(new))))
                .collect();
            ReflectedType::from_parts(
                EvalRef::new(type_arguments::substitute_template(
                    ty.as_type(),
                    &replacements,
                )),
                None,
                environment.clone(),
            )
        });
        Self::with_environment(value, environment).with_original_from(self)
    }
    pub(crate) fn merge_environment(
        &self,
        other: &Self,
        value: Option<ReflectedType>,
    ) -> Option<Self> {
        let merge = |mut environment: ReflectionEnvironment,
                     incoming: Vec<(String, ReflectedVariable)>| {
            for (name, variable) in incoming.into_iter().rev() {
                if let Some(existing) = environment.get(&name) {
                    if !existing.same_reference(&variable) {
                        return None;
                    }
                } else {
                    environment = environment.with(name, variable);
                }
            }
            Some(environment)
        };
        let environment = merge(self.environment(), other.lexical_bindings())?;
        let original = merge(self.original_environment(), other.original_bindings())?;
        let mut result = Self::with_environment(value, environment);
        if let Some(data) = &mut result.0 {
            Arc::make_mut(data).original = original;
        } else if original.0.is_some() {
            result.0 = Some(Arc::new(ProjectedReflectionData {
                value: None,
                environment: ReflectionEnvironment::default(),
                original,
            }));
        }
        Some(result)
    }
    pub(crate) fn carrier(&self, ty: EvalAstType) -> Result<ReflectedType, crate::error::Error> {
        ReflectedType::new(ty, "a closed projected carrier must be inference-free")
            .map(|ty| ty.with_lexical_bindings(self.lexical_bindings()))
    }
    pub(crate) fn opened_variable(
        &self,
        owner: &crate::pass::typecheck_core::ScopedType,
        param: &TypeParam,
    ) -> ReflectedVariable {
        let meaning = match self.as_ref() {
            Some(ty) => OpenedScheme::Complete(ty.clone()),
            None => OpenedScheme::Symbolic {
                owner: owner.clone(),
                environment: self.environment(),
            },
        };
        ReflectedVariable {
            name: param.name.clone(),
            arity: param.effective_kind().arity(),
            meaning: VariableMeaning::Opened(EvalRef::new(meaning)),
        }
    }
    #[cfg(test)]
    pub(crate) fn shares_storage_with(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        let same =
            |left: &ReflectionEnvironment, right: &ReflectionEnvironment| match (&left.0, &right.0)
            {
                (None, None) => true,
                (Some(left), Some(right)) => EvalRef::ptr_eq(left, right),
                _ => false,
            };
        self.as_ref() == other.as_ref()
            && same(&self.environment(), &other.environment())
            && same(&self.original_environment(), &other.original_environment())
    }
}

#[derive(Debug)]
pub(super) enum OpenedScheme {
    Complete(ReflectedType),
    #[cfg(feature = "surface")]
    Symbolic {
        owner: crate::pass::typecheck_core::ScopedType,
        environment: ReflectionEnvironment,
    },
    #[cfg(test)]
    Incomplete,
}

#[derive(Debug)]
pub(super) struct LexicalTypeToken;

#[derive(Clone, Debug)]
pub(super) enum VariableMeaning {
    Caller(crate::pass::typecheck_core::TypeBinderId),
    Live(Arc<LexicalTypeToken>),
    Neutral(Arc<LexicalTypeToken>),
    Opened(EvalRef<OpenedScheme>),
}

#[derive(Clone, Debug)]
pub struct ReflectedVariable {
    pub(super) name: String,
    pub(super) arity: usize,
    pub(super) meaning: VariableMeaning,
}

impl ReflectedVariable {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn arity(&self) -> usize {
        self.arity
    }

    pub(crate) fn caller_identity(&self) -> Option<crate::pass::typecheck_core::TypeBinderId> {
        match self.meaning {
            VariableMeaning::Caller(identity) => Some(identity),
            _ => None,
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        matches!(self.meaning, VariableMeaning::Live(_))
    }

    pub(crate) fn is_opened(&self) -> bool {
        matches!(self.meaning, VariableMeaning::Opened(_))
    }

    pub(crate) fn caller(
        name: String,
        arity: usize,
        binding: crate::pass::typecheck_core::TypeBinderId,
    ) -> Self {
        Self {
            name,
            arity,
            meaning: VariableMeaning::Caller(binding),
        }
    }

    pub(crate) fn live(name: String, arity: usize) -> Self {
        Self {
            name,
            arity,
            meaning: VariableMeaning::Live(Arc::new(LexicalTypeToken)),
        }
    }

    pub(super) fn neutral(name: String, arity: usize) -> Self {
        Self {
            name,
            arity,
            meaning: VariableMeaning::Neutral(Arc::new(LexicalTypeToken)),
        }
    }

    pub(crate) fn as_type(&self) -> ReflectedType {
        ReflectedType::from_parts(
            EvalRef::new(type_path(&self.name)),
            None,
            ReflectionEnvironment::default().with(self.name.clone(), self.clone()),
        )
    }

    pub(crate) fn same_reference(&self, other: &Self) -> bool {
        if self.arity != other.arity {
            return false;
        }
        match (&self.meaning, &other.meaning) {
            (VariableMeaning::Caller(left), VariableMeaning::Caller(right)) => left == right,
            (VariableMeaning::Live(left), VariableMeaning::Live(right))
            | (VariableMeaning::Neutral(left), VariableMeaning::Neutral(right)) => {
                Arc::ptr_eq(left, right)
            }
            (VariableMeaning::Opened(left), VariableMeaning::Opened(right)) => {
                EvalRef::ptr_eq(left, right)
            }
            _ => false,
        }
    }
}

pub(super) fn selected_type<'a>(
    root: &'a EvalAstType,
    cursor: Option<&TypeCursor>,
) -> &'a EvalAstType {
    let mut steps = Vec::new();
    let mut next = cursor;
    while let Some(cursor) = next {
        steps.push(cursor.child);
        next = cursor.parent.as_deref();
    }
    steps
        .into_iter()
        .rev()
        .fold(root, |ty, child| child.select(ty))
}

impl ReflectedType {
    pub(crate) fn with_lexical_bindings(
        mut self,
        bindings: impl IntoIterator<Item = (String, ReflectedVariable)>,
    ) -> Self {
        for (name, binding) in bindings {
            self.environment = self.environment.with(name, binding);
        }
        self
    }

    pub(super) fn from_parts(
        ty: EvalRef<EvalAstType>,
        cursor: Option<EvalRef<TypeCursor>>,
        environment: ReflectionEnvironment,
    ) -> Self {
        Self {
            ty,
            cursor,
            environment,
        }
    }

    pub(crate) fn child(&self, child: TypeChild) -> Self {
        child.select(self.as_type());
        Self::from_parts(
            self.ty.clone(),
            Some(EvalRef::new(TypeCursor {
                parent: self.cursor.clone(),
                child,
            })),
            self.environment.clone(),
        )
    }

    pub(super) fn open_forall(&self) -> Option<(ReflectedVariable, Self)> {
        let Type::Forall { param, .. } = self.as_type() else {
            return None;
        };
        let variable = ReflectedVariable {
            name: param.name.clone(),
            arity: param.effective_kind().arity(),
            meaning: VariableMeaning::Opened(EvalRef::new(OpenedScheme::Complete(self.clone()))),
        };
        let mut body = self.child(TypeChild::ForallBody);
        body.environment = body.environment.with(param.name.clone(), variable.clone());
        Some((variable, body))
    }

    pub(super) fn instantiate_forall(&self, argument: &Self) -> Option<Self> {
        let Type::Forall { param, .. } = self.as_type() else {
            return None;
        };
        let body = self.child(TypeChild::ForallBody);
        Some(type_arguments::substitute_reflected(
            &body,
            &[(param.name.as_str(), argument.clone())],
        ))
    }

    pub(super) fn close_forall(&self, variable: &ReflectedVariable) -> Result<Self, FreeOpened> {
        assert!(
            matches!(variable.meaning, VariableMeaning::Live(_)),
            "generic construction closes its live callback parameter"
        );
        if has_free_opened_reference(self) {
            return Err(FreeOpened);
        }
        let mut taken = HashSet::new();
        reserve_type_names(self.as_type(), &mut taken);
        for variable in self.free_variables() {
            if matches!(
                variable.meaning,
                VariableMeaning::Caller(_) | VariableMeaning::Live(_)
            ) {
                taken.insert(variable.name.clone());
            }
        }
        let name = if taken.contains("T") {
            let base = crate::naming::indexed_name("T", taken.len() + 1);
            crate::pass::typecheck_core::fresh_type_var(&base, &taken)
        } else {
            "T".to_owned()
        };
        let substitutions = self
            .free_bindings()
            .into_iter()
            .filter(|(_, bound)| bound.same_reference(variable))
            .map(|(head, _)| (head, ReflectedType::new_trusted(type_path(&name))))
            .collect();
        let body = type_arguments::substitute_template(self.as_type(), &substitutions);
        Ok(Self::from_parts(
            EvalRef::new(Type::Forall {
                param: TypeParam {
                    name,
                    presentation: Default::default(),
                    kind: Some(Box::new(crate::ast::Kind::arrow_chain(variable.arity))),
                    span: zero_span(),
                },
                body: Box::new(body),
                meta: zero_meta(),
            }),
            None,
            self.environment.clone(),
        ))
    }

    pub(super) fn free_bindings(&self) -> Vec<(&str, &ReflectedVariable)> {
        free_bindings_in(self.as_type(), &self.environment)
    }

    pub(super) fn free_variables(&self) -> Vec<&ReflectedVariable> {
        self.free_bindings()
            .into_iter()
            .map(|(_, variable)| variable)
            .collect()
    }
}

pub(super) fn free_bindings_in<'a>(
    root: &'a EvalAstType,
    environment: &'a ReflectionEnvironment,
) -> Vec<(&'a str, &'a ReflectedVariable)> {
    if environment.0.is_none() {
        return Vec::new();
    }
    enum Work<'a> {
        Enter(&'a EvalAstType),
        Leave(&'a str),
    }
    let mut work = vec![Work::Enter(root)];
    let mut bound = HashMap::<&str, usize>::new();
    let mut seen = HashSet::new();
    let mut variables = Vec::new();
    while let Some(next) = work.pop() {
        match next {
            Work::Leave(name) => {
                let count = bound.get_mut(name).expect("entered forall binder");
                *count -= 1;
                if *count == 0 {
                    bound.remove(name);
                }
            }
            Work::Enter(ty) => match ty {
                Type::Path { segments, args, .. } => {
                    if let [head] = segments.as_slice()
                        && !bound.contains_key(head.as_str())
                        && seen.insert(head.as_str())
                        && let Some(variable) = environment.get(head.as_str())
                    {
                        variables.push((head.as_str(), variable));
                    }
                    work.extend(args.iter().rev().map(Work::Enter));
                }
                Type::Forall { param, body, .. } => {
                    *bound.entry(&param.name).or_default() += 1;
                    work.push(Work::Leave(&param.name));
                    work.push(Work::Enter(body));
                }
                Type::Function { param, ret, .. } => {
                    work.push(Work::Enter(ret));
                    work.push(Work::Enter(param));
                }
                Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                    work.push(Work::Enter(right));
                    work.push(Work::Enter(left));
                }
                Type::Unit { .. } | Type::Bottom { .. } => {}
                Type::Infer { ext, .. } | Type::Goal { ext, .. } => match *ext {},
            },
        }
    }
    variables
}

pub(super) fn reserve_type_names(root: &EvalAstType, taken: &mut HashSet<String>) {
    let mut work = vec![root];
    while let Some(ty) = work.pop() {
        match ty {
            Type::Path { segments, args, .. } => {
                taken.extend(segments.iter().map(|segment| segment.name.clone()));
                work.extend(args);
            }
            Type::Forall { param, body, .. } => {
                taken.insert(param.name.clone());
                work.push(body);
            }
            Type::Function { param, ret, .. } => {
                work.push(ret);
                work.push(param);
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                work.push(right);
                work.push(left);
            }
            Type::Unit { .. } | Type::Bottom { .. } => {}
            Type::Infer { ext, .. } | Type::Goal { ext, .. } => match *ext {},
        }
    }
}

pub(super) struct ReflectionTypeBuilder {
    environment: ReflectionEnvironment,
    taken: HashSet<String>,
    representatives: Vec<(ReflectedVariable, String)>,
    correspondence: bool,
}

impl ReflectionTypeBuilder {
    pub(super) fn new<'a>(roots: impl IntoIterator<Item = &'a ReflectedType>) -> Self {
        Self::with_environment(roots, ReflectionEnvironment::default())
    }

    pub(super) fn for_correspondence<'a>(
        roots: impl IntoIterator<Item = &'a ReflectedType>,
    ) -> Self {
        let mut builder = Self::new(roots);
        builder.correspondence = true;
        builder
    }

    pub(super) fn with_environment<'a>(
        roots: impl IntoIterator<Item = &'a ReflectedType>,
        environment: ReflectionEnvironment,
    ) -> Self {
        let mut taken = HashSet::new();
        for root in roots {
            reserve_type_names(root.as_type(), &mut taken);
        }
        let mut representatives = Vec::new();
        let mut names = HashSet::new();
        let mut next = environment.0.as_deref();
        while let Some(binding) = next {
            if names.insert(binding.name.clone()) {
                representatives.push((binding.variable.clone(), binding.name.clone()));
            }
            next = binding.parent.as_deref();
        }
        Self {
            environment,
            taken,
            representatives,
            correspondence: false,
        }
    }

    pub(super) fn import(&mut self, ty: &ReflectedType) -> ownership::Owned<EvalAstType> {
        let mut replacements = HashMap::new();
        for (head, variable) in ty.free_bindings() {
            let existing = self.representatives.iter().find(|(representative, _)| {
                if self.correspondence {
                    reflected_variables_correspond(representative, variable)
                } else {
                    representative.same_reference(variable)
                }
            });
            let name = match existing {
                Some((_, name)) => name.clone(),
                None => {
                    let name = crate::pass::typecheck_core::fresh_type_var(head, &self.taken);
                    self.taken.insert(name.clone());
                    self.environment = self.environment.with(name.clone(), variable.clone());
                    self.representatives.push((variable.clone(), name.clone()));
                    name
                }
            };
            replacements.insert(head, ReflectedType::new_trusted(type_path(&name)));
        }
        ownership::Owned::new(if replacements.is_empty() {
            type_traversal::clone_type(ty.as_type())
        } else {
            type_arguments::substitute_template(ty.as_type(), &replacements)
        })
    }

    pub(super) fn finish(self, ty: EvalAstType) -> ReflectedType {
        ReflectedType::from_parts(EvalRef::new(ty), None, self.environment)
    }
}

#[derive(Clone, Debug)]
pub(super) struct ReflectedReferenceKey(pub(super) ReflectedVariable);

impl PartialEq for ReflectedReferenceKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.name == other.0.name && self.0.same_reference(&other.0)
    }
}
impl Eq for ReflectedReferenceKey {}
impl std::hash::Hash for ReflectedReferenceKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.name.hash(state);
        self.0.arity.hash(state);
        std::mem::discriminant(&self.0.meaning).hash(state);
        match &self.0.meaning {
            VariableMeaning::Caller(binding) => binding.hash(state),
            VariableMeaning::Live(token) | VariableMeaning::Neutral(token) => {
                Arc::as_ptr(token).hash(state)
            }
            VariableMeaning::Opened(owner) => std::ptr::from_ref(owner.as_ref()).hash(state),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ReflectedSnapshotKey {
    ty: type_traversal::ReflectionTypeKey,
    bindings: Vec<(String, ReflectedReferenceKey)>,
}

impl ReflectedSnapshotKey {
    pub(crate) fn new(ty: &ReflectedType, budget: &mut usize) -> Option<Self> {
        let bindings = charge_reflected_type(ty, budget)?;
        Some(Self {
            ty: type_traversal::ReflectionTypeKey::new(ty.as_type()),
            bindings: bindings
                .into_iter()
                .map(|(head, reference)| (head, ReflectedReferenceKey(reference.clone())))
                .collect(),
        })
    }

    pub(crate) fn presentation_matches(&self, other: &Self) -> bool {
        self.ty.presentation_matches(&other.ty)
    }
}

pub(super) fn charge_reflected_type<'a>(
    root: &'a ReflectedType,
    budget: &mut usize,
) -> Option<Vec<(String, &'a ReflectedVariable)>> {
    enum Work<'a> {
        Type(&'a ReflectedType),
        Cursor(&'a TypeCursor),
        Environment(&'a TypeEnvironment),
        Owner(&'a OpenedScheme),
    }
    let mut pending = vec![Work::Type(root)];
    let mut types = HashSet::new();
    let mut cursors = HashSet::new();
    let mut environments = HashSet::new();
    let mut owners = HashSet::new();
    while let Some(node) = pending.pop() {
        match node {
            Work::Type(ty) => {
                if types.insert(std::ptr::from_ref(ty.ty.as_ref())) {
                    type_traversal::charge_memo_type(&ty.ty, budget)?;
                }
                if let Some(cursor) = ty.cursor.as_deref() {
                    pending.push(Work::Cursor(cursor));
                }
                if let Some(environment) = ty.environment.0.as_deref() {
                    pending.push(Work::Environment(environment));
                }
            }
            Work::Cursor(cursor) => {
                if cursors.insert(std::ptr::from_ref(cursor)) {
                    *budget = budget.checked_sub(1)?;
                    if let Some(parent) = cursor.parent.as_deref() {
                        pending.push(Work::Cursor(parent));
                    }
                }
            }
            Work::Environment(environment) => {
                if environments.insert(std::ptr::from_ref(environment)) {
                    *budget = budget.checked_sub(1)?;
                    if let Some(parent) = environment.parent.as_deref() {
                        pending.push(Work::Environment(parent));
                    }
                    if let VariableMeaning::Opened(owner) = &environment.variable.meaning {
                        pending.push(Work::Owner(owner));
                    }
                }
            }
            Work::Owner(owner) => {
                if owners.insert(std::ptr::from_ref(owner)) {
                    *budget = budget.checked_sub(1)?;
                    match owner {
                        OpenedScheme::Complete(ty) => pending.push(Work::Type(ty)),
                        // Symbolic owners retain invocation capabilities even when
                        // this environment entry is not used by the selected type.
                        #[cfg(feature = "surface")]
                        OpenedScheme::Symbolic { .. } => return None,
                        #[cfg(test)]
                        OpenedScheme::Incomplete => return None,
                    }
                }
            }
        }
    }
    let mut bindings = Vec::new();
    if root.environment.0.is_some() {
        let mut heads: Vec<_> = eval_refl_type_free_bare_heads(root.as_type())
            .into_iter()
            .collect();
        heads.sort();
        for head in heads {
            let mut next = root.environment.0.as_deref();
            while let Some(binding) = next {
                *budget = budget.checked_sub(1)?;
                if binding.name == head {
                    bindings.push((head, &binding.variable));
                    break;
                }
                next = binding.parent.as_deref();
            }
        }
    }
    Some(bindings)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FreeOpened;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncompleteOwner;

pub(super) fn require_determined(root: &ReflectedType) -> Result<(), IncompleteOwner> {
    let mut work = root.free_variables();
    let mut visited = HashSet::new();
    while let Some(variable) = work.pop() {
        if let VariableMeaning::Opened(owner) = &variable.meaning {
            let id = std::ptr::from_ref(owner.as_ref());
            if !visited.insert(id) {
                continue;
            }
            match owner.as_ref() {
                OpenedScheme::Complete(ty) => work.extend(ty.free_variables()),
                #[cfg(feature = "surface")]
                OpenedScheme::Symbolic { .. } => return Err(IncompleteOwner),
                #[cfg(test)]
                OpenedScheme::Incomplete => return Err(IncompleteOwner),
            }
        }
    }
    Ok(())
}

pub(super) fn has_free_opened_reference(root: &ReflectedType) -> bool {
    root.free_variables()
        .iter()
        .any(|variable| matches!(variable.meaning, VariableMeaning::Opened(_)))
}

impl PartialEq for ReflectedType {
    fn eq(&self, other: &Self) -> bool {
        let left = type_traversal::ReflectionTypeKey::new(self.as_type());
        let right = type_traversal::ReflectionTypeKey::new(other.as_type());
        if left != right || !left.presentation_matches(&right) {
            return false;
        }
        let left = self.free_variables();
        let right = other.free_variables();
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(left, right)| left.name == right.name && left.same_reference(right))
    }
}
impl Eq for ReflectedType {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeComparison {
    Equal,
    Different,
    Undetermined,
}

/// A borrowed view lets the same comparison traverse validated reflected
/// types and frozen Lowered owner formulas without cloning either tree.
#[derive(Clone, Copy)]
enum ComparisonType<'a> {
    Closed(&'a EvalAstType),
    #[cfg(feature = "surface")]
    Symbolic(&'a crate::ast::Type<crate::ast::Lowered>),
}

enum ComparisonNode<'a> {
    Unit,
    Bottom,
    Path(&'a [crate::ast::PathSegment], usize),
    Function,
    Product,
    Sum,
    Forall(&'a TypeParam),
    #[cfg(feature = "surface")]
    Goal(crate::ast::TypeGoalRef, usize),
    #[cfg(feature = "surface")]
    Infer,
}

impl<'a> ComparisonType<'a> {
    fn node(self) -> ComparisonNode<'a> {
        macro_rules! node {
            ($ty:expr) => {
                match $ty {
                    Type::Unit { .. } => ComparisonNode::Unit,
                    Type::Bottom { .. } => ComparisonNode::Bottom,
                    Type::Path { segments, args, .. } => ComparisonNode::Path(segments, args.len()),
                    Type::Function { .. } => ComparisonNode::Function,
                    Type::Product { .. } => ComparisonNode::Product,
                    Type::Sum { .. } => ComparisonNode::Sum,
                    Type::Forall { param, .. } => ComparisonNode::Forall(param),
                    Type::Goal { goal, args, .. } => {
                        #[cfg(feature = "surface")]
                        {
                            ComparisonNode::Goal(*goal, args.len())
                        }
                        #[cfg(not(feature = "surface"))]
                        {
                            let _ = (goal, args);
                            unreachable!("reflected types contain no goals")
                        }
                    }
                    Type::Infer { .. } => {
                        #[cfg(feature = "surface")]
                        {
                            ComparisonNode::Infer
                        }
                        #[cfg(not(feature = "surface"))]
                        {
                            unreachable!("reflected types contain no inference placeholders")
                        }
                    }
                }
            };
        }
        match self {
            Self::Closed(ty) => node!(ty),
            #[cfg(feature = "surface")]
            Self::Symbolic(ty) => node!(ty),
        }
    }

    fn child(self, index: usize) -> Self {
        macro_rules! child {
            ($ty:expr, $variant:ident) => {
                Self::$variant(match $ty {
                    Type::Path { args, .. } | Type::Goal { args, .. } => &args[index],
                    Type::Function { param, ret, .. } => {
                        if index == 0 {
                            param
                        } else {
                            ret
                        }
                    }
                    Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                        if index == 0 { left } else { right }
                    }
                    Type::Forall { body, .. } => body,
                    _ => unreachable!("comparison child follows an admitted AST constructor"),
                })
            };
        }
        match self {
            Self::Closed(ty) => child!(ty, Closed),
            #[cfg(feature = "surface")]
            Self::Symbolic(ty) => child!(ty, Symbolic),
        }
    }
}

pub(super) fn semantic_type_comparison(
    left: &ReflectedType,
    right: &ReflectedType,
) -> Result<TypeComparison, IncompleteOwner> {
    require_determined(left)?;
    require_determined(right)?;
    compare_type_formulas(left, right, false)
}

/// Structural validation transports authenticated references without observing
/// unresolved owner information. An unproved symbolic relation stays unknown.
pub(crate) fn type_correspondence(
    left: &ReflectedType,
    right: &ReflectedType,
) -> Result<TypeComparison, IncompleteOwner> {
    compare_type_formulas(left, right, true)
}

pub(crate) fn reflected_variables_correspond(
    left: &ReflectedVariable,
    right: &ReflectedVariable,
) -> bool {
    matches!(
        type_correspondence(&left.as_type(), &right.as_type()),
        Ok(TypeComparison::Equal)
    )
}

fn compare_type_formulas(
    left: &ReflectedType,
    right: &ReflectedType,
    symbolic: bool,
) -> Result<TypeComparison, IncompleteOwner> {
    #[cfg(not(feature = "surface"))]
    let _ = symbolic;
    #[derive(Default)]
    struct Scope {
        left: HashMap<String, Vec<usize>>,
        right: HashMap<String, Vec<usize>>,
        depth: usize,
    }
    enum Work<'a> {
        Types(
            ComparisonType<'a>,
            &'a ReflectionEnvironment,
            ComparisonType<'a>,
            &'a ReflectionEnvironment,
            usize,
            bool,
        ),
        Variables(&'a ReflectedVariable, &'a ReflectedVariable, bool),
        Leave(&'a str, &'a str, usize),
    }
    let mut work = vec![Work::Types(
        ComparisonType::Closed(left.as_type()),
        &left.environment,
        ComparisonType::Closed(right.as_type()),
        &right.environment,
        0,
        true,
    )];
    let mut scopes = vec![Scope::default()];
    let mut owners = HashSet::new();
    let mut undetermined = false;
    macro_rules! different {
        ($decisive:expr) => {{
            if $decisive {
                return Ok(TypeComparison::Different);
            }
            undetermined = true;
            continue;
        }};
    }
    while let Some(next) = work.pop() {
        match next {
            Work::Leave(left, right, scope) => {
                let scope = &mut scopes[scope];
                scope.left.get_mut(left).expect("entered left binder").pop();
                scope
                    .right
                    .get_mut(right)
                    .expect("entered right binder")
                    .pop();
                scope.depth -= 1;
            }
            Work::Variables(left, right, decisive) => {
                if left.arity != right.arity {
                    different!(decisive);
                }
                match (&left.meaning, &right.meaning) {
                    (VariableMeaning::Caller(left), VariableMeaning::Caller(right))
                        if left == right => {}
                    (VariableMeaning::Live(left), VariableMeaning::Live(right))
                    | (VariableMeaning::Neutral(left), VariableMeaning::Neutral(right))
                        if Arc::ptr_eq(left, right) => {}
                    (VariableMeaning::Opened(left), VariableMeaning::Opened(right)) => {
                        if !owners.insert((
                            std::ptr::from_ref(left.as_ref()),
                            std::ptr::from_ref(right.as_ref()),
                            decisive,
                        )) {
                            continue;
                        }
                        fn owner(
                            owner: &OpenedScheme,
                            symbolic: bool,
                        ) -> Result<
                            (ComparisonType<'_>, &ReflectionEnvironment, bool),
                            IncompleteOwner,
                        > {
                            #[cfg(not(feature = "surface"))]
                            let _ = symbolic;
                            match owner {
                                OpenedScheme::Complete(ty) => Ok((
                                    ComparisonType::Closed(ty.as_type()),
                                    &ty.environment,
                                    true,
                                )),
                                #[cfg(feature = "surface")]
                                OpenedScheme::Symbolic { owner, environment } if symbolic => Ok((
                                    ComparisonType::Symbolic(owner.ty().as_type()),
                                    environment,
                                    false,
                                )),
                                #[cfg(feature = "surface")]
                                OpenedScheme::Symbolic { .. } => Err(IncompleteOwner),
                                #[cfg(test)]
                                OpenedScheme::Incomplete => Err(IncompleteOwner),
                            }
                        }
                        let (left, le, lc) = owner(left, symbolic)?;
                        let (right, re, rc) = owner(right, symbolic)?;
                        let decisive = decisive && lc && rc;
                        let scope = scopes.len();
                        scopes.push(Scope::default());
                        work.push(Work::Types(left, le, right, re, scope, decisive));
                    }
                    (VariableMeaning::Neutral(_), _) | (_, VariableMeaning::Neutral(_)) => {
                        undetermined = true
                    }
                    _ => different!(decisive),
                }
            }
            Work::Types(left, le, right, re, scope, decisive) => {
                fn neutral<'a>(
                    ty: ComparisonType<'_>,
                    env: &'a ReflectionEnvironment,
                    bound: &HashMap<String, Vec<usize>>,
                ) -> Option<&'a ReflectedVariable> {
                    let ComparisonNode::Path([head], _) = ty.node() else {
                        return None;
                    };
                    if bound
                        .get(head.as_str())
                        .and_then(|stack| stack.last())
                        .is_some()
                    {
                        return None;
                    }
                    env.get(head.as_str())
                        .filter(|variable| matches!(variable.meaning, VariableMeaning::Neutral(_)))
                }
                let ln = neutral(left, le, &scopes[scope].left);
                let rn = neutral(right, re, &scopes[scope].right);
                if ln.is_some() || rn.is_some() {
                    match (ln, rn, left.node(), right.node()) {
                        (
                            Some(lv),
                            Some(rv),
                            ComparisonNode::Path(_, la),
                            ComparisonNode::Path(_, ra),
                        ) if lv.same_reference(rv) && la == ra => {
                            for index in (0..la).rev() {
                                work.push(Work::Types(
                                    left.child(index),
                                    le,
                                    right.child(index),
                                    re,
                                    scope,
                                    false,
                                ));
                            }
                        }
                        _ => undetermined = true,
                    }
                    continue;
                }
                match (left.node(), right.node()) {
                    (ComparisonNode::Unit, ComparisonNode::Unit)
                    | (ComparisonNode::Bottom, ComparisonNode::Bottom) => {}
                    (ComparisonNode::Path(ls, la), ComparisonNode::Path(rs, ra)) => {
                        if la != ra {
                            different!(decisive);
                        }
                        let lb = if let [head] = ls {
                            scopes[scope].left.get(head.as_str()).and_then(|s| s.last())
                        } else {
                            None
                        };
                        let rb = if let [head] = rs {
                            scopes[scope]
                                .right
                                .get(head.as_str())
                                .and_then(|s| s.last())
                        } else {
                            None
                        };
                        if lb.is_some() || rb.is_some() {
                            if lb != rb {
                                different!(decisive);
                            }
                        } else {
                            let lv = if let [head] = ls {
                                le.get(head.as_str())
                            } else {
                                None
                            };
                            let rv = if let [head] = rs {
                                re.get(head.as_str())
                            } else {
                                None
                            };
                            match (lv, rv) {
                                (Some(lv), Some(rv)) => {
                                    work.push(Work::Variables(lv, rv, decisive))
                                }
                                (None, None)
                                    if ls
                                        .iter()
                                        .map(|n| n.as_str())
                                        .eq(rs.iter().map(|n| n.as_str())) => {}
                                _ => different!(decisive),
                            }
                        }
                        for index in (0..la).rev() {
                            work.push(Work::Types(
                                left.child(index),
                                le,
                                right.child(index),
                                re,
                                scope,
                                decisive,
                            ));
                        }
                    }
                    (ComparisonNode::Function, ComparisonNode::Function)
                    | (ComparisonNode::Product, ComparisonNode::Product)
                    | (ComparisonNode::Sum, ComparisonNode::Sum) => {
                        work.push(Work::Types(
                            left.child(1),
                            le,
                            right.child(1),
                            re,
                            scope,
                            decisive,
                        ));
                        work.push(Work::Types(
                            left.child(0),
                            le,
                            right.child(0),
                            re,
                            scope,
                            decisive,
                        ));
                    }
                    (ComparisonNode::Forall(lp), ComparisonNode::Forall(rp)) => {
                        if lp.effective_kind().arity() != rp.effective_kind().arity() {
                            different!(decisive);
                        }
                        let state = &mut scopes[scope];
                        state
                            .left
                            .entry(lp.name.clone())
                            .or_default()
                            .push(state.depth);
                        state
                            .right
                            .entry(rp.name.clone())
                            .or_default()
                            .push(state.depth);
                        state.depth += 1;
                        work.push(Work::Leave(&lp.name, &rp.name, scope));
                        work.push(Work::Types(
                            left.child(0),
                            le,
                            right.child(0),
                            re,
                            scope,
                            decisive,
                        ));
                    }
                    #[cfg(feature = "surface")]
                    (ComparisonNode::Goal(lg, la), ComparisonNode::Goal(rg, ra)) if symbolic => {
                        if lg != rg || la != ra {
                            different!(decisive);
                        }
                        for index in (0..la).rev() {
                            work.push(Work::Types(
                                left.child(index),
                                le,
                                right.child(index),
                                re,
                                scope,
                                decisive,
                            ));
                        }
                    }
                    _ => different!(decisive),
                }
            }
        }
    }
    Ok(if undetermined {
        TypeComparison::Undetermined
    } else {
        TypeComparison::Equal
    })
}

pub(super) fn semantic_type_equal(
    left: &ReflectedType,
    right: &ReflectedType,
) -> Result<bool, IncompleteOwner> {
    semantic_type_comparison(left, right).map(|comparison| comparison == TypeComparison::Equal)
}

#[cfg(test)]
pub(super) fn semantic_variable_equal(
    left: &ReflectedVariable,
    right: &ReflectedVariable,
) -> Result<bool, IncompleteOwner> {
    semantic_type_equal(&left.as_type(), &right.as_type())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme(first: &str, second: &str) -> ReflectedType {
        let body = Type::Function {
            param: Box::new(type_path(first)),
            ret: Box::new(type_path(second)),
            abi_arity: 1,
            caps: (),
            meta: zero_meta(),
        };
        let forall = |name: &str, body| Type::Forall {
            param: TypeParam {
                presentation: Default::default(),
                name: name.into(),
                kind: None,
                span: zero_span(),
            },
            body: Box::new(body),
            meta: zero_meta(),
        };
        ReflectedType::new_trusted(forall(first, forall(second, body)))
    }

    #[cfg(feature = "surface")]
    #[test]
    fn frozen_symbolic_correspondence_preserves_exact_unknown_leaves() {
        let make = |name: &str, goal| {
            let param = TypeParam {
                name: name.into(),
                kind: None,
                span: zero_span(),
                presentation: Default::default(),
            };
            let owner = crate::pass::typecheck_core::ScopedType::projected_for_test(Type::Forall {
                param: param.clone(),
                body: Box::new(Type::Function {
                    param: Box::new(Type::synth_path(vec![name.into()], Vec::new(), zero_span())),
                    ret: Box::new(Type::Goal {
                        goal,
                        args: Vec::new(),
                        meta: crate::ast::Meta::new(zero_span()),
                        ext: (),
                    }),
                    abi_arity: 1,
                    caps: (),
                    meta: crate::ast::Meta::new(zero_span()),
                }),
                meta: crate::ast::Meta::new(zero_span()),
            });
            ProjectedReflection::default().opened_variable(&owner, &param)
        };
        let left = make("A", crate::ast::TypeGoalRef::for_test(1, 0, 0));
        let renamed = make("B", crate::ast::TypeGoalRef::for_test(1, 0, 0));
        let other_leaf = make("B", crate::ast::TypeGoalRef::for_test(1, 0, 1));
        assert_eq!(
            type_correspondence(&left.as_type(), &renamed.as_type()),
            Ok(TypeComparison::Equal)
        );
        assert_eq!(
            type_correspondence(&left.as_type(), &other_leaf.as_type()),
            Ok(TypeComparison::Undetermined)
        );
        assert_eq!(
            semantic_type_comparison(&left.as_type(), &left.as_type()),
            Err(IncompleteOwner)
        );
        assert_eq!(
            semantic_type_comparison(&left.as_type(), &renamed.as_type()),
            Err(IncompleteOwner)
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn projected_reflection_noop_transforms_retain_storage() {
        let reference = ReflectedVariable::live("L".into(), 0);
        let carrier = ProjectedReflection::closed(Some(reference.as_type()));
        let name = carrier.lexical_bindings()[0].0.clone();
        let mapped = carrier
            .clone()
            .with_lexical_bindings([(name.clone(), reference)])
            .with_scope([])
            .retaining_bindings(&HashSet::from([name]))
            .renamed(&HashMap::new());
        assert!(mapped.shares_storage_with(&carrier));
    }

    #[test]
    fn opened_correspondence_uses_the_containing_scheme_and_used_environment() {
        let left = scheme("A", "B");
        let right = scheme("X", "Y");
        let (la, lb) = left.open_forall().expect("outer");
        let (ra, rb) = right.open_forall().expect("outer");
        assert!(semantic_variable_equal(&la, &ra).unwrap());
        let (lb, _) = lb.open_forall().expect("inner");
        let (rb, _) = rb.open_forall().expect("inner");
        assert!(semantic_variable_equal(&lb, &rb).unwrap());
        assert!(!semantic_variable_equal(&la, &lb).unwrap());
        assert!(semantic_type_equal(&la.as_type(), &ra.as_type()).unwrap());
        let mut noisy = right.clone();
        noisy.environment = noisy
            .environment
            .with("unused".into(), ReflectedVariable::live("unused".into(), 0));
        assert!(semantic_type_equal(&left, &noisy).unwrap());
        let (noisy, _) = noisy.open_forall().expect("outer");
        assert!(semantic_variable_equal(&la, &noisy).unwrap());
    }

    #[test]
    fn owner_determinedness_precedes_identical_reference_shortcuts() {
        let unknown = ReflectedVariable {
            name: "A".into(),
            arity: 0,
            meaning: VariableMeaning::Opened(EvalRef::new(OpenedScheme::Incomplete)),
        };
        assert_eq!(
            semantic_variable_equal(&unknown, &unknown),
            Err(IncompleteOwner)
        );
        let mut containing = scheme("X", "Y");
        containing.environment = containing.environment.with("A".into(), unknown.clone());
        assert!(
            require_determined(&containing).is_ok(),
            "unused metadata does not make an owner incomplete"
        );
        let owner = ReflectedType::from_parts(
            EvalRef::new(Type::Forall {
                param: TypeParam {
                    presentation: Default::default(),
                    name: "B".into(),
                    kind: None,
                    span: zero_span(),
                },
                body: Box::new(Type::Function {
                    param: Box::new(type_path("A")),
                    ret: Box::new(type_path("B")),
                    abi_arity: 1,
                    caps: (),
                    meta: zero_meta(),
                }),
                meta: zero_meta(),
            }),
            None,
            ReflectionEnvironment::default().with("A".into(), unknown),
        );
        let (opened, _) = owner.open_forall().expect("forall");
        assert_eq!(
            semantic_variable_equal(&opened, &opened),
            Err(IncompleteOwner)
        );
    }

    #[test]
    fn fragment_cursors_share_the_root_and_release_deep_ancestry() {
        let mut ty = Type::Unit { meta: zero_meta() };
        for _ in 0..10_000 {
            ty = Type::Forall {
                param: TypeParam {
                    presentation: Default::default(),
                    name: "A".into(),
                    kind: None,
                    span: zero_span(),
                },
                body: Box::new(ty),
                meta: zero_meta(),
            };
        }
        let root = ReflectedType::new_trusted(ty);
        let mut fragment = root.clone();
        for _ in 0..10_000 {
            fragment = ReflectedType::from_parts(
                fragment.ty.clone(),
                Some(EvalRef::new(TypeCursor {
                    parent: fragment.cursor.clone(),
                    child: TypeChild::ForallBody,
                })),
                ReflectionEnvironment::default(),
            );
        }
        assert!(EvalRef::ptr_eq(&root.ty, &fragment.ty));
        assert!(matches!(fragment.as_type(), Type::Unit { .. }));
        drop(root);
        drop(fragment);
    }

    #[test]
    fn lexical_references_and_neutrals_are_distinct_without_name_identity() {
        let left = ReflectedVariable::live("A".into(), 0);
        let right = ReflectedVariable::live("A".into(), 0);
        assert!(!semantic_variable_equal(&left, &right).unwrap());
        let mut renamed = left.clone();
        renamed.name = "B".into();
        assert!(semantic_variable_equal(&left, &renamed).unwrap());
        let neutral = ReflectedVariable::neutral("A".into(), 0);
        assert!(!semantic_variable_equal(&left, &neutral).unwrap());
    }

    #[test]
    fn lexical_caller_and_opened_closure_checks_use_actual_free_occurrences() {
        let caller = ReflectedVariable::caller(
            "A".into(),
            1,
            crate::pass::typecheck_core::TypeBinderId::for_test(91),
        );
        let mut renamed = caller.clone();
        renamed.name = "B".into();
        assert!(semantic_variable_equal(&caller, &renamed).unwrap());
        assert!(!has_free_opened_reference(&caller.as_type()));
        let complete = scheme("A", "B");
        let (opened, fragment) = complete.open_forall().unwrap();
        assert!(has_free_opened_reference(&opened.as_type()));
        assert!(has_free_opened_reference(&fragment));
        assert!(!has_free_opened_reference(&complete));
        let mut intact = complete;
        intact.environment = intact.environment.with("not_used".into(), opened);
        assert!(!has_free_opened_reference(&intact));
    }

    #[test]
    fn composition_preserves_opened_owners_and_reserves_every_operand_name() {
        let (left, _) = scheme("A", "B").open_forall().unwrap();
        let (right, _) = scheme("X", "Y").open_forall().unwrap();
        let left = left.as_type();
        let right = right.as_type();
        let nominal = ReflectedType::new_trusted(type_path("A_n2"));
        let mut builder = ReflectionTypeBuilder::new([&left, &right, &nominal]);
        let l = builder.import(&left);
        let r = builder.import(&right);
        let n = builder.import(&nominal);
        let combined = builder.finish(Type::Product {
            left: Box::new(l.take()),
            right: Box::new(Type::Product {
                left: Box::new(r.take()),
                right: Box::new(n.take()),
                meta: zero_meta(),
            }),
            meta: zero_meta(),
        });
        let l = combined.child(TypeChild::Left);
        let r = combined.child(TypeChild::Right).child(TypeChild::Left);
        let n = combined.child(TypeChild::Right).child(TypeChild::Right);
        assert!(semantic_type_equal(&l, &r).unwrap());
        assert!(semantic_type_equal(&n, &nominal).unwrap());
        assert!(!semantic_type_equal(&l, &n).unwrap());
        assert!(has_free_opened_reference(&combined));
        assert!(l.free_variables()[0].same_reference(left.free_variables()[0]));
        assert!(r.free_variables()[0].same_reference(right.free_variables()[0]));
    }

    #[test]
    fn closing_rebinds_the_live_reference_and_rejects_only_used_opened_fragments() {
        let outer = ReflectedVariable::live("T".into(), 0);
        let inner = ReflectedVariable::live("T".into(), 0);
        let outer_type = outer.as_type();
        let inner_type = inner.as_type();
        let mut builder = ReflectionTypeBuilder::new([&outer_type, &inner_type]);
        let left = builder.import(&outer_type);
        let right = builder.import(&inner_type);
        let body = builder.finish(Type::Product {
            left: Box::new(left.take()),
            right: Box::new(right.take()),
            meta: zero_meta(),
        });
        let closed_inner = body.close_forall(&inner).unwrap();
        assert_eq!(closed_inner.free_variables().len(), 1);
        assert!(closed_inner.free_variables()[0].same_reference(&outer));
        let closed = closed_inner.close_forall(&outer).unwrap();
        assert!(closed.free_variables().is_empty());
        let recreated_outer = ReflectedVariable::live("T".into(), 0);
        let recreated_inner = ReflectedVariable::live("T".into(), 0);
        let recreated = type_arguments::substitute_reflected(
            &ReflectedType::new_trusted(Type::Product {
                left: Box::new(type_path("L")),
                right: Box::new(type_path("R")),
                meta: zero_meta(),
            }),
            &[
                ("L", recreated_outer.as_type()),
                ("R", recreated_inner.as_type()),
            ],
        )
        .close_forall(&recreated_inner)
        .unwrap()
        .close_forall(&recreated_outer)
        .unwrap();
        assert!(semantic_type_equal(&closed, &recreated).unwrap());
        let (opened, fragment) = scheme("A", "B").open_forall().unwrap();
        assert_eq!(opened.as_type().close_forall(&outer), Err(FreeOpened));
        assert_eq!(fragment.close_forall(&outer), Err(FreeOpened));
        let mut intact = scheme("A", "B");
        intact.environment = intact.environment.with("unused".into(), opened);
        assert!(intact.close_forall(&outer).is_ok());
        let rebound = scheme("A", "B").instantiate_forall(&outer_type).unwrap();
        assert!(!has_free_opened_reference(&rebound));
        assert!(rebound.close_forall(&outer).is_ok());
    }

    #[test]
    fn closing_a_forall_checks_collisions_in_its_name_hint() {
        let variable = ReflectedVariable::live("unused".into(), 0);
        let body = ReflectedType::new_trusted(Type::Product {
            left: Box::new(type_path("T")),
            right: Box::new(Type::Product {
                left: Box::new(type_path("T_n4")),
                right: Box::new(type_path("T_n4_n2")),
                meta: zero_meta(),
            }),
            meta: zero_meta(),
        });
        let closed = body.close_forall(&variable).unwrap();
        let Type::Forall {
            param,
            body: actual,
            ..
        } = closed.as_type()
        else {
            panic!("closing adds one quantified layer")
        };
        assert!(!["T", "T_n4", "T_n4_n2"].contains(&param.name.as_str()));
        assert_eq!(actual.as_ref(), body.as_type());
        let expected = ReflectedType::new_trusted(Type::Forall {
            param: TypeParam {
                name: "Bound".into(),
                presentation: Default::default(),
                kind: None,
                span: zero_span(),
            },
            body: Box::new(type_traversal::clone_type(body.as_type())),
            meta: zero_meta(),
        });
        assert!(semantic_type_equal(&closed, &expected).unwrap());
        assert_eq!(closed, body.close_forall(&variable).unwrap());
    }

    #[test]
    fn reflected_constructors_preserve_lexical_operands_in_direct_and_generic_dispatch() {
        let ctx = EvalCtx::new();
        let left = ReflectedVariable::live("A".into(), 0).as_type();
        let right = ReflectedVariable::live("A".into(), 0).as_type();
        for builtin in [ComptimeBuiltin::TypeProduct, ComptimeBuiltin::TypeSum] {
            let l = EvalRef::new(EvalExpr::Const(Value::ReflType(left.clone())));
            let r = EvalRef::new(EvalExpr::Const(Value::ReflType(right.clone())));
            let direct = if builtin == ComptimeBuiltin::TypeProduct {
                EvalExpr::ReflTypeProduct { left: l, right: r }
            } else {
                EvalExpr::ReflTypeSum { left: l, right: r }
            };
            let direct = eval_compiled_expr(&direct, &mut EvalSlots::root(Vec::new()), &ctx);
            let generic = apply(
                Value::Primitive(builtin),
                vec![
                    Value::ReflType(left.clone()),
                    Value::ReflType(right.clone()),
                ],
                &ctx,
            );
            let (Value::ReflType(direct), Value::ReflType(generic)) = (direct, generic) else {
                panic!("reflected binary result");
            };
            assert!(semantic_type_equal(&direct, &generic).unwrap());
            assert!(semantic_type_equal(&direct.child(TypeChild::Left), &left).unwrap());
            assert!(semantic_type_equal(&direct.child(TypeChild::Right), &right).unwrap());
            assert!(
                !semantic_type_equal(
                    &direct.child(TypeChild::Left),
                    &direct.child(TypeChild::Right)
                )
                .unwrap()
            );
        }
        let arrow = apply(
            Value::Primitive(ComptimeBuiltin::TypeArrow),
            vec![
                Value::ReflType(left.clone()),
                Value::ReflType(right.clone()),
            ],
            &ctx,
        );
        let Value::ReflType(arrow) = arrow else {
            panic!("reflected arrow");
        };
        assert!(semantic_type_equal(&arrow.child(TypeChild::Left), &left).unwrap());
        assert!(semantic_type_equal(&arrow.child(TypeChild::Right), &right).unwrap());
        let head = ReflectedVariable::live("A".into(), 1).as_type();
        let applied = apply(
            Value::Primitive(ComptimeBuiltin::TypeApply),
            vec![
                Value::ReflType(head.clone()),
                Value::ReflType(right.clone()),
            ],
            &ctx,
        );
        let Value::ReflType(applied) = applied else {
            panic!("reflected application");
        };
        assert!(applied.free_variables()[0].same_reference(head.free_variables()[0]));
        assert!(semantic_type_equal(&applied.child(TypeChild::Argument(0)), &right).unwrap());
    }

    #[test]
    fn exact_snapshot_keys_retain_used_references_and_charge_owner_graphs() {
        let left = ReflectedVariable::live("A".into(), 0).as_type();
        let right = ReflectedVariable::live("A".into(), 0).as_type();
        let key = |ty: &ReflectedType| ReflectedSnapshotKey::new(ty, &mut 96).unwrap();
        assert_eq!(key(&left), key(&left.clone()));
        assert_ne!(key(&left), key(&right));
        let mut noisy = left.clone();
        noisy.environment = noisy
            .environment
            .with("unused".into(), ReflectedVariable::live("unused".into(), 0));
        assert_eq!(key(&left), key(&noisy));
        let (opened, _) = scheme("A", "B").open_forall().unwrap();
        let opened = opened.as_type();
        assert!(ReflectedSnapshotKey::new(&opened, &mut 2).is_none());
        let owner_key = key(&opened);
        drop(opened);
        assert_eq!(owner_key, owner_key.clone());
        let (recreated, _) = scheme("A", "B").open_forall().unwrap();
        assert_ne!(
            owner_key,
            key(&recreated.as_type()),
            "separate owner snapshots may conservatively miss"
        );
    }

    #[test]
    fn memo_budget_counts_retained_roots_and_unused_environment_owners() {
        let unit = || Type::Unit { meta: zero_meta() };
        let mut large = unit();
        for _ in 0..100 {
            large = Type::Product {
                left: Box::new(unit()),
                right: Box::new(large),
                meta: zero_meta(),
            };
        }
        let root = ReflectedType::new_trusted(Type::Product {
            left: Box::new(unit()),
            right: Box::new(large),
            meta: zero_meta(),
        });
        let tiny = root.child(TypeChild::Left);
        assert!(matches!(tiny.as_type(), Type::Unit { .. }));
        assert!(ReflectedSnapshotKey::new(&tiny, &mut 96).is_none());
        let mut unused = ReflectedType::new_trusted(unit());
        unused.environment = unused.environment.with(
            "unused".into(),
            ReflectedVariable {
                name: "unused".into(),
                arity: 0,
                meaning: VariableMeaning::Opened(EvalRef::new(OpenedScheme::Complete(root))),
            },
        );
        assert!(semantic_type_equal(&unused, &ReflectedType::new_trusted(unit())).unwrap());
        assert!(ReflectedSnapshotKey::new(&unused, &mut 96).is_none());
        assert!(charge_reflected_type(&unused, &mut 512).is_some());
    }

    #[test]
    fn forall_instantiation_preserves_lexical_arguments_and_alias_body_environments() {
        let package = crate::normalization::artifact::tests::lowered_package(&[(
            "a.kio",
            "mod a; type T = .; type Alias[A] = [B] A -> B;",
        )]);
        let ctx = EvalCtx::new().with_package(Some(&package));
        let caller = ReflectedVariable::live("T".into(), 0).as_type();
        let alias = ReflectedType::from_parts(
            EvalRef::new(Type::synth_path(
                vec!["a".into(), "Alias".into()],
                vec![type_traversal::clone_type(caller.as_type())],
                zero_span(),
            )),
            None,
            caller.environment.clone(),
        );
        let expanded = unfold_reflected_carrier(&alias, &ctx);
        assert!(expanded.free_variables()[0].same_reference(caller.free_variables()[0]));
        let again = unfold_reflected_carrier(&expanded, &ctx);
        assert!(
            EvalRef::ptr_eq(&expanded.ty, &again.ty),
            "an already expanded owner keeps its shared root"
        );
        let argument = ReflectedVariable::live("T".into(), 0).as_type();
        let direct = EvalExpr::ReflTypeInstantiate {
            scheme: EvalRef::new(EvalExpr::Const(Value::ReflType(alias.clone()))),
            arg: EvalRef::new(EvalExpr::Const(Value::ReflType(argument.clone()))),
        };
        let direct = eval_compiled_expr(&direct, &mut EvalSlots::root(Vec::new()), &ctx);
        let generic = apply(
            Value::Primitive(ComptimeBuiltin::TypeInstantiate),
            vec![Value::ReflType(alias), Value::ReflType(argument.clone())],
            &ctx,
        );
        let result = |value| {
            let Value::Data(data) = value else {
                panic!("instantiation result");
            };
            assert_eq!(data.ctor(), "__left__");
            let [Value::ReflType(ty)] = data.args() else {
                panic!("instantiated type payload");
            };
            ty.clone()
        };
        let direct = result(direct);
        let generic = result(generic);
        assert!(semantic_type_equal(&direct, &generic).unwrap());
        assert!(semantic_type_equal(&direct.child(TypeChild::Left), &caller).unwrap());
        assert!(semantic_type_equal(&direct.child(TypeChild::Right), &argument).unwrap());
        assert!(
            !semantic_type_equal(
                &direct.child(TypeChild::Left),
                &direct.child(TypeChild::Right)
            )
            .unwrap()
        );
    }

    #[test]
    fn structural_fragments_keep_their_exact_root_and_environment() {
        let local = ReflectedVariable::live("A".into(), 0);
        let root = ReflectedType::from_parts(
            EvalRef::new(Type::Product {
                left: Box::new(type_path("A")),
                right: Box::new(Type::synth_path(
                    vec!["external".into(), "Box".into()],
                    vec![type_path("A")],
                    zero_span(),
                )),
                meta: zero_meta(),
            }),
            None,
            ReflectionEnvironment::default().with("A".into(), local.clone()),
        );
        let left = root.child(TypeChild::Left);
        let argument = root.child(TypeChild::Right).child(TypeChild::Argument(0));
        assert!(semantic_type_equal(&left, &argument).unwrap());
        assert!(semantic_type_equal(&left, &local.as_type()).unwrap());
        assert!(EvalRef::ptr_eq(&left.ty, &argument.ty));
    }
    #[test]
    fn public_type_equality_uses_lexical_and_opened_semantics_in_both_dispatches() {
        let ctx = EvalCtx::new();
        let left = ReflectedVariable::live("A".into(), 0).as_type();
        let right = ReflectedVariable::live("A".into(), 0).as_type();
        let (opened, _) = scheme("A", "B").open_forall().unwrap();
        let (reconstructed, _) = scheme("X", "Y").open_forall().unwrap();
        for (left, right, expected) in [
            (left.clone(), left.clone(), true),
            (left, right, false),
            (opened.as_type(), reconstructed.as_type(), true),
            (scheme("A", "B"), scheme("X", "Y"), true),
        ] {
            let expr = EvalExpr::ReflTypeEqual {
                left: EvalRef::new(EvalExpr::Const(Value::ReflType(left.clone()))),
                right: EvalRef::new(EvalExpr::Const(Value::ReflType(right.clone()))),
            };
            let direct = eval_compiled_expr(&expr, &mut EvalSlots::root(Vec::new()), &ctx);
            let generic = apply(
                Value::Primitive(ComptimeBuiltin::TypeEqual),
                vec![Value::ReflType(left), Value::ReflType(right)],
                &ctx,
            );
            assert!(matches!(direct, Value::Bool(actual) if actual == expected));
            assert!(matches!(generic, Value::Bool(actual) if actual == expected));
        }
        let neutral = ReflectedVariable::neutral("T".into(), 0).as_type();
        let known = ReflectedType::new_trusted(Type::Unit { meta: zero_meta() });
        assert!(matches!(
            apply(
                Value::Primitive(ComptimeBuiltin::TypeEqual),
                vec![
                    Value::ReflType(neutral.clone()),
                    Value::ReflType(neutral.clone())
                ],
                &ctx,
            ),
            Value::Bool(true)
        ));
        assert!(matches!(
            apply(
                Value::Primitive(ComptimeBuiltin::TypeEqual),
                vec![Value::ReflType(neutral), Value::ReflType(known)],
                &ctx,
            ),
            Value::Stuck(_, _)
        ));
    }

    #[test]
    fn public_type_equality_faults_on_incomplete_owner_even_when_discarded() {
        let unknown = ReflectedVariable {
            name: "A".into(),
            arity: 0,
            meaning: VariableMeaning::Opened(EvalRef::new(OpenedScheme::Incomplete)),
        }
        .as_type();
        let ctx = EvalCtx::new();
        for direct in [true, false] {
            let operation = ctx.enter_eval_operation();
            let value = if direct {
                let expr = EvalExpr::ReflTypeEqual {
                    left: EvalRef::new(EvalExpr::Const(Value::ReflType(unknown.clone()))),
                    right: EvalRef::new(EvalExpr::Const(Value::ReflType(unknown.clone()))),
                };
                eval_compiled_expr(&expr, &mut EvalSlots::root(Vec::new()), &ctx)
            } else {
                apply(
                    Value::Primitive(ComptimeBuiltin::TypeEqual),
                    vec![
                        Value::ReflType(unknown.clone()),
                        Value::ReflType(unknown.clone()),
                    ],
                    &ctx,
                )
            };
            assert!(matches!(
                comptime_fault(&value, &ctx),
                Some(ComptimeFault::IncompleteObservation(
                    ComptimeBuiltin::TypeEqual
                ))
            ));
            assert!(!exact_call_memo_result_shareable(&value));
            drop(value);
            assert!(matches!(
                comptime_fault(&operation.finish(Value::Unit), &ctx),
                Some(ComptimeFault::IncompleteObservation(
                    ComptimeBuiltin::TypeEqual
                ))
            ));
        }
        let operation = ctx.enter_eval_operation();
        assert!(matches!(operation.finish(Value::Unit), Value::Unit));
    }
    #[test]
    fn neutral_type_equality_reduces_known_outer_mismatches_without_sampling() {
        let ctx = EvalCtx::new();
        let a = ReflectedVariable::neutral("A".into(), 0).as_type();
        let b = ReflectedVariable::neutral("B".into(), 0).as_type();
        let unit = ReflectedType::new_trusted(Type::Unit { meta: zero_meta() });
        let bottom = ReflectedType::new_trusted(Type::Bottom { meta: zero_meta() });
        let binary = |builtin, left: &ReflectedType, right: &ReflectedType| {
            let Value::ReflType(ty) = reflection_binary_type_value(
                builtin,
                &Value::ReflType(left.clone()),
                &Value::ReflType(right.clone()),
            )
            .unwrap() else {
                unreachable!()
            };
            ty
        };
        let named = |name: &str, argument: &ReflectedType| {
            ReflectedType::from_parts(
                EvalRef::new(Type::synth_path(
                    vec!["known".into(), name.into()],
                    vec![type_traversal::clone_type(argument.as_type())],
                    zero_span(),
                )),
                None,
                argument.environment.clone(),
            )
        };
        let head = ReflectedVariable::neutral("F".into(), 1).as_type();
        let application = |argument: &ReflectedType| {
            let Value::ReflType(ty) = reflection_type_apply_value(
                &Value::ReflType(head.clone()),
                &Value::ReflType(argument.clone()),
            )
            .unwrap() else {
                unreachable!()
            };
            ty
        };
        for (left, right, expected) in [
            (
                binary(ComptimeBuiltin::TypeProduct, &a, &unit),
                unit.clone(),
                Some(false),
            ),
            (
                binary(ComptimeBuiltin::TypeArrow, &a, &unit),
                unit.clone(),
                Some(false),
            ),
            (named("Box", &a), named("Other", &b), Some(false)),
            (
                binary(ComptimeBuiltin::TypeProduct, &a, &unit),
                binary(ComptimeBuiltin::TypeProduct, &b, &bottom),
                Some(false),
            ),
            (
                binary(ComptimeBuiltin::TypeProduct, &a, &unit),
                binary(ComptimeBuiltin::TypeProduct, &unit, &unit),
                None,
            ),
            (application(&unit), application(&unit), Some(true)),
            (application(&unit), application(&bottom), None),
        ] {
            let expr = EvalExpr::ReflTypeEqual {
                left: EvalRef::new(EvalExpr::Const(Value::ReflType(left.clone()))),
                right: EvalRef::new(EvalExpr::Const(Value::ReflType(right.clone()))),
            };
            for result in [
                eval_compiled_expr(&expr, &mut EvalSlots::root(Vec::new()), &ctx),
                apply(
                    Value::Primitive(ComptimeBuiltin::TypeEqual),
                    vec![Value::ReflType(left), Value::ReflType(right)],
                    &ctx,
                ),
            ] {
                match expected {
                    Some(expected) => {
                        assert!(matches!(result, Value::Bool(actual) if actual == expected))
                    }
                    None => assert!(matches!(result, Value::Stuck(_, _))),
                }
            }
        }
    }
}
