#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ComptimeBuiltin {
    ComptimeProof,
    Type,
    CheckedTerm,
    TypeVar,
    TypeName,
    TypeArity,
    DiagnosticText,
    ComptimeBool,
    ComptimeStr,
    ReflectType,
    TypeView,
    TypeIsHost,
    TypeHostConverters,
    TypeEqual,
    TypeNameEqual,
    TypeVarEqual,
    TypeIsProductRoot,
    TypeIsSumRoot,
    TypeUnit,
    TypeBottom,
    TypeProduct,
    TypeSum,
    TypeArrow,
    TypeForall,
    TypeVarFn,
    TypeNameType,
    TypeApply,
    TypeInstantiate,
    TypeArityFn,
    TypeVarArity,
    TypeArityZero,
    TypeAritySucc,
    TypeArityEqual,
    TypeProductSpineFold,
    TypeSumSpineFold,
    TypeArgsFold,
    TypeFunctionParamsFold,
    TypeArityFold,
    TypeNameParamAritiesFold,
    TypeDisplay,
    TypeShortName,
    TypeNameDisplay,
    TypeVarDisplay,
    TypeArityDisplay,
    DiagnosticConcat,
    ElabError,
    TypeError,
    StructuralRecur,
    TermType,
    TermLet,
    TermFn,
    TermCall,
    TermTypeFn,
    TermTypeApp,
    TermUnit,
    TermHostConvert,
    IntrinsicPair,
    IntrinsicFst,
    IntrinsicSnd,
    IntrinsicLeft,
    IntrinsicRight,
    IntrinsicEither,
    IntrinsicAbsurd,
    IntrinsicIfThenElse,
    FillCtx,
    Fill,
    TermSpecialize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComptimeRuntimeErasure {
    Bottom,
    Unit,
}

impl ComptimeBuiltin {
    pub fn from_public_name(name: &str) -> Option<Self> {
        Some(match name {
            "__Comptime__" => Self::ComptimeProof,
            "__Type__" => Self::Type,
            "__Checked_term__" => Self::CheckedTerm,
            "__Type_var__" => Self::TypeVar,
            "__Type_name__" => Self::TypeName,
            "__Type_arity__" => Self::TypeArity,
            "__Diagnostic_text__" => Self::DiagnosticText,
            "Comptime_bool" => Self::ComptimeBool,
            "Comptime_str" => Self::ComptimeStr,
            "__reflect_type__" => Self::ReflectType,
            "__type_view__" => Self::TypeView,
            "__type_is_host__" => Self::TypeIsHost,
            "__type_host_converters__" => Self::TypeHostConverters,
            "__type_equal__" => Self::TypeEqual,
            "__type_name_equal__" => Self::TypeNameEqual,
            "__type_var_equal__" => Self::TypeVarEqual,
            "__type_is_product_root__" => Self::TypeIsProductRoot,
            "__type_is_sum_root__" => Self::TypeIsSumRoot,
            "__type_unit__" => Self::TypeUnit,
            "__type_bottom__" => Self::TypeBottom,
            "__type_product__" => Self::TypeProduct,
            "__type_sum__" => Self::TypeSum,
            "__type_arrow__" => Self::TypeArrow,
            "__type_forall__" => Self::TypeForall,
            "__type_var__" => Self::TypeVarFn,
            "__type_name_type__" => Self::TypeNameType,
            "__type_apply__" => Self::TypeApply,
            "__type_instantiate__" => Self::TypeInstantiate,
            "__type_arity__" => Self::TypeArityFn,
            "__type_var_arity__" => Self::TypeVarArity,
            "__type_arity_zero__" => Self::TypeArityZero,
            "__type_arity_succ__" => Self::TypeAritySucc,
            "__type_arity_equal__" => Self::TypeArityEqual,
            "__type_product_spine_fold__" => Self::TypeProductSpineFold,
            "__type_sum_spine_fold__" => Self::TypeSumSpineFold,
            "__type_args_fold__" => Self::TypeArgsFold,
            "__type_function_params_fold__" => Self::TypeFunctionParamsFold,
            "__type_arity_fold__" => Self::TypeArityFold,
            "__type_name_param_arities_fold__" => Self::TypeNameParamAritiesFold,
            "__type_display__" => Self::TypeDisplay,
            "__type_short_name__" => Self::TypeShortName,
            "__type_name_display__" => Self::TypeNameDisplay,
            "__type_var_display__" => Self::TypeVarDisplay,
            "__type_arity_display__" => Self::TypeArityDisplay,
            "__diagnostic_concat__" => Self::DiagnosticConcat,
            "__elab_error__" => Self::ElabError,
            "__type_error__" => Self::TypeError,
            "__structural_recur__" => Self::StructuralRecur,
            "__term_type__" => Self::TermType,
            "__term_let__" => Self::TermLet,
            "__term_fn__" => Self::TermFn,
            "__term_call__" => Self::TermCall,
            "__term_type_fn__" => Self::TermTypeFn,
            "__term_type_app__" => Self::TermTypeApp,
            "__term_unit__" => Self::TermUnit,
            "__term_host_convert__" => Self::TermHostConvert,
            "__intrinsic_pair__" => Self::IntrinsicPair,
            "__intrinsic_fst__" => Self::IntrinsicFst,
            "__intrinsic_snd__" => Self::IntrinsicSnd,
            "__intrinsic_left__" => Self::IntrinsicLeft,
            "__intrinsic_right__" => Self::IntrinsicRight,
            "__intrinsic_either__" => Self::IntrinsicEither,
            "__intrinsic_absurd__" => Self::IntrinsicAbsurd,
            "__intrinsic_if_then_else__" => Self::IntrinsicIfThenElse,
            "__Fill_ctx__" => Self::FillCtx,
            "__fill__" => Self::Fill,
            "__term_specialize__" => Self::TermSpecialize,
            _ => return None,
        })
    }

