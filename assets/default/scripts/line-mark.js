/**
 * @summary Injection de marqueurs de ligne numérotés et navigation par ancre dans les
 *          pages article. Chaque élément cible reçoit une ancre `#mark-N` navigable.
 *
 * @strategy
 *   – Sélecteur, préfixes et template déclarés en constantes AOT : évalués une fois.
 *   – Création par clonage natif (cloneNode) en boucle indexée : le DOM est stable avant
 *     tout appel de scroll, élimination des allocations de création (createElement).
 *   – Lookup O(1) via getElementById plutôt que parseur CSS querySelector.
 *
 * @architectural-decision
 *   – Le scroll-to-hash est intentionnellement différé de SCROLL_DELAY ms après le boot.
 *     Ce délai n'est pas un correctif technique : c'est une décision UX. L'utilisateur
 *     doit avoir le temps de percevoir la page chargée avant que le défilement ne
 *     s'opère, ce qui lui fournit une indication visuelle explicite de sa position de
 *     navigation dans la page. L'effet repose sur scroll-behavior: smooth déclaré sur
 *     <html> en CSS : sans cette règle, le scroll est instantané et l'intention UX est
 *     perdue.
 *   – Sélecteur explicite (:where(...)) préféré au sélecteur universel '*' avec
 *     exclusions : coût de matching inférieur, intention déclarative.
 *   – Null-guard sur targetEl : un hash malformé ou orphelin ne doit pas
 *     produire de throw silencieux.
 */

// ---------------------------------------------------------------------------
// 1. Data Layout (Invariants & Cache)
// ---------------------------------------------------------------------------

const SELECTOR =
	".add-line-marks > :where(p, h2, h3, h4, h5, h6, blockquote, ul, ol, [class*=grid])";
const SCROLL_DELAY = 2000;
const PREFIX_ID = "mark-";
const PREFIX_HASH = `#${PREFIX_ID}`;

// Template AOT : Allocation isolée hors boucle pour copie mémoire rapide (cloneNode)
const MARKER_TEMPLATE = document.createElement("a");
MARKER_TEMPLATE.className = "line-mark";

// ---------------------------------------------------------------------------
// 2. Systems
// ---------------------------------------------------------------------------

const injectMarkers = () => {
	const els = document.querySelectorAll(SELECTOR);
	const len = els.length;

	// Guard O(1) : Invariant métier.
	// L'injection n'a pas de sens structurel pour <= 1 élément.
	if (len <= 1) return;

	// Boucle indexée : zéro allocation d'itérateur
	for (let i = 0; i < len; i++) {
		const indexStr = (i + 1).toString();
		const targetId = PREFIX_ID + indexStr;

		// Clonage direct de l'empreinte mémoire du template
		const node = MARKER_TEMPLATE.cloneNode(false);
		node.id = targetId;
		node.href = PREFIX_HASH + indexStr;
		node.textContent = indexStr;

		els[i].appendChild(node);
	}
};

const scrollToHash = () => {
	const hash = location.hash;

	if (!hash.startsWith(PREFIX_HASH)) return;

	// O(1) Hash Map lookup. Extraction de l'ID natif en retirant le '#'.
	const targetId = hash.substring(1);
	const targetEl = document.getElementById(targetId);

	if (targetEl !== null) {
		targetEl.scrollIntoView();
	}
};

// ---------------------------------------------------------------------------
// 3. Boot / Export
// ---------------------------------------------------------------------------

export const boot = () => {
	injectMarkers();

	if (location.hash.startsWith(PREFIX_HASH)) {
		setTimeout(scrollToHash, SCROLL_DELAY);
	}
};
