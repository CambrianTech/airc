/**
 * Runtime store for host-provided OpenClaw services used by the airc bundled
 * plugin.
 */
import { createPluginRuntimeStore } from "openclaw/plugin-sdk/runtime-store";
import type { PluginRuntime } from "openclaw/plugin-sdk/runtime-store";

const { setRuntime: setAircRuntime, getRuntime: getAircRuntime } =
  createPluginRuntimeStore<PluginRuntime>({
    pluginId: "airc",
    errorMessage: "airc runtime not initialized",
  });

export { getAircRuntime, setAircRuntime };