    pub fn public_name(self) -> &'static str {
        match self {
            Self::ComptimeProof => "__Comptime__",
            Self::Type => "__Type__",
            Self::CheckedTerm => "__Checked_term__",
            Self::TypeVar => "__Type_var__",
            Self::TypeName => "__Type_name__",
            Self::TypeArity => "__Type_arity__",
            Self::DiagnosticText => "__Diagnostic_text__",
            Self::ComptimeBool => "Comptime_bool",
            Self::ComptimeStr => "Comptime_str",
            Self::ReflectType => "__reflect_type__",
            Self::TypeView => "__type_view__",
            Self::TypeIsHost => "__type_is_host__",
            Self::TypeHostConverters => "__type_host_converters__",
            Self::TypeEqual => "__type_equal__",
            Self::TypeNameEqual => "__type_name_equal__",
            Self::TypeVarEqual => "__type_var_equal__",
            Self::TypeIsProductRoot => "__type_is_product_root__",
            Self::TypeIsSumRoot => "__type_is_sum_root__",
            Self::TypeUnit => "__type_unit__",
            Self::TypeBottom => "__type_bottom__",
            Self::TypeProduct => "__type_product__",
            Self::TypeSum => "__type_sum__",
            Self::TypeArrow => "__type_arrow__",
            Self::TypeForall => "__type_forall__",
            Self::TypeVarFn => "__type_var__",
            Self::TypeNameType => "__type_name_type__",
            Self::TypeApply => "__type_apply__",
            Self::TypeInstantiate => "__type_instantiate__",
            Self::TypeArityFn => "__type_arity__",
            Self::TypeVarArity => "__type_var_arity__",
            Self::TypeArityZero => "__type_arity_zero__",
            Self::TypeAritySucc => "__type_arity_succ__",
            Self::TypeArityEqual => "__type_arity_equal__",
            Self::TypeProductSpineFold => "__type_product_spine_fold__",
            Self::TypeSumSpineFold => "__type_sum_spine_fold__",
            Self::TypeArgsFold => "__type_args_fold__",
            Self::TypeFunctionParamsFold => "__type_function_params_fold__",
            Self::TypeArityFold => "__type_arity_fold__",
            Self::TypeNameParamAritiesFold => "__type_name_param_arities_fold__",
            Self::TypeDisplay => "__type_display__",
            Self::TypeShortName => "__type_short_name__",
            Self::TypeNameDisplay => "__type_name_display__",
            Self::TypeVarDisplay => "__type_var_display__",
            Self::TypeArityDisplay => "__type_arity_display__",
            Self::DiagnosticConcat => "__diagnostic_concat__",
            Self::ElabError => "__elab_error__",
            Self::TypeError => "__type_error__",
            Self::StructuralRecur => "__structural_recur__",
            Self::TermType => "__term_type__",
            Self::TermLet => "__term_let__",
            Self::TermFn => "__term_fn__",
            Self::TermCall => "__term_call__",
            Self::TermTypeFn => "__term_type_fn__",
            Self::TermTypeApp => "__term_type_app__",
            Self::TermUnit => "__term_unit__",
            Self::TermHostConvert => "__term_host_convert__",
            Self::IntrinsicPair => "__intrinsic_pair__",
            Self::IntrinsicFst => "__intrinsic_fst__",
            Self::IntrinsicSnd => "__intrinsic_snd__",
            Self::IntrinsicLeft => "__intrinsic_left__",
            Self::IntrinsicRight => "__intrinsic_right__",
            Self::IntrinsicEither => "__intrinsic_either__",
            Self::IntrinsicAbsurd => "__intrinsic_absurd__",
            Self::IntrinsicIfThenElse => "__intrinsic_if_then_else__",
            Self::FillCtx => "__Fill_ctx__",
            Self::Fill => "__fill__",
            Self::TermSpecialize => "__term_specialize__",
        }
    }

    pub fn is_type_name(self) -> bool {
        matches!(
            self,
            Self::ComptimeProof
                | Self::Type
                | Self::CheckedTerm
                | Self::TypeVar
                | Self::TypeName
                | Self::TypeArity
                | Self::DiagnosticText
                | Self::ComptimeBool
                | Self::ComptimeStr
                | Self::FillCtx
        )
    }

    pub fn is_value_name(self) -> bool {
        !self.is_type_name()
    }

    pub fn runtime_erasure(self) -> Option<ComptimeRuntimeErasure> {
        match self {
            Self::ComptimeProof
            | Self::Type
            | Self::CheckedTerm
            | Self::TypeVar
            | Self::TypeName
            | Self::TypeArity
            | Self::DiagnosticText
            | Self::FillCtx => Some(ComptimeRuntimeErasure::Bottom),
            Self::ComptimeBool | Self::ComptimeStr => Some(ComptimeRuntimeErasure::Unit),
            Self::ReflectType
            | Self::TypeView
            | Self::TypeIsHost
            | Self::TypeHostConverters
            | Self::TypeEqual
            | Self::TypeNameEqual
            | Self::TypeVarEqual
            | Self::TypeIsProductRoot
            | Self::TypeIsSumRoot
            | Self::TypeUnit
            | Self::TypeBottom
            | Self::TypeProduct
            | Self::TypeSum
            | Self::TypeArrow
            | Self::TypeForall
            | Self::TypeVarFn
            | Self::TypeNameType
            | Self::TypeApply
            | Self::TypeInstantiate
            | Self::TypeArityFn
            | Self::TypeVarArity
            | Self::TypeArityZero
            | Self::TypeAritySucc
            | Self::TypeArityEqual
            | Self::TypeProductSpineFold
            | Self::TypeSumSpineFold
            | Self::TypeArgsFold
            | Self::TypeFunctionParamsFold
            | Self::TypeArityFold
            | Self::TypeNameParamAritiesFold
            | Self::TypeDisplay
            | Self::TypeShortName
            | Self::TypeNameDisplay
            | Self::TypeVarDisplay
            | Self::TypeArityDisplay
            | Self::DiagnosticConcat
            | Self::ElabError
            | Self::TypeError
            | Self::StructuralRecur
            | Self::TermType
            | Self::TermLet
            | Self::TermFn
            | Self::TermCall
            | Self::TermTypeFn
            | Self::TermTypeApp
            | Self::TermUnit
            | Self::TermHostConvert
            | Self::IntrinsicPair
            | Self::IntrinsicFst
            | Self::IntrinsicSnd
            | Self::IntrinsicLeft
            | Self::IntrinsicRight
            | Self::IntrinsicEither
            | Self::IntrinsicAbsurd
            | Self::IntrinsicIfThenElse
            | Self::Fill
            | Self::TermSpecialize => None,
        }
    }

    pub fn is_fold(self) -> bool {
        matches!(
            self,
            Self::TypeProductSpineFold
                | Self::TypeSumSpineFold
                | Self::TypeArgsFold
                | Self::TypeFunctionParamsFold
                | Self::TypeArityFold
                | Self::TypeNameParamAritiesFold
        )
    }

    pub fn is_term_helper(self) -> bool {
        matches!(
            self,
            Self::TermType
                | Self::TermLet
                | Self::TermFn
                | Self::TermCall
                | Self::TermTypeFn
                | Self::TermTypeApp
                | Self::TermUnit
                | Self::TermHostConvert
                | Self::IntrinsicPair
                | Self::IntrinsicFst
                | Self::IntrinsicSnd
                | Self::IntrinsicLeft
                | Self::IntrinsicRight
                | Self::IntrinsicEither
                | Self::IntrinsicAbsurd
                | Self::IntrinsicIfThenElse
                | Self::TermSpecialize
        )
    }
}

