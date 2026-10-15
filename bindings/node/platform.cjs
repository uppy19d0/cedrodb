'use strict';

function platformKey() {
  if (process.platform === 'linux') {
    const glibc = process.report?.getReport().header.glibcVersionRuntime;
    return `linux-${process.arch}-${glibc ? 'gnu' : 'musl'}`;
  }
  if (process.platform === 'win32') return `win32-${process.arch}-msvc`;
  return `${process.platform}-${process.arch}`;
}

module.exports = { platformKey };
