import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../../../../kio-rs/src/backends/js/value_runtime.js', import.meta.url), 'utf8');
const realm = vm.createContext({});
export const loadRuntime = (context = realm) => vm.runInContext('(() => {\n' + source + '\nreturn { context: __kioNominalContext, poly: __kioPoly, guest: __kioGuest, call: __kioTailCall, advance: __kioTailAdvance }; })()', context);
export const runtime = loadRuntime();
