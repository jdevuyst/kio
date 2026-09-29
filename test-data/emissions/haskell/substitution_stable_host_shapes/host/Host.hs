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
   . a
  -> t
  -> IO (P.Env_H01100100000001000000036170690000000f67656e657269635f70726f647563740200000000__api__genericProduct_ret HostTypes IO a t)
genericProduct first second =
  pure (P.EnvP_H01200100000001000000036170690000000f67656e657269635f70726f647563740200000000__api__genericProduct_ret_P first second)

directProduct
  :: forall (a :: Type) (b :: Type) (c :: Type)
   . a
  -> b
  -> c
  -> IO (P.Env_H01100100000001000000036170690000000e6469726563745f70726f647563740200000000__api__directProduct_ret HostTypes IO a b c)
directProduct first second third =
  pure (P.EnvP_H01200100000001000000036170690000000e6469726563745f70726f647563740200000000__api__directProduct_ret_P first second third)

genericSum
  :: forall (a :: Type) (t :: Type)
   . P.Env_H01100100000001000000036170690000000b67656e657269635f73756d010000000000000000__api__genericSum_arg0 HostTypes IO a t
  -> IO (P.Env_H01100100000001000000036170690000000b67656e657269635f73756d0200000000__api__genericSum_ret HostTypes IO a t)
genericSum = pure

directSum
  :: forall (a :: Type) (b :: Type) (c :: Type)
   . P.Env_H01100100000001000000036170690000000a6469726563745f73756d010000000000000000__api__directSum_arg0 HostTypes IO a b c
  -> IO (P.Env_H01100100000001000000036170690000000a6469726563745f73756d0200000000__api__directSum_ret HostTypes IO a b c)
directSum = pure

rankProduct
  :: PolyId
  -> ()
  -> IO (P.Env_H01100100000001000000036170690000000c72616e6b5f70726f647563740200000000__api__rankProduct_ret HostTypes IO)
rankProduct identity unit =
  pure (P.EnvP_H01200100000001000000036170690000000c72616e6b5f70726f647563740200000000__api__rankProduct_ret_P identity unit)

rankSum
  :: P.Env_H01100100000001000000036170690000000872616e6b5f73756d010000000000000000__api__rankSum_arg0 HostTypes IO
  -> IO (P.Env_H01100100000001000000036170690000000872616e6b5f73756d0200000000__api__rankSum_ret HostTypes IO)
rankSum = pure

aliasRankProduct
  :: PolyId
  -> PolyId
  -> IO (P.Env_H011001000000010000000361706900000012616c6961735f72616e6b5f70726f647563740200000000__api__aliasRankProduct_ret HostTypes IO)
aliasRankProduct first second =
  pure (P.EnvP_H012001000000010000000361706900000012616c6961735f72616e6b5f70726f647563740200000000__api__aliasRankProduct_ret_P first second)

aliasRankSum
  :: P.Env_H01100100000001000000036170690000000e616c6961735f72616e6b5f73756d010000000000000000__api__aliasRankSum_arg0 HostTypes IO
  -> IO (P.Env_H01100100000001000000036170690000000e616c6961735f72616e6b5f73756d0200000000__api__aliasRankSum_ret HostTypes IO)
aliasRankSum = pure

newtypeRankProduct
  :: PolyId
  -> PolyId
  -> IO (P.Env_H0110010000000100000003617069000000146e6577747970655f72616e6b5f70726f647563740200000000__api__newtypeRankProduct_ret HostTypes IO)
newtypeRankProduct first second =
  pure (P.EnvP_H0120010000000100000003617069000000146e6577747970655f72616e6b5f70726f647563740200000000__api__newtypeRankProduct_ret_P first second)

newtypeRankSum
  :: P.Env_H0110010000000100000003617069000000106e6577747970655f72616e6b5f73756d010000000000000000__api__newtypeRankSum_arg0 HostTypes IO
  -> IO (P.Env_H0110010000000100000003617069000000106e6577747970655f72616e6b5f73756d0200000000__api__newtypeRankSum_ret HostTypes IO)
newtypeRankSum = pure

scopedApplication
  :: forall (f :: Type -> Type) (a :: Type) (b :: Type)
   . f (P.Env_H01100100000001000000036170690000001273636f7065645f6170706c69636174696f6e0100000000000000010100000000__api__scopedApplication_arg0_app0 HostTypes IO a b)
  -> IO (f (P.Env_H01100100000001000000036170690000001273636f7065645f6170706c69636174696f6e02000000010100000000__api__scopedApplication_ret_app0 HostTypes IO a b))
scopedApplication = pure

stagedHost :: forall a. a -> IO (forall b. IO (forall c. IO (b -> c -> IO c)))
stagedHost _ = pure second
  where
    second :: forall b. IO (forall c. IO (b -> c -> IO c))
    second = pure third
      where
        third :: forall c. IO (b -> c -> IO c)
        third = pure $ \_ value -> pure value

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
  :: P.Env_H011001000000010000000361706900000012616c6961735f72616e6b5f70726f647563740200000000__api__aliasRankProduct_ret HostTypes IO
     :~: P.SubstitutionStableHostShapesProduct '[PolyId, PolyId]
aliasProductEquality = Refl

