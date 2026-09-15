/// <reference types="vite/client" />

/** package.json version, baked in by vite.config.ts `define`. */
declare const __APP_VERSION__: string;
/** Short git commit of the build, "+" appended when the tree was dirty; "" if unknown. */
declare const __APP_COMMIT__: string;
