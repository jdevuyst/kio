{-# LANGUAGE DataKinds #-}
{-# LANGUAGE KindSignatures #-}

module Host where

import Data.Kind (Type)
import qualified KioCarrier_H6170690050726f64756374__api_ as P

type StructuralProduct =
  P.KioStructT_H01000000284b696f436172726965725f48363137303639303035303732366636343735363337345f5f6170695f01__KioCarrier_H6170690050726f64756374__api_Product
    '[(), ()]

type NominalProduct (h :: Type) (m :: Type -> Type) (a :: Type) =
  P.KioCarrier_H6170690050726f64756374__api_Product h m a
