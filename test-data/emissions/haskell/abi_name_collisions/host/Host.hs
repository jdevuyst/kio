{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TypeFamilies #-}
module Host (main) where

import BuildHaskellAbiNameCollisions
  ( type HostType__a___ub__Edge,
    type HostType__a_u__b__Edge,
    type HostType__a__b__Token,
  )
import qualified BuildHaskellAbiNameCollisions as P

data HostTypes = HostTypes

instance P.BuildHaskellAbiNameCollisionsHostTypes HostTypes where
  type HostType__a___ub__Edge HostTypes = ()
  type HostType__a_u__b__Edge HostTypes = ()
  type HostType__a__b__Token HostTypes = ()

host :: P.BuildHaskellAbiNameCollisionsHost HostTypes IO
host =
  P.BuildHaskellAbiNameCollisionsHost
    { P.host_H01010201000000020000000161000000025f620000000167__a__b__g = pure,
      P.host_H010102010000000200000002615f00000001620000000167__a__b__g = pure,
      P.host__a__b__f = pure
    }

package :: P.BuildHaskellAbiNameCollisions HostTypes IO
package = P.createBuildHaskellAbiNameCollisions host

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

selectorCollision inner unit qualified =
  P.export__shape__selectorCollision
    package
    (inner, (unit, qualifiedY qualified))

sumPattern
  (P.Export_shape__patternCollision_Arg0_Case0_Type_P value) = value

sumPayloadPattern
  (P.Export_shape__patternCollision_Arg0_Slot0 _ _) = ()

main :: IO ()
main = do
  P.exp_H01010301000000020000000161000000025f620000000163__a__b__c package
  P.exp_H010103010000000200000002615f00000001620000000163__a__b__c package
  P.export__a__b__c package
  P.export__a__bC package
  P.export__api__AB__c package
  P.export__api__A__bC package
  P.exp_H01010301000000010000000161000000085f666f6f5f626172__a___fooBar package
  _ <- P.export__a__Foo__bar package
  returned <- selectorCollision undefined () undefined
  let nested = outerPattern returned
  qualifiedY (outerSelector returned) `seq` pure ()
  innerSelector nested `seq` pure ()
  innerPattern nested `seq` pure ()
  pure ()
