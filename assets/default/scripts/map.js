/**
 * @file map.js
 * @version 2.1.0
 * @description
 * Rendu cartographique GPU via Deck.gl UMD avec overlay d'accessibilité (A11y).
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * ARCHITECTURE
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * Le pipeline sépare strictly les données, le picking, l'accessibilité et
 * le rendu visuel.
 *
 * DATA
 * ----
 * `gpuData` constitue le jeu de données commun aux couches de markers.
 * Il est construit une seule fois lors de l'initialisation de la map et
 * n'est jamais modifié par les animations.
 *
 * PICK LAYER
 * ----------
 * ScatterplotLayer
 *
 * - seule couche pickable des markers ;
 * - position géographique fixe ;
 * - hitbox fixe ;
 * - aucun déplacement visuel ;
 * - aucune transition.
 *
 * VISUAL LAYER
 * ------------
 * IconLayer
 *
 * - seule couche responsable du rendu des markers ;
 * - même position géographique que la couche de picking ;
 * - déplacement vertical via `getPixelOffset` ;
 * - transitions gérées exclusivement par Deck.gl.
 *
 * A11Y OVERLAY (ACCESSIBILITÉ)
 * ───────────────────────────
 * Conteneur DOM Fantôme (HTMLButtonElement)
 *
 * - activé uniquement si `dataLength <= A11Y_MAX_ITEMS` (garde mémoire) ;
 * - capture le focus clavier natif (`Tab`) et les lecteurs d'écran ;
 * - mappe directement les événements `focus`/`blur` sur la machine à états
 *   d'animation GPU (`hoveredIndex`, `animationGeneration`) ;
 * - projection spatiale synchronisée lors des changements de vue via
 *   `WebMercatorViewport.project()` sans provoquer de reflow DOM (utilisant
 *   exclusivement `transform: translate3d`).
 *
 * TILE LAYER
 * ----------
 * TileLayer
 *
 * La couche cartographique possède une identité stable et n'est pas
 * reconstruite lors des transitions des markers.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * ANIMATION
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * Les animations sont entièrement déléguées au système de transitions
 * de Deck.gl.
 *
 * Aucune boucle d'animation applicative n'est utilisée.
 *
 * En particulier :
 *
 * - pas de `requestAnimationFrame()` ;
 * - pas de calcul de progression par frame côté application ;
 * - pas de synchronisation manuelle avec le rendu GPU ;
 * - pas de modification de `gpuData` pendant une animation.
 *
 * Les transitions sont construites comme des mouvements monotones entre deux
 * états successifs. Lorsqu'une trajectoire comporte plusieurs mouvements,
 * ceux-ci sont chaînés par `onEnd()`.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * DROP INITIAL
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * Le drop est une animation d'entrée one-shot.
 *
 * Les markers visuels sont initialisés à leur position nominale, puis
 * "téléportés" hors de la zone visible de la map avant le déclenchement
 * de la chute.
 *
 * Une transition technique très courte, déclenchée une fois que le contexte
 * Deck.gl est opérationnel, sert à établir explicitement l'état source dans
 * le mécanisme de transition.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * HOVER BOUNCE
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * Le bounce de survol / focus est indépendant du drop initial.
 *
 * Chaque demi-mouvement est une transition monotone :
 *
 *   repos → apogée
 *   apogée → repos
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * GÉNÉRATIONS D'ANIMATION
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * `animationGeneration` constitue l'autorité logique permettant d'invalider
 * les callbacks de transitions devenus obsolètes.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * INVARIANTS
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * - `gpuData` est immuable après sa construction ;
 * - le déplacement visuel concerne exclusivement `IconLayer` ;
 * - la hitbox `ScatterplotLayer` reste indépendante du déplacement visuel ;
 * - `TileLayer` conserve son identité pendant toutes les animations ;
 * - aucune animation n'est pilotée par une boucle CPU par frame ;
 * - les trajectoires complexes sont décomposées en transitions monotones ;
 * - les callbacks obsolètes sont invalidés par génération ;
 * - l'animation d'entrée est exécutée une seule fois par instance de map ;
 * - le système de hover/focus ne prend le relais qu'après la phase d'entrée ;
 * - l'accessibilité DOM (A11y) est activée uniquement si dataLength <= A11Y_MAX_ITEMS ;
 * - le repositionnement des nœuds A11y s'effectue via `transform: translate3d`
 *   sans trigger de reflow.
 */

