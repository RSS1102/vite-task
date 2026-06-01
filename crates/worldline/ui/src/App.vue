<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import {
  type ApiData,
  type ApiWrite,
  type Blob,
  basename,
  blobIsBinary,
  blobText,
  loadData,
  loadOutput,
} from './api';
import { type DiffLine, lineDiff } from './diff';
import TerminalView from './TerminalView.vue';

const data = ref<ApiData | null>(null);
const output = ref<Uint8Array>(new Uint8Array(0));
const error = ref<string | null>(null);
const selected = ref(0);

onMounted(async () => {
  try {
    const [d, o] = await Promise.all([loadData(), loadOutput()]);
    data.value = d;
    output.value = o;
    if (d.writes.length > 0) selected.value = d.writes.length - 1;
  } catch (e) {
    error.value = String(e);
  }
});

const write = computed<ApiWrite | null>(() => data.value?.writes[selected.value] ?? null);

function blobOf(id: string | undefined): Blob | null {
  if (!id || !data.value) return null;
  return data.value.blobs[id] ?? null;
}

const beforeBlob = computed(() => blobOf(write.value?.before));
const afterBlob = computed(() => blobOf(write.value?.after));

/** Whether a write changed the file's content. */
function changed(w: ApiWrite): boolean {
  return w.before !== w.after;
}

const isBinary = computed(() => {
  const a = afterBlob.value;
  const b = beforeBlob.value;
  return (a !== null && blobIsBinary(a)) || (b !== null && blobIsBinary(b));
});

const diff = computed<DiffLine[] | null>(() => {
  const a = afterBlob.value;
  const b = beforeBlob.value;
  if (!a || !b || isBinary.value) return null;
  return lineDiff(blobText(b), blobText(a));
});
</script>

<template>
  <div class="app">
    <div class="header">
      <span class="title">worldline</span>
      <span class="cmd">{{ data?.argv.join(' ') }}</span>
      <span class="meta">cwd: {{ data?.cwd }}</span>
      <span v-if="data" :class="data.exit_code === 0 ? 'exit-ok' : 'exit-bad'">
        exit: {{ data.exit_code ?? 'signal' }}
      </span>
    </div>

    <div v-if="error" class="empty">{{ error }}</div>
    <div v-else-if="!data" class="empty">Loading…</div>
    <div v-else-if="data.writes.length === 0" class="empty">
      No file writes were captured for this run.
    </div>

    <div v-else class="body">
      <div class="timeline">
        <div
          v-for="(w, idx) in data.writes"
          :key="w.seq"
          class="event"
          :class="{ selected: idx === selected, changed: changed(w) }"
          :title="w.path"
          @click="selected = idx"
        >
          <span class="seq">{{ w.seq }}</span>
          <span class="kind">write</span>
          <span class="name">{{ basename(w.path) }}</span>
        </div>
      </div>

      <div class="center">
        <div class="files">
          <div class="viewer">
            <div v-if="!write" class="empty">Select a write.</div>
            <div v-else>
              <div class="path-bar">{{ write.path }}</div>
              <div v-if="isBinary" class="binary">Binary file</div>
              <pre v-else-if="diff"><span
                v-for="(l, i) in diff"
                :key="i"
                class="line"
                :class="{ add: l.type === 'add', del: l.type === 'del' }"
              >{{ l.type === 'add' ? '+' : l.type === 'del' ? '-' : ' ' }}{{ l.text }}
</span></pre>
            </div>
          </div>
        </div>

        <div class="terminal-wrap">
          <TerminalView :output="output" :offset="write?.output_offset ?? 0" />
        </div>
      </div>
    </div>
  </div>
</template>
