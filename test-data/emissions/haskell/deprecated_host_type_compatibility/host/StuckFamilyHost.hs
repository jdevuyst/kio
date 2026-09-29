{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE UndecidableInstances #-}

module StuckFamilyHost where

import App (AppHostTypes(..))
import Data.Kind (Type)

data StuckTypes
type family StuckScalar (h :: Type) :: Type
type family StuckArray (h :: Type) :: Type -> Type
type family StuckTransformer (h :: Type) :: Type -> Type -> Type

instance AppHostTypes StuckTypes where
  type HostType__api__Scalar StuckTypes = StuckScalar StuckTypes
  type HostType__api__Array StuckTypes = StuckArray StuckTypes
  type HostType__api__Transformer StuckTypes = StuckTransformer StuckTypes
