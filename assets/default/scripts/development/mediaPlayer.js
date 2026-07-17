// ── 0. AOT Template ───────────────────────────────────────────────────────
//  Parsing HTML O(1) global. cloneNode(true) O(1) par instanciation.
//  data-action = identifiant de commande. Résolution par InputSystem.

const _TEMPLATE = document.createElement("template");
_TEMPLATE.innerHTML = `
<div class="media-player">
  <button class="media-play-pause" data-action="TOGGLE_PLAY" aria-label="play/pause">
    <svg focusable="false"><use href="/sprites/player.svg#play"></use></svg>
    <svg focusable="false"><use href="/sprites/player.svg#play-disabled"></use></svg>
    <svg focusable="false"><use href="/sprites/player.svg#pause"></use></svg>
  </button>
  <div class="media-tags">
    <output class="media-subtitle-langage"></output>
    <output class="media-playback-rate"></output>
  </div>
  <div class="media-time">
    <output class="media-current-time" aria-label="current time">0:00</output>
    &nbsp;/&nbsp;
    <output class="media-duration" aria-label="duration">0:00</output>
  </div>
  <input type="range" class="media-progress-bar" data-action="SEEK"
         aria-label="progress bar" min="0" max="100" step="1" value="0">
  <div class="media-extend-volume">
    <input type="range" class="media-volume-bar" data-action="SET_VOLUME"
           aria-label="volume bar" min="0" max="1" step=".1" value=".5">
    <button class="media-mute" data-action="MUTE" aria-label="mute">
      <svg focusable="false"><use href="/sprites/player.svg#volume-up"></use></svg>
      <svg focusable="false"><use href="/sprites/player.svg#volume-off"></use></svg>
    </button>
  </div>
  <button class="media-fullscreen" data-action="FULLSCREEN" aria-label="fullscreen">
    <svg focusable="false"><use href="/sprites/player.svg#fullscreen"></use></svg>
  </button>
  <button class="media-menu" data-action="MENU" aria-label="menu">
    <svg focusable="false"><use href="/sprites/player.svg#menu"></use></svg>
  </button>
  <div class="media-extend-menu">
    <button class="media-next-reading" data-action="NEXT_READING" aria-label="next reading mode">
      <svg focusable="false"><use href="/sprites/player.svg#move-down"></use></svg>
    </button>
    <button class="media-subtitles" data-action="SUBTITLES" aria-label="subtitles">
      <svg focusable="false"><use href="/sprites/player.svg#subtitles"></use></svg>
    </button>
    <button class="media-picture-in-picture" data-action="PIP" aria-label="picture in picture">
      <svg focusable="false"><use href="/sprites/player.svg#picture-in-picture"></use></svg>
      <svg focusable="false"><use href="/sprites/player.svg#picture-in-picture-alt"></use></svg>
    </button>
    <button class="media-slow-motion" data-action="SLOW_MOTION" aria-label="slow motion">
      <svg focusable="false"><use href="/sprites/player.svg#slow-motion"></use></svg>
    </button>
    <button class="media-leap-rewind" data-action="LEAP_REWIND" aria-label="leap rewind">
      <svg focusable="false"><use href="/sprites/player.svg#rewind-5"></use></svg>
    </button>
    <button class="media-leap-forward" data-action="LEAP_FORWARD" aria-label="leap forward">
      <svg focusable="false"><use href="/sprites/player.svg#forward-5"></use></svg>
    </button>
    <button class="media-stop" data-action="STOP" aria-label="stop">
      <svg focusable="false"><use href="/sprites/player.svg#stop"></use></svg>
    </button>
    <button class="media-replay" data-action="REPLAY" aria-label="replay">
      <svg focusable="false"><use href="/sprites/player.svg#replay"></use></svg>
    </button>
  </div>
</div>`;

// ── 1. Constantes ─────────────────────────────────────────────────────────

