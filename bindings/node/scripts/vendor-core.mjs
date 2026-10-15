import { cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const sourceRoot = path.resolve(packageRoot, '../..');
const coreRoot = path.join(packageRoot, 'core');
// Only engine sources enter the package. Never vendor another binding, build
// products, local databases, credentials, or the whole developer checkout.
const allowlist = ['Cargo.toml', 'Cargo.lock', 'src', 'LICENSE'];
const manifest = await readFile(path.join(sourceRoot, 'Cargo.toml'), 'utf8').catch(() => null);
if (manifest?.includes('name = "cedrodb"')) {
  await rm(coreRoot, { recursive: true, force: true });
  await mkdir(coreRoot, { recursive: true });
  for (const entry of allowlist) {
    await cp(path.join(sourceRoot, entry), path.join(coreRoot, entry), { recursive: true });
  }
  await cp(path.join(sourceRoot, 'LICENSE'), path.join(packageRoot, 'LICENSE'));
} else {
  // A packed package already contains the engine, so repacking remains possible.
  const vendored = await readFile(path.join(coreRoot, 'Cargo.toml'), 'utf8').catch(() => null);
  if (!vendored?.includes('name = "cedrodb"')) {
    throw new Error('CedroDB core source is missing; run npm pack from the engine checkout.');
  }
}
// Add no absolute paths or timestamps: equal sources produce equal package files.
await writeFile(path.join(coreRoot, '.cedrodb-vendor'), 'CedroDB engine sources for the native npm binding.\n');
