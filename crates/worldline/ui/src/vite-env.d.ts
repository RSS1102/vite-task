/// <reference types="vite/client" />

declare module '*.vue' {
  import type { DefineComponent } from 'vue';
  // biome-ignore lint: standard Vue SFC shim
  const component: DefineComponent<{}, {}, any>;
  export default component;
}
