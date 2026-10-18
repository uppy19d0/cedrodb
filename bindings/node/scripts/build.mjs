import { createRequire } from 'node:module';
import { access, copyFile, mkdir } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { platformKey, supportedPlatforms } = require('../platform.cjs');
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const output = path.join(packageRoot, 'prebuilds', platformKey(), 'cedrodb.node');

const ifNeeded = process.argv.includes('--if-needed');
if (ifNeeded) {
  try {
    await access(output);
    require(output); // Reject incompatible/corrupt artifacts before accepting one.
    process.exit(0);
  } catch { /* Build from the vendored Rust source below. */ }
}

// With --if-needed (npm install), failing here must not fail the install:
// the package stays installed and require('cedrodb') reports the problem.
function giveUp(reason, code) {
  if (!ifNeeded) {
    console.error(reason);
    process.exit(code);
  }
  console.warn(
    `warning: cedrodb: no usable native engine for platform "${platformKey()}" and it could not be built (${reason}).\n` +
    `  Supported prebuilt platforms: ${supportedPlatforms.join(', ')}.\n` +
    '  Install will continue, but require("cedrodb") will throw until an engine is available.\n' +
    '  To build from source: install Rust >= 1.88 (cargo) and a C linker, then run `npm rebuild cedrodb`.',
  );
  process.exit(0);
}

await access(path.join(packageRoot, 'core', 'Cargo.toml')).catch(() => {
  giveUp('vendored engine source missing; in a checkout, run npm run build first', 1);
});
const targetDir = path.join(packageRoot, 'target');
const build = spawnSync('cargo', [
  'build', '--release', '--locked', '--manifest-path', path.join(packageRoot, 'Cargo.toml'),
  '--target-dir', targetDir,
], { cwd: packageRoot, stdio: 'inherit', env: process.env });
if (build.error) {
  giveUp(`source builds require Rust >= 1.88 (cargo) and a C linker: ${build.error.message}`, 1);
}
if (build.status !== 0) giveUp(`cargo build exited with status ${build.status}`, build.status ?? 1);
const library = process.platform === 'win32' ? 'cedrodb_node.dll'
  : process.platform === 'darwin' ? 'libcedrodb_node.dylib' : 'libcedrodb_node.so';
try {
  await mkdir(path.dirname(output), { recursive: true });
  await copyFile(path.join(targetDir, 'release', library), output);
} catch (error) {
  giveUp(`could not install the built library: ${error.message}`, 1);
}
require(output);
console.log(`CedroDB native engine ready: ${platformKey()}`);
