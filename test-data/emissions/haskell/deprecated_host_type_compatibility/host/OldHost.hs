{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE TypeFamilies #-}

module OldHost where

import App (AppHostTypes(..))

data OldTypes
data OldScalar
data OldArray a
data OldTransformer a b
data Kept a b

instance AppHostTypes OldTypes where
  type HostType__api__Scalar OldTypes = OldScalar
  type HostType__api__Array OldTypes = OldArray
  type HostType__api__Transformer OldTypes = OldTransformer
  type HostType__api__Legacy OldTypes = Kept

type LegacyUse = HostType__api__Legacy OldTypes
