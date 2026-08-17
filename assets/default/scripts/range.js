/**
 * @summary Système de gestion de sliders (simple/double) par normalisation de données.
 * @architecture
 * - AOT Data Layout    : Invariants structurels (nœuds, min, max, delta) mis en cache à l'init.
 * - Zero Allocation    : Aucune instanciation d'objet ou de tableau dans le hot path (input event).
 * - O(1) Dispatch      : WeakMap liant l'Input DOM à son entité pour court-circuiter l'arbre DOM.
 * - Single Event Loop  : Un seul écouteur délégué au document gère tout le système.
 */

// ─── Registres & Caches (AOT) ───────────────────────────────────────────────

// Cache des formateurs (évite les allocations Intl répétées)
const formatters = new Map();

// O(1) Dispatch : associe un HTMLInputElement à son Entity configuration
const inputEntityRegistry = new WeakMap();

// Flag global pour garantir l'unicité des écouteurs d'événements
let isEventSystemMounted = false;

// ─── Utilitaires purs ────────────────────────────────────────────────────────

/**
 * Borne une valeur entre [lo, hi]. Arrête la propagation de NaN.
 */
const clamp = (v, lo, hi) =>
	Number.isFinite(v) ? Math.min(Math.max(v, lo), hi) : lo;

/**
 * Récupère ou instancie un Intl.NumberFormat.
 */
const getFormatter = (loc, cur) => {
	const key = `${loc}-${cur}`;
	if (!formatters.has(key)) {
		formatters.set(
			key,
			new Intl.NumberFormat(
				loc || undefined,
				cur ? { style: "currency", currency: cur } : {},
			),
		);
	}
	return formatters.get(key);
};

// ─── Pipeline de Rendu (Hot Path - Zero Allocation) ──────────────────────────

/**
 * Met à jour le DOM (CSS & ARIA) directement depuis les données de l'entité.
 * @param {Object} entity - Configuration pré-calculée du slider.
 */
const updateVisuals = (entity) => {
	const { container, inputs, output, min, delta, isMulti, formatter } = entity;

	// Lecture native directe (évite parseFloat)
	const v0 = inputs[0].valueAsNumber;
	const p0 = ((v0 - min) / delta) * 100;

	container.style.setProperty(isMulti ? "--start" : "--percent", `${p0}%`);
	inputs[0].setAttribute("aria-valuenow", v0);
	inputs[0].setAttribute("aria-valuetext", formatter.format(v0));

	let outText = formatter.format(v0);

	if (isMulti) {
		const v1 = inputs[1].valueAsNumber;
		const p1 = ((v1 - min) / delta) * 100;

		container.style.setProperty("--stop", `${p1}%`);
		inputs[1].setAttribute("aria-valuenow", v1);
		inputs[1].setAttribute("aria-valuetext", formatter.format(v1));

		// Concaténation scalaire directe au lieu de tableau.join()
		outText += ` • ${formatter.format(v1)}`;
	}

	if (output) {
		output.textContent = outText;
	}
};

// ─── Input System (O(1) Dispatch) ────────────────────────────────────────────

/**
 * Traite le flux de données de l'événement input/change.
 * Complexité : O(1) via WeakMap. Aucune allocation mémoire.
 */
const handleInput = (e) => {
	// Accès direct à l'entité pré-calculée. Élimine l'appel coûteux à e.target.closest()
	const entity = inputEntityRegistry.get(e.target);
	if (!entity) return;

	const { inputs, min, max, minGap, isMulti } = entity;

	if (isMulti) {
		const v0 = inputs[0].valueAsNumber;
		const v1 = inputs[1].valueAsNumber;

		// Comportement "push" avec contrainte d'écart
		if (e.target === inputs[0] && v0 >= v1) {
			inputs[1].value = clamp(v0 + minGap, min, max);
		} else if (e.target === inputs[1] && v1 <= v0) {
			inputs[0].value = clamp(v1 - minGap, min, max);
		}
	}

	updateVisuals(entity);
};

// ─── API Publique (Initialisation & Cycle de vie) ────────────────────────────

/**
 * Monte un container spécifique. Extrait et met en cache ses invariants structurels.
 * @param {HTMLElement} container
 */
export const mountSlider = (container) => {
	const inputs = Array.from(container.querySelectorAll("input"));
	const output = container.querySelector("output");
	const min = parseFloat(inputs[0].min);
	const max = parseFloat(inputs[0].max);
	const delta = max - min;

	// Guard fail-fast : validation stricte de l'invariant sans coercition implicite
	if (!delta || !Number.isFinite(delta)) {
		console.warn(
			"[range] Invariants invalides (min >= max ou absents).",
			container,
		);
		return;
	}

	const isMulti = inputs.length > 1;
	const step = parseFloat(inputs[0].step) || 1;
	const minGap = isMulti
		? Math.max(Number(container.dataset.minGap) || 0, step)
		: 0;

	const formatter = getFormatter(
		container.dataset.intl,
		container.dataset.currency,
	);

	// Entity Layout : Conteneur d'état statique et d'invariants (AOT)
	const entity = {
		container,
		inputs,
		output,
		min,
		max,
		delta,
		minGap,
		isMulti,
		formatter,
	};

	inputs.forEach((input, idx) => {
		// Nettoyage DOM structurel
		input.removeAttribute("tabindex");

		if (isMulti && !input.hasAttribute("aria-label")) {
			input.setAttribute("aria-label", idx === 0 ? "Minimum" : "Maximum");
		}

		// Enregistrement O(1) de l'input vers son entité pour court-circuiter le DOM
		inputEntityRegistry.set(input, entity);
	});

	if (output && !output.hasAttribute("aria-live")) {
		output.setAttribute("aria-live", "polite");
	}

	// Premier passage dans le pipeline de rendu visuel
	updateVisuals(entity);
};

/**
 * Scanne le DOM, monte tous les sliders trouvés et attache les écouteurs d'événements globaux.
 * Idéal pour l'initialisation bootstrap ou les transitions de vue.
 */
export const initSliders = () => {
	// Monte tous les sliders existants
	document.querySelectorAll(".range, .range-multithumb").forEach(mountSlider);

	// Attache les écouteurs d'événements globaux (une seule fois)
	if (!isEventSystemMounted) {
		document.addEventListener("input", handleInput);
		document.addEventListener("change", handleInput);
		isEventSystemMounted = true;
	}
};
