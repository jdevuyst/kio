import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const [mode, scratch, planFile, backend, checker, ...checkerArgs] = process.argv.slice(2);
const label = backend === 'ts' ? 'tsc-strict' : 'pyright-strict';
const pyrightConfig = path.join(path.dirname(planFile ?? '.'), 'pyrightconfig.json');

function fail(message) {
  throw new Error(message);
}

function descendant(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative !== '' && relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

// Resolve existing ancestors without requiring the planned output to exist.
function physical(candidate) {
  try {
    return fs.realpathSync.native(candidate);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    try {
      if (fs.lstatSync(candidate).isSymbolicLink()) {
        const target = fs.readlinkSync(candidate);
        return physical(path.isAbsolute(target) ? target : `${path.dirname(candidate)}${path.sep}${target}`);
      }
    } catch (statError) {
      if (statError.code !== 'ENOENT') throw statError;
    }
    const parent = path.dirname(candidate);
    if (parent === candidate) throw error;
    return path.join(physical(parent), path.basename(candidate));
  }
}

function owned(root, candidate) {
  const parent = physical(path.dirname(candidate));
  if (!path.isAbsolute(candidate) || !descendant(root, path.resolve(candidate)) ||
      !descendant(root, physical(candidate)) || (parent !== root && !descendant(root, parent))) {
    fail(`planned output is not strictly inside the private workdir: ${JSON.stringify(candidate)}`);
  }
}

function regular(file) {
  try {
    return fs.lstatSync(file).isFile();
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
}

function checkOutput(row) {
  let valid = true;
  const report = message => {
    console.error(`${label}: ${message}`);
    valid = false;
  };
  // Resolve the built directory before host tools normalize their input paths.
  const output = physical(row.output_dir);
  const runtime = path.join(output, `${row.namespace}.${backend === 'ts' ? 'js' : 'py'}`);
  if (!regular(runtime)) report(`missing generated runtime ${JSON.stringify(runtime)}`);
  let input;
  if (backend === 'ts') {
    input = path.join(output, `${row.namespace}.d.ts`);
    if (!regular(input)) {
      report(`no declaration sidecar at ${JSON.stringify(input)}`);
      return false;
    }
  } else {
    input = path.join(output, row.namespace);
    const entrypoint = path.join(input, '__init__.pyi');
    if (!regular(entrypoint)) {
      report(`\`${runtime}\` has no generated stub package`);
      return false;
    }
    const exports = fs.readFileSync(entrypoint, 'utf8').split('\n');
    const shards = fs.readdirSync(input).filter(name => /^_kio_stub_.*\.pyi$/.test(name) && regular(path.join(input, name)));
    if (shards.length === 0) report(`\`${input}\` has no generated declaration shard`);
    for (const shard of shards) {
      if (!exports.includes(`from .${shard.slice(0, -4)} import *`)) {
        report(`\`${entrypoint}\` does not re-export \`${shard}\``);
      }
    }
  }
  const args = backend === 'python' ? [...checkerArgs, '--project', pyrightConfig, input] : [...checkerArgs, input];
  const result = spawnSync(checker, args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  if (result.error || result.status !== 0) {
    report(`\`${input}\` failed strict type-check:`);
    const diagnostic = result.error?.message ?? `${result.stdout ?? ''}${result.stderr ?? ''}`;
    console.error(diagnostic.split('\n').map(line => `    ${line}`).join('\n'));
  }
  return valid;
}

try {
  if (!['prepare', 'check'].includes(mode) || !['ts', 'python'].includes(backend)) fail('invalid helper arguments');
  const root = fs.realpathSync.native(scratch);
  const rows = JSON.parse(fs.readFileSync(planFile, 'utf8'));
  if (!Array.isArray(rows) || rows.length === 0) fail('successful output-plan query returned no selected targets');
  for (const row of rows) {
    if (!row || typeof row.package_root !== 'string' || !path.isAbsolute(row.package_root) ||
        typeof row.target !== 'string' || typeof row.backend !== 'string' || typeof row.output_dir !== 'string' ||
        !(row.namespace === null || typeof row.namespace === 'string')) fail('invalid output-plan row');
    owned(root, row.output_dir);
    if (row.backend === backend && (typeof row.namespace !== 'string' || row.namespace === '' ||
        row.namespace === '.' || row.namespace === '..' || /[\/\\\0]/.test(row.namespace))) fail('invalid output-plan namespace');
  }
  const applicable = rows.filter(row => row.backend === backend);
  if (mode === 'prepare') {
    // Validate the whole inventory before removing any exact selected output.
    for (const row of applicable) {
      const leaf = row.output_dir.replace(process.platform === 'win32' ? /[\/\\]+$/ : /\/+$/, '');
      try {
        if (fs.lstatSync(leaf).isSymbolicLink()) {
          fs.unlinkSync(leaf);
          continue;
        }
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
      }
      fs.rmSync(row.output_dir, { recursive: true, force: true });
    }
  } else {
    if (!checker) fail('checker executable is missing');
    if (backend === 'python') {
      fs.writeFileSync(pyrightConfig, JSON.stringify({ typeCheckingMode: 'strict', pythonVersion: '3.10' }));
    }
    let valid = true;
    for (const row of applicable) if (!checkOutput(row)) valid = false;
    if (!valid) process.exitCode = 1;
  }
} catch (error) {
  console.error(`${label}: ${error.message}`);
  process.exitCode = 1;
}
