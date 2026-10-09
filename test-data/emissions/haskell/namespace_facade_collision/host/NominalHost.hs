{-# LANGUAGE KindSignatures #-}

module NominalHost where

import Data.Kind (Type)
import qualified KioType_api__Box as P

type Box (h :: Type) (m :: Type -> Type) =
  P.KioStructT_N19_KioType_uapi_u_uBox_Nominal_KioType_api__Box h m

wrap
  :: (P.KioType_api__BoxHostTypes h, Monad m)
  => P.KioType_api__Box h m
  -> a
  -> m (Box h m a)
wrap = P.export__api__Box__mk

unwrap
  :: (P.KioType_api__BoxHostTypes h, Monad m)
  => P.KioType_api__Box h m
  -> Box h m a
  -> m a
unwrap = P.export__api__Box__get
