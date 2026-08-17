/**
 * @summary Moteur de grille Masonry optimisé pour le rendu haute performance (ES Module).
 * @strategy
 * - Batch Processing : Séparation stricte des lectures (mesures) et des écritures (styles) pour éviter le Layout Thrashing.
 * - Precision-First Layout : Utilisation de getBoundingClientRect() pour une précision au sous-pixel, évitant les arrondis cumulatifs de clientHeight.
 * - Reactive Scheduling : Utilisation de ResizeObserver pour remplacer les écouteurs "resize" et "scroll" globaux, plus gourmands en CPU.
 * @architectural-decision
 * - Système de traitement par lots sans allocations intermédiaires (Data-Oriented Approach).
 * - Utilisation d'un WeakMap pour associer les timers de debounce aux nœuds DOM sans pollution de prototype.
 * - L'arbitrage du "Row Unit" à 1px garantit une granularité maximale pour le data-layout.
 * @note Ce module est conçu pour être supprimé sans effet de bord dès que `display: grid-lanes`
 * sera activé par défaut dans les navigateurs cibles.
 */

const CONFIG = {
	SELECTOR: ".masonry",
	ROW_UNIT: 1, // Unité de base pour la précision du span
	RESIZE_DEBOUNCE: 150,
};

// Table de hachage à clés faibles pour suivre les timers actifs sans polluer le layout interne du DOM
const activeTimers = new WeakMap();

// Instance unique pour éviter la sur-allocation d'observateurs
let observer = null;

/**
 * Pipeline de redistribution (DOD Batch)
 * @param {HTMLElement} container - Le conteneur de la grille masonry
 */
export const updateGrid = (container) => {
	const children = container.children;
	const count = children.length;
	if (count === 0) return;

	// 1. Invariant Acquisition (Lecture unique des propriétés de la grille)
	const style = window.getComputedStyle(container);
	const rowGap = parseInt(style.getPropertyValue("grid-row-gap"), 10) || 0;

	// Switch temporaire pour mesurer le contenu réel (AOT Measurement)
	container.style.alignItems = "start";

	// 2. Data Gathering (Batch Read - DOD Alignment)
	// Utilisation d'un Float32Array pour stocker les hauteurs brutes de manière contiguë.
	// Évite l'allocation d'objets intermédiaires {entity, height} qui surchargent le ramasse-miettes (GC).
	const heights = new Float32Array(count);
	for (let i = 0; i < count; i++) {
		heights[i] = children[i].getBoundingClientRect().height;
	}

	// 3. Command Buffer (Batch Write)
	// Application directe des styles en une seule passe pour éviter le Layout Thrashing.
	const rowUnitAndGap = CONFIG.ROW_UNIT + rowGap;
	for (let i = 0; i < count; i++) {
		const item = children[i];
		const rowSpan = Math.ceil((heights[i] + rowGap) / rowUnitAndGap);
		item.style.gridRowEnd = `span ${rowSpan}`;
	}

	// Restauration du layout
	container.style.alignItems = "stretch";
};

/**
 * Gestionnaire d'événement réactif pour le ResizeObserver
 */
const handleResize = (entries) => {
	const len = entries.length;
	for (let i = 0; i < len; i++) {
		const target = entries[i].target;
		const currentTimer = activeTimers.get(target);

		if (currentTimer !== undefined) {
			clearTimeout(currentTimer);
		}

		const newTimer = setTimeout(() => {
			updateGrid(target);
			activeTimers.delete(target);
		}, CONFIG.RESIZE_DEBOUNCE);

		activeTimers.set(target, newTimer);
	}
};

/**
 * Initialisation du système
 */
export const init = () => {
	const grids = document.querySelectorAll(CONFIG.SELECTOR);
	if (grids.length === 0) return;

	// Initialisation paresseuse (lazy-instantiation) de l'unique observateur
	if (!observer) {
		observer = new ResizeObserver(handleResize);
	}

	const gridsLen = grids.length;
	for (let i = 0; i < gridsLen; i++) {
		const grid = grids[i];

		// Premier calcul synchrone
		updateGrid(grid);

		// Observation du conteneur
		observer.observe(grid);

		// Observation des images pour contrer le lazy loading
		const imgs = grid.querySelectorAll("img");
		const imgsLen = imgs.length;
		for (let j = 0; j < imgsLen; j++) {
			imgs[j].addEventListener("load", () => updateGrid(grid), { once: true });
		}
	}
};

/**
 * Désabonnement complet du système (Prévient les fuites mémoire lors du démontage)
 */
export const destroy = () => {
	if (observer) {
		observer.disconnect();
		observer = null;
	}
};

// Enregistrement unique au chargement du module
if (typeof document !== "undefined") {
	document.addEventListener("transitionend", (e) => {
		if (e.target instanceof HTMLElement) {
			const parentGrid = e.target.closest(CONFIG.SELECTOR);
			if (parentGrid) updateGrid(parentGrid);
		}
	});

	// Auto-bootstrap
	if (document.readyState === "complete") {
		init();
	} else {
		window.addEventListener("load", init, { once: true });
	}
}
