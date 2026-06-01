<script setup lang="ts">
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { onBeforeUnmount, onMounted, ref, watch } from 'vue';

const props = defineProps<{
  output: Uint8Array;
  offset: number;
}>();

const host = ref<HTMLDivElement | null>(null);
let term: Terminal | null = null;
let fit: FitAddon | null = null;
let resizeObserver: ResizeObserver | null = null;

function render() {
  if (!term) return;
  // Reset and replay the raw terminal bytes captured up to this event.
  term.reset();
  if (props.offset > 0) {
    term.write(props.output.subarray(0, props.offset));
  }
}

onMounted(() => {
  if (!host.value) return;
  term = new Terminal({
    convertEol: false,
    fontSize: 12,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
    theme: { background: '#000000' },
    scrollback: 10000,
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(host.value);
  fit.fit();
  resizeObserver = new ResizeObserver(() => fit?.fit());
  resizeObserver.observe(host.value);
  render();
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  term?.dispose();
  term = null;
});

watch(() => [props.offset, props.output], render);
</script>

<template>
  <div ref="host" class="terminal-host" style="height: 100%; width: 100%"></div>
</template>
