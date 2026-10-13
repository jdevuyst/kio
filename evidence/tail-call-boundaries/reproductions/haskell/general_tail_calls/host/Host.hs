{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE ImpredicativeTypes #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}

module Host (main) where

import Control.Monad (foldM, unless)
import Data.Int (Int32)
import Data.Kind (Type)
import Data.IORef (IORef, modifyIORef', newIORef, readIORef)
import GeneralTailCalls (type HostType__api__I32)
import qualified GeneralTailCalls as P

data Types

instance P.GeneralTailCallsHostTypes Types where
  type HostType__api__I32 Types = Int32

host :: IORef Int -> P.GeneralTailCallsHost Types IO
host count = P.GeneralTailCallsHost
  { P.host__api__tick = modifyIORef' count (+ 1)
  }

type Stage = forall (a :: Type). IO Int32

newtype Poly = Poly Stage

bindStage :: forall b. IO Stage -> (Stage -> IO b) -> IO b
bindStage = (>>=)

linkPoly :: P.GeneralTailCalls Types IO -> Poly -> IO Poly
linkPoly package (Poly previous) =
  bindStage (P.export__api__typeLink package previous) $ \next -> pure (Poly next)

check :: (Eq a, Show a) => a -> a -> IO ()
check expected actual = unless (expected == actual) (fail (show actual))

main :: IO ()
main = do
  let depth = 100001
  first <- newIORef 0
  second <- newIORef 0
  let packages = [P.createGeneralTailCalls (host first), P.createGeneralTailCalls (host second)]
      package index = packages !! (index `mod` 2)
      counts = mapM readIORef [first, second]
  value <- foldM (\previous index -> P.export__api__link (package index) previous) pure [1 .. depth]
  counts >>= check [0, 0]
  value 17 >>= check 17
  value 29 >>= check 29
  counts >>= check [depth - 1, depth + 1]
  Poly stage <- foldM (\previous index -> linkPoly (package index) previous) (Poly (pure 37)) [1 .. depth]
  counts >>= check [depth - 1, depth + 1]
  stage @() >>= check 37
  stage @String >>= check 37
  counts >>= check [2 * (depth - 1), 2 * (depth + 1)]
  putStrLn "value and forall chains"
