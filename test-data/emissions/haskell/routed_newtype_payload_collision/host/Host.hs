{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE RankNTypes #-}

module Host where

import Data.Kind (Type)
import qualified BuildHaskellRoutedNewtypePayloadCollision as P

keepBox
  :: forall (h :: Type) (m :: Type -> Type) (a :: Type)
   . (P.BuildHaskellRoutedNewtypePayloadCollisionHostTypes h, Monad m)
  => P.BuildHaskellRoutedNewtypePayloadCollision h m
  -> P.KioCarrier_H61637475616c00426f78__actual_Box h m a
  -> m (P.KioCarrier_H61637475616c00426f78__actual_Box h m a)
keepBox = P.export__consumer__keepBox
