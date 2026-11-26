// Only scheduling and the ABI live here. Geometry, interpolation, controller,
// scenarios, partial reception and uploaded-plan identity all run in Rust.
self.onmessage = async ({ data }) => {
  try {
    const started = performance.now();
    const response = await fetch("/planner.wasm", { cache: "no-cache" });
    if (!response.ok) throw new Error(`Planner download failed (${response.status})`);
    const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {});
    const api = instance.exports;
    const bytes = new Uint8Array(data);
    if (bytes.length > api.planner_input_capacity()) throw new Error("Planning snapshot is too large");
    new Uint8Array(api.memory.buffer, api.planner_input_ptr(), bytes.length).set(bytes);
    const output = () => JSON.parse(new TextDecoder().decode(new Uint8Array(api.memory.buffer, api.planner_output_ptr(), api.planner_output_len())));
    if (api.planner_init(bytes.length) !== 0) throw new Error(output().error);
    let lastReport = -Infinity;
    for (;;) {
      const status = api.planner_step(2048);
      const result = output();
      const now = performance.now();
      if (status !== 0 || now - lastReport >= 100) {
        self.postMessage({ status, elapsed_ms: now - started, ...result });
        lastReport = now;
      }
      if (status !== 0) break;
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  } catch (error) {
    self.postMessage({ status: 3, error: error.message || "Browser planning failed" });
  }
};
