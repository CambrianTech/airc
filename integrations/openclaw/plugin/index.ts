/**
 * Bundled channel entry metadata for the airc plugin.
 */
import { defineBundledChannelEntry } from "openclaw/plugin-sdk/channel-entry-contract";

export default defineBundledChannelEntry({
  id: "airc",
  name: "airc",
  description: "airc channel plugin",
  importMetaUrl: import.meta.url,
  plugin: {
    specifier: "./channel-plugin-api.js",
    exportName: "aircPlugin",
  },
  runtime: {
    specifier: "./api.js",
    exportName: "setAircRuntime",
  },
});
