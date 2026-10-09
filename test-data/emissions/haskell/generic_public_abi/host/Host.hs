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
   . (a, b)
  -> IO (Either b a)
exchange (_, b) = pure (Left b)

preserve
  :: forall (f :: Type -> Type) (a :: Type) (b :: Type)
   . f (a, b)
  -> IO (f (a, b))
preserve = pure

collision
  :: forall (a :: Type)
   . (a, a)
  -> IO (P.Host_api__collision_Result HostTypes IO a)
collision (left, right) =
  pure (P.Host_api__collision_Result left right)

inspect
  :: forall (f :: Type -> Type)
   . ( forall (a :: Type)
     . IO
         ( forall (b :: Type)
         . IO
             ( f (P.Host_api__inspect_Arg0_CallbackArg0_App0 HostTypes IO a b)
            -> IO (f (P.Host_api__inspect_Arg0_CallbackResult_App0 HostTypes IO a b))
             )
         )
     )
  -> IO ()
inspect _ = pure ()

shadow
  :: forall (outer :: Type)
   . ( forall (inner :: Type)
     . IO
         ( (inner, inner)
        -> IO (P.Host_api__shadow_Arg0_CallbackResult HostTypes IO inner)
         )
     )
  -> IO ()
shadow _ = pure ()

relayPack
  :: forall (a :: Type)
   . (P.KioExists_api__Pack HostTypes IO a, a)
  -> IO (P.Host_api__relayPack_Result HostTypes IO a)
relayPack (carrier, _) =
  pure (P.Host_api__relayPack_Result_Case1_Type_Pack carrier)

originPreserve
  :: forall (a :: Type)
   . P.KioType_origin__Box HostTypes IO a
  -> IO (P.KioType_origin__Box HostTypes IO a)
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
  wrapped <- P.export__api__Pair__mkPair package (7, "seven")
  (number, text) <-
    P.export__api__Pair__unPair package wrapped
  pure (number, text)

swapped :: IO String
swapped = do
  result <- P.export__api__exchangePair package (7, "seven")
  case result of
    Left text -> pure text
    Right number -> pure (show number)

hktRoundTrip :: IO (Int, String)
hktRoundTrip = do
  result <- P.export__api__preservePair package (Just pair)
  case result of
    Just (P.Export_api__preservePair_Result_App0 number text) ->
      pure (number, text)
    Nothing -> pure (0, "missing")

-- The public declaration is the actual nominal head, not an unsaturated
-- synonym: it can be passed directly as the higher-kinded F parameter.
nominalHktRoundTrip :: IO ((Int, String), (Int, String))
nominalHktRoundTrip = do
  wrapped <- P.export__origin__Box__mkBox package (pair, (11, "eleven"))
  result <- preserveNominal wrapped
  P.export__origin__Box__unBox package result
  where
    preserveNominal
      :: P.KioType_origin__Box HostTypes IO (Int, String)
      -> IO (P.KioType_origin__Box HostTypes IO (Int, String))
    preserveNominal = P.export__api__preservePair package

dictionaryRoundTrip :: IO (Int, String)
dictionaryRoundTrip = do
  result <- P.export__api__runPairEndo package (Just pair)
  case result of
    Just (P.Export_api__runPairEndo_Result_App0 number text) ->
      pure (number, text)
    Nothing -> pure (0, "missing")

dictionaryMemberRoundTrip :: IO (Int, String)
dictionaryMemberRoundTrip = do
  dictionary <- P.export__api__PairEndo__mkPairEndo package pure
  result <- P.export__api__applyGiven package dictionary (Just pair)
  case result of
    Just (P.Export_api__applyGiven_Result_App0 number text) ->
      pure (number, text)
    Nothing -> pure (0, "missing")

originRoundTrip :: IO (Int, Int)
originRoundTrip = do
  wrapped <-
    P.export__origin__Box__mkBox
      package
      (7, 11)
  result <-
    P.export__caller__preserveOrigin
      package
      wrapped
  P.Member_origin__Box__unBox_Result first second <-
    P.export__origin__Box__unBox
      package
      result
  pure (first, second)

treeRoundTrip :: IO ()
treeRoundTrip = do
  value <-
    P.export__tree__Tree__mkTree
      package
      (P.Member_tree__Tree__mkTree_Arg0_Case0_Position0 ())
  payload <- P.export__tree__Tree__unTree package value
  case payload of
    P.Member_tree__Tree__unTree_Result_Case0_Position0 () -> pure ()
    P.Member_tree__Tree__unTree_Result_Case1_Type_Tree _ -> error "unexpected recursive arm"
  _ <- P.export__tree__keep package value
  P.Export_tree__branchWrapped_Result _ () <-
    P.export__tree__branchWrapped package
  P.Export_otherTree__branchWrapped_Result _ () <-
    P.export__otherTree__branchWrapped package
  pure ()

leafRoundTrip :: IO ()
leafRoundTrip = do
  result <- P.export__tree__leafWrapped package
  case result of
    P.Export_tree__leafWrapped_Result_Case0_Position0 () -> pure ()
    P.Export_tree__leafWrapped_Result_Case1_Type_Leaf _ -> error "unexpected recursive leaf arm"

qualifiedNodeRoundTrip :: IO ()
qualifiedNodeRoundTrip = do
  P.Export_caller__qualifiedNodes_Result left right <-
    P.export__caller__qualifiedNodes package
  P.Export_caller__qualifiedNodePayloads_Result _ _ <-
    P.export__caller__qualifiedNodePayloads package left right
  pure ()

mutualRecursiveRoundTrip :: IO ()
mutualRecursiveRoundTrip = do
  alpha <-
    P.export__tree__Alpha__mkAlpha
      package
      (P.Member_tree__Alpha__mkAlpha_Arg0_Case0_Position0 ())
  P.Member_tree__Alpha__unAlpha_Result_Case0_Position0 () <-
    P.export__tree__Alpha__unAlpha package alpha
  beta <-
    P.export__tree__Beta__mkBeta
      package
      (P.Member_tree__Beta__mkBeta_Arg0_Case0_Position0 ())
  P.Member_tree__Beta__unBeta_Result_Case0_Position0 () <-
    P.export__tree__Beta__unBeta package beta
  pure ()

packed
  :: IO
       ( P.KioExists_api__Pack
           HostTypes
           IO
           Int
       )
packed =
  P.export__api__Pack__mkPack
    package
    (7, "hidden")

visible
  :: forall (u :: Type)
   . IO
       ( (Int, u)
      -> IO Int
       )
visible =
  pure $ \(number, _) ->
    pure number

unpacked :: IO Int
unpacked = do
  value <- packed
  P.export__api__Pack__unPack package value visible

nestedPacked
  :: IO
       ( P.KioExists_api__Pack
           HostTypes
           IO
           ((), ())
       )
nestedPacked =
  P.export__api__Pack__mkPack
    package
    (((), ()), "nested")

nestedVisible
  :: forall (u :: Type)
   . IO
       ( (((), ()), u)
      -> IO ()
       )
nestedVisible = pure $ \(((), ()), _) -> pure ()

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
      (value, 13)
  case result of
    P.Export_api__relayPacked_Result_Case0_Position0 _ -> pure 0
    P.Export_api__relayPacked_Result_Case1_Type_Pack carrier ->
      P.export__api__Pack__unPack package carrier visible

main :: IO ()
main = do
  check "round trip" (7, "seven") =<< round_trip
  check "swap" "seven" =<< swapped
  check "HKT round trip" (7, "seven") =<< hktRoundTrip
  check "nominal head as F" ((7, "seven"), (11, "eleven")) =<< nominalHktRoundTrip
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
