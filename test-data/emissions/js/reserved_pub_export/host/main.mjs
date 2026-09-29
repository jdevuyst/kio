import { createJrpg } from '../out/js/jrpg.js';

const pkg = createJrpg({});
if (pkg.jrpg.KioModule_main.return() !== 'ok') {
  throw new Error('reserved public item returned the wrong value');
}
