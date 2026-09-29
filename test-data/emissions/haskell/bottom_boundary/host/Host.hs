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
    , P.host__api__inspect = \() impossible ->
        Data.Void.absurd impossible
    , P.host__api__branch = pure (P.EnvS_H0121010000000100000003617069000000066272616e63680200000000030000000000000000__api__branch_ret_S_pos0_0 ())
    }

package :: P.BottomAbi HostTypes IO
package = P.createBottomAbi host

main :: IO ()
main = pure ()
