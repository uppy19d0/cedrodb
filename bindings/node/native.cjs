'use strict';
const path = require('node:path');
const { platformKey, supportedPlatforms } = require('./platform.cjs');
const binary = path.join(__dirname, 'prebuilds', platformKey(), 'cedrodb.node');

try {
  module.exports = require(binary);
} catch (cause) {
  throw new Error(
    `CedroDB could not load its native engine for platform ${platformKey()}. ` +
    `Supported prebuilt platforms: ${supportedPlatforms.join(', ')}. ` +
    'Run npm rebuild cedrodb with Rust >= 1.88 and a C linker installed. ' +
    'The source distribution builds locally when no matching prebuilt binary is included.',
    { cause },
  );
}
