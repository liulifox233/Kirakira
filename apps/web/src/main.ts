const canvas = document.querySelector<HTMLCanvasElement>("#kirakira-canvas");
if (!canvas) throw new Error("Kirakira canvas is missing");

const debugMode = new URLSearchParams(window.location.search).has("debug");
const log = (...args: unknown[]) => console.info("[kirakira]", ...args);
const warn = (...args: unknown[]) => console.warn("[kirakira]", ...args);
const errorLog = (...args: unknown[]) => console.error("[kirakira]", ...args);

const encodeHex = (bytes: Uint8Array) => {
  let value = "";
  for (const byte of bytes) value += byte.toString(16).padStart(2, "0");
  return value;
};

const pendingStorageWrites = new Map<string, { key: string; bytes: Uint8Array }>();
let storageFlushTimer: number | undefined;

const flushStorageWrites = () => {
  storageFlushTimer = undefined;
  for (const [identity, item] of pendingStorageWrites) {
    try {
      localStorage.setItem(item.key, encodeHex(item.bytes));
      log("persistent storage saved", { identity, bytes: item.bytes.byteLength });
    } catch (error) {
      warn("persistent storage save failed", identity, error);
    }
  }
  pendingStorageWrites.clear();
};

const persistStorageWrites = (runtime: any, game: string, flush = false) => {
  if (!game) return;
  const prefix = `kirakira.save.v1.${encodeHex(new TextEncoder().encode(game))}.`;
  for (const item of runtime.drain_storage_writes?.() ?? []) {
    const path = String(item.path ?? "");
    const bytes = item.bytes instanceof Uint8Array
      ? new Uint8Array(item.bytes)
      : new Uint8Array(item.bytes ?? []);
    const key = `${prefix}${encodeHex(new TextEncoder().encode(path))}`;
    pendingStorageWrites.set(`${game}:${path}`, { key, bytes });
  }
  if (flush) flushStorageWrites();
  else if (pendingStorageWrites.size && storageFlushTimer === undefined) {
    storageFlushTimer = window.setTimeout(flushStorageWrites, 100);
  }
};

const bootStartedAt = performance.now();
log("boot", { href: window.location.href, userAgent: navigator.userAgent });
if (window.location.protocol === "file:") {
  const message = "Kirakira Web requires an HTTP(S) static server; file:// cannot fetch WASM or game assets in browsers";
  document.body.dataset.status = message;
  errorLog(message);
  throw new Error(message);
}

const resize = () => {
  const ratio = window.devicePixelRatio || 1;
  canvas.width = Math.max(1, Math.floor(canvas.clientWidth * ratio));
  canvas.height = Math.max(1, Math.floor(canvas.clientHeight * ratio));
};

Object.assign(canvas.style, {
  display: "block",
  width: "100vw",
  height: "100vh",
});
window.addEventListener("resize", resize);
resize();
log("canvas ready", { width: canvas.width, height: canvas.height, devicePixelRatio: window.devicePixelRatio || 1 });

