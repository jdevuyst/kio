{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE RankNTypes #-}

module Host where

import Data.Kind (Type)
import qualified NominalGenericCarrier as P

direct
  :: forall (h :: Type) (m :: Type -> Type) (a :: Type)
   . (P.NominalGenericCarrierHostTypes h, Monad m)
  => P.NominalGenericCarrier h m
  -> P.KioCarrier_H61706900536f6d65__api_Some h m a
  -> m (P.KioCarrier_H61706900536f6d65__api_Some h m a)
direct = P.export__api__direct

nested
  :: forall (h :: Type) (m :: Type -> Type) (a :: Type)
   . (P.NominalGenericCarrierHostTypes h, Monad m)
  => P.NominalGenericCarrier h m
  -> Either (P.KioCarrier_H61706900536f6d65__api_Some h m a) ()
  -> m (Either (P.KioCarrier_H61706900536f6d65__api_Some h m a) ())
nested = P.export__api__nested