const MEDIA_SELECTOR = ".media";
const PLAYBACK_RATES = Object.freeze([0.5, 0.25, 0.5, 1, 2, 4, 2, 1]);

// ── 2. Stores — Data Layout plat indexé par entityId (integer) ────────────

export const timeStore = {};
export const statusStore = {};
export const configStore = {};
export const domStore = {};
export const computedStore = {};

/** @type {WeakMap<HTMLMediaElement, number>} */
const _mediaIndex = new WeakMap();
let _nextEntityId = 0;

// ── 3. Command Buffer ──────────────────────────────────────────────────────

const _commandBuffer = [];

/** * Pousse une commande dans la file. Seul point d'écriture dans le buffer.
 * @param {number} entityId
 * @param {string} type
 * @param {any} [payload]
 */
export const dispatch = (entityId, type, payload) =>
	_commandBuffer.push({ entityId, type, payload });

// ── 4. Utilitaires purs ────────────────────────────────────────────────────

const _toTime = (s) => {
	// Check strict : évite toute tentative de cast si s venait à être corrompu
	if (!Number.isFinite(s) || s < 0) return "0:00";
	const hh = Math.floor(s / 3600);
	const mm = Math.floor((s % 3600) / 60).toString();
	const ss = Math.floor(s % 60)
		.toString()
		.padStart(2, "0");
	return hh > 0 ? `${hh}:${mm.padStart(2, "0")}:${ss}` : `${mm}:${ss}`;
};

const _cls = (el, name, on) => {
	if (el) on ? el.classList.add(name) : el.classList.remove(name);
};

const _closeOtherMenus = (currentId) => {
	for (const id in domStore) {
		const eid = +id;
		if (eid === currentId) continue;
		domStore[eid].extendMenu?.classList.remove("active");
		domStore[eid].menuButton?.classList.remove("active");
	}
};

// ── 5. CommandSystem ───────────────────────────────────────────────────────
//  Zéro DOM. Écrit uniquement dans configStore, les APIs natives et lève dirty.

const _handlers = {
	TOGGLE_PLAY(id) {
		const m = domStore[id].media;
		m.paused ? m.play() : m.pause();
		_closeOtherMenus(id);
		const nextId = configStore[id].nextEntityId;
		if (nextId !== null) {
			const nm = domStore[nextId]?.media;
			if (nm) nm.preload = "auto";
		}
	},

	SEEK(id, payload) {
		const m = domStore[id].media;
		if (!m.duration) return;
		m.currentTime = (payload.value / payload.max) * m.duration;
		computedStore[id].dirty = true;
	},

	SET_VOLUME(id, payload) {
		domStore[id].media.volume =
			parseFloat(payload.value) / parseFloat(payload.max);
		// Le DOM (volumeBar) sera mis à jour par UIRenderSystem après lecture de TimeSystem
		computedStore[id].dirty = true;
	},

	MUTE(id) {
		domStore[id].media.muted = !domStore[id].media.muted;
		computedStore[id].dirty = true;
	},

	STOP(id) {
		domStore[id].media.pause();
		domStore[id].media.currentTime = 0;
		computedStore[id].dirty = true;
	},

	REPLAY(id) {
		domStore[id].media.loop = !domStore[id].media.loop;
		computedStore[id].dirty = true;
	},

	LEAP_REWIND(id) {
		domStore[id].media.currentTime -= 5;
		computedStore[id].dirty = true;
	},

	LEAP_FORWARD(id) {
		domStore[id].media.currentTime += 5;
		computedStore[id].dirty = true;
	},

	SLOW_MOTION(id) {
		const cfg = configStore[id];
		cfg.playbackRateIdx = (cfg.playbackRateIdx + 1) % PLAYBACK_RATES.length;
		domStore[id].media.playbackRate = PLAYBACK_RATES[cfg.playbackRateIdx];
		computedStore[id].dirty = true;
	},

	SUBTITLES(id) {
		const cfg = configStore[id];
		const tracks = cfg.tracks;
		if (!tracks?.length) return;
		if (cfg.subtitleIdx >= 0 && tracks[cfg.subtitleIdx])
			tracks[cfg.subtitleIdx].mode = "disabled";

		const next = cfg.subtitleIdx + 1;
		if (next < tracks.length) {
			tracks[next].mode = "showing";
			cfg.subtitleIdx = next;
		} else {
			cfg.subtitleIdx = -1;
		}
		computedStore[id].dirty = true;
	},

	NEXT_READING(id) {
		const rel = configStore[id].mediaRelationship;
		if (!rel) return;
		const enabling = rel.dataset.nextReading !== "true";
		rel.dataset.nextReading = enabling ? "true" : "false";

		for (const oid in configStore) {
			if (configStore[+oid].mediaRelationship !== rel) continue;
			if (enabling) domStore[+oid].media.loop = false;
			computedStore[+oid].dirty = true; // force refresh de nextReadingButton
		}
	},

	FULLSCREEN(id) {
		domStore[id].media.requestFullscreen?.();
	},

	PIP(id) {
		if (document.pictureInPictureElement) {
			document.exitPictureInPicture();
		} else if (document.pictureInPictureEnabled) {
			domStore[id].media.requestPictureInPicture().catch(() => {});
		}
	},

	MENU(id) {
		const dom = domStore[id];
		if (!dom.extendMenu) return;
		dom.extendMenu.classList.toggle("active");
		dom.menuButton.classList.toggle("active");
		_closeOtherMenus(id);
	},

	_ADVANCE(id) {
		MediaStackSystem.advance(id);
	},

	_STREAM_DETECTED(id) {
		configStore[id].isStream = true;
		computedStore[id].dirty = true;
	},

	_ERROR(id) {
		statusStore[id].error = true;
		computedStore[id].dirty = true;
	},
};

