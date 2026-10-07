/**
 * @summary Système de focus d'image autonome par Object Pooling et délégation d'événements.
 * @strategy
 * - Object Pooling : Instance unique réutilisable hors-DOM pour éviter la fragmentation mémoire.
 * - Strict Data Mapping : Injection JIT (Just-In-Time) des attributs (src, alt) à l'activation.
 * - Flat Loop Execution : Itérations indexées directes sans allocation de fermetures.
 */

const CONFIG = {
	TRIGGER_SELECTOR: ".figure-image-focus",
	OVERLAY_ID: "picture-focus-overlay",
	OVERLAY_CLASS: "picture-area",
};

const _SVG_SPRITES = `
<svg xmlns="http://www.w3.org/2000/svg" id="media-sprites" hidden aria-hidden="true">
  <symbol id="maximize" viewBox="0 0 512 512"><path d="M200 32H56C42.7 32 32 42.7 32 56V200c0 9.7 5.8 18.5 14.8 22.2s19.3 1.7 26.2-5.2l40-40 79 79-79 79L73 295c-6.9-6.9-17.2-8.9-26.2-5.2S32 302.3 32 312V456c0 13.3 10.7 24 24 24H200c9.7 0 18.5-5.8 22.2-14.8s1.7-19.3-5.2-26.2l-40-40 79-79 79 79-40 40c-6.9 6.9-8.9 17.2-5.2 26.2s12.5 14.8 22.2 14.8H456c13.3 0 24-10.7 24-24V312c0-9.7-5.8-18.5-14.8-22.2s-19.3-1.7-26.2 5.2l-40 40-79-79 79-79 40 40c6.9 6.9 17.2 8.9 26.2 5.2s14.8-12.5 14.8-22.2V56c0-13.3-10.7-24-24-24H312c-9.7 0-18.5 5.8-22.2 14.8s-1.7 19.3 5.2 26.2l40 40-79 79-79-79 40-40c6.9-6.9 8.9-17.2 5.2-26.2S209.7 32 200 32z" /></symbol>
  <symbol id="minimize" viewBox="0 0 512 512"><path d="M456 224H312c-13.3 0-24-10.7-24-24V56c0-9.7 5.8-18.5 14.8-22.2s19.3-1.7 26.2 5.2l40 40L442.3 5.7C446 2 450.9 0 456 0s10 2 13.7 5.7l36.7 36.7C510 46 512 50.9 512 56s-2 10-5.7 13.7L433 143l40 40c6.9 6.9 8.9 17.2 5.2 26.2s-12.5 14.8-22.2 14.8zm0 64c9.7 0 18.5 5.8 22.2 14.8s1.7 19.3-5.2 26.2l-40 40 73.4 73.4c3.6 3.6 5.7 8.5 5.7 13.7s-2 10-5.7 13.7l-36.7 36.7C466 510 461.1 512 456 512s-10-2-13.7-5.7L369 433l-40 40c-6.9 6.9-17.2 8.9-26.2 5.2s-14.8-12.5-14.8-22.2V312c0-13.3 10.7-24 24-24H456zm-256 0c13.3 0 24 10.7 24 24V456c0 9.7-5.8 18.5-14.8 22.2s-19.3 1.7-26.2-5.2l-40-40L69.7 506.3C66 510 61.1 512 56 512s-10-2-13.7-5.7L5.7 469.7C2 466 0 461.1 0 456s2-10 5.7-13.7L79 369 39 329c-6.9-6.9-8.9-17.2-5.2-26.2s12.5-14.8 22.2-14.8H200zM56 224c-9.7 0-18.5-5.8-22.2-14.8s-1.7-19.3 5.2-26.2l40-40L5.7 69.7C2 66 0 61.1 0 56s2-10 5.7-13.7L42.3 5.7C46 2 50.9 0 56 0s10 2 13.7 5.7L143 79l40-40c6.9-6.9 17.2-8.9 26.2-5.2s14.8 12.5 14.8 22.2V200c0 13.3-10.7 24-24 24H56z" /></symbol>
</svg>
`;

const state = {
	activeTrigger: null,
	overlay: null,
	imgEntity: null,
	mutatedElements: [], // Conserve les références des nœuds modifiés pour éviter le clobbering
};

/**
 * Instanciation unique du Prefab en mémoire (AOT)
 */
