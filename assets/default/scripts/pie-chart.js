/**
 * @summary Web Component SVG de graphique en camembert (simple/donut), animé.
 *          Refonte Data-Oriented avec zéro allocation dans la boucle de rendu.
 * @see https://grafikart.fr/tutoriels/graph-pie-camembert-1965
 *
 * @strategy
 *   – `_ratios` pré-calculés en AOT (Float64Array contigu) : élimine les opérations
 *     de division (valeur / total) et le parsing dans la boucle critique.
 *   – Positionnement des labels résolu en AOT : le calcul trigonométrique des coordonnées
 *     des labels est exécuté une seule fois dans le constructeur au lieu d'attendre
 *     la fin de l'animation ou d'évaluer une condition `if (progress === 1)` à chaque frame.
 *   – Paths et lines pré-alloués : zéro allocation (zéro instanciation de Point) pendant
 *     le rendu. La boucle `_draw()` effectue uniquement des mutations pures via setAttribute.
 *   – La constante `PATCH` (epsilon géométrique) compense la limite de précision
 *     des flottants sur les arcs SVG à 360° pour éviter les arcs dégénérés.
 *
 * @architectural-decision
 *   – Custom Elements conservés : connectedCallback déclenche l'animation
 *     via le timestamp natif déterministe de requestAnimationFrame.
 *   – Shadow DOM conservé : isolation stricte des styles du graphe.
 *   – Délégation d'événements (Event Delegation) : centralisation des écouteurs
 *     mouseover/mouseout sur le nœud parent `g`. Évite l'allocation de N closures
 *     individuelles et prévient les fuites de mémoire lors du cycle de vie du composant.
 */

// ---------------------------------------------------------------------------
// Utilitaires purs (Hors structure mémoire de l'instance)
// ---------------------------------------------------------------------------

const strToDom = (str) =>
	document.createRange().createContextualFragment(str).firstChild;

const easeOutExpo = (x) => (x === 1 ? 1 : 1 - 2 ** (-10 * x));

// Constantes AOT
const SVG_NS = "http://www.w3.org/2000/svg";
const PI2 = Math.PI * 2;
const PI_MINUS_HALF = Math.PI / -2;
const PATCH = 0.0000001; // Epsilon géométrique

// ---------------------------------------------------------------------------
// Composant
// ---------------------------------------------------------------------------

export class PieChart extends HTMLElement {
	constructor() {
		super();
		const shadow = this.attachShadow({ mode: "open" });

		// — Lecture des attributs et AOT —
		const rawLabels = this.getAttribute("labels")?.split(";") ?? [];
		const donut = this.getAttribute("donut") ?? "0.7";
		const gap = this.getAttribute("gap") ?? "0.04";
		const colors = this.getAttribute("colors")?.split(";") ?? [
			"hsl(9,100%,64%)",
			"hsl(29,100%,64%)",
			"hsl(49,100%,64%)",
			"hsl(69,100%,64%)",
			"hsl(89,100%,64%)",
			"hsl(109,100%,64%)",
		];

		const rawData = this.getAttribute("data").split(";").map(parseFloat);
		const total = rawData.reduce((acc, v) => acc + v, 0);
		const len = rawData.length;

		// Data Layout : Tableau contigu de flottants pour pré-calculer les divisions (SoA)
		this._ratios = new Float64Array(len);
		for (let i = 0; i < len; i++) {
			this._ratios[i] = rawData[i] / total;
		}

		// — Structure SVG —
		const svg = strToDom(`<svg viewBox="-1 -1 2 2">
			<g mask="url(#graphMask)"></g>
			<mask id="graphMask">
				<rect fill="white" x="-1" y="-1" width="2" height="2"/>
				<circle r="${donut}" fill="black"/>
			</mask>
		</svg>`);

		const pathGroup = svg.querySelector("g");
		const maskGroup = svg.querySelector("mask");

		// Délégation d'événements (Event Delegation) : 2 écouteurs au lieu de 2*N
		pathGroup.addEventListener("mouseover", this._handleHover.bind(this));
		pathGroup.addEventListener("mouseout", this._handleOut.bind(this));

		this.paths = new Array(len);
		this.lines = new Array(len);
		this.labels = new Array(len);

		let currentAngle = PI_MINUS_HALF;

		// — Pré-allocation et calculs statiques —
		for (let k = 0; k < len; k++) {
			const ratio = this._ratios[k];

			// 1. Instanciation des sections (paths)
			const path = document.createElementNS(SVG_NS, "path");
			path.setAttribute("fill", colors[k % colors.length].trim());
			path.dataset.index = k; // Index pour la délégation d'événement
			pathGroup.appendChild(path);
			this.paths[k] = path;

			// 2. Instanciation des séparateurs (lines)
			const line = document.createElementNS(SVG_NS, "line");
			line.setAttribute("stroke", "#000");
			line.setAttribute("stroke-width", gap);
			line.setAttribute("x1", "0");
			line.setAttribute("y1", "0");
			maskGroup.appendChild(line);
			this.lines[k] = line;

			// 3. Labels et Positionnement AOT (hors du pipeline de rendu)
			if (rawLabels[k]) {
				const div = document.createElement("div");
				div.id = `label${k}`;
				div.textContent = rawLabels[k];
				div.setAttribute("tabindex", "0");

				// Calcul de l'angle médian pour positionner le label
				const labelAngle = currentAngle + ratio * Math.PI;
				const lx = Math.cos(labelAngle) * 0.5 + 0.5;
				const ly = Math.sin(labelAngle) * 0.5 + 0.5;

				div.style.top = `${ly * 100}%`;
				div.style.left = `${lx * 100}%`;
				shadow.appendChild(div);
				this.labels[k] = div;
			}

			// Avancement de l'angle pour la prochaine section
			currentAngle += ratio * PI2 - PATCH;
		}

		// — Styles encapsulés —
		const style = document.createElement("style");
		style.textContent = `
			:host { display: block; position: relative; }
			svg { width: 100%; height: 100%; }
			path { cursor: pointer; transition: filter .3s; }
			path:hover, path.active { filter: invert(1); }
			div {
				position: absolute;
				padding: .2em .5em;
				white-space: nowrap;
				transform: translate(-50%, -50%);
				background-color: var(--pie-chart-color-label, #222);
				opacity: 0;
				transition: opacity .3s;
				pointer-events: none;
			}
			div:focus, div:active, div.active { opacity: 1; outline: none; }
		`;
		shadow.appendChild(style);
		shadow.appendChild(svg);
	}