pub const PUBLIC_COMPTIME_NAMES: &[&str] = &[
    "__Comptime__",
    "__Type__",
    "__Checked_term__",
    "__Type_var__",
    "__Type_name__",
    "__Type_arity__",
    "__Diagnostic_text__",
    "Comptime_bool",
    "Comptime_str",
    "__Fill_ctx__",
    "__reflect_type__",
    "__type_view__",
    "__type_is_host__",
    "__type_host_converters__",
    "__type_equal__",
    "__type_name_equal__",
    "__type_var_equal__",
    "__type_is_product_root__",
    "__type_is_sum_root__",
    "__type_unit__",
    "__type_bottom__",
    "__type_product__",
    "__type_sum__",
    "__type_arrow__",
    "__type_forall__",
    "__type_var__",
    "__type_name_type__",
    "__type_apply__",
    "__type_instantiate__",
    "__type_arity__",
    "__type_var_arity__",
    "__type_arity_zero__",
    "__type_arity_succ__",
    "__type_arity_equal__",
    "__type_product_spine_fold__",
    "__type_sum_spine_fold__",
    "__type_args_fold__",
    "__type_function_params_fold__",
    "__type_arity_fold__",
    "__type_name_param_arities_fold__",
    "__type_display__",
    "__type_short_name__",
    "__type_name_display__",
    "__type_var_display__",
    "__type_arity_display__",
    "__diagnostic_concat__",
    "__elab_error__",
    "__type_error__",
    "__structural_recur__",
    "__term_type__",
    "__term_let__",
    "__term_fn__",
    "__term_call__",
    "__term_type_fn__",
    "__term_type_app__",
    "__term_unit__",
    "__term_host_convert__",
    "__term_specialize__",
    "__intrinsic_pair__",
    "__intrinsic_fst__",
    "__intrinsic_snd__",
    "__intrinsic_left__",
    "__intrinsic_right__",
    "__intrinsic_either__",
    "__intrinsic_absurd__",
    "__intrinsic_if_then_else__",
    "__fill__",
];

