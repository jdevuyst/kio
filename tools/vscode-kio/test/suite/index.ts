// Mocha runner — discovers every `*.test.ts` under `test/suite/`
// (compiled to `*.test.js` in `out/`) and runs them inside the
// VS Code instance launched by `@vscode/test-electron`.

import * as path from "node:path";
import { glob } from "glob";
import Mocha from "mocha";

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: "tdd", color: true, timeout: 30_000,
    grep: process.env.KIO_DEBUG_VSCODE_TEST_FILTER,
  });
  const testsRoot = path.resolve(__dirname);
  return new Promise(async (resolve, reject) => {
    try {
      const files = await glob("**/*.test.js", { cwd: testsRoot });
      for (const f of files) mocha.addFile(path.resolve(testsRoot, f));
      mocha.run((failures) => {
        if (failures > 0) reject(new Error(`${failures} tests failed.`));
        else resolve();
      });
    } catch (err) {
      reject(err);
    }
  });
}