const bootstrapSystem = () => {
	if (document.getElementById(CONFIG.OVERLAY_ID)) {
		state.overlay = document.getElementById(CONFIG.OVERLAY_ID);
		state.imgEntity = state.overlay.querySelector("img");
		return;
	}

	const overlay = document.createElement("div");
	overlay.id = CONFIG.OVERLAY_ID;
	overlay.className = CONFIG.OVERLAY_CLASS;

	overlay.innerHTML = `
    <img loading="lazy">
    <button class="shrink-button" aria-label="shrink"></button>
  `;

	state.overlay = overlay;
	state.imgEntity = overlay.querySelector("img");

	const shrinkBtn = overlay.querySelector(".shrink-button");
	if (typeof globalThis.injectSvgSprite === "function") {
		//globalThis.injectSvgSprite(shrinkBtn, "minimize");
		globalThis.insertAdjacentHTML("beforeend", "<svg class='icon' aria-hidden='true'><use href='#minimize'></use></svg>");
	}
};

/**
 * Machine à états du système
 * @param {HTMLElement|null} target - Élément déclencheur à activer, ou null pour désactiver.
 */
export const setSystemState = (target = null) => {
	const isOpening = !!target;
	const root = document.documentElement;

	root.classList.toggle("freeze", isOpening);

	if (isOpening) {
		state.activeTrigger = target;
		const sourceImg = target.querySelector("img");
		if (!sourceImg) return;

		// Data Injection par propriété directe (plus rapide que setAttribute)
		state.imgEntity.src = sourceImg.src;
		if (sourceImg.alt) {
			state.imgEntity.alt = sourceImg.alt;
		} else {
			state.imgEntity.removeAttribute("alt");
		}

		// Rattachement physique au DOM
		document.body.appendChild(state.overlay);

		// TODO test inject SVG qprite
		document.body.insertAdjacentHTML("afterbegin", _SVG_SPRITES);

		// Isolation sémantique sans altérer l'état préexistant (No-Clobbering)
		state.mutatedElements = [];
		const children = document.body.children;
		const len = children.length;
		for (let i = 0; i < len; i++) {
			const el = children[i];
			if (el !== state.overlay && !el.hasAttribute("inert")) {
				el.setAttribute("inert", "");
				state.mutatedElements.push(el);
			}
		}

		state.overlay.querySelector("button")?.focus();
	} else {
		// Restauration de l'état sémantique
		const len = state.mutatedElements.length;
		for (let i = 0; i < len; i++) {
			state.mutatedElements[i].removeAttribute("inert");
		}
		state.mutatedElements = [];

		state.activeTrigger?.querySelector("button")?.focus();
		state.activeTrigger = null;

		// Nettoyage des références (Zéro fuite mémoire)
		state.imgEntity.removeAttribute("src");
		state.imgEntity.removeAttribute("alt");
		state.overlay.remove();
	}
};

/**
 * Processeur d'Entrées unique
 */
const handleInteraction = (e) => {
	const trigger = e.target.closest(CONFIG.TRIGGER_SELECTOR);

	if (!state.activeTrigger) {
		if (trigger) {
			setSystemState(trigger);
		}
	} else {
		// Tout clic actif en dehors ou sur l'overlay déclenche la fermeture
		setSystemState(null);
	}
};

/**
 * Décoration AOT des cibles disponibles dans le DOM actuel.
 * Exporté pour permettre une ré-exécution manuelle lors de mutations DOM dynamiques.
 */
export const decorateTargets = () => {
	const targets = document.querySelectorAll(CONFIG.TRIGGER_SELECTOR);
	const len = targets.length;
	for (let i = 0; i < len; i++) {
		const item = targets[i];
		if (item.querySelector("button")) continue;

		const btn = document.createElement("button");
		btn.ariaLabel = "enlarge";
		if (typeof globalThis.injectSvgSprite === "function") {
			//globalThis.injectSvgSprite(btn, "maximize");
			globalThis.insertAdjacentHTML("beforeend", "<svg class='icon' aria-hidden='true'><use href='#maximize'></use></svg>");
		}
		item.appendChild(btn);
	}
};

/**
 * Initialisation globale (Méthode idempotente)
 */
export const initImageFocus = () => {
	bootstrapSystem();
	decorateTargets();

	document.removeEventListener("click", handleInteraction);
	document.addEventListener("click", handleInteraction);

	const handleKeyDown = (e) => {
		if (e.key === "Escape" && state.activeTrigger) {
			setSystemState(null);
		}
	};
	document.removeEventListener("keydown", handleKeyDown);
	document.addEventListener("keydown", handleKeyDown);
};
