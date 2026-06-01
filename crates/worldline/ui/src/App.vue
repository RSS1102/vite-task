<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import {
  type ApiData,
  type ApiEvent,
  type EventKind,
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
const selectedEvent = ref(0);
const selectedPath = ref<string | null>(null);

onMounted(async () => {
  try {
    const [d, o] = await Promise.all([loadData(), loadOutput()]);
    data.value = d;
    output.value = o;
    if (d.events.length > 0) selectEvent(d.events.length - 1);
  } catch (e) {
    error.value = String(e);
  }
});

function selectEvent(idx: number) {
  selectedEvent.value = idx;
  const ev = data.value?.events[idx];
  // Default the file view to the file that changed at this event.
  selectedPath.value = ev ? ev.path : null;
}

function kindLabel(kind: EventKind): string {
  if (kind === 'write-open') return 'open';
  if (kind === 'write-close') return 'close';
  return 'final';
}

const event = computed<ApiEvent | null>(() => data.value?.events[selectedEvent.value] ?? null);
const prevEvent = computed<ApiEvent | null>(() =>
  selectedEvent.value > 0 ? (data.value?.events[selectedEvent.value - 1] ?? null) : null,
);

const fileList = computed(() => {
  const ev = event.value;
  if (!ev) return [];
  return Object.keys(ev.files)
    .sort()
    .map((path) => ({
      path,
      changed: ev.files[path] !== prevEvent.value?.files[path],
    }));
});

function blobOf(ev: ApiEvent | null, path: string | null) {
  if (!ev || !path || !data.value) return null;
  const id = ev.files[path];
  return id ? data.value.blobs[id] : null;
}

const currentBlob = computed(() => blobOf(event.value, selectedPath.value));
const prevBlob = computed(() => blobOf(prevEvent.value, selectedPath.value));

const diff = computed<DiffLine[] | null>(() => {
  const cur = currentBlob.value;
  if (!cur || blobIsBinary(cur)) return null;
  const prev = prevBlob.value;
  // Only diff when the file actually changed from the previous event.
  if (prev && !blobIsBinary(prev) && event.value?.files[selectedPath.value ?? ''] !== prevEvent.value?.files[selectedPath.value ?? '']) {
    return lineDiff(blobText(prev), blobText(cur));
  }
  return null;
});

const plainText = computed(() => (currentBlob.value ? blobText(currentBlob.value) : ''));
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
    <div v-else-if="data.events.length === 0" class="empty">
      No file writes were captured for this run.
    </div>

    <div v-else class="body">
      <div class="timeline">
        <div
          v-for="(ev, idx) in data.events"
          :key="ev.seq"
          class="event"
          :class="{ selected: idx === selectedEvent }"
          @click="selectEvent(idx)"
        >
          <span class="seq">{{ ev.seq }}</span>
          <span class="kind" :class="`kind-${ev.kind}`">{{ kindLabel(ev.kind) }}</span>
          <span class="name" :title="ev.path">{{ basename(ev.path) }}</span>
        </div>
      </div>

      <div class="center">
        <div class="files">
          <div class="file-list">
            <div
              v-for="f in fileList"
              :key="f.path"
              class="file-row"
              :class="{ selected: f.path === selectedPath, changed: f.changed }"
              :title="f.path"
              @click="selectedPath = f.path"
            >
              {{ basename(f.path) }}
            </div>
          </div>

          <div class="viewer">
            <div v-if="!currentBlob" class="empty">Select a file.</div>
            <div v-else-if="blobIsBinary(currentBlob)" class="binary">
              Binary file ({{ currentBlob.data.length }} base64 chars)
            </div>
            <pre v-else-if="diff"><span
              v-for="(l, i) in diff"
              :key="i"
              class="line"
              :class="{ add: l.type === 'add', del: l.type === 'del' }"
            >{{ l.type === 'add' ? '+' : l.type === 'del' ? '-' : ' ' }}{{ l.text }}
</span></pre>
            <pre v-else>{{ plainText }}</pre>
          </div>
        </div>

        <div class="terminal-wrap">
          <TerminalView :output="output" :offset="event?.output_offset ?? 0" />
        </div>
      </div>
    </div>
  </div>
</template>
