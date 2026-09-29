use super::*;
use std::ops::Deref;
#[cfg(test)]
use std::sync::Weak;

/// Shared evaluator storage whose final owner releases recursive children
/// without using the native call stack.
pub struct EvalRef<T>(Arc<Payload<T>>);

struct Payload<T> {
    value: Option<T>,
    release: fn(T),
}

/// A transient reconstruction result with one owner and iterative unwind release.
pub(super) struct Owned<T: Reclaim>(Option<T>);

impl<T: Reclaim> Owned<T> {
    pub(super) fn new(value: T) -> Self {
        Self(Some(value))
    }
    pub(super) fn take(mut self) -> T {
        self.0.take().expect("live reconstruction result")
    }
}

impl<T: Reclaim> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.as_ref().expect("live reconstruction result")
    }
}

impl<T: Reclaim> std::ops::DerefMut for Owned<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0.as_mut().expect("live reconstruction result")
    }
}

impl<T: Reclaim> Drop for Owned<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            release(value);
        }
    }
}

#[cfg(feature = "surface")]
impl<T: Clone> Clone for Payload<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            release: self.release,
        }
    }
}

impl<T> Drop for Payload<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            (self.release)(value);
        }
    }
}

impl<T> Clone for EvalRef<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Deref for EvalRef<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.0.value.as_ref().expect("live evaluator owner")
    }
}

impl<T> AsRef<T> for EvalRef<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for EvalRef<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(formatter)
    }
}

impl<T: PartialEq> PartialEq for EvalRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl<T: Eq> Eq for EvalRef<T> {}

impl<T> EvalRef<T> {
    #[cfg(feature = "surface")]
    pub(crate) fn make_mut(value: &mut Self) -> &mut T
    where
        T: Clone,
    {
        Arc::make_mut(&mut value.0)
            .value
            .as_mut()
            .expect("live evaluator owner")
    }

    #[cfg(test)]
    pub(crate) fn get_mut(value: &mut Self) -> Option<&mut T> {
        Arc::get_mut(&mut value.0).and_then(|payload| payload.value.as_mut())
    }

    pub(crate) fn unwrap_or_clone(self) -> T
    where
        T: Clone,
    {
        self.try_unwrap().unwrap_or_else(|value| (*value).clone())
    }

    pub(crate) fn into_inner(self) -> Option<T> {
        // Consuming extraction assigns the final release to exactly one owner,
        // including when other threads release their references concurrently.
        Arc::into_inner(self.0).and_then(|mut payload| payload.value.take())
    }

    pub(crate) fn try_unwrap(self) -> Result<T, Self> {
        Arc::try_unwrap(self.0)
            .map(|mut payload| payload.value.take().expect("live evaluator owner"))
            .map_err(Self)
    }

    #[cfg(any(feature = "surface", test))]
    pub(crate) fn ptr_eq(left: &Self, right: &Self) -> bool {
        Arc::ptr_eq(&left.0, &right.0)
    }

    #[cfg(feature = "surface")]
    pub(crate) fn as_ptr(value: &Self) -> *const T {
        std::ptr::from_ref(value.deref())
    }

    #[cfg(test)]
    pub(crate) fn downgrade(&self) -> EvalWeak<T> {
        EvalWeak(Arc::downgrade(&self.0))
    }
}

#[cfg(test)]
pub(crate) struct EvalWeak<T>(Weak<Payload<T>>);

#[cfg(test)]
impl<T> EvalWeak<T> {
    pub(crate) fn strong_count(&self) -> usize {
        self.0.strong_count()
    }

    pub(crate) fn upgrade(&self) -> Option<EvalRef<T>> {
        self.0.upgrade().map(EvalRef)
    }
}

impl<T> EvalRef<T> {
    pub(crate) fn new(value: T) -> Self
    where
        T: Reclaim,
    {
        Self(Arc::new(Payload {
            value: Some(value),
            release: release::<T>,
        }))
    }
}

impl<T: Reclaim> From<T> for EvalRef<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub(crate) trait Reclaim: Sized + Into<Node> {
    fn reclaim(self, pending: &mut Vec<Node>);

    fn enqueue(self, pending: &mut Vec<Node>) {
        pending.push(self.into());
    }
}

