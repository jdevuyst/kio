{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE ImpredicativeTypes #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}

module Host (main) where

import Control.Monad (foldM, unless)
import Data.Kind (Type)
import Data.IORef (IORef, modifyIORef', newIORef, readIORef)
import qualified GeneralTailFunctionResult as P

data Types

instance P.GeneralTailFunctionResultHostTypes Types

host :: IORef Int -> P.GeneralTailFunctionResultHost Types IO
host count = P.GeneralTailFunctionResultHost
  { P.host__api__tick = modifyIORef' count (+ 1)
  }

type Stage = forall (a :: Type). IO (IO ())

newtype Poly = Poly Stage

bindStage :: forall b. IO Stage -> (Stage -> IO b) -> IO b
bindStage = (>>=)

link :: P.GeneralTailFunctionResult Types IO -> Poly -> IO Poly
link package (Poly previous) =
  bindStage (P.export__api__link package previous) $ \next -> pure (Poly next)

check :: (Eq a, Show a) => a -> a -> IO ()
check expected actual = unless (expected == actual) (fail (show actual))

checkChain :: Int -> IO ()
checkChain depth = do
  first <- newIORef 0
  second <- newIORef 0
  returnedCalls <- newIORef (0 :: Int)
  let packages = [P.createGeneralTailFunctionResult (host first), P.createGeneralTailFunctionResult (host second)]
      package index = packages !! (index `mod` 2)
      counts = mapM readIORef [first, second]
      expectedTicks = [depth `div` 2, (depth + 1) `div` 2]
  Poly stage <- foldM
    (\previous index -> link (package index) previous)
    (Poly (pure (modifyIORef' returnedCalls (+ 1)))) [1 .. depth]
  counts >>= check [0, 0]
  firstAction <- stage @()
  counts >>= check expectedTicks
  readIORef returnedCalls >>= check 0
  firstAction
  readIORef returnedCalls >>= check 1
  secondAction <- stage @String
  counts >>= check (map (* 2) expectedTicks)
  readIORef returnedCalls >>= check 1
  secondAction
  readIORef returnedCalls >>= check 2

main :: IO ()
main = do
  checkChain 3
  putStrLn "shallow function-result stages"
  checkChain 100001
  putStrLn "function-result stages"