const CommandSystem = {
	run() {
		const len = _commandBuffer.length;
		for (let i = 0; i < len; i++) {
			const { entityId, type, payload } = _commandBuffer[i];
			_handlers[type]?.(entityId, payload);
		}
		_commandBuffer.splice(0, len);
	},
};

// ── 6. TimeSystem ──────────────────────────────────────────────────────────

const TimeSystem = {
	run() {
		for (const id in domStore) {
			const eid = +id;
			const m = domStore[eid].media;
			const ts = timeStore[eid];
			const ss = statusStore[eid];

			const prevTime = ts.currentTime;
			const prevDuration = ts.duration;
			const prevBuffered = ts.bufferedEnd;
			const prevVolume = ss.volume;
			const prevRate = ss.playbackRate;

			ts.currentTime = m.currentTime;
			ts.duration = m.duration;
			ts.bufferedEnd =
				m.buffered.length > 0 ? m.buffered.end(m.buffered.length - 1) : 0;

			ss.paused = m.paused;
			ss.muted = m.muted;
			ss.volume = m.volume;
			ss.loop = m.loop;
			ss.playbackRate = m.playbackRate;

			if (
				ts.currentTime !== prevTime ||
				ts.duration !== prevDuration ||
				ts.bufferedEnd !== prevBuffered ||
				ss.volume !== prevVolume ||
				ss.playbackRate !== prevRate
			) {
				computedStore[eid].dirty = true;
			}
		}
	},
};

// ── 7. LogicSystem ─────────────────────────────────────────────────────────