pub(crate) struct Node(NodeKind);

enum NodeKind {
    Value(Value),
    Values(Vec<Value>),
    Data(DataValue),
    Function(EvalFunction),
    Code(EvalExpr),
    Checked(CheckedTerm),
    CheckedNode(CheckedTermNode),
    Env(EnvFrame),
    Type(EvalAstType),
    Source(EvalAstExpr),
    Kind(crate::ast::Kind),
    ReplayType(Type<PrePrime>),
    ReplayExpr(Expr<PrePrime>),
    ReplaySignature(Signature<PrePrime>),
    #[cfg(feature = "surface")]
    Projected(crate::pass::typecheck_core::apply::fills::ProjectedRecipeNode),
    #[cfg(feature = "surface")]
    Producers(crate::pass::typecheck_core::apply::fills::ProducerDag),
    #[cfg(feature = "surface")]
    Fills(crate::pass::typecheck_core::apply::fills::FillRelation),
}

fn release<T: Reclaim>(value: T) {
    let mut pending = Vec::new();
    value.reclaim(&mut pending);
    while let Some(Node(node)) = pending.pop() {
        match node {
            NodeKind::Value(value) => value.reclaim(&mut pending),
            NodeKind::Values(values) => values.reclaim(&mut pending),
            NodeKind::Data(data) => data.reclaim(&mut pending),
            NodeKind::Function(function) => function.reclaim(&mut pending),
            NodeKind::Code(code) => code.reclaim(&mut pending),
            NodeKind::Checked(term) => term.reclaim(&mut pending),
            NodeKind::CheckedNode(node) => node.reclaim(&mut pending),
            NodeKind::Env(env) => env.reclaim(&mut pending),
            NodeKind::Type(ty) => ty.reclaim(&mut pending),
            NodeKind::Source(expr) => expr.reclaim(&mut pending),
            NodeKind::Kind(kind) => kind.reclaim(&mut pending),
            NodeKind::ReplayType(ty) => ty.reclaim(&mut pending),
            NodeKind::ReplayExpr(expr) => expr.reclaim(&mut pending),
            NodeKind::ReplaySignature(sig) => sig.reclaim(&mut pending),
            #[cfg(feature = "surface")]
            NodeKind::Projected(recipe) => recipe.reclaim(&mut pending),
            #[cfg(feature = "surface")]
            NodeKind::Producers(producers) => producers.reclaim(&mut pending),
            #[cfg(feature = "surface")]
            NodeKind::Fills(relation) => relation.reclaim(&mut pending),
        }
    }
}

macro_rules! node {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for Node {
            fn from(value: $ty) -> Self {
                Self(NodeKind::$variant(value))
            }
        }
    };
}

node!(Value, Value);
node!(Vec<Value>, Values);
node!(DataValue, Data);
node!(EvalFunction, Function);
node!(EvalExpr, Code);
node!(CheckedTerm, Checked);
node!(CheckedTermNode, CheckedNode);
node!(EnvFrame, Env);
node!(EvalAstType, Type);
node!(EvalAstExpr, Source);
node!(crate::ast::Kind, Kind);
node!(Type<PrePrime>, ReplayType);
node!(Expr<PrePrime>, ReplayExpr);
node!(Signature<PrePrime>, ReplaySignature);
#[cfg(feature = "surface")]
node!(
    crate::pass::typecheck_core::apply::fills::ProjectedRecipeNode,
    Projected
);
#[cfg(feature = "surface")]
node!(
    crate::pass::typecheck_core::apply::fills::ProducerDag,
    Producers
);
#[cfg(feature = "surface")]
node!(
    crate::pass::typecheck_core::apply::fills::FillRelation,
    Fills
);

pub(crate) fn shared<T: Reclaim>(value: EvalRef<T>, pending: &mut Vec<Node>) {
    if let Some(value) = value.into_inner() {
        value.enqueue(pending);
    }
}

impl Reclaim for Vec<Value> {
    fn reclaim(self, pending: &mut Vec<Node>) {
        for value in self {
            value.enqueue(pending);
        }
    }

    fn enqueue(self, pending: &mut Vec<Node>) {
        self.reclaim(pending);
    }
}

