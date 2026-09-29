{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE TypeFamilies #-}

module MissingScalarHost where

import App (AppHostTypes(..))

data MissingScalarTypes
data PresentArray a
data PresentTransformer a b

instance AppHostTypes MissingScalarTypes where
  type HostType__api__Array MissingScalarTypes = PresentArray
  type HostType__api__Transformer MissingScalarTypes = PresentTransformer
