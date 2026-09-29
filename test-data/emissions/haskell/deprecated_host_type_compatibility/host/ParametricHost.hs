{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE TypeFamilies #-}

module ParametricHost where

import App (AppHostTypes(..))
import Data.Kind (Type)

data ParametricTypes
  (scalar :: Type)
  (array :: Type -> Type)
  (transformer :: Type -> Type -> Type)

instance AppHostTypes (ParametricTypes scalar array transformer) where
  type HostType__api__Scalar (ParametricTypes scalar array transformer) = scalar
  type HostType__api__Array (ParametricTypes scalar array transformer) = array
  type HostType__api__Transformer (ParametricTypes scalar array transformer) = transformer
