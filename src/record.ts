// ---------------------------------------------------------------------------
// Screen recording — capture a short clip via getDisplayMedia + MediaRecorder
// and hand back a webm data URL. Feature-detected: callers should check
// recordingSupported() and hide the entry point when it returns false (older
// WebViews / missing OS Screen Recording permission won't expose the API).
// ---------------------------------------------------------------------------

let mediaRecorder: MediaRecorder | null = null;
let activeStream: MediaStream | null = null;
let chunks: Blob[] = [];

/** True when the running WebView exposes screen capture + MediaRecorder. */
export function recordingSupported(): boolean {
  return (
    typeof MediaRecorder !== "undefined" &&
    typeof navigator.mediaDevices?.getDisplayMedia === "function"
  );
}

export function isRecording(): boolean {
  return mediaRecorder !== null;
}

/** Pick a webm MIME the platform can actually encode. */
function pickMimeType(): string {
  const candidates = ["video/webm;codecs=vp9", "video/webm;codecs=vp8", "video/webm"];
  for (const m of candidates) {
    if (MediaRecorder.isTypeSupported(m)) return m;
  }
  return "video/webm";
}

function cleanup(): void {
  activeStream?.getTracks().forEach((t) => t.stop());
  activeStream = null;
  mediaRecorder = null;
  chunks = [];
}

/**
 * Start recording the user-chosen screen/window. `onComplete` fires once with a
 * `data:video/webm;base64,…` URL when recording ends — whether via stopRecording()
 * or the browser's own "Stop sharing" control. `onError` fires if the user
 * denies the picker or capture fails.
 */
export async function startRecording(
  onComplete: (dataUrl: string) => void,
  onError: (message: string) => void
): Promise<void> {
  if (isRecording()) return;
  try {
    activeStream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: false });
  } catch (e) {
    onError(e instanceof Error ? e.message : String(e));
    return;
  }
  chunks = [];
  const recorder = new MediaRecorder(activeStream, { mimeType: pickMimeType() });
  mediaRecorder = recorder;

  recorder.ondataavailable = (e) => {
    if (e.data && e.data.size > 0) chunks.push(e.data);
  };
  recorder.onstop = () => {
    const blob = new Blob(chunks, { type: "video/webm" });
    cleanup();
    if (blob.size === 0) {
      onError("rekaman kosong");
      return;
    }
    const reader = new FileReader();
    reader.onload = () => onComplete(String(reader.result));
    reader.onerror = () => onError("gagal membaca rekaman");
    reader.readAsDataURL(blob);
  };
  // If the user stops sharing via the OS/browser control, end gracefully.
  activeStream.getVideoTracks()[0]?.addEventListener("ended", () => stopRecording());
  recorder.start();
}

/** Stop an in-progress recording (triggers the onComplete passed to start). */
export function stopRecording(): void {
  if (mediaRecorder && mediaRecorder.state !== "inactive") {
    mediaRecorder.stop();
  }
}
