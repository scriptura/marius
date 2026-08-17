/**
 * @summary Système de gestion de l'état "Flip" (ES Module)
 *
 * @strategy
 * - Event Delegation : Réduction de l'empreinte mémoire (1 listener).
 * - State Mirroring : Set en mémoire pour un accès O(1) aux entités actives, évitant le parsing DOM.
 * - Timer Pooling (WeakMap) : Prévention des fuites mémoire par références faibles.
 *
 * @architectural-decision
 * - Remplacement de tous les `forEach` par des `for...of` (zéro allocation de closure).
 * - Remplacement de `querySelectorAll` par le miroir d'état `activeEntities` dans le hot path.
 */

const CONFIG = {
	CLASS_FLIP: "flip",
	ATTR_STATE: "data-flipped",
	DEMO_KEY: "demoCounterFlipCards",
	AUTO_FLIP_LIMIT: 4,
	AUTO_UNFLIP_MS: 1500,
	GLOBAL_UNFLIP_MS: 3000,
};

// Data Layout (Module Scope)
let autoFlipping = true;
let viewCount = 0;

// WeakMap : Indexation des timers sans bloquer le Garbage Collector
const timers = new WeakMap();

// Set : Miroir d'état pour éviter les requêtes DOM (O(N) -> O(1))
const activeEntities = new Set();

/**
 * Update unique du compteur (I/O)
 */
const updateAnalytics = () => {
	viewCount = parseInt(localStorage.getItem(CONFIG.DEMO_KEY), 10) || 0;
	viewCount++;
	localStorage.setItem(CONFIG.DEMO_KEY, viewCount.toString());
};

/**
 * Fonction pure de transition d'état (Mutation DOM & Miroir)
 */
const setFlipState = (el, isActive) => {
	if (!el) return;

	if (isActive) {
		el.setAttribute(CONFIG.ATTR_STATE, "true");
		el.classList.add("active");
		activeEntities.add(el);
	} else {
		el.setAttribute(CONFIG.ATTR_STATE, "false");
		el.classList.remove("active");
		activeEntities.delete(el);
	}
};

/**
 * Gestionnaire de cycle de vie des timers
 */
const scheduleUnflip = (el, delay) => {
	if (timers.has(el)) {
		clearTimeout(timers.get(el));
	}

	const timerId = setTimeout(() => {
		setFlipState(el, false);
		timers.delete(el);
	}, delay);

	timers.set(el, timerId);
};

/**
 * Input System : Pipeline déterministe sur interaction
 */
const handleInput = (e) => {
	const card = e.target.closest(`.${CONFIG.CLASS_FLIP}`);
	if (!card) return;

	// Interruption définitive du flux automatique
	autoFlipping = false;

	const isCurrentlyActive = activeEntities.has(card);

	if (isCurrentlyActive) {
		setFlipState(card, false);
	} else {
		// Unflip asynchrone des autres entités (Boucle native, 0 closure)
		for (const activeCard of activeEntities) {
			if (activeCard !== card) {
				scheduleUnflip(activeCard, CONFIG.GLOBAL_UNFLIP_MS);
			}
		}
		setFlipState(card, true);
	}
};

/**
 * Séquence démo (Pipeline d'initialisation)
 * @param {HTMLCollection} cards - Collection d'entités cibles
 */
const runDemoSequence = (cards) => {
	if (cards.length === 0 || viewCount >= CONFIG.AUTO_FLIP_LIMIT) {
		autoFlipping = false;
		return;
	}

	const firstCard = cards[0];
	setTimeout(() => {
		if (!autoFlipping) return;
		setFlipState(firstCard, true);
		scheduleUnflip(firstCard, CONFIG.AUTO_UNFLIP_MS);
		autoFlipping = false;
	}, 1000);
};

/**
 * Entry point public du module
 */
export function initFlipSystem() {
	updateAnalytics();

	// Utilisation de getElementsByClassName (Live Collection : plus performant que querySelectorAll)
	const cards = document.getElementsByClassName(CONFIG.CLASS_FLIP);

	// Nettoyage de l'accessibilité via boucle native (AOT fallback)
	for (const el of cards) {
		el.removeAttribute("tabindex");
	}

	runDemoSequence(cards);

	// Event Bus (Root Delegation)
	document.addEventListener("click", handleInput);
}