// Extraction depuis le namespace global instancié par le script statique UMD.
const { DeckGL, TileLayer, IconLayer, ScatterplotLayer } = window.deck;

// ─── Configuration statique ──────────────────────────────────────────────────

const TILE_DEFAULT = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

const SUBDOMAINS = Object.freeze(["a", "b", "c"]);

// ─── Accessibilité (A11y) ───────────────────────────────────────────────────

const A11Y_MAX_ITEMS = 100;

// ─── Géométrie des markers ───────────────────────────────────────────────────

const MARKER_SIZE = 40;

const MARKER_HIT_DIAMETER = 40;
const MARKER_HIT_RADIUS = MARKER_HIT_DIAMETER / 2;

// ─── Dimensions natives de l'asset SVG ───────────────────────────────────────

const ICON_WIDTH = 512;
const ICON_HEIGHT = 512;
const ICON_ANCHOR_Y = ICON_HEIGHT;

// ─── Animation des markers ───────────────────────────────────────────────────

const MARKER_BOUNCE_OFFSET = -16;
const MARKER_BOUNCE_HALF_DURATION = 350;

// ─── Drop initial ────────────────────────────────────────────────────────────

const MARKER_DROP_DURATION = 600;
const MARKER_DROP_BOUNCE_OFFSET = -30;
const MARKER_DROP_BOUNCE_HALF_DURATION = 150;

// ─── Asset SVG ────────────────────────────────────────────────────────────────

const SVG_ICON = `<svg xmlns="http://www.w3.org/2000/svg" width="${ICON_WIDTH}" height="${ICON_HEIGHT}" viewBox="0 0 ${ICON_WIDTH} ${ICON_HEIGHT}">
  <defs>
	<filter id="shadow" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="30" dy="30" stdDeviation="45" flood-color="#000000" flood-opacity="0.5" />
    </filter>
  </defs>
  <g filter="url(#shadow)">
    <path fill="#ff8052" d="M256 34c-99 0-179 79-179 177 0 154 179 265 179 265s179-108 179-265c0-98-80-177-179-177m0 88a85 85 0 0 1 85 85 85 85 0 0 1-85 85 85 85 0 0 1-85-85 85 85 0 0 1 85-85"/>
    <path fill="#d96d45" d="M256 34v88a85 85 0 0 1 85 85 85 85 0 0 1-85 85v183s179-108 179-265c0-98-80-177-179-177"/>
  </g>
</svg>`;

