#!/usr/bin/env node
// Compare numcodecs.js with the Rust decoder harness on a directory of raw Zarr chunks.
// Install numcodecs outside the workspace, then set NUMCODECS_ROOT to that installation root.

import { readdirSync, readFileSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const [chunkDir] = process.argv.slice(2);
if (!chunkDir || process.argv.length !== 3) {
  throw new Error('usage: benchmark_numcodecs.mjs CHUNK_DIRECTORY');
}

const packageRoot = process.env.NUMCODECS_ROOT;
if (!packageRoot) {
  throw new Error('set NUMCODECS_ROOT to an installation containing node_modules/numcodecs');
}
const { Blosc, LZ4, Zstd } = await import(pathToFileURL(
  join(resolve(packageRoot), 'node_modules/numcodecs/dist/index.js'),
));
const chunks = listFiles(resolve(chunkDir)).map(path => new Uint8Array(readFileSync(path)));
if (chunks.length === 0 || chunks.some(chunk => chunk.length === 0)) {
  throw new Error('chunk directory must contain non-empty files');
}

const codecs = [
  ['zstd', Zstd.fromConfig({ id: 'zstd', level: 3 })],
  ['lz4', LZ4.fromConfig({ id: 'lz4', acceleration: 1 })],
  ['blosc-lz4-shuffle', Blosc.fromConfig({
    id: 'blosc', cname: 'lz4', clevel: 5, shuffle: 1, blocksize: 0,
  })],
];
const checksums = chunks.map(checksum);
const results = [];
for (const [name, codec] of codecs) {
  const encoded = await Promise.all(chunks.map(chunk => codec.encode(chunk)));
  for (let iteration = 0; iteration < 3; iteration += 1) {
    await verify(codec, encoded, checksums, name);
  }
  const started = performance.now();
  let decodedBytes = 0;
  for (let iteration = 0; iteration < 20; iteration += 1) {
    decodedBytes += await verify(codec, encoded, checksums, name);
  }
  const seconds = (performance.now() - started) / 1_000;
  results.push({
    codec: name,
    encodedBytes: encoded.reduce((total, chunk) => total + chunk.byteLength, 0),
    decodeMiBPerSecond: Number((decodedBytes / seconds / 1024 / 1024).toFixed(3)),
  });
}

console.log(JSON.stringify({
  kind: 'chunk-corpus-numcodecs',
  packageVersion: 'numcodecs@0.3.2',
  chunkCount: chunks.length,
  payloadBytes: chunks.reduce((total, chunk) => total + chunk.byteLength, 0),
  warmupIterations: 3,
  measuredIterations: 20,
  results,
}));

async function verify(codec, encoded, expected, name) {
  let decodedBytes = 0;
  for (let index = 0; index < encoded.length; index += 1) {
    const decoded = await codec.decode(encoded[index]);
    if (checksum(decoded) !== expected[index]) {
      throw new Error(`${name} checksum mismatch for chunk ${index}`);
    }
    decodedBytes += decoded.byteLength;
  }
  return decodedBytes;
}

function listFiles(directory) {
  return readdirSync(directory, { withFileTypes: true })
    .flatMap(entry => entry.isDirectory()
      ? listFiles(join(directory, entry.name))
      : [join(directory, entry.name)])
    .sort();
}

function checksum(bytes) {
  let hash = 0x811c9dc5;
  for (const byte of bytes) {
    hash = Math.imul(hash ^ byte, 0x01000193) >>> 0;
  }
  return hash;
}
