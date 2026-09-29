{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE TypeFamilies #-}

module MissingTransformerHost where

import App (AppHostTypes(..))

data MissingTransformerTypes
data PresentScalar
data PresentArray a

instance AppHostTypes MissingTransformerTypes where
  type HostType__api__Scalar MissingTransformerTypes = PresentScalar
  type HostType__api__Array MissingTransformerTypes = PresentArray
