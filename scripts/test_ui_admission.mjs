// Regression test for the inline desktop-render admission boundary.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const html = fs.readFileSync(new URL('../crates/newvolim-ui/index.html', import.meta.url), 'utf8');
assert.match(html, /newvolim browser linear RGBA16F target/,
  'browser raymarch must retain its linear physical-pixel colour target');
assert.match(html, /newvolim browser first-opacity ray distance/,
  'browser raymarch must retain its first-opacity distance attachment');
assert.match(html, /format: "rgba16float" \}, \{ format: "r32float" \}/,
  'volume pass must bind both colour and ray-distance attachment formats');
assert.match(html, /first-opacity ray distance readback/,
  'browser ray-distance attachment must have a CPU readback boundary for picking diagnostics');
assert.match(html, /copyTextureToBuffer\(/,
  'browser ray-distance click handling must copy a physical-pixel depth sample from the attachment');
assert.match(html, /pick_open_dataset_annotation/,
  'native depth clicks must invoke the host-owned camera-ray and paired-depth picker');
assert.match(html, /window\.newvolimNativeRayDistancePfm && !window\.newvolimNativePortableFrame/,
  'only native Palace frames may request desktop annotation picking; portable and remote frames stay diagnostic');
assert.match(html, /prepare_default_portable_image_layer/,
  'local opens must explicitly prepare a source-bound portable layer before invoking WGPU');
assert.match(html, /render_native_portable_camera_draw/,
  'admitted local camera updates must be able to select the native portable recorder');
assert.match(html, /newvolimNativePortableRegion/,
  'native portable draws must derive their chunk region from the source-bound metadata');
assert.match(html, /source\.spatialAxesXyz\.map/,
  'native portable chunk selection must honor the declared spatial axis order');
assert.match(html, /maxResidentChunks/,
  'native portable selection must cap its contiguous source residency to the four-page contract');
assert.match(html, /residentExtentXyz/,
  'native portable selection must form a bounded contiguous three-dimensional region');
assert.match(html, /window\.newvolimNativePortableDrawRequest\(canvas, camera\)/,
  'native portable render and pick paths must reconstruct the same source-derived draw request');
assert.match(html, /pick_native_portable_annotation/,
  'portable frames must route annotation picks through their matching host-owned recorder');
assert.match(html, /GPUTextureUsage\.COPY_SRC/,
  'ray-distance attachment must permit its explicit texture-to-buffer readback');
assert.match(html, /new ResizeObserver\(\(\) => \{/,
  'a displayed volume must request a fresh physical frame when its CSS canvas size changes');
assert.match(html, /newvolimScheduleVolumeCamera\(\);/,
  'resize updates must share the existing newest-only camera admission boundary');
assert.match(html, /newvolimBeginPolygonAnnotation/,
  'the desktop UI must expose explicit polygon-ROI drafting');
assert.match(html, /add_polygon_annotation/,
  'finished polygon drafts must be sent through the native physical-coordinate boundary');
assert.match(html, /draft\.plane !== plane/,
  'a polygon draft must reject vertices from a different orthogonal plane');
assert.match(html, /NEWVOLIM_MAX_POLYGON_VERTICES = 4096/,
  'the browser draft must share the persisted polygon vertex limit');
assert.match(html, /maximumDraftPoints/,
  'the browser must bound polygon vertices and two-corner slice ROI drafts before adding points');
assert.match(html, /add_rectangle_annotation/,
  'finished rectangle drafts must use the native physical-coordinate boundary');
assert.match(html, /add_ellipse_annotation/,
  'finished ellipse drafts must use the native physical-coordinate boundary');
assert.match(html, /kind, plane: null, points: \[\]/,
  'a two-corner ROI must let its first click choose any linked slice plane');
assert.match(html, /if \(!draft\.plane\) draft\.plane = plane/,
  'the selected slice plane must be fixed after the first ROI corner');
assert.match(html, /activeRequestId: null/,
  'remote frame admission must retain the active request identity');
assert.match(html, /response\.requestId !== remote\.activeRequestId/,
  'remote replies must be correlated to the active request before they are drawn');
assert.match(html, /remote\.socket !== socket/,
  'a reply from a replaced WebSocket must not affect the current remote view');
assert.match(html, /NEWVOLIM_MAX_REMOTE_FRAME_PIXELS = 16 \* 1024 \* 1024/,
  'remote frame metadata must share the service pixel admission budget');
assert.match(html, /encoded\.length !== encodedBytes/,
  'a PFM sidecar must be bounded before base64 decoding allocates its payload');
assert.match(html, /newvolimValidateRemotePngData/,
  'remote PNG bytes must be checked against the declared physical target before image decoding');
assert.match(html, /decode_zstd_chunk_bounded\(bytes, maxDecodedBytes\)/,
  'browser Zarr preview must use the bounded WASM Zstd decoder');
assert.match(html, /decode_lz4_chunk_bounded\(bytes, maxDecodedBytes\)/,
  'browser Zarr preview must use the bounded WASM LZ4 decoder');
assert.match(html, /decode_blosc_chunk_bounded\(bytes, maxDecodedBytes\)/,
  'browser Zarr preview must use the bounded WASM Blosc decoder');
assert.match(html, /const presentPipeline = device\.createRenderPipeline/,
  'browser canvas presentation must remain a separate display-encoding pass');
assert.match(html, /browser annotation records must be a Uint32Array of 13-word records/,
  'browser GPU compositing must admit only fixed-width portable annotation records');
assert.match(html, /maxAnnotationPrimitives = 4096/,
  'browser GPU compositing must bound projected annotation records like native admission');
assert.match(html, /exceeds the portable 4096-record bound/,
  'browser GPU compositing must reject annotation streams beyond the fixed recorder capacity');
assert.match(html, /var<storage, read> annotations: array<u32>/,
  'browser presentation must bind the portable annotation record buffer directly');
assert.match(html, /hit\.ray_distance <= first_opacity/,
  'browser annotations must compare interpolated record depth to the first-opacity attachment');
assert.match(html, /browser annotation records are not sealed to this browser camera and extent/,
  'browser annotation records must be rejected when projected by another camera or extent');
assert.match(html, /for \(var layer_index = 0u; layer_index < 4u;/,
  'browser fragment shader must evaluate bounded ordered layer descriptors');
assert.match(html, /source_colour = layer_colour \+ source_colour \* \(1\.0 - layer_alpha\)/,
  'browser fragment shader must compose later layers over earlier layers in linear light');
assert.match(html, /srgb_to_linear\(hit\.colour\.r\)/,
  'browser annotation colours must be converted to the linear volume target before presentation');
const script = html.match(/<script>([\s\S]*?)<\/script>/)?.[1];
assert.ok(script, 'index.html must contain the inline UI script');
assert.match(script, /credentials: "omit", redirect: "error"/,
  'direct browser stores must be credential-free and must not follow redirects');
const elements = new Map([
  ['newvolim-volume', {
    width: 200, height: 100, dataset: {}, clientWidth: 200, clientHeight: 100,
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 200, height: 100 }),
    addEventListener() {}, setPointerCapture() {},
  }],
  ['newvolim-remote-url', { value: 'ws://frames.test/v1/frames' }],
  ['newvolim-remote-dataset', { value: 'cells3d' }],
  ['newvolim-render-status', { textContent: '' }],
  ['newvolim-browser-zarr-url', { value: '' }],
  ['newvolim-browser-zarr-level', { value: '0' }],
  ['newvolim-browser-zarr-chunk', { value: '0,0,0' }],
]);
const document = { getElementById: id => elements.get(id) ?? null };
const sockets = [];
class FakeWebSocket {
  static OPEN = 1;
  constructor(url) {
    this.url = url;
    this.readyState = FakeWebSocket.OPEN;
    this.listeners = new Map();
    this.sent = [];
    sockets.push(this);
  }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  send(value) { this.sent.push(value); }
  close() { this.readyState = 3; this.listeners.get('close')?.(); }
  emit(type, event = {}) { this.listeners.get(type)?.(event); }
}
const window = { addEventListener() {} };
const sandbox = {
  window,
  document,
  WebSocket: FakeWebSocket,
  TextDecoder,
  TextEncoder,
  performance,
  atob,
  URL,
  URLSearchParams,
  clearTimeout() {},
  setTimeout() {},
  requestAnimationFrame() {},
  fetch: async () => { throw new Error('test did not configure browser-store fetch'); },
};
vm.runInNewContext(script, sandbox);

// The local portable route must select a real source-declared XYZ region. This fixture is CZYX,
// so a positional ZYX assumption would choose the wrong chunks for the crosshair.
window.newvolimNativePortableSource = {
  shape: [1, 1024, 1024, 1024],
  chunkShape: [1, 64, 64, 64],
  spatialAxesXyz: [3, 2, 1],
};
window.newvolimCrosshair = { x: 900, y: 127, z: 511 };
const nativeRegion = window.newvolimNativePortableRegion();
assert.equal(nativeRegion.extentXyz.reduce((product, extent) => product * extent, 1) <= 16, true,
  'the source-derived XYZ region must stay inside four 1,048,576-word pages');
for (let axis = 0; axis < 3; axis += 1) {
  const selected = Math.floor([900, 127, 511][axis] / 64);
  assert.equal(selected >= nativeRegion.originXyz[axis]
    && selected < nativeRegion.originXyz[axis] + nativeRegion.extentXyz[axis], true,
  'the source-derived region must contain the crosshair chunk on every XYZ axis');
}
window.newvolimNativePortableSource.spatialAxesXyz = [3, 3, 1];
assert.throws(() => window.newvolimNativePortableRegion(), /metadata is unavailable or invalid/,
  'the portable region selector must reject duplicate source spatial axes');

const browserPlan = [{ request: { source: {
  axes: ['c', 'z', 'y', 'x'], shape: [2, 1, 2, 2], chunkShape: [1, 1, 2, 2], dtype: 'uint16',
}}, chunks: [{ assetPath: '0/c/1/0/0/0', coordinates: [1, 0, 0, 0], logicalExtent: [1, 1, 2, 2], spatialChunkXyz: [0, 0, 0] }] }];
const fetchedAssets = [];
sandbox.fetch = async url => {
  fetchedAssets.push(String(url));
  return new Response(new Uint8Array(8));
};
const plannedChunks = await window.newvolimFetchBrowserChunkPlan('https://example.test/data/', browserPlan, 8, 8);
assert.equal(plannedChunks.length, 1, 'browser plan adapter must return only planned assets');
assert.match(fetchedAssets[0], /\/data\/0\/c\/1\/0\/0\/0$/, 'browser plan adapter must fetch the declared asset path');
await assert.rejects(
  window.newvolimFetchBrowserChunkPlan('https://example.test/data/', [{ ...browserPlan[0], chunks: [{ ...browserPlan[0].chunks[0], assetPath: '../escape' }] }]),
  /normal-relative/,
  'browser plan adapter must reject traversal before fetch',
);
await assert.rejects(
  window.newvolimFetchBrowserChunkPlan('https://example.test/data/', browserPlan, 7, 8),
  /byte budget/,
  'browser plan adapter must reject an oversized declared chunk before fetch',
);
const rendererPlan = [{ request: { source: { ...browserPlan[0].request.source, spatialAxesXyz: [3, 2, 1], channelAxis: 0 }, layer: { channels: [{ sourceIndex: 1, state: { colorSrgb: [255, 0, 0], window: { start: 0, end: 10 }, opacity: 1 }}] } }, chunks: browserPlan[0].chunks }];
const rendererInput = window.newvolimBrowserChunkPlanToRendererInput(rendererPlan, plannedChunks);
assert.deepEqual(Array.from(rendererInput.dimensions), [2, 2, 1], 'planned CZYX chunks must map into renderer XYZ dimensions');
assert.equal(rendererInput.bricks.length, 1, 'one selected channel must populate one static renderer page');
const layerDescriptors = window.newvolimBuildBrowserLayerDescriptors([
  { ...rendererInput, bricks: [rendererInput.bricks[0]], transform: { scale: [1, 2, 3], translation: [4, 5, 6] } },
  { ...rendererInput, bricks: [rendererInput.bricks[0]] },
]);
assert.deepEqual(JSON.parse(JSON.stringify(layerDescriptors.map(({ pageOffset, pageCount, transform }) => ({ pageOffset, pageCount, transform })))), [
  { pageOffset: 0, pageCount: 1, transform: { scale: [1, 2, 3], translation: [4, 5, 6] } },
  { pageOffset: 1, pageCount: 1, transform: { scale: [1, 1, 1], translation: [0, 0, 0] } },
], 'ordered layer descriptors must retain transform and static-page allocation');
assert.throws(
  () => window.newvolimBuildBrowserLayerDescriptors(Array.from({ length: 3 }, () => ({ ...rendererInput, bricks: [1, 2] }))),
  /incompatible layer descriptor/,
  'browser renderer must reject layers exceeding the four-page pool',
);
const packedLayers = window.newvolimPackBrowserLayerDescriptors(layerDescriptors);
assert.equal(packedLayers.bytes.byteLength, 256, 'four portable layer records must have a fixed uniform size');
assert.deepEqual(Array.from(packedLayers.u32.slice(0, 8)), [2, 2, 1, 0, 2, 2, 1, 1], 'first layer record must preserve dimensions and page range');
assert.deepEqual(Array.from(packedLayers.f32.slice(8, 15)), [1, 2, 3, 0, 4, 5, 6], 'first layer record must preserve physical transform');

const interleavedChannels = new Uint8Array([
  1, 0, 2, 0, 3, 0, 4, 0,
  11, 0, 12, 0, 13, 0, 14, 0,
]);
assert.deepEqual(
  Array.from(window.newvolimExtractBrowserChannelPage(interleavedChannels, [2, 1, 2, 2], { c: 0, z: 1, y: 2, x: 3 }, 1)),
  [11, 0, 12, 0, 13, 0, 14, 0],
  'browser preview must extract a C-chunked channel without assuming C axis placement',
);
const middleChannelAxis = new Uint8Array([1, 0, 2, 0, 11, 0, 12, 0]);
assert.deepEqual(
  Array.from(window.newvolimExtractBrowserChannelPage(middleChannelAxis, [1, 2, 1, 2], { z: 0, c: 1, y: 2, x: 3 }, 1)),
  [11, 0, 12, 0],
  'browser preview must respect Zarr strides when C is not the leading axis',
);
assert.throws(
  () => window.newvolimExtractBrowserChannelPage(middleChannelAxis, [1, 2, 1, 2], { z: 0, c: 1, y: 2, x: 3 }, 2),
  /invalid chunk metadata/,
  'browser preview must reject a channel outside the bounded decoded chunk',
);

assert.equal(
  JSON.stringify(await window.newvolimReadBrowserJson(new Response('{"datasets":["cells3d"]}'))),
  JSON.stringify({ datasets: ['cells3d'] }),
  'browser JSON reader must decode bounded metadata',
);
await assert.rejects(
  window.newvolimReadBrowserJson(new Response(new Uint8Array(1024 * 1024 + 1))),
  /metadata exceeds the 1048576-byte transport limit/,
  'browser JSON reader must stop oversized metadata before parsing it',
);
await assert.rejects(
  window.newvolimReadBrowserJson(new Response('{not JSON}')),
  /metadata is not valid JSON/,
  'browser JSON reader must report malformed metadata',
);

assert.deepEqual(
  { ...window.newvolimPhysicalCanvasPoint(200, 100, { left: 10, top: 20, width: 100, height: 50 }, 60, 45) },
  { x: 100, y: 50 },
  'depth picking must convert CSS event coordinates to physical render-target pixels',
);
assert.deepEqual(
  { ...window.newvolimPhysicalCanvasPoint(200, 100, { left: 10, top: 20, width: 100, height: 50 }, -50, 999) },
  { x: 0, y: 99 },
  'depth picking must clamp a pointer that falls outside the canvas bounds',
);
assert.throws(
  () => window.newvolimPhysicalCanvasPoint(0, 100, { left: 0, top: 0, width: 1, height: 1 }, 0, 0),
  /physical canvas point/,
  'depth picking must reject an empty physical render target',
);

const nativePngPayload = {
  mimeType: 'image/png', dataUrl: 'data:image/png;base64,iVBORw==', width: 200, height: 100,
  target: {
    extent: { width: 200, height: 100 }, colorFormat: 'rgba8Unorm', colorEncoding: 'srgb', depth: 'none',
  },
  progress: 'final',
};
assert.equal(
  window.newvolimValidateNativePngPayload(nativePngPayload), nativePngPayload,
  'desktop colour PNGs must retain their explicit physical frame contract',
);
for (const badPayload of [
  { ...nativePngPayload, width: 0 },
  { ...nativePngPayload, target: { ...nativePngPayload.target, depth: 'rayDistanceF32' } },
  { ...nativePngPayload, target: { ...nativePngPayload.target, extent: { width: 201, height: 100 } } },
  { ...nativePngPayload, progress: 'preview' },
]) {
  assert.throws(
    () => window.newvolimValidateNativePngPayload(badPayload),
    /native (?:frame payload|PNG payload)/,
    'desktop PNG drawing must reject mismatched, non-final, or depth-capable payload metadata',
  );
}

const remotePngFrame = {
  mimeType: 'image/png', width: 200, height: 100, progress: 'final',
  target: nativePngPayload.target,
};
const pngHeader = Buffer.alloc(24);
pngHeader.set([137, 80, 78, 71, 13, 10, 26, 10]);
pngHeader.writeUInt32BE(13, 8);
pngHeader.write('IHDR', 12, 'ascii');
pngHeader.writeUInt32BE(200, 16);
pngHeader.writeUInt32BE(100, 20);
const declaredPng = pngHeader.toString('base64');
assert.equal(window.newvolimValidateRemotePngData(declaredPng, 200, 100), true,
  'remote PNG IHDR dimensions must match the declared physical frame');
assert.equal(window.newvolimValidateRemotePngData(declaredPng, 201, 100), false,
  'remote PNG IHDR dimensions must not silently disagree with the envelope');
assert.equal(window.newvolimValidateRemotePngData('not-base64', 200, 100), false,
  'remote PNG transport must reject malformed base64 before image creation');
assert.equal(window.newvolimValidateRemotePngFrame(remotePngFrame), true,
  'remote PNG frames must carry an explicit physical colour-only contract when no sidecar exists');
const validRayDistancePfm = Buffer.concat([
  Buffer.from('Pf\n1 1\n-1.0\n'),
  Buffer.from(new Float32Array([Infinity]).buffer),
]).toString('base64');
assert.equal(window.newvolimValidateRayDistancePfm(validRayDistancePfm, 1, 1), true,
  'a remote PFM sidecar must preserve its declared physical extent and +infinity no-hit value');
assert.equal(window.newvolimValidateRayDistancePfm('UGYKMSAxCi0xLjAK', 1, 1), false,
  'a remote PFM sidecar without its f32 sample payload must be rejected');
assert.equal(window.newvolimValidateRayDistancePfm(validRayDistancePfm + 'A', 1, 1), false,
  'a PFM sidecar with a non-exact base64 length must be rejected before decoding');
const orientedRayDistancePfm = Buffer.concat([
  Buffer.from('Pf\n2 2\n-1.0\n'),
  Buffer.from(new Float32Array([3, 4, 1, 2]).buffer),
]).toString('base64');
assert.equal(window.newvolimReadRayDistancePfm(orientedRayDistancePfm, 2, 2, 0, 0), 1,
  'PFM sampling must map a top-left canvas pixel to PFM’s bottom-up storage');
assert.equal(window.newvolimReadRayDistancePfm(orientedRayDistancePfm, 2, 2, 1, 1), 4,
  'PFM sampling must retain the bottom canvas row’s physical distance');
assert.throws(
  () => window.newvolimReadRayDistancePfm(orientedRayDistancePfm, 2, 2, 2, 0),
  /ray-distance PFM pixel/,
  'PFM sampling must reject out-of-bounds physical pixels',
);
assert.equal(window.newvolimValidateNativePngPayload({
  ...nativePngPayload, width: 1, height: 1,
  target: { ...nativePngPayload.target, extent: { width: 1, height: 1 }, depth: 'rayDistanceF32' },
  rayDistancePfmBase64: validRayDistancePfm,
}).target.depth, 'rayDistanceF32',
  'desktop PNG frames may carry the same validated first-opacity PFM attachment');
assert.equal(window.newvolimValidateRemotePngFrame({
  ...remotePngFrame, width: 1, height: 1,
  target: { ...remotePngFrame.target, extent: { width: 1, height: 1 }, depth: 'rayDistanceF32' },
  rayDistancePfmBase64: validRayDistancePfm,
}), true,
  'remote PNG frames may carry a declared first-opacity PFM attachment');
assert.equal(window.newvolimValidateRemotePngFrame({
  ...remotePngFrame,
  target: { ...remotePngFrame.target, depth: 'rayDistanceF32' },
  rayDistancePfmBase64: 'UGYKMSAxCi0xLjAK',
}), false,
  'a declared ray-distance attachment must use a complete valid PFM sidecar');
for (const badFrame of [
  { ...remotePngFrame, width: 0 },
  { ...remotePngFrame, width: 4097, height: 4096,
    target: { ...remotePngFrame.target, extent: { width: 4097, height: 4096 } } },
  { ...remotePngFrame, target: { ...remotePngFrame.target, depth: 'rayDistanceF32' } },
  { ...remotePngFrame, rayDistancePfmBase64: 'UGYKMSAxCi0xLjAK' },
  { ...remotePngFrame, target: { ...remotePngFrame.target, extent: { width: 201, height: 100 } } },
  { ...remotePngFrame, progress: 'preview' },
]) {
  assert.equal(window.newvolimValidateRemotePngFrame(badFrame), false,
    'remote PNG drawing must reject invalid, stale, or depth-capable envelope metadata');
}

const started = [];
let finishFirst;
window.newvolimDrawPayload = async command => {
  started.push(command);
  if (command === 'first') await new Promise(resolve => { finishFirst = resolve; });
};
const first = window.newvolimAdmitFrame('first', { camera: 1 });
await Promise.resolve();
const second = window.newvolimAdmitFrame('second', { camera: 2 });
const third = window.newvolimAdmitFrame('third', { camera: 3 });
finishFirst();
await Promise.all([first, second, third]);

assert.deepEqual(started, ['first', 'third']);
assert.equal(window.newvolimFrameAdmission.active, false);
assert.equal(window.newvolimFrameAdmission.pending, null);

window.newvolimOpenRemoteFrameServer();
const firstSocket = sockets.at(-1);
firstSocket.emit('open');
assert.equal(firstSocket.sent.length, 1, 'opening a remote socket must send its initial request');
assert.equal(window.newvolimRemoteFrames.activeRequestId, 1);
firstSocket.emit('message', { data: JSON.stringify({ type: 'error', requestId: 999, message: 'stale' }) });
assert.equal(window.newvolimRemoteFrames.activeRequestId, 1,
  'a mismatched reply must not release the active remote request');
assert.match(elements.get('newvolim-render-status').textContent, /stale or unsolicited/);
firstSocket.emit('message', { data: JSON.stringify({ type: 'error', requestId: 1, message: 'expected failure' }) });
assert.equal(window.newvolimRemoteFrames.activeRequestId, null,
  'the matching reply must release the active remote request');
window.newvolimOpenRemoteFrameServer();
const secondSocket = sockets.at(-1);
secondSocket.emit('open');
assert.equal(window.newvolimRemoteFrames.socket, secondSocket);
firstSocket.emit('message', { data: JSON.stringify({ type: 'error', requestId: 1, message: 'old socket' }) });
assert.equal(window.newvolimRemoteFrames.socket, secondSocket,
  'a replaced socket must not alter the current remote connection');

const browserSpacing = window.newvolimComposeBrowserSpacing(
  [{ name: 'x' }, { name: 'y' }, { name: 'z' }],
  [{ type: 'scale', scale: [10, 2, 1] }, { type: 'translation', translation: [3, 4, 5] }],
  [{ type: 'scale', scale: [0.5, 4, 5] }],
);
assert.equal(browserSpacing.z, 2);
assert.equal(browserSpacing.y, 1);
assert.equal(browserSpacing.x, 0);
assert.deepEqual(Array.from(browserSpacing.spacing), [5, 8, 5, 0]);
assert.deepEqual(
  Array.from(window.newvolimAxisOrderedChunkCoordinates(browserSpacing, [7, 8, 9])),
  [9, 8, 7],
  'a Z,Y,X user coordinate must be ordered by a declared X,Y,Z NGFF array',
);
assert.deepEqual(
  Array.from(window.newvolimAxisOrderedChunkCoordinates({ z: 0, y: 1, x: 2 }, [7, 8, 9])),
  [7, 8, 9],
  'a declared Z,Y,X NGFF array keeps the semantic user order',
);
assert.deepEqual(
  Array.from(window.newvolimAxisOrderedChunkCoordinates({ c: 1, z: 3, y: 2, x: 0 }, [7, 8, 9])),
  [9, 0, 8, 7],
  'a C,Z,Y,X array reserves its channel chunk coordinate while ordering spatial input',
);
const channelTransfers = window.newvolimBrowserChannelTransfers([
  { color: 'FF0000', window: { start: 10, end: 110 } },
  { color: '00FF00', window: { start: 20, end: 20 }, active: false },
], 2);
assert.deepEqual(Array.from(channelTransfers[0].window), [10, 110]);
assert.deepEqual(Array.from(channelTransfers[0].color), [1, 0, 0], 'OMERO colours are converted to linear light');
assert.equal(channelTransfers[1].opacity, 0, 'disabled OMERO channels retain a fixed binding with zero opacity');
assert.throws(
  () => window.newvolimBrowserChannelTransfers([], 5),
  /one to four channels/,
  'the static browser page pool must not admit an unbounded channel count',
);
const browserResponses = new Map([
  ['http://127.0.0.1:9916/store/zarr.json', JSON.stringify({
    attributes: {
      multiscales: [{
        axes: [{ name: 'c' }, { name: 'z' }, { name: 'y' }, { name: 'x' }],
        coordinateTransformations: [{ type: 'scale', scale: [1, 2, 3, 4] }],
        datasets: [{ path: '0' }],
      }],
      omero: { channels: [
        { color: 'FF0000', window: { start: 1, end: 100 } },
        { color: '00FF00', window: { start: 2, end: 200 }, active: false },
      ] },
    },
  })],
  ['http://127.0.0.1:9916/store/0/zarr.json', JSON.stringify({
    data_type: 'uint16', shape: [2, 2, 2, 2],
    chunk_grid: { configuration: { chunk_shape: [2, 2, 2, 2] } },
    codecs: [{ name: 'bytes', configuration: { endian: 'little' } }],
  })],
  ['http://127.0.0.1:9916/store/0/c/0/0/0/0', new Uint8Array(32)],
]);
const browserRequests = [];
sandbox.fetch = async url => {
  const href = String(url);
  browserRequests.push(href);
  const body = browserResponses.get(href);
  assert.notEqual(body, undefined, `unexpected direct-store request ${href}`);
  return new Response(body);
};
let loadedBrowserPreview;
window.newvolimRenderBrowserSynthetic = async (_canvas, _generation, preview) => {
  loadedBrowserPreview = preview;
  elements.get('newvolim-render-status').textContent = 'Browser WebGPU OME-Zarr chunk frame (test renderer)';
};
elements.get('newvolim-browser-zarr-url').value = 'http://127.0.0.1:9916/store/';
await window.newvolimOpenBrowserOmeZarr();
assert.deepEqual(browserRequests, [
  'http://127.0.0.1:9916/store/zarr.json',
  'http://127.0.0.1:9916/store/0/zarr.json',
  'http://127.0.0.1:9916/store/0/c/0/0/0/0',
], 'a two-channel C,Z,Y,X preview must extract both fixed pages from one bounded C chunk');
assert.equal(loadedBrowserPreview.bricks.length, 2);
assert.deepEqual(Array.from(loadedBrowserPreview.spacing), [4, 3, 2, 0]);
assert.deepEqual(Array.from(loadedBrowserPreview.channelTransfers[0].window), [1, 100]);
assert.equal(loadedBrowserPreview.channelTransfers[1].opacity, 0,
  'disabled OMERO channels must remain a zero-opacity fixed-page binding');
assert.match(elements.get('newvolim-render-status').textContent, /Browser WebGPU OME-Zarr chunk frame/,
  'the complete bounded multi-channel load must finish without a swallowed DOM/control error');

browserResponses.set('http://127.0.0.1:9916/store/0/zarr.json', JSON.stringify({
  data_type: 'uint16', shape: [2, 2, 2, 4],
  chunk_grid: { configuration: { chunk_shape: [2, 2, 2, 2] } },
  codecs: [{ name: 'bytes', configuration: { endian: 'little' } }],
}));
browserResponses.set('http://127.0.0.1:9916/store/0/c/0/0/0/1', new Uint8Array(32));
elements.get('newvolim-browser-zarr-chunk').value = '0,0,0;0,0,1';
await window.newvolimOpenBrowserOmeZarr();
assert.deepEqual(browserRequests.slice(-4), [
  'http://127.0.0.1:9916/store/zarr.json',
  'http://127.0.0.1:9916/store/0/zarr.json',
  'http://127.0.0.1:9916/store/0/c/0/0/0/0',
  'http://127.0.0.1:9916/store/0/c/0/0/0/1',
], 'two bounded Z,Y,X requests must fetch their distinct reviewed chunk assets');
assert.equal(loadedBrowserPreview.layers.length, 2,
  'two spatial chunks must remain two ordered static-page layers');
assert.deepEqual(JSON.parse(JSON.stringify(loadedBrowserPreview.layers.map(layer => layer.transform))), [
  { scale: [0.5, 1, 1], translation: [-0.5, 0, 0] },
  { scale: [0.5, 1, 1], translation: [0.5, 0, 0] },
], 'adjacent X chunks must retain their separate normalized physical placement');
assert.deepEqual(Array.from(loadedBrowserPreview.dimensions), [4, 2, 2],
  'the multi-chunk packet must retain the full source extent as its camera domain');
elements.get('newvolim-browser-zarr-chunk').value = '0,0,0';
assert.equal(window.newvolimNormalBrowserDatasetPath('/0/labels/cells/'), '0/labels/cells');
for (const unsafePath of [
  '../outside', '0/../outside', '%2e%2e/outside', '0//array', '0%2Foutside',
  '0?query', '0%23fragment', '0%0Aarray',
]) {
  assert.throws(
    () => window.newvolimNormalBrowserDatasetPath(unsafePath),
    /dataset path/,
    `${unsafePath} must not escape or alter the selected browser store root`,
  );
}
assert.equal(
  window.newvolimDirectBrowserStoreRoot('https://example.test/ome.zarr').href,
  'https://example.test/ome.zarr/',
);
assert.equal(
  window.newvolimDirectBrowserStoreRoot('http://127.0.0.1:9916/ome.zarr/').hostname,
  '127.0.0.1',
);
for (const unsafeRoot of ['http://example.test/ome.zarr/', 'file:///tmp/ome.zarr/']) {
  assert.throws(
    () => window.newvolimDirectBrowserStoreRoot(unsafeRoot),
    /must use HTTPS/,
    `${unsafeRoot} must not be a direct browser store root`,
  );
}
assert.throws(
  () => window.newvolimComposeBrowserSpacing(
    [{ name: 'z' }, { name: 'y' }, { name: 'x' }],
    [{ type: 'affine', matrix: [[1, 0, 0], [0, 1, 0], [0, 0, 1]] }],
    [],
  ),
  /cannot represent NGFF affine/,
);

const listed = [];
window.newvolimRenderAnnotationList = annotations => listed.push(annotations);
window.__TAURI__ = {
  core: {
    invoke: async command => {
      assert.equal(command, 'list_annotations');
      return [
        { annotation: { id: 1, label: 'projectable' }, voxelPoints: [[4, 6, 2]] },
        { annotation: { id: 2, label: 'non-point' }, voxelPoints: null },
      ];
    },
  },
};
await window.newvolimRefreshAnnotations();
assert.deepEqual(window.newvolimAnnotationMarkers, [
  { annotation: { id: 1, label: 'projectable' }, voxelPoints: [[4, 6, 2]] },
]);
assert.deepEqual(listed, [[
  { id: 1, label: 'projectable' },
  { id: 2, label: 'non-point' },
]]);

const drawing = [];
const context = {
  save() {}, restore() {}, beginPath() { drawing.push('begin'); },
  moveTo(x, y) { drawing.push(['move', x, y]); },
  lineTo(x, y) { drawing.push(['line', x, y]); },
  arc(x, y) { drawing.push(['arc', x, y]); }, closePath() { drawing.push('close'); },
  stroke() { drawing.push('stroke'); },
};
window.newvolimCrosshair = { x: 0, y: 0, z: 2 };
window.newvolimCrosshairDimensions = [10, 10, 10];
window.newvolimAnnotationMarkers = [
  {
    annotation: { color_srgb: [1, 2, 3], geometry: { Polygon: [] } },
    voxelPoints: [[1, 2, 2], [3, 2, 2], [3, 4, 2]],
  },
  {
    annotation: { geometry: { Polyline: [] } },
    voxelPoints: [[1, 2, 1], [3, 2, 2]],
  },
];
window.newvolimDrawAnnotations(context, 'xy', 100, 100);
assert.deepEqual(drawing, [
  'begin', ['move', 15, 25], ['line', 35, 25], ['line', 35, 45], 'close', 'stroke',
]);

drawing.length = 0;
window.newvolimAnnotationMarkers = [{
  annotation: { color_srgb: [1, 2, 3], geometry: { Rectangle: {} } },
  voxelPoints: [[1, 2, 2], [3, 2, 2], [3, 4, 2], [1, 4, 2]],
}];
window.newvolimDrawAnnotations(context, 'xy', 100, 100);
assert.deepEqual(drawing, [
  'begin', ['move', 15, 25], ['line', 35, 25], ['line', 35, 45], ['line', 15, 45], 'close', 'stroke',
]);
console.log('UI frame admission retains only the newest pending request');