const LogicSystem = {
	run() {
		for (const id in timeStore) {
			const eid = +id;
			const cs = computedStore[eid];
			if (!cs.dirty) continue;

			const ts = timeStore[eid];
			const ss = statusStore[eid];
			const cfg = configStore[eid];
			const dur = ts.duration;

			cs.ratio = dur > 0 ? Math.floor((ts.currentTime / dur) * 1000) / 10 : 0;
			cs.bufferRatio = dur > 0 ? Math.floor((ts.bufferedEnd / dur) * 100) : 0;
			cs.timeStr = _toTime(ts.currentTime);
			cs.durationStr = _toTime(dur);
			cs.isPlaying = !ss.paused;
			cs.isMuted = ss.muted || ss.volume === 0;
			cs.isStopped = ss.paused && ts.currentTime === 0;

			// Extraction des états discrets nécessitant une mise à jour UI
			if (cfg.subtitleIdx >= 0 && cfg.tracks[cfg.subtitleIdx]) {
				cs.subtitleStr = `cc: ${cfg.tracks[cfg.subtitleIdx].language}`;
				cs.hasSubtitles = true;
			} else {
				cs.subtitleStr = "";
				cs.hasSubtitles = false;
			}

			cs.isNextReading = cfg.mediaRelationship?.dataset.nextReading === "true";
			cs.isPip = document.pictureInPictureElement === domStore[eid].media;
		}
	},
};

// ── 8. UIRenderSystem ──────────────────────────────────────────────────────
//  Unique point d'écriture DOM (INV-1).
//  Prend en charge les mutations structurelles uniques via nullification de références.

const UIRenderSystem = {
	run() {
		for (const id in computedStore) {
			const eid = +id;
			const cs = computedStore[eid];
			if (!cs.intersecting || !cs.dirty) continue;

			const dom = domStore[eid];
			const ss = statusStore[eid];
			const cfg = configStore[eid];

			// ── Mutations topologiques uniques (Structural Changes) ──

			if (cfg.isStream && dom.progressBar) {
				const timeEl = dom.player.querySelector(".media-time");
				if (timeEl) {
					timeEl.textContent = "Lecture en continu";
					timeEl.style.marginRight = "auto";
				}
				dom.progressBar.remove();
				dom.progressBar = null;
				dom.menuButton?.remove();
				dom.menuButton = null;
				dom.extendMenu?.remove();
				dom.extendMenu = null;
			}

			if (ss.error && !dom.player.hasAttribute("inert")) {
				dom.player.setAttribute("inert", "");

				// Remplacement du .forEach par un parcours indexé O(N) direct sans allocation.
				const controls = dom.player.querySelectorAll("button, input");
				for (let i = 0; i < controls.length; i++) {
					controls[i].disabled = true;
				}

				dom.media.classList.add("error");
				dom.player.classList.add("error");
				dom.player.querySelector(".media-time").textContent =
					"Erreur de lecture";
				if ("poster" in dom.media) dom.media.poster = "";
			}

			// ── Mises à jour cycliques ──

			if (dom.progressBar) {
				dom.progressBar.value = cs.ratio;
				dom.progressBar.style.setProperty("--position", `${cs.ratio}%`);
				dom.progressBar.style.setProperty(
					"--position-buffer",
					`${cs.bufferRatio}%`,
				);
			}

			if (dom.volumeBar) {
				dom.volumeBar.style.setProperty("--position", `${ss.volume * 100}%`);
			}

			if (
				dom.playbackRateOutput &&
				dom.playbackRateOutput.textContent !== `x${ss.playbackRate}`
			) {
				dom.playbackRateOutput.textContent = `x${ss.playbackRate}`;
				_cls(dom.playbackRateOutput, "active", ss.playbackRate !== 1);
				_cls(dom.slowMotionButton, "active", ss.playbackRate !== 1);
			}

			if (
				dom.subtitleLangageOutput &&
				dom.subtitleLangageOutput.value !== cs.subtitleStr
			) {
				dom.subtitleLangageOutput.value = cs.subtitleStr;
				_cls(dom.subtitlesButton, "active", cs.hasSubtitles);
				_cls(dom.subtitleLangageOutput, "active", cs.hasSubtitles);
			}

			dom.currentTimeOutput.value = cs.timeStr;

			if (dom.durationOutput.value !== cs.durationStr) {
				dom.durationOutput.value = cs.durationStr;
			}

			_cls(dom.playPauseButton, "active", cs.isPlaying);
			_cls(dom.muteButton, "active", cs.isMuted);
			_cls(dom.nextReadingButton, "active", cs.isNextReading);
			_cls(dom.pipButton, "active", cs.isPip);

			if (dom.stopButton) {
				_cls(dom.stopButton, "active", cs.isStopped);
				dom.stopButton.disabled = cs.isStopped;
			}

			if (dom.replayButton) _cls(dom.replayButton, "active", ss.loop);

			cs.dirty = false;
		}
	},
};

