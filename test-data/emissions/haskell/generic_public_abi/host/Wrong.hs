module Wrong where

import Host (package)
import qualified PublicAbi as P

wrong :: IO Bool
wrong = P.export__api__Pair__mkPair package True
