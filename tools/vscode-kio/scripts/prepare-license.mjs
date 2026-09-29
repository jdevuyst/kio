import { readFileSync, writeFileSync } from 'node:fs';

const mit = readFileSync(new URL('../../../LICENSE-MIT', import.meta.url), 'utf8');
const apache = readFileSync(new URL('../../../LICENSE-APACHE', import.meta.url), 'utf8');

writeFileSync(
  new URL('../LICENSE.txt', import.meta.url),
  `MIT OR Apache-2.0\n\n${mit}\n${apache}`,
);
