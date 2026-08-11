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
		globalThis.injectSvgSprite(shrinkBtn, "minimize");
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
			globalThis.injectSvgSprite(btn, "maximize");
		}
		item.appendChild(btn);
	}
};

/**
 * Initialisation globale (Méthode idempotente)
 */
export const init = () => {
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
