{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE GeneralizedNewtypeDeriving #-}
{-# LANGUAGE TypeFamilies #-}

module ConstrainedHost where

import qualified ScopedConstraints as P
import ScopedConstraints (ScopedConstraintsHostTypes(..))

data Marker
newtype ProducerCount = ProducerCount Integer
  deriving (Num)
data DecoyCount

instance P.ScopedConstraintsHostTypes Marker where
  type HostType__producer__Count Marker = ProducerCount
  type HostType__decoy__Count Marker = DecoyCount

host :: P.ScopedConstraintsHost Marker IO
host =
  P.ScopedConstraintsHost
    { P.host__producer__consume = \_ -> pure ()
    }

package :: P.ScopedConstraints Marker IO
package = P.createScopedConstraints host

direct :: IO ProducerCount
direct = P.export__api__direct package

byUnqualified :: IO ProducerCount
byUnqualified = P.export__api__byUnqualified package

byValue :: IO ProducerCount
byValue = P.export__api__byValue package

byPartial :: IO ProducerCount
byPartial = P.export__api__byPartial package

byClosure :: IO ProducerCount
byClosure = P.export__api__byClosure package
