{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE ImpredicativeTypes #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}

module Host where

import Data.Kind (Type)
import qualified PublicAbi as P

data HostTypes

instance P.PublicAbiHostTypes HostTypes

exchange
  :: forall (a :: Type) (b :: Type)
   . a
  -> b
  -> IO (P.Env_H01100100000001000000036170690000000865786368616e67650200000000__api__exchange_ret HostTypes IO a b)
exchange _ b =
  pure (P.EnvS_H01210100000001000000036170690000000865786368616e67650200000000030000000000000000__api__exchange_ret_S_pos0_0 b)

preserve
  :: forall (f :: Type -> Type) (a :: Type) (b :: Type)
   . f (P.Env_H01100100000001000000036170690000000870726573657276650100000000000000010100000000__api__preserve_arg0_app0 HostTypes IO a b)
  -> IO (f (P.Env_H011001000000010000000361706900000008707265736572766502000000010100000000__api__preserve_ret_app0 HostTypes IO a b))
preserve = pure

collision
  :: forall (a :: Type)
   . a
  -> a
  -> IO (P.Env_H011001000000010000000361706900000009636f6c6c6973696f6e0200000000__api__collision_ret HostTypes IO a)
collision left right =
  pure (P.EnvP_H012001000000010000000361706900000009636f6c6c6973696f6e0200000000__api__collision_ret_P left right)

inspect
  :: forall (f :: Type -> Type)
   . ( forall (a :: Type)
     . IO
         ( forall (b :: Type)
         . IO
             ( f (P.Env_H011001000000010000000361706900000007696e737065637401000000000000000203000000000100000000__api__inspect_arg0_cbarg0_app0 HostTypes IO a b)
            -> IO (f (P.Env_H011001000000010000000361706900000007696e7370656374010000000000000002040100000000__api__inspect_arg0_cbret_app0 HostTypes IO a b))
             )
         )
     )
  -> IO ()
inspect _ = pure ()

shadow
  :: forall (outer :: Type)
   . ( forall (inner :: Type)
     . IO
         ( inner
        -> inner
        -> IO (P.Env_H011001000000010000000361706900000006736861646f7701000000000000000104__api__shadow_arg0_cbret HostTypes IO inner)
         )
     )
  -> IO ()
shadow _ = pure ()

relayPack
  :: forall (a :: Type)
   . P.KioExistential_H617069005061636b__api_Pack HostTypes IO a
  -> a
  -> IO (P.Env_H01100100000001000000036170690000000a72656c61795f7061636b0200000000__api__relayPack_ret HostTypes IO a)
relayPack carrier _ =
  pure (P.EnvS_H01210100000001000000036170690000000a72656c61795f7061636b020000000001000000045061636b00000001__api__relayPack_ret_S_Pack_1 carrier)

originPreserve
  :: forall (a :: Type)
   . P.KioCarrier_H6f726967696e00426f78__origin_Box HostTypes IO a
  -> IO (P.KioCarrier_H6f726967696e00426f78__origin_Box HostTypes IO a)
originPreserve = pure

host :: P.PublicAbiHost HostTypes IO
host =
  P.PublicAbiHost
    { P.host__api__exchange = exchange
    , P.host__api__preserve = preserve
    , P.host__api__collision = collision
    , P.host__api__inspect = inspect
    , P.host__api__shadow = shadow
    , P.host__api__relayPack = relayPack
    , P.host__origin__preserve = originPreserve
    }

package :: P.PublicAbi HostTypes IO
package = P.createPublicAbi host

pair
  :: (Int, String)
pair = (7, "seven")

round_trip :: IO (Int, String)
round_trip = do
  wrapped <- P.export__api__Pair__mkPair package 7 "seven"
  P.ExpP_H012002020000000100000003617069000000045061697200000007756e5f706169720200000000__api__Pair__unPair_ret_P number text <-
    P.export__api__Pair__unPair package wrapped
  pure (number, text)

swapped :: IO String
swapped = do
  result <- P.export__api__exchangePair package 7 "seven"
  case result of
    P.ExpS_H0121020100000001000000036170690000000d65786368616e67655f706169720200000000030000000000000000__api__exchangePair_ret_S_pos0_0 text -> pure text
    P.ExpS_H0121020100000001000000036170690000000d65786368616e67655f706169720200000000030000000100000001__api__exchangePair_ret_S_pos1_1 number -> pure (show number)

hktRoundTrip :: IO (Int, String)
hktRoundTrip = do
  result <- P.export__api__preservePair package (Just pair)
  case result of
    Just (P.ExpP_H0120020100000001000000036170690000000d70726573657276655f7061697202000000010100000000__api__preservePair_ret_app0_P number text) ->
      pure (number, text)
    Nothing -> pure (0, "missing")

dictionaryRoundTrip :: IO (Int, String)
dictionaryRoundTrip = do
  result <- P.export__api__runPairEndo package (Just pair)
  case result of
    Just (P.ExpP_H0120020100000001000000036170690000000d72756e5f706169725f656e646f02000000010100000000__api__runPairEndo_ret_app0_P number text) ->
      pure (number, text)
    Nothing -> pure (0, "missing")

dictionaryMemberRoundTrip :: IO (Int, String)
dictionaryMemberRoundTrip = do
  dictionary <- P.export__api__PairEndo__mkPairEndo package pure
  result <- P.export__api__applyGiven package dictionary (Just pair)
  case result of
    Just (P.ExpP_H0120020100000001000000036170690000000b6170706c795f676976656e02000000010100000000__api__applyGiven_ret_app0_P number text) ->
      pure (number, text)
    Nothing -> pure (0, "missing")

originRoundTrip :: IO (Int, Int)
originRoundTrip = do
  wrapped <-
    P.export__origin__Box__mkBox
      package
      7
      11
  result <-
    P.export__caller__preserveOrigin
      package
      wrapped
  P.ExpP_H0120020200000001000000066f726967696e00000003426f7800000006756e5f626f780200000000__origin__Box__unBox_ret_P first second <-
    P.export__origin__Box__unBox
      package
      result
  pure (first, second)

treeRoundTrip :: IO ()
treeRoundTrip = do
  value <-
    P.export__tree__Tree__mkTree
      package
      (P.ExpS_H012102020000000100000004747265650000000454726565000000076d6b5f74726565010000000000000000030000000000000000__tree__Tree__mkTree_arg0_S_pos0_0 ())
  payload <- P.export__tree__Tree__unTree package value
  case payload of
    P.ExpS_H01210202000000010000000474726565000000045472656500000007756e5f747265650200000000030000000000000000__tree__Tree__unTree_ret_S_pos0_0 () -> pure ()
    P.ExpS_H01210202000000010000000474726565000000045472656500000007756e5f74726565020000000001000000045472656500000001__tree__Tree__unTree_ret_S_Tree_1 _ -> error "unexpected recursive arm"
  _ <- P.export__tree__keep package value
  P.ExpP_H012002010000000100000004747265650000000e6272616e63685f777261707065640200000000__tree__branchWrapped_ret_P _ () <-
    P.export__tree__branchWrapped package
  P.ExpP_H01200201000000010000000a6f746865725f747265650000000e6272616e63685f777261707065640200000000__otherTree__branchWrapped_ret_P _ () <-
    P.export__otherTree__branchWrapped package
  pure ()

leafRoundTrip :: IO ()
leafRoundTrip = do
  result <- P.export__tree__leafWrapped package
  case result of
    P.ExpS_H012102010000000100000004747265650000000c6c6561665f777261707065640200000000030000000000000000__tree__leafWrapped_ret_S_pos0_0 () -> pure ()
    P.ExpS_H012102010000000100000004747265650000000c6c6561665f77726170706564020000000001000000044c65616600000001__tree__leafWrapped_ret_S_Leaf_1 _ -> error "unexpected recursive leaf arm"

qualifiedNodeRoundTrip :: IO ()
qualifiedNodeRoundTrip = do
  P.ExpP_H01200201000000010000000663616c6c65720000000f7175616c69666965645f6e6f6465730200000000__caller__qualifiedNodes_ret_P left right <-
    P.export__caller__qualifiedNodes package
  P.ExpP_H01200201000000010000000663616c6c6572000000177175616c69666965645f6e6f64655f7061796c6f6164730200000000__caller__qualifiedNodePayloads_ret_P _ _ <-
    P.export__caller__qualifiedNodePayloads package left right
  pure ()

mutualRecursiveRoundTrip :: IO ()
mutualRecursiveRoundTrip = do
  alpha <-
    P.export__tree__Alpha__mkAlpha
      package
      (P.ExpS_H0121020200000001000000047472656500000005416c706861000000086d6b5f616c706861010000000000000000030000000000000000__tree__Alpha__mkAlpha_arg0_S_pos0_0 ())
  P.ExpS_H0121020200000001000000047472656500000005416c70686100000008756e5f616c7068610200000000030000000000000000__tree__Alpha__unAlpha_ret_S_pos0_0 () <-
    P.export__tree__Alpha__unAlpha package alpha
  beta <-
    P.export__tree__Beta__mkBeta
      package
      (P.ExpS_H012102020000000100000004747265650000000442657461000000076d6b5f62657461010000000000000000030000000000000000__tree__Beta__mkBeta_arg0_S_pos0_0 ())
  P.ExpS_H01210202000000010000000474726565000000044265746100000007756e5f626574610200000000030000000000000000__tree__Beta__unBeta_ret_S_pos0_0 () <-
    P.export__tree__Beta__unBeta package beta
  pure ()

packed
  :: IO
       ( P.KioExistential_H617069005061636b__api_Pack
           HostTypes
           IO
           Int
       )
packed =
  P.export__api__Pack__mkPack
    package
    7
    "hidden"

visible
  :: forall (u :: Type)
   . IO
       ( Int
      -> u
      -> IO Int
       )
visible =
  pure $ \number _ ->
    pure number

unpacked :: IO Int
unpacked = do
  value <- packed
  P.export__api__Pack__unPack package value visible

nestedPacked
  :: IO
       ( P.KioExistential_H617069005061636b__api_Pack
           HostTypes
           IO
           ((), ())
       )
nestedPacked =
  P.export__api__Pack__mkPack
    package
    ((), ())
    "nested"

nestedVisible
  :: forall (u :: Type)
   . IO
       ( ((), ())
      -> u
      -> IO ()
       )
nestedVisible = pure $ \((), ()) _ -> pure ()

nestedPackRoundTrip :: IO ()
nestedPackRoundTrip = do
  value <- nestedPacked
  kept <- P.export__api__keepNestedPack package value
  P.export__api__Pack__unPack package kept nestedVisible

relayed :: IO Int
relayed = do
  value <- packed
  result <-
    P.export__api__relayPacked
      package
      value
      13
  case result of
    P.ExpS_H0121020100000001000000036170690000000c72656c61795f7061636b65640200000000030000000000000000__api__relayPacked_ret_S_pos0_0 _ -> pure 0
    P.ExpS_H0121020100000001000000036170690000000c72656c61795f7061636b6564020000000001000000045061636b00000001__api__relayPacked_ret_S_Pack_1 carrier ->
      P.export__api__Pack__unPack package carrier visible

main :: IO ()
main = do
  check "round trip" (7, "seven") =<< round_trip
  check "swap" "seven" =<< swapped
  check "HKT round trip" (7, "seven") =<< hktRoundTrip
  check "dictionary round trip" (7, "seven") =<< dictionaryRoundTrip
  check "dictionary member round trip" (7, "seven") =<< dictionaryMemberRoundTrip
  check "qualified host scheme" (7, 11) =<< originRoundTrip
  check "existential unpack" 7 =<< unpacked
  check "nested existential round trip" 7 =<< relayed
  nestedPackRoundTrip
  treeRoundTrip
  leafRoundTrip
  qualifiedNodeRoundTrip
  mutualRecursiveRoundTrip

check :: (Eq a, Show a) => String -> a -> a -> IO ()
check label expected actual
  | expected == actual = pure ()
  | otherwise = error (label ++ ": expected " ++ show expected ++ ", got " ++ show actual)
