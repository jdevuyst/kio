{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE TypeFamilies #-}

module NewHost where

import App (AppHostTypes(..))

data NewTypes
data NewScalar
data NewArray a
data NewTransformer a b

instance AppHostTypes NewTypes where
  type HostType__api__Scalar NewTypes = NewScalar
  type HostType__api__Array NewTypes = NewArray
  type HostType__api__Transformer NewTypes = NewTransformer
