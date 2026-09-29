{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE TypeFamilies #-}

module SafeHost where

import qualified ScopedConstraints as P
import ScopedConstraints (ScopedConstraintsHostTypes(..))

data Marker
data ProducerCount
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

safe :: IO ()
safe = P.export__api__safe package
