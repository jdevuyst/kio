{-# LANGUAGE KindSignatures #-}

module Host where

import Data.Kind (Type)
import qualified UnbridgedPublicSurface as P

instantiate
  :: (P.UnbridgedPublicSurfaceHostTypes h, Monad m)
  => P.UnbridgedPublicSurfaceHost h m
  -> P.UnbridgedPublicSurface h m
instantiate = P.createUnbridgedPublicSurface

invokeMain
  :: (P.UnbridgedPublicSurfaceHostTypes h, Monad m)
  => P.UnbridgedPublicSurface h m
  -> m ()
invokeMain = P.export__api__main
