'use strict';

// Platforms for which release packages ship a prebuilt native engine.
const supportedPlatforms = ['darwin-arm64', 'darwin-x64', 'linux-x64-gnu', 'win32-x64-msvc'];

function platformKey() {
  // Test-only override so unsupported platforms can be simulated without cargo.
  if (process.env.CEDRODB_TEST_PLATFORM_KEY) return process.env.CEDRODB_TEST_PLATFORM_KEY;
  if (process.platform === 'linux') {
    const glibc = process.report?.getReport().header.glibcVersionRuntime;
    return `linux-${process.arch}-${glibc ? 'gnu' : 'musl'}`;
  }
  if (process.platform === 'win32') return `win32-${process.arch}-msvc`;
  return `${process.platform}-${process.arch}`;
}

module.exports = { platformKey, supportedPlatforms };
