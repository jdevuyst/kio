{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE TypeFamilies #-}

module MissingArrayHost where

import App (AppHostTypes(..))

data MissingArrayTypes
data PresentScalar
data PresentTransformer a b

instance AppHostTypes MissingArrayTypes where
  type HostType__api__Scalar MissingArrayTypes = PresentScalar
  type HostType__api__Transformer MissingArrayTypes = PresentTransformer
