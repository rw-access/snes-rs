import init, { SnesWeb } from "./pkg/snes_wasm.js";

const debugEnabled = new URLSearchParams(location.search).get("debug") === "1";
const debugEndpoint = debugEnabled ? new URL("__debug", document.baseURI).href : null;
const recordEndpoint = new URL("__record", document.baseURI).href;
function debugValue(value) {
  if (value instanceof Error) return { name: value.name, message: value.message, stack: value.stack };
  if (typeof value === "string") return value;
  try { return JSON.parse(JSON.stringify(value)); }
  catch (_) { return String(value); }
}
function reportDebug(kind, ...values) {
  if (!debugEndpoint) return;
  fetch(debugEndpoint, { method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ timestamp: new Date().toISOString(), kind, values: values.map(debugValue), url: location.href }), keepalive: true }).catch(() => {});
}
window.addEventListener("error", event => reportDebug("window.error", event.error || event.message));
window.addEventListener("unhandledrejection", event => reportDebug("unhandledrejection", event.reason));

const canvas = document.querySelector("#screen");
const context = canvas.getContext("2d", { alpha: false });
const status = document.querySelector("#status");
const romInput = document.querySelector("#rom-input");
const audioButton = document.querySelector("#audio-button");
const snapshotButton = document.querySelector("#snapshot-button");
const restoreButton = document.querySelector("#restore-button");
const rewindButton = document.querySelector("#rewind-button");
const recordButton = document.querySelector("#record-button");
const stopRecordButton = document.querySelector("#stop-record-button");
const recordingStatus = document.querySelector("#recording-status");
const BUTTONS = Object.freeze({ b: 1 << 0, y: 1 << 1, select: 1 << 2, start: 1 << 3, up: 1 << 4, down: 1 << 5, left: 1 << 6, right: 1 << 7, a: 1 << 8, x: 1 << 9, l: 1 << 10, r: 1 << 11 });
const FRAME_MS = 1000 / 60;
let emulator = null, romName = null, savedSnapshot = null, heldButtons = 0, rewinding = false, previousTime = 0, accumulator = 0;
let image = null;
let recording = null;

function jsonNumber(value) {
  return value == null ? null : typeof value === "bigint" ? Number(value) : value;
}

function recordingTime() {
  return recording ? Math.max(0, Math.round(performance.now() - recording.started_perf_at)) : 0;
}

function recordInputEvent(type, details = {}) {
  if (!recording) return;
  recording.input_events.push({ t_ms: recordingTime(), type, buttons: heldButtons, ...details });
}

function recordFrame(kind, metadata, details = {}) {
  if (!recording) return;
  recording.frames.push({
    index: recording.frames.length,
    t_ms: recordingTime(),
    kind,
    buttons: heldButtons,
    emulator_frame: jsonNumber(metadata?.frame_number),
    ...details,
  });
  recordingStatus.textContent = `Recording · ${(recordingTime() / 1000).toFixed(1)}s · ${recording.frames.length} frames`;
}

function setRecordingButtons(active) {
  recordButton.disabled = !emulator || active;
  stopRecordButton.disabled = !active;
  recordButton.classList.toggle("recording", active);
}

function startRecording() {
  if (!emulator || recording) return;
  const metadata = emulator.frame_metadata();
  recording = {
    version: 1,
    source: "snes-rs-browser",
    rom_name: romName,
    frame_rate: 60,
    started_at: new Date().toISOString(),
    started_perf_at: performance.now(),
    started_frame: jsonNumber(metadata.frame_number),
    frames: [],
    input_events: [],
  };
  recordInputEvent("record-start");
  setRecordingButtons(true);
  recordingStatus.textContent = "Recording · 0.0s · 0 frames";
}

