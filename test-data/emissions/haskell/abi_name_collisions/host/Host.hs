{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TypeFamilies #-}
module Host (main) where

import Control.Monad (unless)
import Data.IORef (IORef, modifyIORef', newIORef, readIORef)
import BuildHaskellAbiNameCollisions
  ( type HostType__a___ub__Edge,
    type HostType__a_u__b__Edge,
    type HostType__a__b__Token,
    type HostType__slot0__Value,
  )
import qualified BuildHaskellAbiNameCollisions as P

data HostTypes = HostTypes

instance P.BuildHaskellAbiNameCollisionsHostTypes HostTypes where
  type HostType__a___ub__Edge HostTypes = Int
  type HostType__a_u__b__Edge HostTypes = Int
  type HostType__a__b__Token HostTypes = ()
  type HostType__slot0__Value HostTypes = Int

host :: IORef [String] -> P.BuildHaskellAbiNameCollisionsHost HostTypes IO
host calls =
  P.BuildHaskellAbiNameCollisionsHost
    { P.host_H010201000000020000000161000000025f620000000167__a__b__g = \value -> do
        modifyIORef' calls (++ ["a/_b.g"])
        pure (value + 1),
      P.host_H0102010000000200000002615f00000001620000000167__a__b__g = \value -> do
        modifyIORef' calls (++ ["a_/b.g"])
        pure (value * 2),
      P.host__a__b__f = pure
    }

outerSelector value =
  P.export_shape__selectorCollision_Result_Field2_Type_Y value

innerSelector value =
  P.export_shape__selectorCollision_Result_Slot0_Field2_Type_Y value

outerPattern
  (P.Export_shape__selectorCollision_Result nested _ _) = nested

innerPattern
  (P.Export_shape__selectorCollision_Result_Slot0 _ _ value) = value

qualifiedY
  :: P.KioType_slot0__Y HostTypes IO
  -> P.KioType_slot0__Y HostTypes IO
qualifiedY = id

selectorCollision package inner unit qualified =
  P.export__shape__selectorCollision
    package
    (inner, (unit, qualifiedY qualified))

sumPattern
  (P.Export_shape__patternCollision_Arg0_Case0_Type_P value) = value

sumPayloadPattern
  (P.Export_shape__patternCollision_Arg0_Slot0 first second) = (first, second)

main :: IO ()
main = do
  calls <- newIORef []
  let package = P.createBuildHaskellAbiNameCollisions (host calls)
  first <- P.exp_H010301000000020000000161000000025f620000000163__a__b__c package 10
  second <- P.exp_H0103010000000200000002615f00000001620000000163__a__b__c package 10
  observed <- readIORef calls
  unless (first == 11 && second == 20 && observed == ["a/_b.g", "a_/b.g"]) $
    fail (show (first, second, observed))
  P.export__a__b__c package
  P.export__a__bC package
  P.export__api__AB__c package
  P.export__api__A__bC package
  P.exp_H010301000000010000000161000000085f666f6f5f626172__a___fooBar package
  _ <- P.export__a__Foo__bar package
  innerY <- P.export__shape__makeY package 17
  outerY <- P.export__slot0__Y__mkY package 29
  returned <- selectorCollision package ((), ((), innerY)) () outerY
  let nested = outerPattern returned
  outer <- P.export__slot0__readY package (outerSelector returned)
  inner <- P.export__shape__readY package (innerSelector nested)
  innerFromPattern <- P.export__shape__readY package (innerPattern nested)
  unless ((outer, inner, innerFromPattern) == (29, 17, 17)) $
    fail (show (outer, inner, innerFromPattern))
  pair <- P.export__slot0__P__mkP package (41, 53)
  sumResult <- P.export__shape__patternCollision package (Left pair)
  case sumResult of
    Left _ -> do
      let selected = sumPattern sumResult
      unless (sumPayloadPattern selected == (41, 53)) $
        fail (show (sumPayloadPattern selected))
    Right () -> fail "product sum arm was lost"
  pure ()
