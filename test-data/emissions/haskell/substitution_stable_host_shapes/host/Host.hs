{-# LANGUAGE DataKinds #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE ImpredicativeTypes #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

module Host where

import Data.Kind (Type)
import Data.Type.Equality ((:~:) (Refl))
import qualified SubstitutionStableHostShapes as P
import SubstitutionStableHostShapes (type HostType__api__Box)

data HostTypes

instance P.SubstitutionStableHostShapesHostTypes HostTypes where
  type HostType__api__Box HostTypes = Maybe

genericProduct
  :: forall (a :: Type) (t :: Type)
   . (a, t)
  -> IO (P.Host_api__genericProduct_Result HostTypes IO a t)
genericProduct (first, second) =
  pure (P.Host_api__genericProduct_Result first second)

directProduct
  :: forall (a :: Type) (b :: Type) (c :: Type)
   . (a, (b, c))
  -> IO (P.Host_api__directProduct_Result HostTypes IO a b c)
directProduct (first, (second, third)) =
  pure (P.Host_api__directProduct_Result first second third)

genericSum
  :: forall (a :: Type) (t :: Type)
   . P.Host_api__genericSum_Arg0 HostTypes IO a t
  -> IO (P.Host_api__genericSum_Result HostTypes IO a t)
genericSum = pure

directSum
  :: forall (a :: Type) (b :: Type) (c :: Type)
   . P.Host_api__directSum_Arg0 HostTypes IO a b c
  -> IO (P.Host_api__directSum_Result HostTypes IO a b c)
directSum = pure

rankProduct
  :: (PolyId, ())
  -> IO (P.Host_api__rankProduct_Result HostTypes IO)
rankProduct (identity, unit) =
  pure (P.Host_api__rankProduct_Result identity unit)

rankSum
  :: P.Host_api__rankSum_Arg0 HostTypes IO
  -> IO (P.Host_api__rankSum_Result HostTypes IO)
rankSum = pure

aliasRankProduct
  :: (PolyId, PolyId)
  -> IO (P.Host_api__aliasRankProduct_Result HostTypes IO)
aliasRankProduct (first, second) =
  pure (P.Host_api__aliasRankProduct_Result first second)

aliasRankSum
  :: P.Host_api__aliasRankSum_Arg0 HostTypes IO
  -> IO (P.Host_api__aliasRankSum_Result HostTypes IO)
aliasRankSum = pure

newtypeRankProduct
  :: (PolyId, PolyId)
  -> IO (P.Host_api__newtypeRankProduct_Result HostTypes IO)
newtypeRankProduct (first, second) =
  pure (P.Host_api__newtypeRankProduct_Result first second)

newtypeRankSum
  :: P.Host_api__newtypeRankSum_Arg0 HostTypes IO
  -> IO (P.Host_api__newtypeRankSum_Result HostTypes IO)
newtypeRankSum = pure

scopedApplication
  :: forall (f :: Type -> Type) (a :: Type) (b :: Type)
   . f (P.Host_api__scopedApplication_Arg0_App0 HostTypes IO a b)
  -> IO (f (P.Host_api__scopedApplication_Result_App0 HostTypes IO a b))
scopedApplication = pure

stagedHost :: forall a. a -> IO (forall b. IO (forall c. IO ((b, c) -> IO c)))
stagedHost _ = pure second
  where
    second :: forall b. IO (forall c. IO ((b, c) -> IO c))
    second = pure third
      where
        third :: forall c. IO ((b, c) -> IO c)
        third = pure $ \(_, value) -> pure value

host :: P.SubstitutionStableHostShapesHost HostTypes IO
host =
  P.SubstitutionStableHostShapesHost
    { P.host__api__pairSeed = pure Nothing,
      P.host__api__choiceSeed = pure Nothing,
      P.host__api__tie = \_ wrapped -> pure wrapped,
      P.host__api__genericProduct = genericProduct,
      P.host__api__directProduct = directProduct,
      P.host__api__genericSum = genericSum,
      P.host__api__directSum = directSum,
      P.host__api__rankProduct = rankProduct,
      P.host__api__rankSum = rankSum,
      P.host__api__aliasRankProduct = aliasRankProduct,
      P.host__api__aliasRankSum = aliasRankSum,
      P.host__api__newtypeRankProduct = newtypeRankProduct,
      P.host__api__newtypeRankSum = newtypeRankSum,
      P.host__api__scopedApplication = scopedApplication,
      P.host__api__stagedHost = stagedHost
    }

package :: P.SubstitutionStableHostShapes HostTypes IO
package = P.createSubstitutionStableHostShapes host

productSubstitution
  :: P.SubstitutionStableHostShapesProduct
       '[Int, P.SubstitutionStableHostShapesProduct '[Bool, Char]]
     :~: P.SubstitutionStableHostShapesProduct '[Int, Bool, Char]
productSubstitution = Refl

sumSubstitution
  :: P.SubstitutionStableHostShapesSum
       '[Int, P.SubstitutionStableHostShapesSum '[Bool, Char]]
     :~: P.SubstitutionStableHostShapesSum '[Int, Bool, Char]
sumSubstitution = Refl

type PolyId = forall a. IO (a -> IO a)