#[cfg(test)]
mod tests {
    use super::{ComptimeBuiltin, PUBLIC_COMPTIME_NAMES};

    #[test]
    fn comptime_builtin_ordinals_and_serialized_discriminants_are_append_only() {
        use ComptimeBuiltin::*;
        let ordered = [
            ComptimeProof,
            Type,
            CheckedTerm,
            TypeVar,
            TypeName,
            TypeArity,
            DiagnosticText,
            ComptimeBool,
            ComptimeStr,
            ReflectType,
            TypeView,
            TypeIsHost,
            TypeHostConverters,
            TypeEqual,
            TypeNameEqual,
            TypeVarEqual,
            TypeIsProductRoot,
            TypeIsSumRoot,
            TypeUnit,
            TypeBottom,
            TypeProduct,
            TypeSum,
            TypeArrow,
            TypeForall,
            TypeVarFn,
            TypeNameType,
            TypeApply,
            TypeInstantiate,
            TypeArityFn,
            TypeVarArity,
            TypeArityZero,
            TypeAritySucc,
            TypeArityEqual,
            TypeProductSpineFold,
            TypeSumSpineFold,
            TypeArgsFold,
            TypeFunctionParamsFold,
            TypeArityFold,
            TypeNameParamAritiesFold,
            TypeDisplay,
            TypeShortName,
            TypeNameDisplay,
            TypeVarDisplay,
            TypeArityDisplay,
            DiagnosticConcat,
            ElabError,
            TypeError,
            StructuralRecur,
            TermType,
            TermLet,
            TermFn,
            TermCall,
            TermTypeFn,
            TermTypeApp,
            TermUnit,
            TermHostConvert,
            IntrinsicPair,
            IntrinsicFst,
            IntrinsicSnd,
            IntrinsicLeft,
            IntrinsicRight,
            IntrinsicEither,
            IntrinsicAbsurd,
            IntrinsicIfThenElse,
            FillCtx,
            Fill,
            TermSpecialize,
        ];
        for (ordinal, builtin) in ordered.into_iter().enumerate() {
            assert_eq!(builtin as usize, ordinal);
            assert_eq!(
                postcard::to_allocvec(&builtin).expect("serialize builtin discriminant"),
                vec![u8::try_from(ordinal).expect("builtin ordinal fits one byte")]
            );
            assert_eq!(
                ComptimeBuiltin::from_public_name(builtin.public_name()),
                Some(builtin)
            );
        }
    }

    #[test]
    fn comptime_names_do_not_overlap_intrinsics() {
        const INTRINSICS: &[&str] = &[
            "__absurd__",
            "__either__",
            "__fst__",
            "__if_then_else__",
            "__left__",
            "__pair__",
            "__right__",
            "__snd__",
        ];
        for name in PUBLIC_COMPTIME_NAMES {
            assert!(
                !INTRINSICS.contains(name),
                "`{name}` appears in both __comptime__ and __intrinsics__"
            );
        }
    }
}
