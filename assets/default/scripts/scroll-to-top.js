/**
 * @module scroll-to-top
 * @summary Composant isolé avec verrou d'état (State Latch) et initialisation safe-sync.
 *
 * @strategy
 *   – Utilise un scroll event passif ({ passive: true }) : lecture de scrollY
 *     seule, sans lecture de layout (getBoundingClientRect, offsetTop) — zéro reflow.
 *   – Seuil capturé une fois au boot — suffisant pour un déclencheur de visibilité.
 *   – Zéro injection HTML utilitaire : le bouton est créé dynamiquement.
 *
 * @architectural-decision
 *   – Le bouton est injecté dans le footer (`.footer`) comme point d'ancrage
 *     sémantique naturel, pas en fin de body.
 *   – Les classes CSS `.scroll-top`, `.fade-out`, `.go-top-cmd` restent
 *     inchangées pour la rétrocompatibilité avec les styles existants.
 *   – L'injection SVG utilise `window.injectSvgSprite` si disponible,
 *     sinon fallback interne.
 */

export const initScrollToTop = () => {
	const footer = document.querySelector(".footer");
	if (!footer) return null;

	// Allocation des primitives
	const button = document.createElement("button");
	button.type = "button";
	button.className = "scroll-top fade-out go-top-cmd";
	button.setAttribute("aria-label", "Scroll to top");

	// Note AOT: Le innerHTML casse le principe de zéro-allocation au runtime.
	// Préférable de le pré-rendre via le moteur HTML et de l'hydrater ici.
	button.innerHTML = `<svg aria-hidden="true" width="1024" height="1024" viewBox="0 0 1024 1024"><path d="M640 1024V512l192 192 192-192L512 0 0 512l192 192 192-192v512z"></path></svg>`;

	footer.appendChild(button);

	const threshold = window.innerHeight / 2;

	// State Latch : registre en mémoire pour bloquer les écritures DOM redondantes
	let isVisible = false;

	// Pipeline de lecture (O(1), zéro reflow)
	const handleScroll = () => {
		const past = window.scrollY > threshold;

		// Branchement conditionnel : mutation uniquement sur transition d'état
		if (past !== isVisible) {
			isVisible = past;
			button.classList.toggle("fade-in", isVisible);
			button.classList.toggle("fade-out", !isVisible);
		}
	};

	// Actuateur de mutation (Scroll)
	const handleClick = () => {
		window.scrollTo({ top: 0, behavior: "smooth" });
	};

	// Attachement des listeners
	window.addEventListener("scroll", handleScroll, { passive: true });
	button.addEventListener("click", handleClick);

	return {
		show: () => {
			if (isVisible) return;
			isVisible = true;
			button.classList.add("fade-in");
			button.classList.remove("fade-out");
		},
		hide: () => {
			if (!isVisible) return;
			isVisible = false;
			button.classList.remove("fade-in");
			button.classList.add("fade-out");
		},
		destroy: () => {
			window.removeEventListener("scroll", handleScroll);
			button.removeEventListener("click", handleClick);
			button.remove();
		},
		getButton: () => button,
	};
};

// Barrière de synchronisation : Garantie que l'arbre DOM est monté avant l'allocation
if (document.readyState === "loading") {
	document.addEventListener("DOMContentLoaded", initScrollToTop);
} else {
	initScrollToTop();
}