// ── 9. MediaStackSystem ────────────────────────────────────────────────────

const MediaStackSystem = {
	advance(id) {
		const cfg = configStore[id];
		if (!cfg.mediaRelationship) return;
		if (cfg.mediaRelationship.dataset.nextReading !== "true") return;

		let candidateId = cfg.nextEntityId;
		while (candidateId !== null && statusStore[candidateId]?.error) {
			candidateId = configStore[candidateId]?.nextEntityId ?? null;
		}
		if (candidateId === null || candidateId === id) return;

		domStore[candidateId].media.play();

		const nextNextId = configStore[candidateId]?.nextEntityId ?? null;
		if (nextNextId !== null) {
			const m = domStore[nextNextId]?.media;
			if (m) m.preload = "auto";
		}
	},
};

// ── 10. IntersectionObserver ───────────────────────────────────────────────

const _observer = new IntersectionObserver(
	(entries) => {
		for (const entry of entries) {
			const id = +entry.target.dataset.entityId;
			if (computedStore[id])
				computedStore[id].intersecting = entry.isIntersecting;
		}
	},
	{ threshold: 0.1 },
);

// ── 11. InputSystem ────────────────────────────────────────────────────────

const InputSystem = {
	_ac: null,
	_initialized: false,

	init() {
		if (this._initialized) return;
		this._initialized = true;

		const ac = new AbortController();
		this._ac = ac;
		const sig = ac.signal;

		document.addEventListener("click", this._route, { signal: sig });
		document.addEventListener("input", this._route, { signal: sig });

		document.addEventListener(
			"play",
			(e) => {
				const srcId = _mediaIndex.get(e.target);
				if (srcId === undefined) return;
				for (const id in domStore) {
					const eid = +id;
					if (eid !== srcId) domStore[eid].media.pause();
				}
			},
			{ signal: sig, capture: true },
		);

		document.addEventListener(
			"fullscreenchange",
			() => {
				const active = !!document.fullscreenElement;
				for (const id in domStore)
					_cls(domStore[+id].fullscreenButton, "active", active);
			},
			{ signal: sig },
		);
	},

	_route(e) {
		const el = e.target.closest("[data-action]");
		if (!el) return;
		const playerEl = el.closest("[data-entity-id]");
		if (!playerEl) return;
		dispatch(
			+playerEl.dataset.entityId,
			el.dataset.action,
			e.target.type === "range"
				? { value: e.target.value, max: e.target.max }
				: undefined,
		);
	},

	dispose() {
		this._ac?.abort();
		this._initialized = false;
	},
};

// ── 12. Engine ─────────────────────────────────────────────────────────────

const Engine = {
	_rafId: null,
	_running: false,

	tick() {
		CommandSystem.run();
		TimeSystem.run();
		LogicSystem.run();
		UIRenderSystem.run();
		Engine._rafId = requestAnimationFrame(Engine.tick);
	},

	start() {
		if (this._running) return;
		this._running = true;
		this._rafId = requestAnimationFrame(Engine.tick);
	},

	stop() {
		cancelAnimationFrame(this._rafId);
		this._running = false;
	},
};

// ── 13. Initialisation d'une entité ────────────────────────────────────────