const SVG_URI = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(SVG_ICON)}`;

// ─── Extraction DOM (Single-Pass) ────────────────────────────────────────────

const collectMapConfigs = () => {
	const elements = document.querySelectorAll(".map");
	const configs = new Array(elements.length);

	for (let i = 0; i < elements.length; i++) {
		const el = elements[i];

		configs[i] = {
			el,
			tileServer: el.dataset.tileserver || null,
			minZoom: Number(el.dataset.minzoom) || 2,
			maxZoom: Number(el.dataset.maxzoom) || 19,
			zoom: el.dataset.zoom ? Number(el.dataset.zoom) : null,
			attribution: el.dataset.attribution || "",
			placesRaw: el.dataset.places || "[]",
		};
	}

	return configs;
};

// ─── Résolution statique ─────────────────────────────────────────────────────

const parseTileServer = (template) => {
	if (!template) {
		return TILE_DEFAULT;
	}

	const cleanTmpl = template.replace(/&amp;/g, "&");

	if (cleanTmpl.includes("{s}")) {
		const urls = new Array(SUBDOMAINS.length);

		for (let i = 0; i < SUBDOMAINS.length; i++) {
			urls[i] = cleanTmpl.replace("{s}", SUBDOMAINS[i]);
		}

		return urls;
	}

	return cleanTmpl;
};

// ─── Easings physiques ───────────────────────────────────────────────────────

const easeOutQuad = (t) => t * (2 - t);
const easeInQuad = (t) => t * t;

// ─── Pipeline GPU : Shader de post-traitement des tuiles ─────────────────────

class ThemedBitmapLayer extends deck.BitmapLayer {
	static layerName = "ThemedBitmapLayer";
	static componentName = "ThemedBitmapLayer";

	getShaders() {
		const shaders = super.getShaders();

		shaders.inject = {
			"fs:#decl": "uniform float tileTheme;",

			"fs:DECKGL_FILTER_COLOR": `
				if (tileTheme == 1.0) {
					float luma = dot(
						color.rgb,
						vec3(0.299, 0.587, 0.114)
					);

					color.rgb = vec3(luma);
				}
				else if (tileTheme == 2.0) {
					float luma = dot(
						color.rgb,
						vec3(0.299, 0.587, 0.114)
					);

					float inverted = 1.0 - luma;
					float bright = inverted * 1.1;
					float finalLuma = (bright - 0.5) * 0.7 + 0.5;

					color.rgb = vec3(finalLuma);
				}
				else if (tileTheme == 3.0) {
					vec3 sepia = vec3(
						dot(
							color.rgb,
							vec3(0.393, 0.769, 0.189)
						),
						dot(
							color.rgb,
							vec3(0.349, 0.686, 0.131)
						),
						dot(
							color.rgb,
							vec3(0.272, 0.534, 0.131)
						)
					);

					color.rgb = mix(color.rgb, sepia, 0.5);
				}
			`,
		};

		return shaders;
	}

	draw(opts) {
		const { tileTheme = 0.0 } = this.props;

		super.draw(
			Object.assign({}, opts, {
				uniforms: Object.assign({}, opts.uniforms, {
					tileTheme,
				}),
			}),
		);
	}
}

// ─── Instanciation WebGL (Lazy) ──────────────────────────────────────────────

const initMap = async (config) => {
	const { el, tileServer, minZoom, maxZoom, zoom, attribution, placesRaw } =
		config;

	// ─── Phase 1 : Layout mémoire CPU ────────────────────────────────────────

	const rawData = JSON.parse(placesRaw);
	const dataLength = rawData.length;
	const gpuData = new Array(dataLength);

	let minX = Infinity;
	let minY = Infinity;
	let maxX = -Infinity;
	let maxY = -Infinity;

	let themeValue = 0.0;

	if (el.classList.contains("map-grayscale")) {
		themeValue = 1.0;
	} else if (el.classList.contains("map-dark")) {
		themeValue = 2.0;
	} else if (el.classList.contains("map-vintage")) {
		themeValue = 3.0;
	}

	for (let i = 0; i < dataLength; i++) {
		const item = rawData[i];
		const popup = item[0];
		const lat = item[1][0];
		const lng = item[1][1];

		gpuData[i] = {
			position: [lng, lat],
			popup,
		};

		if (lng < minX) minX = lng;
		if (lat < minY) minY = lat;
		if (lng > maxX) maxX = lng;
		if (lat > maxY) maxY = lat;
	}

	// ─── Phase 2 : Calcul de la matrice de vue globale ───────────────────────

	const ZOOM_OFFSET = -1;
	const targetZoom = zoom !== null ? zoom + ZOOM_OFFSET : null;

	const camMinZoom =
		minZoom !== null && minZoom !== undefined ? minZoom + ZOOM_OFFSET : 0;

	const camMaxZoom =
		maxZoom !== null && maxZoom !== undefined ? maxZoom + ZOOM_OFFSET : 20;

	let initialViewState = {
		longitude: 2.2137,
		latitude: 46.2276,
		zoom: 5,
		minZoom: camMinZoom,
		maxZoom: camMaxZoom,
	};

	if (dataLength > 0) {
		if (minX === maxX && minY === maxY) {
			initialViewState = {
				longitude: minX,
				latitude: minY,
				zoom: targetZoom || 16,
				minZoom: camMinZoom,
				maxZoom: camMaxZoom,
			};
		} else {
			const rect = el.getBoundingClientRect();
			const width = rect.width || 800;
			const height = rect.height || 400;

			const viewport = new deck.WebMercatorViewport({
				width,
				height,
			});

			const fitted = viewport.fitBounds(
				[
					[minX, minY],
					[maxX, maxY],
				],
				{ padding: 40 },
			);

			initialViewState = {
				longitude: fitted.longitude,
				latitude: fitted.latitude,
				zoom: Math.min(fitted.zoom, camMaxZoom),
				minZoom: camMinZoom,
				maxZoom: camMaxZoom,
			};
		}
	}

	// ─── Phase 3 : UI déportée ──────────────────────────────────────────────

	if (attribution) {
		const attrNode = document.createElement("div");

		attrNode.style.cssText =
			"position: absolute; bottom: 0; right: 0; z-index: 1;" +
			"background: rgba(255,255,255,0.9); padding: 2px 5px;" +
			"font-size: 11px; font-family: sans-serif;";

		attrNode.innerHTML = attribution;

		el.appendChild(attrNode);
	}

	// ─── Phase 3.1 : Overlay DOM d'accessibilité (A11y) ─────────────────────

	const isA11yEnabled = dataLength <= A11Y_MAX_ITEMS;
	let a11yButtons = null;
	let a11yContainer = null;

	if (isA11yEnabled) {
		a11yContainer = document.createElement("div");
		a11yContainer.className = "deck-a11y-overlay";
		a11yContainer.style.cssText =
			"position: absolute; inset: 0; pointer-events: none; z-index: 2; overflow: hidden;";

		a11yButtons = new Array(dataLength);

		for (let i = 0; i < dataLength; i++) {
			const item = gpuData[i];
			const btn = document.createElement("button");

			btn.type = "button";
			btn.setAttribute("aria-label", item.popup);
			btn.style.cssText =
				"position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; " +
				"overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; " +
				"pointer-events: auto;";

			btn.addEventListener("focus", () => {
				if (dropPhase !== 2 || hoveredIndex === i) return;
				hoveredIndex = i;
				animationGeneration++;

				const generation = animationGeneration;

				deckgl.setProps({
					layers: [
						tileLayer,
						pickLayer,
						createVisualLayer(
							MARKER_BOUNCE_OFFSET,
							generation,
							"bounce-up",
							"normal",
						),
					],
				});
			});

			btn.addEventListener("blur", () => {
				if (dropPhase !== 2 || hoveredIndex !== i) return;
				hoveredIndex = -1;
				animationGeneration++;

				const generation = animationGeneration;

				deckgl.setProps({
					layers: [
						tileLayer,
						pickLayer,
						createVisualLayer(0, generation, "leave", "normal"),
					],
				});
			});

			a11yContainer.appendChild(btn);
			a11yButtons[i] = btn;
		}

		el.appendChild(a11yContainer);
	}

	const resolvedServer = parseTileServer(tileServer);

	// ─── État d'interaction ─────────────────────────────────────────────────

	let hoveredIndex = -1;
	let animationGeneration = 0;
	let dropPhase = 0;

	// ─── Calcul du décalage initial ──────────────────────────────────────────

	const rect = el.getBoundingClientRect();
	const dropOffset = -(rect.height + MARKER_SIZE);

	// ─── Identifiants stables ────────────────────────────────────────────────

	const mapId = el.id || "default";

	const tileLayerId = `raster-tiles-${mapId}`;
	const pickLayerId = `marker-pick-${mapId}`;
	const visualLayerId = `marker-visual-${mapId}`;

	// ─── TileLayer ────────────────────────────────────────────────────────────

	const tileLayer = new TileLayer({
		id: tileLayerId,
		data: resolvedServer,
		minZoom,
		maxZoom,
		tileSize: 256,
		renderSubLayers: (props) => {
			const { boundingBox } = props.tile;

			return new ThemedBitmapLayer(props, {
				data: null,
				image: props.data,
				bounds: [
					boundingBox[0][0],
					boundingBox[0][1],
					boundingBox[1][0],
					boundingBox[1][1],
				],
				tileTheme: themeValue,
			});
		},
	});

	// ─── Référence DeckGL ────────────────────────────────────────────────────

	let deckgl;

	// ─── Synchronisation de la projection A11y ────────────────────────────────

	const syncA11yPositions = (viewState) => {
		if (!isA11yEnabled || !deckgl) return;

		const width = el.clientWidth || 800;
		const height = el.clientHeight || 400;

		const viewport = new deck.WebMercatorViewport({
			width,
			height,
			...viewState,
		});

		for (let i = 0; i < dataLength; i++) {
			const [x, y] = viewport.project(gpuData[i].position);
			const btn = a11yButtons[i];

			// Repositionnement GPU compositor via translate3d (sans trigger de reflow)
			btn.style.transform = `translate3d(${x}px, ${y}px, 0)`;
		}
	};

	// ─── Construction des couches visuelles et transitions ───────────────────

	const createVisualLayer = (
		targetOffset,
		generation,
		transition = "init",
		visualMode = "normal",
	) =>
		new IconLayer({
			id: visualLayerId,
			data: gpuData,
			pickable: false,
			getPosition: (d) => d.position,
			getIcon: () => ({
				url: SVG_URI,
				width: ICON_WIDTH,
				height: ICON_HEIGHT,
				anchorY: ICON_ANCHOR_Y,
			}),
			getSize: MARKER_SIZE,
			getPixelOffset: (_, { index }) => {
				if (visualMode === "drop") return [0, targetOffset];
				if (index === hoveredIndex) return [0, targetOffset];
				return [0, 0];
			},

			transitions: {
				getPixelOffset: {
					duration:
						transition === "init"
							? 0
							: transition === "teleport"
								? 1
								: transition === "drop"
									? MARKER_DROP_DURATION
									: transition === "drop-bounce-up" ||
											transition === "drop-bounce-down"
										? MARKER_DROP_BOUNCE_HALF_DURATION
										: MARKER_BOUNCE_HALF_DURATION,

					easing:
						transition === "drop" ||
						transition === "drop-bounce-down" ||
						transition === "bounce-down"
							? easeInQuad
							: easeOutQuad,

					onEnd: () => {
						if (generation !== animationGeneration) return;

						if (transition === "teleport") {
							deckgl.setProps({
								layers: [
									tileLayer,
									pickLayer,
									createVisualLayer(0, generation, "drop", "drop"),
								],
							});
							return;
						}

						if (transition === "drop") {
							deckgl.setProps({
								layers: [
									tileLayer,
									pickLayer,
									createVisualLayer(
										MARKER_DROP_BOUNCE_OFFSET,
										generation,
										"drop-bounce-up",
										"drop",
									),
								],
							});
							return;
						}

						if (transition === "drop-bounce-up") {
							deckgl.setProps({
								layers: [
									tileLayer,
									pickLayer,
									createVisualLayer(0, generation, "drop-bounce-down", "drop"),
								],
							});
							return;
						}

						if (transition === "drop-bounce-down") {
							dropPhase = 2;

							deckgl.setProps({
								layers: [
									tileLayer,
									pickLayer,
									createVisualLayer(0, generation, null, "normal"),
								],
							});

							return;
						}

						if (transition === "leave") return;

						if (transition === "bounce-up") {
							deckgl.setProps({
								layers: [
									tileLayer,
									pickLayer,
									createVisualLayer(0, generation, "bounce-down", "normal"),
								],
							});

							return;
						}

						if (transition === "bounce-down") {
							deckgl.setProps({
								layers: [
									tileLayer,
									pickLayer,
									createVisualLayer(
										MARKER_BOUNCE_OFFSET,
										generation,
										"bounce-up",
										"normal",
									),
								],
							});
						}
					},

					onInterrupt: () => {},
				},
			},

			updateTriggers: {
				getPixelOffset: [hoveredIndex, targetOffset, visualMode],
			},
		});

	// ─── Couche de picking ───────────────────────────────────────────────────

	const pickLayer = new ScatterplotLayer({
		id: pickLayerId,
		data: gpuData,
		pickable: true,
		stroked: false,
		filled: true,
		radiusUnits: "pixels",
		getRadius: MARKER_HIT_RADIUS,
		getPixelOffset: () => [0, MARKER_SIZE / 2],
		getFillColor: [255, 255, 255, 0],
		getPosition: (d) => d.position,

		onHover: ({ index }) => {
			if (index === hoveredIndex) return;
			if (dropPhase !== 2) return;

			hoveredIndex = index;
			animationGeneration++;

			const generation = animationGeneration;

			if (index >= 0) {
				deckgl.setProps({
					layers: [
						tileLayer,
						pickLayer,
						createVisualLayer(
							MARKER_BOUNCE_OFFSET,
							generation,
							"bounce-up",
							"normal",
						),
					],
				});

				return;
			}

			deckgl.setProps({
				layers: [
					tileLayer,
					pickLayer,
					createVisualLayer(0, generation, "leave", "normal"),
				],
			});
		},
	});

	// ─── Phase 4 : Injection GPU ─────────────────────────────────────────────

	const visualLayer = createVisualLayer(
		dropOffset,
		animationGeneration,
		"init",
		"drop",
	);

	deckgl = new DeckGL({
		container: el,
		initialViewState,
		controller: true,

		onViewStateChange: ({ viewState }) => {
			syncA11yPositions(viewState);
			return viewState;
		},

		getTooltip: ({ object }) => {
			if (!object) return null;

			return {
				html: `<strong>${object.popup}</strong>`,
				style: {
					backgroundColor: "#ffffff",
					color: "#333333",
					padding: ".5em 1em",
					borderRadius: ".2em",
					boxShadow: "0 2px 4px rgba(0,0,0,0.3)",
					fontFamily: "sans-serif",
					fontSize: "1em",
					pointerEvents: "none",
				},
			};
		},

		layers: [tileLayer, pickLayer, visualLayer],

		onLoad: () => {
			syncA11yPositions(initialViewState);

			deckgl.setProps({
				layers: [
					tileLayer,
					pickLayer,
					createVisualLayer(
						dropOffset,
						animationGeneration,
						"teleport",
						"drop",
					),
				],
			});
		},
	});

	// ─── Déclenchement du drop ───────────────────────────────────────────────

	dropPhase = 1;

	deckgl.setProps({
		layers: [
			tileLayer,
			pickLayer,
			createVisualLayer(0, animationGeneration, "drop", "drop"),
		],
	});
};

// ─── Pipeline de boot (Intersection Observer) ────────────────────────────────

const observeMaps = (configs) => {
	const pending = new Map(configs.map((c) => [c.el, c]));

	const observer = new IntersectionObserver(
		(entries) => {
			for (let i = 0; i < entries.length; i++) {
				const entry = entries[i];

				if (!entry.isIntersecting || entry.intersectionRatio < 0.1) {
					continue;
				}

				const el = entry.target;
				const config = pending.get(el);

				if (!config) continue;

				initMap(config);

				pending.delete(el);
				observer.unobserve(el);
			}
		},
		{
			threshold: 0.1,
		},
	);

	for (let i = 0; i < configs.length; i++) {
		observer.observe(configs[i].el);
	}
};

// ─── Bootstrap ───────────────────────────────────────────────────────────────

export const bootstrap = () => {
	if (typeof window.deck === "undefined") {
		console.warn("Pipeline AOT: deck.gl global namespace is missing.");
		return;
	}

	const configs = collectMapConfigs();

	if (configs.length) {
		observeMaps(configs);
	}
};

if (document.readyState === "loading") {
	document.addEventListener("DOMContentLoaded", bootstrap);
} else {
	bootstrap();
}
