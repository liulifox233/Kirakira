import { defineConfig } from "vite";

// The shell is copied next to a game's semantic package and may be served
// from any object-store prefix. Relative asset URLs keep that deployment
// layout independent of the origin root.
export default defineConfig({
  base: "./",
});