	connectedCallback() {
		const duration = 1000;
		let start = null;

		// Utilisation du timestamp natif de rAF, pipeline déterministe
		const tick = (timestamp) => {
			if (!start) start = timestamp;
			const elapsed = timestamp - start;
			const t = Math.min(elapsed / duration, 1);

			this._draw(easeOutExpo(t));

			if (t < 1) {
				requestAnimationFrame(tick);
			}
		};
		requestAnimationFrame(tick);
	}

	/**
	 * Pipeline de rendu (hot loop).
	 * Zéro allocation d'objet. Zéro branchement conditionnel complexe.
	 * @param {number} progress - [0, 1]
	 */
	_draw(progress) {
		let angle = PI_MINUS_HALF;
		let startX = 0;
		let startY = -1;
		const len = this._ratios.length;

		for (let k = 0; k < len; k++) {
			// Mutation directe : écriture DOM
			this.lines[k].setAttribute("x2", startX);
			this.lines[k].setAttribute("y2", startY);

			const arcRatio = this._ratios[k] * progress;
			angle += arcRatio * PI2 - PATCH;

			// Mathématiques résolues localement (pas d'allocation de struct 'Point')
			const endX = Math.cos(angle);
			const endY = Math.sin(angle);
			const largeFlag = arcRatio > 0.5 ? "1" : "0";

			// Concaténation de la string SVG (seule allocation inévitable imposée par l'API DOM)
			this.paths[k].setAttribute(
				"d",
				`M 0 0 L ${startX} ${startY} A 1 1 0 ${largeFlag} 1 ${endX} ${endY} L 0 0`,
			);

			// Transfert des registres pour l'itération suivante
			startX = endX;
			startY = endY;
		}
	}

	/**
	 * Résolution des événements via délégation
	 */
	_handleHover(e) {
		const index = e.target.dataset.index;
		if (index !== undefined) {
			this.dispatchEvent(
				new CustomEvent("sectionhover", { detail: parseInt(index, 10) }),
			);
			this.labels[index]?.classList.add("active");
		}
	}

	_handleOut(e) {
		const index = e.target.dataset.index;
		if (index !== undefined) {
			this.labels[index]?.classList.remove("active");
		}
	}
}

// Enregistrement optionnel du composant au chargement du module
if (!customElements.get("pie-chart")) {
	customElements.define("pie-chart", PieChart);
}
