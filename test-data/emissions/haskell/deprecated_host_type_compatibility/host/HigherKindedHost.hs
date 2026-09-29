{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE TypeFamilies #-}

module HigherKindedHost where

import App (AppHostTypes(..))
import Data.Kind (Type)

data HigherKindedTypes (a :: Type)
data HigherScalar
data HigherArray a
data HigherTransformer a b

instance AppHostTypes HigherKindedTypes where
  type HostType__api__Scalar HigherKindedTypes = HigherScalar
  type HostType__api__Array HigherKindedTypes = HigherArray
  type HostType__api__Transformer HigherKindedTypes = HigherTransformer
