import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
const [wasmPath, corpus] = process.argv.slice(2);
const wasm = readFileSync(wasmPath);
const module = await WebAssembly.compile(wasm);
assert.deepEqual(WebAssembly.Module.imports(module), [], "self-contained shared-Rust module");
const workerSource = readFileSync(new URL("../src/planner-worker.js", import.meta.url), "utf8");
for (const name of ["nominal", "scenarios", "partial", "unsupported", "invalid", "source-error"]) {
  const input = readFileSync(`${corpus}/${name}.input`);
  const expected = JSON.parse(readFileSync(`${corpus}/${name}.json`, "utf8"));
  for (const batch of [17, 8192]) {
    const api = (await WebAssembly.instantiate(module, {})).exports;
    assert.ok(input.length <= api.planner_input_capacity());
    new Uint8Array(api.memory.buffer, api.planner_input_ptr(), input.length).set(input);
    let status = api.planner_init(input.length);
    const output = () => JSON.parse(new TextDecoder().decode(new Uint8Array(api.memory.buffer, api.planner_output_ptr(), api.planner_output_len())));
    let steps = 0;
    while (status === 0) {
      status = api.planner_step(batch);
      const current = output();
      if (status === 0) {
        assert.equal(current.candidates_total, (current.partial && name !== "partial" ? 2 : 1) * 24);
      }
      if (current.work_steps !== undefined) { assert.ok(current.work_steps >= steps); steps = current.work_steps; }
    }
    assert.deepEqual({ status, ...output() }, expected, `${name}, batch ${batch}: exact native/WASM result and work count`);
    const final = output();
    assert.equal(api.planner_step(1), status, "terminal status retained");
    assert.deepEqual(output(), final, "terminal report retained");
    assert.equal(api.planner_init(api.planner_input_capacity() + 1), 3, "oversized snapshot rejected");
    assert.equal(api.planner_init(input.length - 1), 3, "truncated snapshot rejected");
  }
  // Execute the actual served worker, including its fetch, ABI and scheduling.
  const messages = [];
  const self = { postMessage: (message) => messages.push(message) };
  runInNewContext(workerSource, {
    self, WebAssembly, Uint8Array, TextDecoder, performance,
    fetch: async (url, options) => {
      assert.equal(url, "/planner.wasm"); assert.equal(options.cache, "no-cache");
      return { ok: true, arrayBuffer: async () => wasm };
    },
    setTimeout: (callback) => setImmediate(callback),
  });
  await self.onmessage({ data: input });
  const { elapsed_ms, ...actual } = messages.at(-1);
  if (actual.status !== 3) assert.ok(elapsed_ms >= 0);
  // Initialization errors are reported by the worker's catch as status 3.
  assert.deepEqual(actual, expected, `${name}: served worker matches native Rust`);
  console.log(`PASS ${name}: native = WASM = served worker, both batch sizes`);
}
