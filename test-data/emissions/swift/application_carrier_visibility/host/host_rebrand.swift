import CarrierControls

struct One: CarrierControlsHost {}
struct Two: CarrierControlsHost {}
let pkg = createCarrierControls(host: One())
let value = pkg.KioNewtypeApplication_api__Opaque_lift(pkg.api.opaque(42, "opaque"))
let wrong: KioNewtype_api__Opaque<Two, Int, String> =
    createCarrierControls(host: Two()).KioNewtypeApplication_api__Opaque_project(value)
