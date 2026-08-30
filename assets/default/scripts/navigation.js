/**
 * @module NavigationSystem
 * @summary Pipeline de contrôle O(1) avec teardown déterministe encapsulé.
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
