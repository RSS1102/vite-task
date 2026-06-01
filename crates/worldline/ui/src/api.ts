// Types mirroring the Rust `serve::data::ApiData` payload, plus loaders.

export interface Blob {
  enc: 'utf8' | 'base64';
  data: string;
}

/** One write: the file's content before (open) and after (close), as blob ids. */
export interface ApiWrite {
  seq: number;
  path: string;
  before: string;
  after: string;
  output_offset: number;
}

export interface ApiData {
  argv: string[];
  cwd: string;
  exit_code: number | null;
  output_len: number;
  writes: ApiWrite[];
  blobs: Record<string, Blob>;
}

export async function loadData(): Promise<ApiData> {
  const res = await fetch('api/data');
  if (!res.ok) throw new Error(`failed to load /api/data: ${res.status}`);
  return (await res.json()) as ApiData;
}

export async function loadOutput(): Promise<Uint8Array> {
  const res = await fetch('api/output');
  if (!res.ok) throw new Error(`failed to load /api/output: ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

const decoder = new TextDecoder();

/** Decode a blob to text; base64 blobs are decoded to a UTF-8 (lossy) string. */
export function blobText(blob: Blob): string {
  if (blob.enc === 'utf8') return blob.data;
  const bin = atob(blob.data);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return decoder.decode(bytes);
}

/** Whether a blob is binary (non-UTF-8) content. */
export function blobIsBinary(blob: Blob): boolean {
  return blob.enc === 'base64';
}

/** The final path segment, for compact display. */
export function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}