async function stopRecording() {
  if (!recording) return;
  if (rewinding) stopRewind();
  const payload = {
    ...recording,
    stopped_at: new Date().toISOString(),
    stopped_frame: jsonNumber(emulator?.frame_metadata()?.frame_number),
  };
  window.lastRecordingPayload = payload;
  recording = null;
  setRecordingButtons(false);
  recordingStatus.textContent = "Uploading…";
  try {
    const response = await fetch(recordEndpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    const body = await response.text();
    let result = {};
    try { result = JSON.parse(body); } catch (_) {}
    if (!response.ok || !result.ok) throw new Error(result.error || body || `HTTP ${response.status}`);
    recordingStatus.textContent = `Saved · ${result.frames} frames`;
    setStatus(`Recording saved on server as ${result.filename}`);
  } catch (error) {
    reportDebug("record-upload", error);
    recordingStatus.textContent = `Upload failed — retry: ${error?.message ?? error}`;
    setStatus(`Could not upload recording: ${error?.message ?? error}`);
  }
}

class AudioScheduler {
  constructor() {
    this.context = null;
    this.contextConstructor = window.AudioContext || window.webkitAudioContext;
    this.nextTime = 0;
    this.sources = new Set();
    this.prefillSeconds = 0.05;
    this.muted = false;
  }
  updateButton() { audioButton.textContent = this.muted ? "Audio muted" : this.context?.state === "running" ? "Audio enabled" : "Enable audio"; }
  async enable() {
    if (!this.contextConstructor) throw new Error("Web Audio is not supported by this browser");
    if (navigator.audioSession) {
      try { navigator.audioSession.type = "playback"; }
      catch (error) { reportDebug("audio-session", error); }
    }
    if (!this.context) {
      this.context = new this.contextConstructor();
      this.nextTime = this.context.currentTime + this.prefillSeconds;
      const unlockBuffer = this.context.createBuffer(1, 1, this.context.sampleRate);
      const unlockSource = this.context.createBufferSource();
      unlockSource.buffer = unlockBuffer;
      unlockSource.connect(this.context.destination);
      unlockSource.start(0);
    }
    if (this.context.state !== "running") await this.context.resume();
    if (this.context.state !== "running") throw new Error(`AudioContext is ${this.context.state}`);
    this.updateButton();
  }
  toggleMute() { this.muted = !this.muted; if (this.muted) this.flush(); this.updateButton(); }
  flush() {
    if (!this.context) return;
    for (const source of this.sources) { try { source.stop(); } catch (_) {} }
    this.sources.clear();
    this.nextTime = this.context.currentTime + this.prefillSeconds;
  }
  schedule(interleaved, frames, rate, discontinuity) {
    if (discontinuity) this.flush();
    if (!this.context || this.muted || !frames || !interleaved.length) return;
    const sourceFrames = Math.min(frames, Math.floor(interleaved.length / 2));
    const samples = rate === this.context.sampleRate
      ? interleaved.subarray(0, sourceFrames * 2)
      : resampleStereo(interleaved.subarray(0, sourceFrames * 2), rate, this.context.sampleRate);
    const outputFrames = Math.floor(samples.length / 2);
    if (!outputFrames) return;
    const output = this.context.createBuffer(2, outputFrames, this.context.sampleRate);
    const left = output.getChannelData(0), right = output.getChannelData(1);
    for (let i = 0; i < outputFrames; i++) { left[i] = samples[i * 2]; right[i] = samples[i * 2 + 1]; }
    const source = this.context.createBufferSource(); source.buffer = output; source.connect(this.context.destination);
    const start = Math.max(this.nextTime, this.context.currentTime + .01); source.start(start); this.nextTime = start + outputFrames / this.context.sampleRate;
    this.sources.add(source); source.addEventListener("ended", () => this.sources.delete(source), { once: true });
  }
}
const audio = new AudioScheduler();
function resampleStereo(samples, sourceRate, targetRate) {
  const sourceFrames = Math.floor(samples.length / 2);
  if (!sourceFrames) return new Float32Array(0);
  const outputFrames = Math.max(1, Math.round(sourceFrames * targetRate / sourceRate));
  const output = new Float32Array(outputFrames * 2);
  const ratio = sourceRate / targetRate;
  for (let frame = 0; frame < outputFrames; frame++) {
    const position = frame * ratio;
    const leftFrame = Math.floor(position);
    const rightFrame = Math.min(leftFrame + 1, sourceFrames - 1);
    const fraction = position - leftFrame;
    for (let channel = 0; channel < 2; channel++) {
      const left = samples[leftFrame * 2 + channel];
      const right = samples[rightFrame * 2 + channel];
      output[frame * 2 + channel] = left * (1 - fraction) + right * fraction;
    }
  }
  return output;
}
function setStatus(text) { status.textContent = text; }
function render(metadata) {
  if (!emulator) return;
  const rgba = emulator.rgba_buffer();
  if (!image || canvas.width !== metadata.width || canvas.height !== metadata.height) {
    canvas.width = metadata.width;
    canvas.height = metadata.height;
    image = context.createImageData(metadata.width, metadata.height);
  }
  if (rgba.length !== image.data.length) throw new Error(`unexpected RGBA buffer length: ${rgba.length}`);
  image.data.set(rgba);
  context.putImageData(image, 0, 0);
}
function drawFrame(schedule = true) {
  const metadata = emulator.step_frame();
  render(metadata);
  setStatus(`${rewinding ? "Rewinding · " : ""}Frame ${metadata.frame_number}`);
  if (schedule) {
    try { audio.schedule(emulator.audio_buffer(), metadata.audio_samples, metadata.audio_sample_rate, metadata.audio_discontinuity); }
    catch (error) { reportDebug("audio-scheduling", error); setStatus(`Frame ${metadata.frame_number} · audio unavailable`); }
  }
  recordFrame("frame", metadata);
  return metadata;
}
function rewindFrame() {
  if (!emulator) return false;
  const available = emulator.rewind();
  const metadata = emulator.frame_metadata();
  render(metadata);
  setStatus(`Rewinding · Frame ${metadata.frame_number}`);
  recordFrame("rewind", metadata, { available });
  return available;
}
function updateButtons() { if (emulator && !rewinding) emulator.set_controller(heldButtons); }
function setButton(name, pressed) {
  if (pressed) heldButtons |= BUTTONS[name]; else heldButtons &= ~BUTTONS[name];
  recordInputEvent(pressed ? "button-down" : "button-up", { button: name });
  updateButtons();
}
function startRewind() {
  if (!emulator || rewinding) return;
  rewinding = true;
  audio.flush();
  rewindButton.classList.add("pressed");
  accumulator = 0;
  recordInputEvent("rewind-start");
  setStatus("Rewinding…");
}
function stopRewind() {
  if (!rewinding) return;
  rewinding = false;
  rewindButton.classList.remove("pressed");
  recordInputEvent("rewind-stop");
  updateButtons();
  previousTime = performance.now();
  accumulator = 0;
}

async function loadRom(file) {
  try {
    setStatus(`Reading ${file.name}…`);
    audio.flush();
    const bytes = new Uint8Array(await file.arrayBuffer());
    const previousEmulator = emulator;
    emulator = null;
    previousEmulator?.free();
    emulator = SnesWeb.load_rom(bytes);
    romName = file.name;
    savedSnapshot = null;
    heldButtons = 0;
    image = null;
    rewinding = false;
    snapshotButton.disabled = false;
    restoreButton.disabled = true;
    rewindButton.disabled = false;
    recording = null;
    recordButton.disabled = false;
    stopRecordButton.disabled = true;
    recordButton.classList.remove("recording");
    recordingStatus.textContent = "Recording is off";
    previousTime = performance.now();
    accumulator = FRAME_MS;
    updateButtons();
    setStatus(`${file.name} loaded · press Enable audio to hear it`);
  } catch (error) {
    reportDebug("rom-load", error);
    emulator = null;
    savedSnapshot = null;
    snapshotButton.disabled = true;
    restoreButton.disabled = true;
    rewindButton.disabled = true;
    recording = null;
    setRecordingButtons(false);
    recordingStatus.textContent = "Recording is off";
    setStatus(`Could not load ROM: ${error?.message ?? error}`);
  }
}
romInput.addEventListener("click", () => { romInput.value = ""; });
romInput.addEventListener("change", () => { if (romInput.files?.[0]) void loadRom(romInput.files[0]); });
audioButton.addEventListener("click", () => audio.enable().catch(error => { reportDebug("audio", error); setStatus(`Audio unavailable: ${error?.message ?? error}`); }));
snapshotButton.addEventListener("click", () => { if (emulator) { savedSnapshot = emulator.save_snapshot(); restoreButton.disabled = false; setStatus("State saved"); } });
restoreButton.addEventListener("click", () => {
  if (!emulator || !savedSnapshot) return;
  try { emulator.restore_snapshot(savedSnapshot); audio.flush(); accumulator = FRAME_MS; setStatus("State restored"); }
  catch (error) { reportDebug("snapshot-restore", error); setStatus(`Could not restore state: ${error?.message ?? error}`); }
});
recordButton.addEventListener("click", startRecording);
stopRecordButton.addEventListener("click", () => { void stopRecording(); });
rewindButton.addEventListener("pointerdown", event => { event.preventDefault(); rewindButton.setPointerCapture(event.pointerId); startRewind(); });
for (const name of ["pointerup", "pointercancel", "lostpointercapture"]) rewindButton.addEventListener(name, event => { event.preventDefault(); stopRewind(); });
for (const button of document.querySelectorAll("[data-button]")) {
  const name = button.dataset.button;
  button.addEventListener("pointerdown", event => { event.preventDefault(); button.setPointerCapture(event.pointerId); button.classList.add("pressed"); setButton(name, true); });
  for (const eventName of ["pointerup", "pointercancel", "lostpointercapture"]) button.addEventListener(eventName, event => { event.preventDefault(); button.classList.remove("pressed"); setButton(name, false); });
}
const keyboard = new Map([["w", "up"], ["a", "left"], ["s", "down"], ["d", "right"], ["ArrowUp", "up"], ["ArrowDown", "down"], ["ArrowLeft", "left"], ["ArrowRight", "right"], ["j", "b"], ["k", "a"], ["z", "b"], ["x", "a"], ["u", "y"], ["i", "x"], ["o", "l"], ["p", "r"], ["Shift", "select"], ["Enter", "start"]]);
window.addEventListener("keydown", event => { const key = event.key.length === 1 ? event.key.toLowerCase() : event.key; if (key === "r") { event.preventDefault(); startRewind(); return; } if (key === "m" && !event.repeat) { event.preventDefault(); audio.toggleMute(); return; } const name = keyboard.get(key); if (name) { event.preventDefault(); setButton(name, true); } });
window.addEventListener("keyup", event => { const key = event.key.length === 1 ? event.key.toLowerCase() : event.key; if (key === "r") { event.preventDefault(); stopRewind(); return; } const name = keyboard.get(key); if (name) { event.preventDefault(); setButton(name, false); } });
window.addEventListener("blur", () => { heldButtons = 0; stopRewind(); updateButtons(); });
document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible" && audio.context) audio.enable().catch(error => reportDebug("audio-resume", error)); });
function tick(now) { try { if (!previousTime) previousTime = now; accumulator += Math.min(now - previousTime, 250); previousTime = now; let count = 0; while (emulator && accumulator >= FRAME_MS && count++ < 4) { if (rewinding) rewindFrame(); else drawFrame(); accumulator -= FRAME_MS; } } catch (error) { reportDebug("emulation-frame", error); setStatus(`Emulation stopped: ${error?.message ?? error}`); emulator = null; savedSnapshot = null; recording = null; rewinding = false; snapshotButton.disabled = true; restoreButton.disabled = true; rewindButton.disabled = true; setRecordingButtons(false); recordingStatus.textContent = "Recording is off"; } requestAnimationFrame(tick); }
try {
  await init("./pkg/snes_wasm_bg.wasm?v=2");
  requestAnimationFrame(tick);
} catch (error) {
  reportDebug("wasm-init", error);
  setStatus(`WASM initialization failed: ${error?.message ?? error}`);
}
