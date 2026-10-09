{-# LANGUAGE DataKinds #-}
{-# LANGUAGE KindSignatures #-}

module Host where

import Data.Kind (Type)
import qualified KioType_api__ as P

type StructuralProduct =
  P.KioStructT_N16_KioType_uapi_u_u_ProductFamily_KioType_api__Product
    '[(), ()]

type NominalProduct (h :: Type) (m :: Type -> Type) (a :: Type) =
  P.KioType_api__Product h m a