const _initEntity = (media, entityId) => {
	const player = _TEMPLATE.content
		.cloneNode(true)
		.querySelector(".media-player");
	player.dataset.entityId = entityId;
	media.insertAdjacentElement("afterend", player);

	const q = (sel) => player.querySelector(sel);
	domStore[entityId] = {
		media,
		player,
		playPauseButton: q(".media-play-pause"),
		playbackRateOutput: q(".media-playback-rate"),
		subtitleLangageOutput: q(".media-subtitle-langage"),
		currentTimeOutput: q(".media-current-time"),
		durationOutput: q(".media-duration"),
		progressBar: q(".media-progress-bar"),
		volumeBar: q(".media-volume-bar"),
		muteButton: q(".media-mute"),
		fullscreenButton: q(".media-fullscreen"),
		menuButton: q(".media-menu"),
		extendMenu: q(".media-extend-menu"),
		nextReadingButton: q(".media-next-reading"),
		subtitlesButton: q(".media-subtitles"),
		pipButton: q(".media-picture-in-picture"),
		slowMotionButton: q(".media-slow-motion"),
		stopButton: q(".media-stop"),
		replayButton: q(".media-replay"),
	};

	timeStore[entityId] = { currentTime: 0, duration: NaN, bufferedEnd: 0 };

	statusStore[entityId] = {
		paused: true,
		muted: false,
		volume: 0.5,
		loop: false,
		playbackRate: 1,
		waiting: false,
		error: false,
	};

	const mediaRelationship = media.closest(".media-relationship");
	const ac = new AbortController();

	configStore[entityId] = {
		isAudio: media.tagName === "AUDIO",
		isStream: false,
		tracks: media.textTracks,
		subtitleIdx: -1,
		playbackRateIdx: 0,
		mediaRelationship,
		nextEntityId: null,
		nextNextEntityId: null,
		_ac: ac,
	};

	computedStore[entityId] = {
		ratio: 0,
		bufferRatio: 0,
		timeStr: "0:00",
		durationStr: "0:00",
		subtitleStr: "",
		hasSubtitles: false,
		isPlaying: false,
		isMuted: false,
		isStopped: true,
		isNextReading: false,
		isPip: false,
		dirty: true,
		intersecting: true,
	};

	_mediaIndex.set(media, entityId);

	const dom = domStore[entityId];
	const isAudio = media.tagName === "AUDIO";

	if (isAudio || !document.fullscreenEnabled) {
		dom.fullscreenButton?.remove();
		dom.fullscreenButton = null;
	}
	if (isAudio || !document.pictureInPictureEnabled) {
		dom.pipButton?.remove();
		dom.pipButton = null;
	}
	if (!media.textTracks[0]) {
		dom.subtitlesButton?.remove();
		dom.subtitlesButton = null;
	}
	if (!mediaRelationship) {
		dom.nextReadingButton?.remove();
		dom.nextReadingButton = null;
	}

	dom.progressBar.style.setProperty("--position", "0%");
	dom.progressBar.style.setProperty("--position-buffer", "0%");
	dom.volumeBar.style.setProperty("--position", "50%");

	const sig = ac.signal;

	const _setDuration = () => {
		// Validation stricte du Float64 renvoyé par l'API HTMLMediaElement
		if (Number.isFinite(media.duration)) computedStore[entityId].dirty = true;
	};

	media.readyState >= 1
		? _setDuration()
		: media.addEventListener("loadedmetadata", _setDuration, {
				signal: sig,
				once: true,
			});

	const _handleInfinity = () => {
		if (media.duration !== Infinity) return;
		dispatch(entityId, "_STREAM_DETECTED");

		// Unrolling : zéro allocation, pas de closure, pas de tableau éphémère.
		media.removeEventListener("loadeddata", _handleInfinity);
		media.removeEventListener("loadedmetadata", _handleInfinity);
		media.removeEventListener("play", _handleInfinity);
	};

	media.addEventListener("loadeddata", _handleInfinity, { signal: sig });
	media.addEventListener("loadedmetadata", _handleInfinity, { signal: sig });
	media.addEventListener("play", _handleInfinity, { signal: sig });

	media.addEventListener(
		"waiting",
		() => {
			statusStore[entityId].waiting = true;
			player.classList.add("waiting");
		},
		{ signal: sig },
	);

	media.addEventListener(
		"canplay",
		() => {
			statusStore[entityId].waiting = false;
			player.classList.remove("waiting");
			if (mediaRelationship) {
				computedStore[entityId].dirty = true;
			}
		},
		{ signal: sig },
	);

	media.addEventListener(
		"ended",
		() => {
			media.currentTime = 0;
			computedStore[entityId].dirty = true;
			dispatch(entityId, "_ADVANCE");
			const nn = configStore[entityId].nextNextEntityId;
			if (nn !== null) {
				const m = domStore[nn]?.media;
				if (m) m.preload = "auto";
			}
		},
		{ signal: sig },
	);

	if (dom.subtitlesButton) {
		for (let i = 0; i < media.textTracks.length; i++) {
			if (media.textTracks[i].mode !== "showing") continue;
			configStore[entityId].subtitleIdx = i;
			break;
		}
	}

	media.src = media.currentSrc;
	media.addEventListener("error", () => dispatch(entityId, "_ERROR"), {
		signal: sig,
		capture: true,
	});

	_observer.observe(player);
};

