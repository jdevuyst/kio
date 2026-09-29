import CarrierControls

struct One: CarrierControlsHost {}
let forged = KioApply1<KioApply1<KioNewtypeMk_api__Opaque<One>, Int>, String>(42)