aliasSumEquality
  :: P.Env_H01100100000001000000036170690000000e616c6961735f72616e6b5f73756d010000000000000000__api__aliasRankSum_arg0 HostTypes IO
     :~: P.SubstitutionStableHostShapesSum '[PolyId, PolyId]
aliasSumEquality = Refl

newtypeProductEquality
  :: P.Env_H0110010000000100000003617069000000146e6577747970655f72616e6b5f70726f647563740200000000__api__newtypeRankProduct_ret HostTypes IO
     :~: P.SubstitutionStableHostShapesProduct '[PolyId, PolyId]
newtypeProductEquality = Refl

newtypeSumEquality
  :: P.Env_H0110010000000100000003617069000000106e6577747970655f72616e6b5f73756d010000000000000000__api__newtypeRankSum_arg0 HostTypes IO
     :~: P.SubstitutionStableHostShapesSum '[PolyId, PolyId]
newtypeSumEquality = Refl

directProductValue
  :: P.Env_H01100100000001000000036170690000000e6469726563745f70726f647563740200000000__api__directProduct_ret HostTypes IO Int Bool Char
directProductValue = P.EnvP_H01200100000001000000036170690000000e6469726563745f70726f647563740200000000__api__directProduct_ret_P 7 False 'x'

genericProductValue
  :: P.Env_H01100100000001000000036170690000000f67656e657269635f70726f647563740200000000__api__genericProduct_ret HostTypes IO Int (Bool, Char)
genericProductValue = directProductValue

productSelectors :: (Int, Bool, Char)
productSelectors = case directProductValue of
  P.EnvP_H01200100000001000000036170690000000e6469726563745f70726f647563740200000000__api__directProduct_ret_P first second third ->
    (first, second, third)

directSumValue :: P.Env_H01100100000001000000036170690000000a6469726563745f73756d010000000000000000__api__directSum_arg0 HostTypes IO Int Bool Char
directSumValue = P.EnvS_H01210100000001000000036170690000000a6469726563745f73756d010000000000000000030000000200000002__api__directSum_arg0_S_pos2_2 'z'

genericSumValue
  :: P.Env_H01100100000001000000036170690000000b67656e657269635f73756d010000000000000000__api__genericSum_arg0 HostTypes IO Int (Either Bool Char)
genericSumValue = directSumValue

rankProductValue :: P.Env_H01100100000001000000036170690000000c72616e6b5f70726f647563740200000000__api__rankProduct_ret HostTypes IO
rankProductValue = P.EnvP_H01200100000001000000036170690000000c72616e6b5f70726f647563740200000000__api__rankProduct_ret_P polyId ()

rankProductResult :: IO (Bool, Char)
rankProductResult = case rankProductValue of
  P.EnvP_H01200100000001000000036170690000000c72616e6b5f70726f647563740200000000__api__rankProduct_ret_P apply () -> do
    boolId <- apply @Bool
    bool <- boolId False
    charId <- apply @Char
    char <- charId 'q'
    pure (bool, char)

rankSumValue :: P.Env_H01100100000001000000036170690000000872616e6b5f73756d010000000000000000__api__rankSum_arg0 HostTypes IO
rankSumValue = P.EnvS_H01210100000001000000036170690000000872616e6b5f73756d010000000000000000030000000000000000__api__rankSum_arg0_S_pos0_0 polyId

rankSumResult :: IO Bool
rankSumResult = case rankSumValue of
  P.EnvS_H01210100000001000000036170690000000872616e6b5f73756d010000000000000000030000000000000000__api__rankSum_arg0_S_pos0_0 apply -> apply @Bool >>= \identity -> identity True
  P.EnvS_H01210100000001000000036170690000000872616e6b5f73756d010000000000000000030000000100000001__api__rankSum_arg0_S_pos1_1 () -> pure False

aliasRankProductValue :: P.Env_H011001000000010000000361706900000012616c6961735f72616e6b5f70726f647563740200000000__api__aliasRankProduct_ret HostTypes IO
aliasRankProductValue = P.EnvP_H012001000000010000000361706900000012616c6961735f72616e6b5f70726f647563740200000000__api__aliasRankProduct_ret_P polyId polyId

newtypeRankProductValue :: P.Env_H0110010000000100000003617069000000146e6577747970655f72616e6b5f70726f647563740200000000__api__newtypeRankProduct_ret HostTypes IO
newtypeRankProductValue = P.EnvP_H0120010000000100000003617069000000146e6577747970655f72616e6b5f70726f647563740200000000__api__newtypeRankProduct_ret_P polyId polyId

aliasRankResults :: IO (Bool, Char)
aliasRankResults = case aliasRankProductValue of
  P.EnvP_H012001000000010000000361706900000012616c6961735f72616e6b5f70726f647563740200000000__api__aliasRankProduct_ret_P aliasId expandedId -> do
    boolId <- aliasId @Bool
    bool <- boolId False
    charId <- expandedId @Char
    char <- charId 'a'
    pure (bool, char)

newtypeRankResults :: IO (Bool, Char)
newtypeRankResults = case newtypeRankProductValue of
  P.EnvP_H0120010000000100000003617069000000146e6577747970655f72616e6b5f70726f647563740200000000__api__newtypeRankProduct_ret_P newtypeId expandedId -> do
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
  _ <- genericProduct (7 :: Int) (False, 'x')
  _ <- directProduct (7 :: Int) False 'x'
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
