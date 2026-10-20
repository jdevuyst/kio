{-# LANGUAGE BangPatterns #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE TypeFamilies #-}

module Main (main) where

import Bench (BenchHostTypes(..))
import qualified Bench as P
import Control.Exception (evaluate)
import Control.Monad (foldM, unless, when)
import Data.Int (Int32, Int64)
import Data.IORef
import Data.List (foldl', intercalate)
import Data.Maybe (isNothing)
import Data.Word (Word64)
import GHC.Clock (getMonotonicTimeNSec)
import System.Environment (getArgs, lookupEnv)
import Text.Read (readMaybe)

data Types

instance P.BenchHostTypes Types where
  type HostType__bench__I32 Types = Int32

type Callback = Int32 -> IO Int32

data Config = Config
  { arm :: String, mode :: String, depth :: Int, repetitions :: Int
  , seed :: Int, warmup :: Int, purpose :: String
  }

require :: Bool -> String -> IO ()
require condition message = unless condition (ioError (userError message))

bounded :: Int -> Int -> String -> IO Int
bounded lower upper text = case readMaybe text of
  Just value | lower <= value && value <= upper -> pure value
  _ -> ioError (userError "out-of-range integer")

parseConfig :: [String] -> IO Config
parseConfig [a, m, d, r, s, w, p] = do
  require (a `elem` ["B", "T", "C", "S", "P"]) "unknown arm"
  require (m `elem` ["direct", "native", "host-loop", "callback-tail", "callback-pending", "io-bind-native", "io-bind-public"]) "unknown mode"
  require (p `elem` ["time", "witness"]) "unknown purpose"
  d' <- bounded 0 10000 d
  r' <- bounded 1 1000000 r
  s' <- bounded 0 65535 s
  w' <- bounded 0 1000000 w
  require (m `notElem` ["direct", "native"] || d' == 0) "direct/native require depth zero"
  require (p /= "witness" || (d' <= 8 && r' == 1 && w' == 0)) "invalid witness bounds"
  pure (Config a m d' r' s' w' p)
parseConfig _ = ioError (userError "usage: driver ARM MODE DEPTH REPEAT SEED WARMUP PURPOSE")

data Events = Events Bool (IORef [Int])

hostStep :: Events -> Int32 -> Int32 -> IO Int32
hostStep (Events witness events) value label = do
  when witness (modifyIORef' events (fromIntegral label :))
  evaluate (value + label + 1)

data Prepared = Prepared
  { packageRoot :: IORef (Maybe (P.Bench Types IO))
  , invokeRoot :: IORef (Maybe Callback)
  , prefixRoots :: IORef [Callback]
  , inputRoots :: IORef [Int32]
  }

createPrepared :: Events -> IO Prepared
createPrepared events = do
  let host :: P.BenchHost Types IO
      host = P.BenchHost { P.host__bench__step = hostStep events }
  pkg <- evaluate (P.createBench host)
  Prepared <$> newIORef (Just pkg) <*> newIORef Nothing <*> newIORef [] <*> newIORef []

needed :: IORef (Maybe a) -> IO a
needed ref = do
  value <- readIORef ref
  case value of
    Just result -> pure result
    Nothing -> ioError (userError "required host root was cleared")

construct :: Config -> Events -> Prepared -> IO ()
construct config events prepared = do
  pkg <- needed (packageRoot prepared)
  case mode config of
    "callback-tail" -> chain pkg P.export__bench__callbackTail
    "callback-pending" -> chain pkg P.export__bench__callbackPending
    "native" -> install (P.export__bench__callNative pkg (\value -> hostStep events value 0))
    "direct" -> install (P.export__bench__direct pkg)
    "host-loop" -> install (hostLoop (depth config))
    "io-bind-native" -> binds (\value label -> hostStep events value label)
    "io-bind-public" -> binds (\value label -> P.export__bench__callNative pkg (\v -> hostStep events v label) value)
    _ -> ioError (userError "validated mode was lost")
  where
    install :: Callback -> IO ()
    install callback = do
      forced <- evaluate callback
      writeIORef (invokeRoot prepared) (Just forced)
    chain :: P.Bench Types IO -> (P.Bench Types IO -> Callback -> Int32 -> IO Callback) -> IO ()
    chain pkg link = do
      initial <- P.export__bench__callbackSeed pkg >>= evaluate
      (lastCallback, prefixes) <- foldM
        (\(previous, retained) label -> do
          next <- link pkg previous (fromIntegral label) >>= evaluate
          pure (next, next : retained))
        (initial, [initial]) [1 .. depth config]
      _ <- evaluate (length prefixes)
      writeIORef (prefixRoots prepared) prefixes
      install lastCallback
    binds :: (Int32 -> Int32 -> IO Int32) -> IO ()
    binds step = do
      let labels = map fromIntegral [0 .. depth config] :: [Int32]
      _ <- evaluate (foldl' (\n label -> label `seq` n + 1) (0 :: Int) labels)
      writeIORef (inputRoots prepared) labels
      install (\input -> foldM step input labels)
    hostLoop :: Int -> Callback
    hostLoop label !value
      | label < 0 = pure value
      | otherwise = hostStep events value (fromIntegral label) >>= hostLoop (label - 1)

inputAt :: Config -> Int -> Int32
inputAt config index = fromIntegral ((seed config + index * 17) `mod` 65536)

expectedChecksum :: Config -> Int -> Int64
expectedChecksum config count = go 0 0
  where
    delta = fromIntegral ((depth config + 1) * (depth config + 2) `div` 2)
    go !index !accumulator
      | index == count = accumulator
      | otherwise = go (index + 1) (accumulator + fromIntegral (inputAt config index) + delta)

runBatch :: Config -> Prepared -> Int -> IO Int64
runBatch config prepared count = do
  callback <- needed (invokeRoot prepared)
  let loop !index !accumulator
        | index == count = evaluate accumulator
        | otherwise = do
            value <- callback (inputAt config index) >>= evaluate
            let !next = accumulator + fromIntegral value
            loop (index + 1) next
  loop 0 0

timed :: IO a -> IO (a, Word64)
timed action = do
  before <- getMonotonicTimeNSec
  result <- action
  after <- getMonotonicTimeNSec
  pure (result, after - before)

rootCounts :: Prepared -> IO (Int, Int)
rootCounts prepared = do
  prefixes <- readIORef (prefixRoots prepared) >>= evaluate . length
  inputs <- readIORef (inputRoots prepared) >>= evaluate . length
  pure (prefixes, inputs)

releaseRoots :: Prepared -> IO ()
releaseRoots prepared = do
  writeIORef (invokeRoot prepared) Nothing
  writeIORef (prefixRoots prepared) []
  writeIORef (inputRoots prepared) []
  writeIORef (packageRoot prepared) Nothing

json :: [(String, String)] -> String
json fields = "{" ++ intercalate "," [show key ++ ":" ++ value | (key, value) <- fields] ++ "}"

boolean :: Bool -> String
boolean True = "true"
boolean False = "false"

main :: IO ()
main = do
  config <- getArgs >>= parseConfig
  ambientRTS <- lookupEnv "GHCRTS"
  require (isNothing ambientRTS || ambientRTS == Just "") "GHCRTS override is not allowed in the default-policy panel"
  traceRef <- newIORef []
  let events = Events (purpose config == "witness") traceRef
  expected <- evaluate (expectedChecksum config (repetitions config))
  expectedWarmup <- evaluate (expectedChecksum config (warmup config))
  (prepared, packageNs) <- timed (createPrepared events)
  (_, constructNs) <- timed (construct config events prepared)
  readIORef traceRef >>= \trace -> require (null trace) "construction executed a host body"
  (warmed, warmupNs) <- timed (runBatch config prepared (warmup config))
  require (warmed == expectedWarmup) "warmup checksum mismatch"
  (checksum, invokeNs) <- timed (runBatch config prepared (repetitions config))
  require (checksum == expected) "measured checksum mismatch"
  trace <- reverse <$> readIORef traceRef
  let ascending = mode config `elem` ["callback-pending", "io-bind-native", "io-bind-public"]
      labels = if ascending then [0 .. depth config] else reverse [0 .. depth config]
  require (trace == (if purpose config == "witness" then labels else [])) "effect order mismatch"
  (prefixesBefore, inputsBefore) <- rootCounts prepared
  (_, releaseNs) <- timed (releaseRoots prepared)
  (prefixesAfter, inputsAfter) <- rootCounts prepared
  invokerCleared <- isNothing <$> readIORef (invokeRoot prepared)
  packageCleared <- isNothing <$> readIORef (packageRoot prepared)
  putStrLn (json
    [ ("schema", show "kio-haskell-targeted-driver-v1"), ("backend", show "haskell")
    , ("arm", show (arm config)), ("mode", show (mode config))
    , ("depth", show (depth config)), ("repeat", show (repetitions config))
    , ("seed", show (seed config)), ("warmup", show (warmup config)), ("purpose", show (purpose config))
    , ("checksum", show checksum), ("expected", show expected), ("trace", show trace)
    , ("package_ns", show packageNs), ("construct_ns", show constructNs)
    , ("warmup_ns", show warmupNs), ("invoke_ns", show invokeNs), ("release_roots_ns", show releaseNs)
    , ("total_phases_ns", show (packageNs + constructNs + warmupNs + invokeNs + releaseNs))
    , ("callback_root_slots_before_release", show prefixesBefore), ("callback_root_slots_after_release", show prefixesAfter)
    , ("input_root_slots_before_release", show inputsBefore), ("input_root_slots_after_release", show inputsAfter)
    , ("invoker_root_cleared", boolean invokerCleared), ("package_root_cleared", boolean packageCleared)
    , ("demand_policy", show "every-Int32-result-and-strict-Int64-checksum-before-stop")
    , ("rts_stack_policy", show "unmodified-defaults-required")
    , ("capture_collection", "null")
    , ("capture_collection_reason", show "not measured; clearing IORef roots does not prove capture collection")
    ])
