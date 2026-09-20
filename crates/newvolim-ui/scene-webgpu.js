// The WebGPU half of the browser renderer. The Rust app owns the residency loop: it plans, fetches
// the chunks the shader missed, and packs the nine bindings (`newvolim_residency`); this file only
// speaks the WebGPU JavaScript API — dispatch a packed scene once and read back the output and
// the request table, and blit a colour buffer to a canvas. The nine bindings are the recorder's
// order, pinned by a server test that reads this file.
window.newvolimSceneWebGpu = (function () {
  let gpu = null; // { adapter, device }

  async function adapter() {
    if (gpu && gpu.adapter) return gpu.adapter;
    if (!navigator.gpu) throw new Error("WebGPU is unavailable in this browser");
    const found = await navigator.gpu.requestAdapter();
    if (!found) throw new Error("WebGPU is present but no adapter was granted");
    gpu = { adapter: found, device: null };
    return found;
  }

  async function device() {
    const found = await adapter();
    if (!gpu.device) {
      const created = await found.requestDevice();
      const owner = gpu;
      created.lost.then(() => { if (gpu === owner) owner.device = null; });
      gpu.device = created;
    }
    return gpu.device;
  }

  // One pass of the scene shader over packed inputs. `pages` is an array of four Uint32Arrays;
  // `sceneData`, `rays`, `params` are Uint32Arrays. Resolves to { output, requests }: the output
  // words (colour, depth bits, trace) and the request table (0xffffffff where empty).
  async function dispatch(pages, sceneData, rays, params, requestCapacity, outputWords, workgroups) {
    if (!Array.isArray(pages) || pages.length !== 4 || params.length !== 16) throw new Error("scene dispatch inputs are malformed");
    const gpuDevice = await device();
    const storage = (label, data, writable) => {
      const buffer = gpuDevice.createBuffer({
        label, size: Math.max(4, data.byteLength),
        usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST | (writable ? GPUBufferUsage.COPY_SRC : 0),
      });
      if (data.byteLength) gpuDevice.queue.writeBuffer(buffer, 0, data);
      return buffer;
    };
    const pageBuffers = pages.map((page, index) => storage(`scene page ${index}`, page, false));
    const sceneBuffer = storage("scene data", sceneData, false);
    const rayBuffer = storage("scene rays", rays, false);
    const output = storage("scene output", new Uint32Array(outputWords), true);
    const uniform = gpuDevice.createBuffer({ label: "scene params", size: 64, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    gpuDevice.queue.writeBuffer(uniform, 0, params);
    const requests = storage("scene requests", new Uint32Array(requestCapacity).fill(0xffffffff), true);
    const module = gpuDevice.createShaderModule({ label: "scene shader", code: window.newvolimSceneWebGpu.shader });
    const pipeline = gpuDevice.createComputePipeline({ label: "scene pipeline", layout: "auto", compute: { module, entryPoint: "main" } });
    const group = gpuDevice.createBindGroup({
      layout: pipeline.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: pageBuffers[0] } },
        { binding: 1, resource: { buffer: pageBuffers[1] } },
        { binding: 2, resource: { buffer: pageBuffers[2] } },
        { binding: 3, resource: { buffer: pageBuffers[3] } },
        { binding: 4, resource: { buffer: sceneBuffer } },
        { binding: 5, resource: { buffer: rayBuffer } },
        { binding: 6, resource: { buffer: output } },
        { binding: 7, resource: { buffer: uniform } },
        { binding: 8, resource: { buffer: requests } },
      ],
    });
    const outputReadback = gpuDevice.createBuffer({ label: "scene output readback", size: outputWords * 4, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const requestReadback = gpuDevice.createBuffer({ label: "scene request readback", size: requestCapacity * 4, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const encoder = gpuDevice.createCommandEncoder();
    const pass = encoder.beginComputePass();
    pass.setPipeline(pipeline);
    pass.setBindGroup(0, group);
    pass.dispatchWorkgroups(workgroups);
    pass.end();
    encoder.copyBufferToBuffer(output, 0, outputReadback, 0, outputWords * 4);
    encoder.copyBufferToBuffer(requests, 0, requestReadback, 0, requestCapacity * 4);
    gpuDevice.queue.submit([encoder.finish()]);
    await Promise.all([outputReadback.mapAsync(GPUMapMode.READ), requestReadback.mapAsync(GPUMapMode.READ)]);
    const outputCopy = new Uint32Array(outputReadback.getMappedRange().slice(0));
    const requestCopy = new Uint32Array(requestReadback.getMappedRange().slice(0));
    outputReadback.unmap();
    requestReadback.unmap();
    for (const buffer of [...pageBuffers, sceneBuffer, rayBuffer, output, uniform, requests, outputReadback, requestReadback]) buffer.destroy();
    return { output: outputCopy, requests: requestCopy };
  }

  // Blit RGBA bytes to the canvas through its WebGPU context.
  async function present(canvas, rgba, width, height) {
    const gpuDevice = await device();
    canvas.width = width; canvas.height = height;
    const context = canvas.getContext("webgpu");
    const format = navigator.gpu.getPreferredCanvasFormat();
    context.configure({ device: gpuDevice, format, alphaMode: "opaque" });
    const texture = gpuDevice.createTexture({ size: [width, height], format: "rgba8unorm", usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST });
    gpuDevice.queue.writeTexture({ texture }, rgba, { bytesPerRow: width * 4 }, [width, height]);
    const module = gpuDevice.createShaderModule({ code: `
      @group(0) @binding(0) var frame: texture_2d<f32>;
      @vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
        var positions = array<vec2f, 3>(vec2f(-1.0, -1.0), vec2f(3.0, -1.0), vec2f(-1.0, 3.0));
        return vec4f(positions[index], 0.0, 1.0);
      }
      @fragment fn fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
        let texel = textureLoad(frame, vec2u(position.xy), 0);
        return vec4f(texel.rgb, 1.0);
      }` });
    const pipeline = gpuDevice.createRenderPipeline({ layout: "auto", vertex: { module, entryPoint: "vertex" }, fragment: { module, entryPoint: "fragment", targets: [{ format }] }, primitive: { topology: "triangle-list" } });
    const bindings = gpuDevice.createBindGroup({ layout: pipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: texture.createView() }] });
    const encoder = gpuDevice.createCommandEncoder();
    const pass = encoder.beginRenderPass({ colorAttachments: [{ view: context.getCurrentTexture().createView(), loadOp: "clear", storeOp: "store", clearValue: { r: 0, g: 0, b: 0, a: 1 } }] });
    pass.setPipeline(pipeline); pass.setBindGroup(0, bindings); pass.draw(3); pass.end();
    gpuDevice.queue.submit([encoder.finish()]);
    texture.destroy();
  }

  // The Rust app sets `shader` (the desktop's WGSL, fetched once from the plan) before the
  // first dispatch.
  return { dispatch, present, adapter, shader: null };
})();