// The wasm module is injected by the build pipeline. Keeping this import
// dynamic lets static package hosting work before a generated wasm artifact is
// available and avoids coupling the shell to a particular bundler.
const wasmUrl = document.body.dataset.wasm;
if (wasmUrl) {
  // Resolve against the document URL, not this bundled module's `/assets/`
  // directory. Otherwise `./pkg/krkr_web.js` becomes `/assets/pkg/...` after
  // Vite emits the shell and static hosting returns the HTML fallback.
  const wasmModuleUrl = new URL(wasmUrl, window.location.href).href;
  log("loading wasm module", wasmModuleUrl);
  const wasm = await import(/* @vite-ignore */ wasmModuleUrl);
  // Initialize from bytes instead of relying on instantiateStreaming. Some
  // development servers (including Vite's wasm response path) expose a
  // valid `application/wasm` MIME type but still produce an incomplete
  // import object during streaming instantiation. Passing the bytes follows
  // the same path as the Node smoke test and works consistently in browsers
  // and static hosting.
  const wasmBinaryUrl = new URL("krkr_web_bg.wasm", wasmModuleUrl);
  const wasmResponse = await fetch(wasmBinaryUrl);
  if (!wasmResponse.ok) throw new Error(`Unable to load wasm binary: ${wasmResponse.status}`);
  const wasmBytes = await wasmResponse.arrayBuffer();
  log("initializing wasm", { url: wasmBinaryUrl.href, bytes: wasmBytes.byteLength });
  await wasm.default(wasmBytes);
  wasm.attach_canvas("kirakira-canvas");
  const runtime = new wasm.WebRuntime(canvas.clientWidth, canvas.clientHeight);
  runtime.set_device_pixel_ratio?.(window.devicePixelRatio || 1);
  window.addEventListener("resize", () => {
    runtime.set_device_pixel_ratio?.(window.devicePixelRatio || 1);
  });
  log("runtime created", { elapsedMs: Math.round(performance.now() - bootStartedAt) });
  if (debugMode) {
    (window as any).__kirakiraRuntime = runtime;
  }
  const updateViewportMetadata = () => {
    const orientation = screen.orientation?.type?.startsWith("portrait") ? "portrait" : "landscape";
    runtime.set_orientation(orientation);
    // Browsers do not expose safe-area env() values as numbers directly;
    // native shells can provide them through this same host method.
    runtime.set_safe_area(0, 0, 0, 0);
  };
  updateViewportMetadata();
  window.addEventListener("orientationchange", updateViewportMetadata);
  const requestedPackage = document.body.dataset.package;
  if (requestedPackage) {
    document.body.dataset.status = `Loading game package: ${requestedPackage}`;
    log("package load started", requestedPackage);
    const loadTimer = window.setInterval(() => {
      warn("package load is still pending", {
        package: requestedPackage,
        elapsedMs: Math.round(performance.now() - bootStartedAt),
      });
    }, 5000);
    try {
      await runtime.load_package(requestedPackage);
      document.body.dataset.status = "Game package ready";
      log("package ready", { base: requestedPackage, elapsedMs: Math.round(performance.now() - bootStartedAt) });

      // A published game may provide an explicit entry in the static shell;
      // otherwise use the entry recorded in manifest.json.  Never infer an
      // entry from conventional filenames: first.ks/title.ks are frequently
      // game-specific dispatchers (including first-run settings flows).
      const explicitScenario = document.body.dataset.scenario?.trim();
      const requestedScenario = explicitScenario || runtime.entry_scenario().trim();
      if (!explicitScenario && requestedScenario) {
        document.body.dataset.scenario = requestedScenario;
        log("manifest entry scenario selected", requestedScenario);
      }
      if (requestedScenario) {
        document.body.dataset.status = `Loading scenario: ${requestedScenario}`;
        log("scenario load started", requestedScenario);
        try {
          const ready = runtime.load_scenario(requestedScenario);
          document.body.dataset.status = ready
            ? `Scenario ready: ${requestedScenario}`
            : `Loading scenario: ${requestedScenario}`;
          log(ready ? "scenario ready" : "scenario waiting for resource", {
            storage: requestedScenario,
            elapsedMs: Math.round(performance.now() - bootStartedAt),
          });
        } catch (error) {
          errorLog("scenario load failed", requestedScenario, error);
          document.body.dataset.status = `Scenario failed: ${error}`;
          throw error;
        }
      } else {
        // A normal KRKR game owns scenario selection inside startup.tjs. In
        // particular, GINKA creates its KAGWindow and dispatches first.ks
        // asynchronously after bootstrap resources arrive.
        document.body.dataset.status = "Game startup owns scenario selection";
        log("no host scenario; continuing startup.tjs dispatch");
      }
    } catch (error) {
      errorLog("package load failed", requestedPackage, error);
      if (String(error).includes("HTTP 404") && String(error).includes("manifest.json")) {
        document.body.dataset.status = "No game package configured";
      } else {
        document.body.dataset.status = `Game package failed: ${error}`;
        throw error;
      }
    } finally {
      window.clearInterval(loadTimer);
    }
  } else {
    log("no package configured; starting empty runtime");
  }
  const persistRuntimeState = () => {
    try {
      runtime.persist_runtime_state();
      persistStorageWrites(runtime, runtime.package_game(), true);
      log("runtime state persisted");
    } catch (error) {
      warn("runtime state persistence failed", error);
    }
  };
  // `pagehide` fires for tab/window closes and navigations, while the
  // visibility fallback covers mobile browsers that suspend a page without a
  // conventional unload event. Both routes use the same Desktop shutdown
  // hook; no Web-specific game-flow decision is made here.
  window.addEventListener("pagehide", persistRuntimeState);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") persistRuntimeState();
  });
  // Avoid claiming the canvas' WebGPU context when no adapter is available;
  // once a canvas has a GPU context browsers refuse a later 2D context.
  const gpuAvailable = Boolean(
    navigator.gpu && await navigator.gpu.requestAdapter(),
  );
  const gpuRenderer = gpuAvailable && await runtime.init_renderer("kirakira-canvas");
  document.body.dataset.renderer = gpuRenderer ? "webgpu" : "canvas2d";
  log("renderer ready", { renderer: gpuRenderer ? "webgpu" : "canvas2d", elapsedMs: Math.round(performance.now() - bootStartedAt) });
  const context = gpuRenderer ? null : canvas.getContext("2d");
  if (!gpuRenderer && !context) throw new Error("2D canvas is unavailable");
  const textures = new Map<number, HTMLCanvasElement>();
  if (debugMode) (window as any).__kirakiraTextures = textures;
  let audioContext: AudioContext | undefined;
  const getAudioContext = () => {
    audioContext ??= new AudioContext();
    return audioContext;
  };
  type AudioNodeState = {
    source: AudioBufferSourceNode;
    gain: GainNode;
    buffer: AudioBuffer;
    bus: string;
    volume: number;
    looping: boolean;
    offset: number;
    startedAt: number;
    paused: boolean;
  };
  const audioNodes = new Map<number, AudioNodeState>();
  // KRKR's BGM/voice channels are logically single-instance even when the
  // script creates a fresh WaveSoundBuffer object for a replacement track.
  // Keep that channel identity separate from the object id so an async fetch
  // cannot resurrect an older track over the new one.
  const audioChannels = new Map<string, number>();
  // Keep an epoch for every KRKR channel. A stop/replace arriving while an
  // asset is being fetched must invalidate that request before it can start.
  const audioEpochs = new Map<number, number>();
  const audioPending = new Map<number, { bus: string; epoch: number }>();
  const audioBusVolumes = new Map<string, number>([["master", 1], ["bgm", 1], ["sound-effect", 1]]);
  const pendingGestureAudio = new Map<number, any>();
  const pendingPaused = new Set<number>();
  let audioQueue: Promise<void> = Promise.resolve();
  const videoElements = new Map<string, HTMLVideoElement>();
  const videoLoads = new Map<string, Promise<void>>();
  const videoPlayRequests = new Map<string, Promise<void>>();
  const videoDesiredStatus = new Map<string, string>();
  const videoObjectUrls = new Map<string, string>();
  const videoAudioNodes = new Map<string, { source: MediaElementAudioSourceNode; panner: StereoPannerNode }>();
  const endedVideos = new Set<string>();
  let userGesture = false;

  const requestVideoPlay = (storage: string, video: HTMLVideoElement) => {
    if (videoDesiredStatus.get(storage) !== "play" || !video.src || !video.paused) return;
    if (videoPlayRequests.has(storage)) return;
    // Muted autoplay is allowed by browsers and lets the opening animation
    // remain visible even before the first pointer/key gesture. Once the user
    // interacts, activateMedia() unmutes the same element and retries it.
    video.muted = !userGesture;
    const request = video.play()
      .then(() => {
        log("video playing", { storage, muted: video.muted });
      })
      .catch((error) => {
        if (!video.muted) {
          warn("video autoplay with audio blocked; retrying muted", storage, error);
          video.muted = true;
          return video.play()
            .then(() => log("video playing muted", { storage }))
            .catch((mutedError) => warn("video autoplay blocked", storage, mutedError));
        }
        warn("video autoplay blocked", storage, error);
      })
      .finally(() => {
        if (videoPlayRequests.get(storage) === request) videoPlayRequests.delete(storage);
      });
    videoPlayRequests.set(storage, request);
  };

  const activateMedia = () => {
    userGesture = true;
    if (audioContext) {
      void audioContext.resume().catch((error) => warn("AudioContext resume failed", error));
    }
    for (const [storage, video] of videoElements) {
      video.muted = false;
      requestVideoPlay(storage, video);
    }
    if (pendingGestureAudio.size) {
      const commands = [...pendingGestureAudio.values()];
      pendingGestureAudio.clear();
      audioQueue = audioQueue
        .then(() => handleAudio(commands))
        .catch((error) => warn("deferred WebAudio batch failed", error));
      log("deferred audio activated", { count: commands.length });
    }
  };

  const handleVideos = async (videos: any[]) => {
    const active = new Set<string>();
    for (const item of videos ?? []) {
      if (!item.storage || item.visible === 0 || item.status === "unload" || item.status === "stop") continue;
      if (endedVideos.has(item.storage)) continue;
      const storage = String(item.storage);
      active.add(storage);
      videoDesiredStatus.set(storage, String(item.status ?? "ready"));
      let video = videoElements.get(storage);
      if (!video) {
        log("video requested", { storage, status: item.status });
        video = document.createElement("video");
        video.muted = !userGesture;
        video.playsInline = true;
        video.autoplay = true;
        video.style.position = "fixed";
        video.style.pointerEvents = "none";
        video.style.zIndex = "2";
        document.body.appendChild(video);
        videoElements.set(storage, video);
        try {
          const videoContext = getAudioContext();
          const source = videoContext.createMediaElementSource(video);
          const panner = videoContext.createStereoPanner();
          source.connect(panner).connect(videoContext.destination);
          videoAudioNodes.set(storage, { source, panner });
        } catch (error) {
          // Older browsers may not expose MediaElementSource/StereoPanner;
          // the element still plays with its native volume path in that case.
          warn("video balance bridge unavailable", { storage, error });
        }
        video.addEventListener("ended", () => {
          try {
            runtime.notify_video_ended(storage);
          } catch (error) {
            console.warn("Web video end notification failed", storage, error);
          }
          endedVideos.add(storage);
          videoElements.delete(storage);
          videoDesiredStatus.delete(storage);
          videoPlayRequests.delete(storage);
          videoLoads.delete(storage);
          const objectUrl = videoObjectUrls.get(storage);
          if (objectUrl) URL.revokeObjectURL(objectUrl);
          videoObjectUrls.delete(storage);
          const audioNodes = videoAudioNodes.get(storage);
          audioNodes?.source.disconnect();
          audioNodes?.panner.disconnect();
          videoAudioNodes.delete(storage);
          video.remove();
        }, { once: true });
        const load = (async () => {
          try {
            const bytes = await runtime.load_video(storage);
            log("video bytes ready", { storage, bytes: bytes.byteLength });
            if (videoElements.get(storage) !== video) return;
            // Prefer the container signature over the semantic extension: a
            // number of KRKR projects keep MPEG-4 data under a `.wmv` name.
            // Supplying the right MIME lets browsers select the decoder and
            // avoids the silent black overlay caused by a mismatched Blob.
            const signature = bytes.byteLength >= 12
              ? String.fromCharCode(...bytes.subarray(4, 8))
              : "";
            const extension = storage.toLowerCase().split(".").pop() ?? "";
            const extensionMime: Record<string, string> = {
              mp4: "video/mp4",
              m4v: "video/mp4",
              webm: "video/webm",
              ogv: "video/ogg",
              ogg: "video/ogg",
              mov: "video/quicktime",
              wmv: "video/x-ms-wmv",
              avi: "video/x-msvideo",
            };
            const mime = signature === "ftyp"
              ? "video/mp4"
              : (extensionMime[extension] ?? "application/octet-stream");
            const objectUrl = URL.createObjectURL(new Blob([bytes], { type: mime }));
            videoObjectUrls.set(storage, objectUrl);
            video.src = objectUrl;
            // `play()` used to run on the frame immediately after element
            // creation, before this async fetch completed. Start only after
            // the source is attached, and keep retrying on later frames if a
            // browser rejects the first autoplay attempt.
            requestVideoPlay(storage, video);
          } catch (error) {
            warn("video load failed", storage, error);
          } finally {
            videoLoads.delete(storage);
          }
        })();
        videoLoads.set(storage, load);
      }
      video.style.left = `${item.left}px`;
      video.style.top = `${item.top}px`;
      video.style.width = `${item.width || canvas.width}px`;
      video.style.height = `${item.height || canvas.height}px`;
      video.loop = Boolean(item.looping);
      if (Number.isFinite(item.playRate) && item.playRate > 0) {
        video.playbackRate = Number(item.playRate);
      }
      if (Number.isFinite(item.audioVolume)) {
        video.volume = Math.max(0, Math.min(1, Number(item.audioVolume) / 100000));
      }
      const videoAudio = videoAudioNodes.get(storage);
      if (videoAudio && Number.isFinite(item.audioBalance)) {
        videoAudio.panner.pan.value = Math.max(-1, Math.min(1, Number(item.audioBalance) / 100000));
      }
      if (Number.isFinite(item.position) && video.readyState >= 1) {
        const target = Math.max(0, Number(item.position) / 1000);
        // Do not seek every frame; the engine position is authoritative only
        // when a script explicitly seeks or a new overlay starts.
        if (Math.abs(video.currentTime - target) > 0.35) {
          try { video.currentTime = target; } catch { /* metadata not ready */ }
        }
      }
      if (item.status === "play") requestVideoPlay(storage, video);
      else if (item.status === "pause") video.pause();
    }
    for (const [storage, video] of videoElements) {
      if (!active.has(storage)) {
        videoDesiredStatus.delete(storage);
        videoPlayRequests.delete(storage);
        videoLoads.delete(storage);
        const objectUrl = videoObjectUrls.get(storage);
        if (objectUrl) URL.revokeObjectURL(objectUrl);
        videoObjectUrls.delete(storage);
        const audioNodes = videoAudioNodes.get(storage);
        audioNodes?.source.disconnect();
        audioNodes?.panner.disconnect();
        videoAudioNodes.delete(storage);
        video.remove();
        videoElements.delete(storage);
      }
    }
  };

  const handleAudio = async (commands: any[]) => {
    const effectiveGain = (state: { bus: string; volume: number }) =>
      state.volume
      * (audioBusVolumes.get(state.bus) ?? 1)
      * (audioBusVolumes.get("master") ?? 1);
    const rampGain = (gain: GainNode, target: number, fadeSeconds = 0) => {
      const context = getAudioContext();
      const now = context.currentTime;
      gain.gain.cancelScheduledValues(now);
      if (fadeSeconds > 0) {
        gain.gain.setValueAtTime(gain.gain.value, now);
        gain.gain.linearRampToValueAtTime(target, now + fadeSeconds);
      } else {
        gain.gain.setValueAtTime(target, now);
      }
    };
    const stopNode = (id: number, fadeSeconds = 0) => {
      const state = audioNodes.get(id);
      if (!state) return;
      try {
        if (fadeSeconds > 0) {
          const context = getAudioContext();
          rampGain(state.gain, 0, fadeSeconds);
          state.source.stop(context.currentTime + fadeSeconds);
        } else {
          state.source.stop();
        }
      } catch { /* already ended */ }
      audioNodes.delete(id);
    };
    const installEndedHandler = (id: number, source: AudioBufferSourceNode) => {
      source.addEventListener("ended", () => {
        const state = audioNodes.get(id);
        if (state?.source !== source) return;
        audioNodes.delete(id);
        if (!state.paused) {
          try {
            runtime.notify_audio_stopped(id);
          } catch (error) {
            warn("audio completion callback failed", { id, error });
          }
          log("audio ended", { id });
        }
      }, { once: true });
    };
    const pauseNode = (id: number) => {
      const state = audioNodes.get(id);
      if (!state || state.paused) return;
      const context = getAudioContext();
      const elapsed = Math.max(0, context.currentTime - state.startedAt);
      const duration = state.buffer.duration;
      state.offset = state.looping && duration > 0
        ? (state.offset + elapsed) % duration
        : Math.min(duration, state.offset + elapsed);
      state.paused = true;
      try { state.source.stop(); } catch { /* already ended */ }
      log("audio paused", { id, offset: state.offset });
    };
    const resumeNode = async (id: number) => {
      const state = audioNodes.get(id);
      if (!state?.paused) return;
      const context = getAudioContext();
      await context.resume();
      const source = context.createBufferSource();
      source.buffer = state.buffer;
      source.loop = state.looping;
      source.connect(state.gain);
      source.start(0, state.offset);
      const resumed = {
        ...state,
        source,
        startedAt: context.currentTime,
        paused: false,
      };
      audioNodes.set(id, resumed);
      installEndedHandler(id, source);
      log("audio resumed", { id, offset: resumed.offset });
    };
    const nextEpoch = (id: number) => {
      const epoch = (audioEpochs.get(id) ?? 0) + 1;
      audioEpochs.set(id, epoch);
      return epoch;
    };
    const stopBus = (bus: string) => {
      for (const [id, state] of audioNodes) {
        if (state.bus === bus || bus === "master") {
          nextEpoch(id);
          stopNode(id);
        }
      }
      for (const [id, pending] of audioPending) {
        if (pending.bus === bus || bus === "master") {
          nextEpoch(id);
          audioPending.delete(id);
        }
      }
      for (const [id, pending] of pendingGestureAudio) {
        if (pending.bus === bus || bus === "master") pendingGestureAudio.delete(id);
      }
      for (const [channel, id] of audioChannels) {
        if (channel === bus || bus === "master") audioChannels.delete(channel);
      }
    };
    for (const command of commands ?? []) {
      if (command.kind === "play") {
        // A play command may remain in flight across several animation
        // frames while its bytes are fetched/decoded. Do not start duplicate
        // WebAudio sources for the same KRKR channel during that window;
        // completed playback on that channel is replaced explicitly below.
        if (audioPending.has(command.id)) continue;
        const bus = command.bus ?? "master";
        const epoch = nextEpoch(command.id);
        if (command.looping && bus === "bgm") {
          const previousId = audioChannels.get(bus);
          if (previousId !== undefined && previousId !== command.id) {
            nextEpoch(previousId);
            audioPending.delete(previousId);
            pendingGestureAudio.delete(previousId);
            stopNode(previousId);
          }
          audioChannels.set(bus, command.id);
        }
        // KRKR reuses stable WaveSoundBuffer ids. Starting a new source on
        // that id replaces the old one; leaving it alive is what causes
        // overlapping BGM/SE after a scenario transition.
        stopNode(command.id);
        if (!userGesture) {
          pendingGestureAudio.set(command.id, { ...command, bus });
          log("audio deferred until user gesture", { source: command.source, id: command.id });
          continue;
        }
        audioPending.set(command.id, { bus, epoch });
        try {
          const context = getAudioContext();
          log("audio requested", { source: command.source, id: command.id, looping: command.looping });
          await context.resume();
          const bytes = await runtime.load_storage(command.source);
          log("audio bytes ready", { source: command.source, bytes: bytes.byteLength });
          const buffer = await context.decodeAudioData(bytes.buffer.slice(0));
          const source = context.createBufferSource();
          source.buffer = buffer;
          source.loop = Boolean(command.looping);
          const gain = context.createGain();
          const busGain = (audioBusVolumes.get(bus) ?? 1) * (audioBusVolumes.get("master") ?? 1);
          const volume = command.volume ?? 1;
          gain.gain.value = volume * busGain;
          source.connect(gain).connect(context.destination);
          if (audioEpochs.get(command.id) !== epoch) {
            // A stop-bus/stop command invalidated this request while it was
            // waiting on fetch/decode. Do not resurrect the cancelled sound.
            try { source.stop(); } catch { /* not started */ }
            continue;
          }
          audioNodes.set(command.id, {
            source,
            gain,
            buffer,
            bus,
            volume,
            looping: Boolean(command.looping),
            offset: 0,
            startedAt: context.currentTime,
            paused: false,
          });
          installEndedHandler(command.id, source);
          source.start();
          if (pendingPaused.has(command.id)) {
            pendingPaused.delete(command.id);
            pauseNode(command.id);
          }
          log("audio playing", { source: command.source, id: command.id });
        } catch (error) {
          // A game may request codecs unavailable in the current browser.
          // Keep the frame loop alive and leave the diagnostic in the console.
          warn("WebAudio could not play command", command.source, error);
        } finally {
          if (audioPending.get(command.id)?.epoch === epoch) audioPending.delete(command.id);
        }
      } else if (command.kind === "stop") {
        nextEpoch(command.id);
        audioPending.delete(command.id);
        pendingGestureAudio.delete(command.id);
        pendingPaused.delete(command.id);
        stopNode(command.id, Number(command.fadeSeconds ?? 0));
        for (const [channel, id] of audioChannels) {
          if (id === command.id) audioChannels.delete(channel);
        }
        log("audio stopped", { id: command.id });
      } else if (command.kind === "stop-bus") {
        const bus = command.bus ?? "master";
        stopBus(bus);
        log("audio bus stopped", { bus });
      } else if (command.kind === "set-volume") {
        const state = audioNodes.get(command.id);
        if (state) {
          state.volume = command.volume ?? 1;
          rampGain(state.gain, effectiveGain(state), Number(command.fadeSeconds ?? 0));
        } else {
          const pending = pendingGestureAudio.get(command.id);
          if (pending) pending.volume = command.volume ?? 1;
        }
      } else if (command.kind === "set-bus-volume") {
        const bus = command.bus ?? "master";
        audioBusVolumes.set(bus, command.volume ?? 1);
        for (const state of audioNodes.values()) {
          if (bus === "master" || state.bus === bus) {
            rampGain(state.gain, effectiveGain(state), Number(command.fadeSeconds ?? 0));
          }
        }
      } else if (command.kind === "preload") {
        // The browser's fetch cache handles preloads; no source is started.
      } else if (command.kind === "play-pcm") {
        warn("WebAudio PCM stream is not supported by the browser bridge yet", command.id);
      } else if (command.kind === "pause") {
        if (audioNodes.has(command.id)) pauseNode(command.id);
        else if (audioPending.has(command.id)) pendingPaused.add(command.id);
      } else if (command.kind === "resume") {
        pendingPaused.delete(command.id);
        await resumeNode(command.id);
      }
    }
  };

  const color = (item: any) => `rgba(${Math.round(item.r * 255)},${Math.round(item.g * 255)},${Math.round(item.b * 255)},${item.a ?? 1})`;
  const transitionTextures = new Map<number, HTMLCanvasElement>();
  const uploadTextures = (uploads: any[], target: Map<number, HTMLCanvasElement>) => {
    for (const upload of uploads ?? []) {
      const bitmap = document.createElement("canvas");
      bitmap.width = upload.width;
      bitmap.height = upload.height;
      const bitmapContext = bitmap.getContext("2d");
      if (!bitmapContext) continue;
      bitmapContext.putImageData(
        new ImageData(new Uint8ClampedArray(upload.rgba), upload.width, upload.height),
        0,
        0,
      );
      target.set(Number(upload.textureId), bitmap);
    }
  };
  const render = (model: any) => {
    if (!context) return;
    const logicalWidth = Math.max(1, canvas.clientWidth || canvas.width);
    const logicalHeight = Math.max(1, canvas.clientHeight || canvas.height);
    const scaleX = canvas.width / logicalWidth;
    const scaleY = canvas.height / logicalHeight;
    context.save();
    // The engine works in CSS/logical pixels while the backing canvas may be
    // device-pixel-ratio scaled. Keep both paths aligned with the native
    // renderer instead of drawing the 1280x720 scene into the top-left
    // quarter of a Retina/HiDPI canvas.
    context.setTransform(scaleX, 0, 0, scaleY, 0, 0);
    context.fillStyle = color(model);
    context.fillRect(0, 0, logicalWidth, logicalHeight);
    uploadTextures(model.uploads, textures);
    for (const textureId of model.imageReleases ?? []) {
      textures.delete(Number(textureId));
    }
    const drawCommands = (commands: any[], alpha: number, preferred?: Map<number, HTMLCanvasElement>) => {
      context.save();
      context.globalAlpha = alpha;
      for (const item of commands ?? []) {
        if (item.kind === "rect") {
          context.fillStyle = color(item);
          context.fillRect(item.x, item.y, item.width, item.height);
        } else if (item.kind === "text") {
          context.fillStyle = color(item);
          const face = item.fontFace?.trim() || "sans-serif";
          const style = item.italic ? "italic " : "";
          const weight = item.bold ? "700 " : "400 ";
          context.font = `${style}${weight}${item.size}px "${face.replaceAll('"', '')}"`;
          context.textBaseline = "top";
          context.shadowColor = item.shadowR !== undefined
            ? `rgba(${Math.round(item.shadowR * 255)},${Math.round(item.shadowG * 255)},${Math.round(item.shadowB * 255)},${item.shadowA ?? 1})`
            : "transparent";
          context.shadowOffsetX = item.shadowX ?? 0;
          context.shadowOffsetY = item.shadowY ?? 0;
          context.fillText(item.text, item.x, item.y);
          context.shadowColor = "transparent";
          if (item.underline || item.strikeout) {
            const metrics = context.measureText(item.text);
            context.fillRect(item.x, item.y + item.size * (item.strikeout ? 0.55 : 0.9), metrics.width, Math.max(1, item.size / 16));
          }
        } else if (item.kind === "image") {
          const bitmap = preferred?.get(Number(item.textureId)) ?? textures.get(Number(item.textureId));
          if (!bitmap) continue;
          context.globalAlpha = alpha * (item.opacity ?? 1);
          const sx = item.sourceX ?? 0;
          const sy = item.sourceY ?? 0;
          const sw = item.sourceWidth ?? bitmap.width;
          const sh = item.sourceHeight ?? bitmap.height;
          // Draw the same atlas sub-rectangle selected by the Rust renderer.
          if (sw > 0 && sh > 0 && item.width > 0 && item.height > 0) {
            context.drawImage(bitmap, sx, sy, sw, sh, item.x, item.y, item.width, item.height);
          }
          context.globalAlpha = alpha;
        }
      }
      context.restore();
    };
    const transition = model.transition;
    if (transition) {
      uploadTextures(transition.frozenUploads, transitionTextures);
      uploadTextures(transition.ruleUploads, transitionTextures);
      const progress = Math.max(0, Math.min(1, Number(transition.progress ?? 1)));
      // Canvas2D does not implement every KRKR transition shader. Rendering
      // the frozen frame beneath the live frame is a deterministic crossfade
      // fallback for universal/scroll/wave/etc. and, importantly, avoids the
      // abrupt black/flash frame of the old fallback.
      drawCommands(transition.frozenDrawList, 1 - progress, transitionTextures);
      drawCommands(model.drawList, progress);
    } else {
      transitionTextures.clear();
      drawCommands(model.drawList, 1);
    }
    context.restore();
  };

  const toCanvasPoint = (event: PointerEvent) => {
    const rect = canvas.getBoundingClientRect();
    return {
      x: (event.clientX - rect.left) * ((canvas.clientWidth || rect.width) / Math.max(1, rect.width)),
      y: (event.clientY - rect.top) * ((canvas.clientHeight || rect.height) / Math.max(1, rect.height)),
    };
  };
  canvas.addEventListener("pointermove", (event) => {
    const point = toCanvasPoint(event);
    if (event.pointerType === "touch") runtime.touch_event(event.pointerId, point.x, point.y, "move");
    else runtime.pointer_move(point.x, point.y);
  });
  canvas.addEventListener("pointerdown", (event) => {
    activateMedia();
    canvas.setPointerCapture(event.pointerId);
    const point = toCanvasPoint(event);
    if (event.pointerType === "touch") runtime.touch_event(event.pointerId, point.x, point.y, "start");
    else runtime.pointer_down(point.x, point.y);
  });
  canvas.addEventListener("pointerup", (event) => {
    const point = toCanvasPoint(event);
    if (event.pointerType === "touch") runtime.touch_event(event.pointerId, point.x, point.y, "end");
    else runtime.pointer_up(point.x, point.y);
  });
  canvas.addEventListener("pointercancel", (event) => {
    const point = toCanvasPoint(event);
    if (event.pointerType === "touch") runtime.touch_event(event.pointerId, point.x, point.y, "cancel");
  });
  window.addEventListener("keydown", (event) => {
    activateMedia();
    runtime.key_down(event.key);
  });
  window.addEventListener("keyup", (event) => {
    runtime.key_up(event.key);
  });
  window.addEventListener("beforeinput", (event) => {
    const data = (event as InputEvent).data;
    if (data) runtime.text_input(data, false);
  });
  window.addEventListener("compositionupdate", (event) => {
    if (event.data) runtime.text_input(event.data, true);
  });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") {
      void runtime.lifecycle("background");
      runtime.suspend_renderer();
    } else {
      void runtime.lifecycle("foreground");
      runtime.resume_renderer();
    }
  });

  let totalFrames = 0;
  let intervalFrames = 0;
  let lastFrameLog = performance.now();
  let lastPendingSignature = "";
  const frame = (timestamp: number) => {
    const frameNumber = totalFrames + 1;
    const tickStartedAt = performance.now();
    if (debugMode) log("frame tick started", { frame: frameNumber, timestamp: Math.round(timestamp) });
    runtime.resize(canvas.clientWidth || canvas.width, canvas.clientHeight || canvas.height);
    const model = runtime.tick(timestamp);
    persistStorageWrites(runtime, runtime.package_game());
    const tickElapsedMs = performance.now() - tickStartedAt;
    if (debugMode || tickElapsedMs >= 50) {
      log("frame tick complete", { frame: frameNumber, elapsedMs: Math.round(tickElapsedMs) });
    }
    totalFrames += 1;
    intervalFrames += 1;
    const configuredScenario = document.body.dataset.scenario?.trim();
    if (configuredScenario
      && model.locationStorage?.toLowerCase() === configuredScenario.toLowerCase()
      && document.body.dataset.status?.startsWith("Loading scenario")) {
      document.body.dataset.status = `Scenario ready: ${configuredScenario}`;
      log("scenario ready", { storage: configuredScenario, frame: totalFrames });
    }
    if (model.logs?.length) {
      for (const message of model.logs) log("engine", message);
    }
    if (debugMode) {
      (window as any).__kirakiraLastModel = model;
    }
    const pending = model.pendingAssetPaths ?? [];
    const pendingSignature = pending.join("|");
    if (pendingSignature !== lastPendingSignature) {
      lastPendingSignature = pendingSignature;
      log("asset queue changed", { frame: totalFrames, pending: pending.length, paths: pending.slice(0, 8) });
    }
    if (timestamp - lastFrameLog >= 2000) {
      log("running", {
        frame: totalFrames,
        fps: Math.round((intervalFrames * 1000) / Math.max(1, timestamp - lastFrameLog)),
        drawCommands: model.drawList?.length ?? model.drawCommands,
        uploads: model.uploads?.length ?? model.imageUploads,
        pendingAssets: model.pendingAssets ?? pending.length,
        cacheBytes: model.cacheBytes ?? 0,
        cacheEntries: model.cacheEntries ?? 0,
        kagState: model.kagState,
        location: model.locationStorage,
      });
      intervalFrames = 0;
      lastFrameLog = timestamp;
    }
    const renderStartedAt = performance.now();
    render(model);
    const renderElapsedMs = performance.now() - renderStartedAt;
    if (debugMode || renderElapsedMs >= 50) {
      log("frame render complete", { frame: frameNumber, elapsedMs: Math.round(renderElapsedMs) });
    }
    // Keep command batches serialized.  A frame can arrive while a previous
    // decode/fetch is still pending; running both handlers concurrently was
    // the source of duplicate BGM/voice sources and overlapping fades.
    audioQueue = audioQueue
      .then(() => handleAudio(model.audio))
      .catch((error) => warn("audio command batch failed", error));
    void handleVideos(model.videos);
    window.requestAnimationFrame(frame);
  };
  log("frame loop starting");
  window.requestAnimationFrame(frame);
}
