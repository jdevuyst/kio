{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}

module Host (main) where

import Data.IORef
import qualified SourceParameters as P
import SourceParameters (type HostType__api__Box, type HostType__api__Text)

data Types

instance P.SourceParametersHostTypes Types where
  type HostType__api__Text Types = String
  type HostType__api__Box Types = Maybe

check :: (Eq a, Show a) => a -> a -> IO ()
check expected actual
  | expected == actual = pure ()
  | otherwise = error (show actual)

main :: IO ()
main = do
  events <- newIORef []
  let record note = modifyIORef' events (++ [note])
      first :: forall a b. a -> b -> IO (a, b)
      first left right = record "first" >> pure (left, right)
      second :: forall a b. (a, b) -> IO (a, b)
      second pair = record "second" >> pure pair
      host :: P.SourceParametersHost Types IO
      host = P.SourceParametersHost
        { P.host__api__first = first
        , P.host__api__second = second
        , P.host__api__mark = record
        , P.host__api__boxEcho = pure
        }
      package = P.createSourceParameters host
      split :: Int -> Bool -> IO (Int, Bool)
      split = P.export__api__split package
      packed :: (Int, Bool) -> IO (Int, Bool)
      packed = P.export__api__packed package
      mixed :: Int -> (Bool, Char) -> IO (Int, (Bool, Char))
      mixed = P.export__api__mixed package
      leftPacked :: (Int, Bool) -> Char -> IO ((Int, Bool), Char)
      leftPacked = P.export__api__leftPacked package
  split 7 True >>= check (7, True)
  packed (8, False) >>= check (8, False)
  mixed 9 (True, 'x') >>= check (9, (True, 'x'))
  leftPacked (10, False) 'y' >>= check ((10, False), 'y')
  P.export__api__generic package (11 :: Int, True) >>= check (11, True)
  P.export__api__generic package () >>= check ()
  P.export__api__directUnit package
  P.export__api__invoke package (\(left, right) -> pure (left ++ right)) "a" "b" >>= check "ab"
  callback <- P.export__api__firstValue package
  callback ("c", "d") >>= check ("c", "d")
  boxed <- P.export__api__boxedCallback package (Just (\(left, right) -> pure (left ++ right)))
  case boxed of
    Just step -> step ("boxed", " callback") >>= check "boxed callback"
    Nothing -> error "boxed callback disappeared"
  genericBox <- P.export__api__boxedGeneric package (Just (\(left, right) -> pure (left + right)))
  case genericBox of
    Just step -> step (14 :: Int, 15) >>= check 29
    Nothing -> error "generic boxed callback disappeared"
  genericApplication <- P.export__api__appliedGeneric package (Just (\(left, right) -> pure (left ++ right)))
  case genericApplication of
    Just step -> step ("generic", " application") >>= check "generic application"
    Nothing -> error "higher-kinded callback disappeared"
  later <- P.export__api__returned package "e"
  readIORef events >>= check ["first", "second", "unit", "first", "head"]
  later "f" >>= check ("e", "f")
  readIORef events >>= check ["first", "second", "unit", "first", "head", "tail"]
  pair <- P.export__api__Pair__pair package (12 :: Int, 13)
  P.export__api__Pair__unPair package pair >>= check (12, 13)
  putStrLn "source grouping ok"
