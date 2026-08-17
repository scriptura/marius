/**
 * @module NavigationSystem
 * @description Pipeline de contrôle du menu (Hamburger / Sub-nav).
 */

// --- DATA LAYOUT (Pointeurs Mémoire) ---
const DOM = {
	html: document.documentElement,
	body: document.body,
	btn: null,
	subNav: null,
	contentNode: null, // Cible unique O(1) privilégiée
	contentList: null, // Fallback O(N)
};

let mql = null; // MediaQueryList (Détecteur matériel de point de rupture)

// --- SYSTEM : State Synchronization ---
export const toggleNav = () => {
	if (!DOM.btn) return;

	const isActive = DOM.html.classList.toggle("active");
	DOM.body.classList.toggle("active");

	DOM.btn.setAttribute("aria-expanded", isActive);
	DOM.subNav.setAttribute("aria-hidden", !isActive);

	// Privilégier un conteneur unique O(1), sinon fallback sur le NodeList O(N)
	if (DOM.contentNode) {
		DOM.contentNode.toggleAttribute("inert", isActive);
	} else if (DOM.contentList) {
		DOM.contentList.forEach((node) => {
			node.toggleAttribute("inert", isActive);
		});
	}
};

// --- SYSTEM : Event Bus (Hardware breakpoint) ---
const handleBreakpointChange = (e) => {
	const isDesktop = e.matches;

	// Fermeture propre si passage en desktop avec menu ouvert
	if (isDesktop && DOM.btn.getAttribute("aria-expanded") === "true") {
		toggleNav();
	}

	// En desktop, le sub-nav est visible (CSS), on retire aria-hidden
	DOM.subNav.setAttribute("aria-hidden", !isDesktop);
};

// --- EXPORT PUBLIC (Entrypoint) ---
export const initNavigation = () => {
	DOM.btn = document.querySelector(".cmd-nav");
	DOM.subNav = document.querySelector(".sub-nav");

	if (!DOM.btn || !DOM.subNav) return;

	// TODO: Dans le HTML, encapsuler le contenu hors-nav dans <main id="main-content">
	DOM.contentNode = document.getElementById("main-content");
	// Fallback conservé temporairement
	if (!DOM.contentNode) {
		DOM.contentList = document.querySelectorAll("body > :not(.nav)");
	}

	// Extraction AOT de la variable CSS
	// Note: Si --size-nav est fixe (ex: 60rem), remplacez ce getComputedStyle par la valeur en dur
	// pour éliminer totalement le Layout Thrashing.
	const rawSize =
		getComputedStyle(DOM.html).getPropertyValue("--size-nav").trim() || "60rem";

	// Compilateur de Media Query
	mql = window.matchMedia(`(min-width: ${rawSize})`);

	// Initialisation synchrone de l'état ARIA selon le layout initial
	DOM.btn.setAttribute("aria-expanded", "false");
	DOM.subNav.setAttribute("aria-hidden", !mql.matches);

	// Bindings
	DOM.btn.addEventListener("click", toggleNav);

	// Écouteur natif (remplace la boucle resize O(N) + setTimeout)
	mql.addEventListener("change", handleBreakpointChange);
};