polyId :: PolyId
polyId = pure pure

publicRankRoundTrip :: IO Bool
publicRankRoundTrip = do
  wrapped <-
    P.export__api__WrappedPolyId__mkWrappedPolyId
      package
      polyId
  projected <-
    P.export__api__WrappedPolyId__unWrappedPolyId
      package
      wrapped
  identity <- projected @Bool
  identity True

aliasProductEquality
  :: P.Host_api__aliasRankProduct_Result HostTypes IO
     :~: P.SubstitutionStableHostShapesProduct '[PolyId, PolyId]
aliasProductEquality = Refl

aliasSumEquality
  :: P.Host_api__aliasRankSum_Arg0 HostTypes IO
     :~: P.SubstitutionStableHostShapesSum '[PolyId, PolyId]
aliasSumEquality = Refl

newtypeProductEquality
  :: P.Host_api__newtypeRankProduct_Result HostTypes IO
     :~: P.SubstitutionStableHostShapesProduct '[PolyId, PolyId]
newtypeProductEquality = Refl

newtypeSumEquality
  :: P.Host_api__newtypeRankSum_Arg0 HostTypes IO
     :~: P.SubstitutionStableHostShapesSum '[PolyId, PolyId]
newtypeSumEquality = Refl

directProductValue
  :: P.Host_api__directProduct_Result HostTypes IO Int Bool Char
directProductValue = P.Host_api__directProduct_Result 7 False 'x'

genericProductValue
  :: P.Host_api__genericProduct_Result HostTypes IO Int (Bool, Char)
genericProductValue = directProductValue

productSelectors :: (Int, Bool, Char)
productSelectors = case directProductValue of
  P.Host_api__directProduct_Result first second third ->
    (first, second, third)

directSumValue :: P.Host_api__directSum_Arg0 HostTypes IO Int Bool Char
directSumValue = P.Host_api__directSum_Arg0_Case2_Position2 'z'

genericSumValue
  :: P.Host_api__genericSum_Arg0 HostTypes IO Int (Either Bool Char)
genericSumValue = directSumValue

rankProductValue :: P.Host_api__rankProduct_Result HostTypes IO
rankProductValue = P.Host_api__rankProduct_Result polyId ()

rankProductResult :: IO (Bool, Char)
rankProductResult = case rankProductValue of
  P.Host_api__rankProduct_Result apply () -> do
    boolId <- apply @Bool
    bool <- boolId False
    charId <- apply @Char
    char <- charId 'q'
    pure (bool, char)

rankSumValue :: P.Host_api__rankSum_Arg0 HostTypes IO
rankSumValue = P.Host_api__rankSum_Arg0_Case0_Position0 polyId

rankSumResult :: IO Bool
rankSumResult = case rankSumValue of
  P.Host_api__rankSum_Arg0_Case0_Position0 apply -> apply @Bool >>= \identity -> identity True
  P.Host_api__rankSum_Arg0_Case1_Position1 () -> pure False

aliasRankProductValue :: P.Host_api__aliasRankProduct_Result HostTypes IO
aliasRankProductValue = P.Host_api__aliasRankProduct_Result polyId polyId

newtypeRankProductValue :: P.Host_api__newtypeRankProduct_Result HostTypes IO
newtypeRankProductValue = P.Host_api__newtypeRankProduct_Result polyId polyId

aliasRankResults :: IO (Bool, Char)
aliasRankResults = case aliasRankProductValue of
  P.Host_api__aliasRankProduct_Result aliasId expandedId -> do
    boolId <- aliasId @Bool
    bool <- boolId False
    charId <- expandedId @Char
    char <- charId 'a'
    pure (bool, char)

newtypeRankResults :: IO (Bool, Char)
newtypeRankResults = case newtypeRankProductValue of
  P.Host_api__newtypeRankProduct_Result newtypeId expandedId -> do
    boolId <- newtypeId @Bool
    bool <- boolId True
    charId <- expandedId @Char
    char <- charId 'b'
    pure (bool, char)

main :: IO ()
main = do
  _ <-
    P.export__api__runStagedCalls
      package
  _ <- pure productSubstitution
  _ <- pure sumSubstitution
  _ <- pure aliasProductEquality
  _ <- pure aliasSumEquality
  _ <- pure newtypeProductEquality
  _ <- pure newtypeSumEquality
  _ <- genericProduct (7 :: Int, (False, 'x'))
  _ <- directProduct (7 :: Int, (False, 'x'))
  _ <- genericSum genericSumValue
  if productSelectors == (7, False, 'x')
    then pure ()
    else error "flat product selectors changed"
  rankResult <- rankProductResult
  if rankResult == (False, 'q')
    then pure ()
    else error "rank-N product pattern changed"
  choice <- rankSumResult
  if choice
    then pure ()
    else error "rank-N sum pattern changed"
  aliases <- aliasRankResults
  newtypes <- newtypeRankResults
  if aliases == (False, 'a') && newtypes == (True, 'b')
    then pure ()
    else error "rank-N alias/newtype equality changed"
  publicRank <- publicRankRoundTrip
  if publicRank
    then pure ()
    else error "rank-N public input/export staging changed"
