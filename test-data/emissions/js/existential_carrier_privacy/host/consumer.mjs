import {
  createCarrierPrivacy,
  liftCarrierPrivacyCallable,
  liftCarrierPrivacyPoly,
} from "../workdir/out/js/carrier_privacy.js";

const pkg = createCarrierPrivacy({});
const foreign = createCarrierPrivacy({});
const secret = { witness: 37 };
const packed = pkg.api.hold(secret);
if (Reflect.ownKeys(packed).length !== 0 || "Pack" in packed) {
  throw new Error("existential payload is publicly inspectable");
}
let openings = 0;
const continuation = liftCarrierPrivacyPoly({
  advance: () => liftCarrierPrivacyCallable(value => {
    openings++;
    if (value !== secret) throw new Error("abstract witness identity changed");
    return null;
  }),
});
const result = pkg.api.KioType_Pack.open(packed);
if (openings !== 0) throw new Error("projector entered its continuation early");
result.advance()(continuation);
if (openings !== 1) throw new Error("continuation must execute once");
let rejected = false;
try {
  foreign.api.KioType_Pack.open(packed).advance()(continuation);
} catch (error) {
  if (!(error instanceof TypeError)) throw error;
  rejected = true;
}
if (!rejected || openings !== 1) throw new Error("foreign existential carrier admitted");
console.log("existential carrier privacy preserved");
