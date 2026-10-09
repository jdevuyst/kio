{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE TypeFamilies #-}

module Host where

import qualified BottomAbi as P
import qualified Data.Void

data HostTypes

instance P.BottomAbiHostTypes HostTypes

host :: P.BottomAbiHost HostTypes IO
host =
  P.BottomAbiHost
    { P.host__api__stop = error "unreachable stop"
    , P.host__api__discard = \value -> Data.Void.absurd value
    , P.host__api__inspect = \((), impossible) ->
        Data.Void.absurd impossible
    , P.host__api__branch = pure (P.Host_api__branch_Result_Case0_Position0 ())
    }

package :: P.BottomAbi HostTypes IO
package = P.createBottomAbi host

main :: IO ()
main = pure ()
