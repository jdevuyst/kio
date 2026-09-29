{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TypeFamilies #-}
module Host (main) where

import BuildHaskellAbiNameCollisions
  ( type HostType_H612f5f620045646765__a___b__Edge,
    type HostType_H615f2f620045646765__a___b__Edge,
    type HostType__a__b__Token,
  )
import qualified BuildHaskellAbiNameCollisions as P

data HostTypes = HostTypes

instance P.BuildHaskellAbiNameCollisionsHostTypes HostTypes where
  type HostType_H612f5f620045646765__a___b__Edge HostTypes = ()
  type HostType_H615f2f620045646765__a___b__Edge HostTypes = ()
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
  P.expSel_H01220201000000010000000573686170650000001273656c6563746f725f636f6c6c6973696f6e020000000001000000015900000002__shape__selectorCollision_ret_Sel_Y_2 value

innerSelector value =
  P.expSel_H01220201000000010000000573686170650000001273656c6563746f725f636f6c6c6973696f6e0200000001020000000001000000015900000002__shape__selectorCollision_ret_slot0_Sel_Y_2 value

outerPattern
  (P.ExpP_H01200201000000010000000573686170650000001273656c6563746f725f636f6c6c6973696f6e0200000000__shape__selectorCollision_ret_P nested _ _) = nested

innerPattern
  (P.ExpP_H01200201000000010000000573686170650000001273656c6563746f725f636f6c6c6973696f6e02000000010200000000__shape__selectorCollision_ret_slot0_P _ _ value) = value

qualifiedY
  :: P.KioCarrier_H736c6f74300059__slot0_Y HostTypes IO
  -> P.KioCarrier_H736c6f74300059__slot0_Y HostTypes IO
qualifiedY = id

selectorCollision inner unit qualified =
  P.export__shape__selectorCollision
    package
    inner
    unit
    (qualifiedY qualified)

sumPattern
  (P.ExpS_H0121020100000001000000057368617065000000117061747465726e5f636f6c6c6973696f6e01000000000000000001000000015000000000__shape__patternCollision_arg0_S_P_0 value) = value

sumPayloadPattern
  (P.ExpP_H0120020100000001000000057368617065000000117061747465726e5f636f6c6c6973696f6e0100000000000000010200000000__shape__patternCollision_arg0_slot0_P _ _) = ()

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
