import { createRequire } from 'node:module';
import { access, copyFile, mkdir } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { platformKey } = require('../platform.cjs');
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const output = path.join(packageRoot, 'prebuilds', platformKey(), 'cedrodb.node');

if (process.argv.includes('--if-needed')) {
  try {
    await access(output);
    require(output); // Reject incompatible/corrupt artifacts before accepting one.
    process.exit(0);
  } catch { /* Build from the vendored Rust source below. */ }
}

await access(path.join(packageRoot, 'core', 'Cargo.toml')).catch(() => {
  throw new Error('Vendored engine source missing. In a checkout, run npm run build first.');
});
const targetDir = path.join(packageRoot, 'target');
const build = spawnSync('cargo', [
  'build', '--release', '--locked', '--manifest-path', path.join(packageRoot, 'Cargo.toml'),
  '--target-dir', targetDir,
], { cwd: packageRoot, stdio: 'inherit', env: process.env });
if (build.error) {
  console.error('CedroDB source builds require Rust >= 1.88 (cargo) and a C linker.');
  console.error(build.error.message);
  process.exit(1);
}
if (build.status !== 0) process.exit(build.status ?? 1);
const library = process.platform === 'win32' ? 'cedrodb_node.dll'
  : process.platform === 'darwin' ? 'libcedrodb_node.dylib' : 'libcedrodb_node.so';
await mkdir(path.dirname(output), { recursive: true });
await copyFile(path.join(targetDir, 'release', library), output);
require(output);
console.log(`CedroDB native engine ready: ${platformKey()}`);