impl Reclaim for Value {
    fn enqueue(self, pending: &mut Vec<Node>) {
        match self {
            Value::Unit
            | Value::Bool(_)
            | Value::IntAtom(..)
            | Value::FloatAtom(..)
            | Value::StrAtom(_)
            | Value::ReflTypeArity(_)
            | Value::ReflTypeVar { .. }
            | Value::ReflTypeName { .. }
            | Value::DiagnosticText(_)
            | Value::Primitive(_)
            | Value::NewtypeMember(_)
            | Value::Atom(_) => {}
            value => pending.push(value.into()),
        }
    }

    fn reclaim(self, pending: &mut Vec<Node>) {
        match self {
            Value::ReflType(ty) => shared(ty.ty, pending),
            Value::CheckedTerm(term) => shared(term, pending),
            Value::EvalClosure { function, captured } => {
                shared(function, pending);
                shared(captured, pending);
            }
            Value::StructuralRecur { step, .. } => shared(step, pending),
            Value::Data(data) => shared(data, pending),
            Value::CaseSplit {
                scrutinee,
                left,
                right,
                ..
            } => {
                shared(scrutinee, pending);
                shared(left, pending);
                shared(right, pending);
            }
            Value::Seq { value, body } => {
                shared(value, pending);
                shared(body, pending);
            }
            Value::Stuck(callee, args) => {
                shared(callee, pending);
                shared(args, pending);
            }
            Value::Unit
            | Value::Bool(_)
            | Value::IntAtom(..)
            | Value::FloatAtom(..)
            | Value::StrAtom(_)
            | Value::ReflTypeArity(_)
            | Value::ReflTypeVar { .. }
            | Value::ReflTypeName { .. }
            | Value::DiagnosticText(_)
            | Value::Primitive(_)
            | Value::NewtypeMember(_)
            | Value::Atom(_) => {}
            #[cfg(feature = "surface")]
            Value::Projected(_) => {}
        }
    }
}

impl Reclaim for DataValue {
    fn reclaim(self, pending: &mut Vec<Node>) {
        shared(self.args, pending);
        #[cfg(feature = "surface")]
        if let DataConstructorIdentity::FillContext(context) = self.identity {
            context.release_children(pending);
        }
    }
}

impl Reclaim for EvalFunction {
    fn reclaim(self, pending: &mut Vec<Node>) {
        shared(self.body, pending);
        shared(self.diagnostic_body, pending);
    }
}

impl Reclaim for CheckedTerm {
    fn reclaim(self, pending: &mut Vec<Node>) {
        shared(self.node, pending);
        shared(self.ty.ty, pending);
    }
}

impl Reclaim for EnvFrame {
    fn reclaim(self, pending: &mut Vec<Node>) {
        if let Some(parent) = self.parent {
            shared(parent.frame, pending);
        }
        match self.bindings {
            EnvBindings::Empty => {}
            EnvBindings::One(_, value) => pending.push(value.into()),
        }
    }
}

impl Reclaim for EvalExpr {
    fn enqueue(self, pending: &mut Vec<Node>) {
        match self {
            Self::Unit
            | Self::Bool(_)
            | Self::IntAtom(..)
            | Self::FloatAtom(..)
            | Self::StrAtom(_)
            | Self::DiagnosticText(_)
            | Self::Primitive(_)
            | Self::Local(_)
            | Self::Atom(_)
            | Self::NewtypeMember(_)
            | Self::ReflTypeUnit
            | Self::ReflTypeBottom
            | Self::ReflTermUnit => {}
            Self::Const(value) => value.enqueue(pending),
            code => pending.push(code.into()),
        }
    }

