import CarrierControls

struct One: CarrierControlsHost {}
struct Two: CarrierControlsHost {}
let pkg = createCarrierControls(host: One())
let value = pkg.api.opaque(42, "opaque")
let wrong: KioNewtype_api__Opaque<Two, Int, String> = value
