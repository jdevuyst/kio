# Materialized loader contracts

```kio {}
module contracts;

import docs_loader/loader as loader;
import docs_loader/testapi as host;

fn expected_contract() -> loader.Contract {
  loader.contract_cons(
    , loader.mk_expect(
        , "add3"(host.String)
        , "(h{hostapi.I32} & (h{hostapi.I32} & h{hostapi.I32})) -> h{hostapi.I32}"(host.String)
        )
    , loader.contract_nil()
    )
}
```