// ── 14. Résolution des adjacences cross-entités ────────────────────────────

const _resolveAdjacencies = () => {
	for (const id in configStore) {
		const eid = +id;
		const rel = configStore[eid].mediaRelationship;
		if (!rel) continue;
		const siblings = [...rel.querySelectorAll(MEDIA_SELECTOR)];
		const idx = siblings.indexOf(domStore[eid].media);
		const nextM = siblings[idx + 1] ?? siblings[0] ?? null;
		const nextNM = siblings[idx + 2] ?? siblings[0] ?? null;
		const nextId = nextM ? (_mediaIndex.get(nextM) ?? null) : null;
		const nextNId = nextNM ? (_mediaIndex.get(nextNM) ?? null) : null;
		configStore[eid].nextEntityId = nextId === eid ? null : nextId;
		configStore[eid].nextNextEntityId = nextNId === eid ? null : nextNId;
	}
};

// ── 15. Export API ─────────────────────────────────────────────────────────

export const disposeEntity = (entityId) => {
	configStore[entityId]?._ac.abort();
	_observer.unobserve(domStore[entityId]?.player);
	domStore[entityId]?.player.remove();
	_mediaIndex.delete(domStore[entityId]?.media);

	// Note DOD: Le mot clé 'delete' dé-optimise le dictionnaire V8.
	// Acceptable ici car utilisé occasionnellement pour la libération de la GC.
	delete timeStore[entityId];
	delete statusStore[entityId];
	delete configStore[entityId];
	delete computedStore[entityId];
	delete domStore[entityId];
};

/**
 * Initialise le lecteur.
 * @param {HTMLElement|Document} container
 */
export const bootstrap = (container = document) => {
	const medias = container.querySelectorAll(MEDIA_SELECTOR);
	for (const media of medias) {
		if (_mediaIndex.has(media)) continue;
		media.removeAttribute("controls");
		media.id = media.id || `media-${_nextEntityId}`;
		_initEntity(media, _nextEntityId);
		_nextEntityId++;
	}
	_resolveAdjacencies();
	InputSystem.init();
	Engine.start();
};

// Compatibilité pour l'exécution automatique si le module est importé
// de façon synchrone dans un document déjà chargé.
if (document.readyState === "loading") {
	document.addEventListener("DOMContentLoaded", () => bootstrap(), {
		once: true,
	});
} else {
	// Optionnel : Vous pouvez retirer cet appel automatique si vous
	// préférez maîtriser strictement l'amorçage via l'import dans votre main.js
	bootstrap();
}

export const stores = {
	time: timeStore,
	status: statusStore,
	config: configStore,
	computed: computedStore,
};
