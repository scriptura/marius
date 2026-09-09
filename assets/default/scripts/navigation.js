/**
 * @module NavigationSystem
 * @summary Pipeline de contrôle O(1) avec amélioration progressive et teardown déterministe.
 *
 * 1. AMÉLIORATION PROGRESSIVE (HAND-OFF DÉCLARATIF -> IMPÉRATIF)
 *    - Le composant DOM est instancié nativement fonctionnel via l'API Popover (fallback zero-JS).
 *    - L'initialisation ampute de manière synchrone les attributs matériels (`popover`, 
 *      `popovertarget`) pour transférer la pleine autorité au pipeline JavaScript.
 *    - Ce transfert permet le déploiement d'un comportement enrichi : animations matricielles 
 *      séquencées et isolation stricte de l'arbre de focus via l'attribut `inert`.
 *
 * 2. TEARDOWN DÉTERMINISTE (LIFECYCLE MANAGEMENT)
 *    - Encapsulation des branchements asynchrones (`addEventListener`) sous un `AbortController`.
 *    - Garantit une purge O(1) des listeners précédents, prévenant toute fuite mémoire ou 
 *      désynchronisation d'état lors de la re-projection dynamique du fragment HTML par le moteur.
 *
 * 3. EXÉCUTION FRUGALE & DATA-ORIENTED
 *    - AOT Media Query : Évaluation statique du point de rupture (`matchMedia`) en amont de 
 *      l'interaction pour court-circuiter le Layout Thrashing synchrone du navigateur.
 *    - Zéro Allocation : La mutation des attributs d'état (`aria-expanded`, `aria-hidden`) exploite 
 *      l'internement littéral des chaînes (String literal interning) pour garantir un toggle
 *      sans allocation sur le tas (heap).
 */

let activeController = null;

/**
 * Initialise le système et lie le cycle de vie au DOM actuel.
 * @returns {boolean} État de l'initialisation.
 */
export const initNavigation = () => {
	// Teardown O(1) du pipeline précédent en cas de re-projection du fragment HTML
	if (activeController) activeController.abort();
	activeController = new AbortController();
	const { signal } = activeController;

	const btn = document.querySelector(".cmd-nav");
	const subNav = document.querySelector(".sub-nav");
	if (!btn || !subNav) return false;

	btn.removeAttribute("popovertarget");
	btn.removeAttribute("popovertargetaction");
	subNav.removeAttribute("popover");

	const html = document.documentElement;
	const body = document.body;
	const contentNode = document.getElementById("main-content");
	const contentList = contentNode
		? null
		: document.querySelectorAll("body > :not(.nav)");

	// Résolution AOT : Suppression du Layout Thrashing synchrone.
	const breakpoint = "60rem";
	const mql = window.matchMedia(`(min-width: ${breakpoint})`);

	const toggleNav = () => {
		const isActive = html.classList.toggle("active");
		body.classList.toggle("active");

		// Résolution statique (String literal interning) = 0 allocation heap
		btn.setAttribute("aria-expanded", isActive ? "true" : "false");
		subNav.setAttribute("aria-hidden", isActive ? "false" : "true");

		if (contentNode) {
			contentNode.toggleAttribute("inert", isActive);
		} else if (contentList) {
			for (let i = 0; i < contentList.length; i++) {
				contentList[i].toggleAttribute("inert", isActive);
			}
		}
	};

	// Branchement direct O(1)
	btn.addEventListener("click", toggleNav, { signal });

	mql.addEventListener(
		"change",
		(event) => {
			const isDesktop = event.matches;
			if (isDesktop && btn.getAttribute("aria-expanded") === "true") {
				toggleNav();
			}
			if (subNav) {
				subNav.setAttribute("aria-hidden", isDesktop ? "false" : "true");
			}
		},
		{ signal },
	);

	// Alignement immédiat sur l'état matériel
	btn.setAttribute("aria-expanded", "false");
	subNav.setAttribute("aria-hidden", mql.matches ? "false" : "true");

	return true;
};