    fn reclaim(self, pending: &mut Vec<Node>) {
        match self {
            Self::Unit => {}
            Self::Bool(..) => {}
            Self::IntAtom(..) => {}
            Self::FloatAtom(..) => {}
            Self::StrAtom(..) => {}
            Self::DiagnosticText(..) => {}
            Self::Primitive(..) => {}
            Self::StructuralRecurPrimitive(value) => {
                shared(value, pending);
            }
            Self::Const(value) => {
                pending.push(value.into());
            }
            Self::Local(..) => {}
            Self::Atom(..) => {}
            Self::FnRef { function, .. } => {
                shared(function, pending);
            }
            Self::FnExpr {
                function,
                empty_closure_cache,
                ..
            } => {
                shared(function, pending);
                if let Some(cache) = Arc::into_inner(empty_closure_cache)
                    && let Some(value) = cache.into_inner()
                {
                    pending.push(value.into());
                }
            }
            Self::NewtypeMember(..) => {}
            Self::Let { value, body } => {
                shared(value, pending);
                shared(body, pending);
            }
            Self::Seq { value, body } => {
                shared(value, pending);
                shared(body, pending);
            }
            Self::Call { callee, args } => {
                shared(callee, pending);
                for child in args {
                    shared(child, pending);
                }
            }
            Self::CallAtom { args, .. } => {
                for child in args {
                    shared(child, pending);
                }
            }
            Self::CallPrimitive { args, .. } => {
                for child in args {
                    shared(child, pending);
                }
            }
            Self::ProofedReflection { proof, direct, .. } => {
                shared(proof, pending);
                shared(direct, pending);
            }
            Self::CallFn { function, args } => {
                shared(function, pending);
                for child in args {
                    shared(child, pending);
                }
            }
            Self::CallFnExpr { function, args, .. } => {
                shared(function, pending);
                for child in args {
                    shared(child, pending);
                }
            }
            Self::NewtypeCtor { value, .. } => {
                shared(value, pending);
            }
            Self::NewtypeProj { value, .. } => {
                shared(value, pending);
            }
            Self::CoreCtor { args, .. } => {
                for child in args {
                    shared(child, pending);
                }
            }
            Self::CoreProj { pair, .. } => {
                shared(pair, pending);
            }
            Self::CoreIf {
                condition,
                then_thunk,
                else_thunk,
            } => {
                shared(condition, pending);
                shared(then_thunk, pending);
                shared(else_thunk, pending);
            }
            Self::CoreEither {
                scrutinee,
                left,
                right,
            } => {
                shared(scrutinee, pending);
                shared(left, pending);
                shared(right, pending);
            }
            Self::CoreEitherThunk {
                scrutinee,
                left_thunk,
                right_thunk,
            } => {
                shared(scrutinee, pending);
                shared(left_thunk, pending);
                shared(right_thunk, pending);
            }
            Self::ReflTypeUnit => {}
            Self::ReflTypeBottom => {}
            Self::ReflTypeProduct { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::ReflTypeSum { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::ReflTypeView { typ } => {
                shared(typ, pending);
            }
            Self::ReflTypeEqual { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::ReflTypeNameEqual { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::ReflTypeVarEqual { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::ReflTypeArityEqual { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::ReflTypeInstantiate { scheme, arg } => {
                shared(scheme, pending);
                shared(arg, pending);
            }
            Self::ReflTypeRootPredicate { typ, .. } => {
                shared(typ, pending);
            }
            Self::ReflTypeFold {
                typ, init, step, ..
            } => {
                shared(typ, pending);
                shared(init, pending);
                shared(step, pending);
            }
            Self::ReflTermType { term } => {
                shared(term, pending);
            }
            Self::ReflTermUnit => {}
            Self::ReflTermLet {
                value_type,
                value,
                body,
            } => {
                shared(value_type, pending);
                shared(value, pending);
                shared(body, pending);
            }
            Self::ReflTermFn { fn_type, body } => {
                shared(fn_type, pending);
                shared(body, pending);
            }
            Self::ReflTermCall {
                fn_type,
                fn_value,
                arg_packet,
            } => {
                shared(fn_type, pending);
                shared(fn_value, pending);
                shared(arg_packet, pending);
            }
            Self::ReflTermTypeFn { arity, body } => {
                shared(arity, pending);
                shared(body, pending);
            }
            Self::ReflTermTypeApp { fn_value, arg } => {
                shared(fn_value, pending);
                shared(arg, pending);
            }
            Self::ReflIntrinsicPair { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::ReflIntrinsicProjection {
                product_type,
                value,
                ..
            } => {
                shared(product_type, pending);
                shared(value, pending);
            }
            Self::ReflIntrinsicInjection {
                sum_type, value, ..
            } => {
                shared(sum_type, pending);
                shared(value, pending);
            }
            Self::ReflIntrinsicEither {
                sum_type,
                result_type,
                value,
                on_left,
                on_right,
            } => {
                shared(sum_type, pending);
                shared(result_type, pending);
                shared(value, pending);
                shared(on_left, pending);
                shared(on_right, pending);
            }
            Self::ReflIntrinsicAbsurd {
                bottom_value,
                result_type,
            } => {
                shared(bottom_value, pending);
                shared(result_type, pending);
            }
            Self::ReflIntrinsicIfThenElse {
                result_type,
                condition,
                on_true,
                on_false,
            } => {
                shared(result_type, pending);
                shared(condition, pending);
                shared(on_true, pending);
                shared(on_false, pending);
            }
        }
    }
}

impl Reclaim for CheckedTermNode {
    fn reclaim(self, pending: &mut Vec<Node>) {
        match self {
            Self::Raw(value) => {
                pending.push(value.into());
            }
            Self::TemplateValue { .. } => {}
            Self::Local { .. } => {}
            Self::TermLet { value, body, .. } => {
                shared(value, pending);
                shared(body, pending);
            }
            Self::TermFn { sig, body } => {
                signature(sig, pending);
                shared(body, pending);
            }
            Self::TermCall {
                fn_value,
                params,
                arg_packet,
            } => {
                shared(fn_value, pending);
                pending.extend(params.into_iter().map(Node::from));
                shared(arg_packet, pending);
            }
            Self::TermTypeApp { fn_value, arg } => {
                shared(fn_value, pending);
                pending.push(arg.into());
            }
            Self::IntrinsicPair { left, right } => {
                shared(left, pending);
                shared(right, pending);
            }
            Self::IntrinsicProjection {
                left_ty,
                right_ty,
                value,
                ..
            } => {
                pending.push(left_ty.into());
                pending.push(right_ty.into());
                shared(value, pending);
            }
            Self::IntrinsicInjection {
                left_ty,
                right_ty,
                value,
                ..
            } => {
                pending.push(left_ty.into());
                pending.push(right_ty.into());
                shared(value, pending);
            }
            Self::IntrinsicEither {
                left_ty,
                right_ty,
                result_ty,
                value,
                left_body,
                right_body,
                ..
            } => {
                pending.push(left_ty.into());
                pending.push(right_ty.into());
                pending.push(result_ty.into());
                shared(value, pending);
                shared(left_body, pending);
                shared(right_body, pending);
            }
            Self::IntrinsicAbsurd {
                result_ty,
                bottom_value,
            } => {
                pending.push(result_ty.into());
                shared(bottom_value, pending);
            }
            Self::IntrinsicIfThenElse {
                result_ty,
                condition,
                true_body,
                false_body,
            } => {
                pending.push(result_ty.into());
                shared(condition, pending);
                shared(true_body, pending);
                shared(false_body, pending);
            }
            Self::ElabError { .. } => {}
        }
    }
}

fn signature<P: crate::ast::Phase>(sig: Signature<P>, pending: &mut Vec<Node>)
where
    Type<P>: Reclaim,
{
    for param in sig.params {
        match param {
            SignatureParam::Type(param) => pending.extend(param.kind.map(Node::from)),
            SignatureParam::Value(param) => pending.extend(param.ty.map(Into::into)),
        }
    }
}

impl Reclaim for crate::ast::Kind {
    fn reclaim(self, pending: &mut Vec<Node>) {
        if let Self::Arrow(left, right) = self {
            pending.push((*left).into());
            pending.push((*right).into());
        }
    }
}

impl Reclaim for Signature<PrePrime> {
    fn reclaim(self, pending: &mut Vec<Node>) {
        signature(self, pending);
    }
}

macro_rules! reclaim_ast {
    ($phase:ty, $annotation:expr) => {
        impl Reclaim for Type<$phase> {
            fn reclaim(self, pending: &mut Vec<Node>) {
                match self {
                    Type::Path { args, .. } => pending.extend(args.into_iter().map(Node::from)),
                    Type::Unit { .. } | Type::Bottom { .. } => {}
                    Type::Function { param, ret, .. } => {
                        pending.push((*param).into());
                        pending.push((*ret).into());
                    }
                    Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                        pending.push((*left).into());
                        pending.push((*right).into());
                    }
                    Type::Forall { param, body, .. } => {
                        pending.extend(param.kind.map(Node::from));
                        pending.push((*body).into());
                    }
                    Type::LabelSugar { ext, .. }
                    | Type::Infer { ext, .. }
                    | Type::Goal { ext, .. } => match ext {},
                }
            }
        }

        impl Reclaim for Expr<$phase> {
            fn reclaim(self, pending: &mut Vec<Node>) {
                match self {
                    Expr::Path { .. } | Expr::Unit { .. } => {}
                    Expr::Call { callee, args, .. } => {
                        pending.push((*callee).into());
                        for arg in args {
                            pending.push(match arg {
                                CallArg::Value(value) => value.into(),
                                CallArg::Type(ty) => ty.into(),
                            });
                        }
                    }
                    Expr::FnExpr {
                        sig, ret_ty, body, ..
                    } => {
                        signature(sig, pending);
                        pending.extend(ret_ty.map(Node::from));
                        pending.push((*body).into());
                    }
                    Expr::Let {
                        ty, value, body, ..
                    } => {
                        pending.extend(ty.map(Node::from));
                        pending.push((*value).into());
                        pending.push((*body).into());
                    }
                    Expr::Seq { value, body, .. } => {
                        pending.push((*value).into());
                        pending.push((*body).into());
                    }
                    Expr::StrLit { annotation, .. }
                    | Expr::IntLit { annotation, .. }
                    | Expr::FloatLit { annotation, .. }
                    | Expr::BoolLit { annotation, .. } => {
                        pending.extend(($annotation)(annotation).map(Node::from));
                    }
                    Expr::RecCall { ext, .. }
                    | Expr::RowLet { ext, .. }
                    | Expr::Tuple { ext, .. }
                    | Expr::FnPlaceholder { ext, .. }
                    | Expr::LabelValue { ext, .. }
                    | Expr::Elaborator { ext, .. }
                    | Expr::RecOrder { ext, .. }
                    | Expr::RecQuote { ext, .. }
                    | Expr::UserElaborator { ext, .. }
                    | Expr::Ufcs { ext, .. }
                    | Expr::OpChain { ext, .. }
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
                    | Expr::LowModuleFnValueRef { ext, .. } => match ext {},
                }
            }
        }
    };
}
reclaim_ast!(EvalPhase, |annotation: Option<EvalAstType>| annotation);
reclaim_ast!(PrePrime, Some);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalization::tests::test_eval_function;

    fn deep_value(depth: usize) -> Value {
        let function = test_eval_function(&[], EvalExpr::Unit);
        let mut value = Value::Unit;
        for index in 0..depth {
            value = match index % 6 {
                0 => data_value("__left__", vec![value]),
                1 => Value::Seq {
                    value: EvalRef::new(Value::Unit),
                    body: EvalRef::new(value),
                },
                2 => Value::Stuck(
                    EvalRef::new(Value::Atom("f".to_owned())),
                    vec![value].into(),
                ),
                3 => Value::CaseSplit {
                    scrutinee: EvalRef::new(Value::Atom("sum".to_owned())),
                    left_payload: "left".to_owned(),
                    left: EvalRef::new(value),
                    right_payload: "right".to_owned(),
                    right: EvalRef::new(Value::Unit),
                },
                4 => Value::StructuralRecur {
                    root_measure: depth + 1,
                    current_measure: index + 1,
                    step: EvalRef::new(value),
                },
                _ => Value::EvalClosure {
                    function: function.clone(),
                    captured: vec![value].into(),
                },
            };
        }
        value
    }

    #[test]
    fn escaping_values_release_on_the_default_stack() {
        let (value, weak) = std::thread::spawn(|| {
            let ctx = EvalCtx::new();
            let value = EvalRef::new(deep_value(30_000));
            let weak = value.downgrade();
            drop(ctx);
            (Value::Stuck(value, Vec::new().into()), weak)
        })
        .join()
        .expect("value construction completes on the default stack");
        let retained = value.clone();
        drop(value);
        assert!(weak.upgrade().is_some());
        drop(retained);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn concurrent_final_owners_and_weak_upgrades_release_the_graph() {
        let value = EvalRef::new(deep_value(30_000));
        let weak = value.downgrade();
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let value = EvalRef::new(Value::Seq {
                value: value.clone(),
                body: EvalRef::new(Value::Unit),
            });
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                drop(value);
            }));
        }
        drop(value);
        barrier.wait();
        for _ in 0..2_000 {
            drop(weak.upgrade());
        }
        for worker in workers {
            worker.join().expect("final-owner release completes");
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(weak.strong_count(), 0);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn copy_on_write_preserves_shared_children_and_dissociates_weak_owners() {
        let child = EvalRef::new(deep_value(30_000));
        let child_weak = child.downgrade();
        let mut values = EvalRef::new(vec![Value::Stuck(child, Vec::new().into())]);
        let retained = values.clone();
        let original_weak = values.downgrade();
        EvalRef::make_mut(&mut values).push(Value::Unit);
        assert_eq!(retained.len(), 1);
        assert_eq!(values.len(), 2);
        drop(retained);
        assert!(original_weak.upgrade().is_none());
        let moved_weak = values.downgrade();
        EvalRef::make_mut(&mut values).push(Value::Unit);
        assert!(moved_weak.upgrade().is_none());
        assert!(child_weak.upgrade().is_some());
        drop(values);
        assert!(child_weak.upgrade().is_none());
    }

    #[test]
    fn code_checked_terms_types_and_environments_release_on_the_default_stack() {
        std::thread::spawn(|| {
            let template = test_eval_function(&[], EvalExpr::Unit);
            let mut function = template.clone();
            let unit = EvalRef::new(CheckedTerm::new(
                Expr::Unit {
                    occurrence: Default::default(),
                    meta: zero_meta(),
                },
                Type::Unit { meta: zero_meta() },
            ));
            let mut term = unit.clone();
            let mut source = Expr::Unit {
                occurrence: Default::default(),
                meta: zero_meta(),
            };
            let mut ty = Type::Unit { meta: zero_meta() };
            let mut env = Env::new();
            for _ in 0..20_000 {
                let mut next = (*template).clone();
                next.body = EvalRef::new(EvalExpr::Const(Value::EvalClosure {
                    function,
                    captured: Vec::new().into(),
                }));
                function = EvalRef::new(next);
                term = EvalRef::new(CheckedTerm::from_node(
                    CheckedTermNode::TermLet {
                        name: "x".to_owned(),
                        value: unit.clone(),
                        body: term,
                    },
                    Type::Unit { meta: zero_meta() },
                ));
                source = Expr::Seq {
                    occurrence: Default::default(),
                    value: Box::new(Expr::Unit {
                        occurrence: Default::default(),
                        meta: zero_meta(),
                    }),
                    body: Box::new(source),
                    meta: zero_meta(),
                };
                ty = Type::Product {
                    left: Box::new(Type::Unit { meta: zero_meta() }),
                    right: Box::new(ty),
                    meta: zero_meta(),
                };
                env.insert("x".to_owned(), Value::Unit);
            }
            let weak_function = function.downgrade();
            let weak_term = term.downgrade();
            let weak_env = env.frame.downgrade();
            drop((function, term, env, EvalRef::new(source), EvalRef::new(ty)));
            assert!(weak_function.upgrade().is_none());
            assert!(weak_term.upgrade().is_none());
            assert!(weak_env.upgrade().is_none());
        })
        .join()
        .expect("all evaluator-owned graphs release on the default stack");
    }

    #[test]
    fn escaping_owners_release_during_unwind_and_context_remains_reusable() {
        let ctx = EvalCtx::new();
        let value = EvalRef::new(deep_value(30_000));
        let weak = value.downgrade();
        let result = std::panic::catch_unwind(move || {
            let _owned = value;
            panic!("ownership unwind witness");
        });
        assert!(result.is_err());
        assert!(weak.upgrade().is_none());
        assert!(matches!(apply(Value::Unit, Vec::new(), &ctx), Value::Unit));
    }
}
