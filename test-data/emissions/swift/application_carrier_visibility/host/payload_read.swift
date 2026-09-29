import CarrierControls

struct One: CarrierControlsHost {}
let pkg = createCarrierControls(host: One())
let value = pkg.KioNewtypeApplication_api__Opaque_lift(pkg.api.opaque(42, "opaque"))
let leaked: Int = value.value(as: Int.self)
