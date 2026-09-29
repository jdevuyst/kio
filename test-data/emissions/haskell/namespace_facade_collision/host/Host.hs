{-# LANGUAGE KindSignatures #-}

module Host where

import Data.Kind (Type)
import qualified Type as P

instantiate
  :: (P.TypeHostTypes h, Monad m)
  => P.TypeHost h m
  -> P.Type h m
instantiate = P.createType

invokeMain
  :: (P.TypeHostTypes h, Monad m)
  => P.Type h m
  -> m ()
invokeMain = P.export__main__main
