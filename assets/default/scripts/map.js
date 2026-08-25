/**
 * @file map.js
 * @version 2.0.0
 * @description
 * Rendu cartographique GPU via Deck.gl UMD.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * ARCHITECTURE DES MARKERS
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * Le rendu visuel et le picking sont volontairement désolidarisés.
 *
 * PICK LAYER
 * ----------
 * ScatterplotLayer
 * - pickable: true
 * - position logique fixe
 * - hitbox fixe
 * - rendu visuel transparent
 * - aucune animation
 *
 * VISUAL LAYER
 * ------------
 * IconLayer
 * - pickable: false
 * - même position logique
 * - déplacement vertical animé
 *
 * Cette séparation empêche le déplacement du marker visuel de déplacer
 * également sa hitbox.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * ANIMATION
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * DROP INITIAL
 * ------------
 * Déclenché une seule fois lorsque la map entre dans le viewport.
 *
 *   - position initiale : au-dessus de la map
 *   - arrivée : position géographique définitive
 *   - durée : 1000 ms
 *   - rebond léger après impact
 *
 * HOVER BOUNCE
 * ------------
 * Le comportement reproduit la sémantique de :
 *
 *   animation: anim-bounce 0.35s ease infinite alternate;
 *
 * Une demi-animation dure donc 350 ms :
 *
 *   repos ──350ms──> -16px
 *   -16px ──350ms──> repos
 *
 * Le cycle complet dure 700 ms.
 *
 * Chaque demi-mouvement est une véritable transition Deck.gl monotone
 * utilisant le même easing que CSS `ease`.
 *
 * À la fin d'une demi-transition, la suivante est créée immédiatement.
 * Il n'y a donc pas de temps mort entre montée et descente.
 *
 * Lors d'un mouseout, la transition courante est interrompue et une
 * transition unique ramène le marker vers 0 px.
 *
 * Il n'y a volontairement :
 *
 *   - aucun requestAnimationFrame()
 *   - aucune modification de gpuData
 *   - aucune reconstruction de TileLayer
 *   - aucune boucle CPU par frame
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * HITBOX
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * Le rendu et le picking utilisent deux géométries différentes.
 *
 * MARKER_SIZE
 * - taille du marker visuel dans IconLayer
 *
 * MARKER_HIT_DIAMETER
 * - diamètre de la zone de picking dans ScatterplotLayer
 *
 * MARKER_HIT_RADIUS
 * - rayon correspondant au diamètre de picking
 *
 * L'IconLayer est ancrée par son bord inférieur via anchorY.
 * Le ScatterplotLayer est naturellement centré sur getPosition.
 *
 * Le picking est donc remonté de MARKER_SIZE / 2 pixels afin que son centre
 * corresponde au centre visuel du marker au repos.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * TUILES
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * La TileLayer possède une identité stable et n'est jamais reconstruite
 * pendant les animations des markers.
 */

// Extraction depuis le namespace global instancié par le script statique UMD.
const { DeckGL, TileLayer, IconLayer, ScatterplotLayer } = window.deck;

// ─── Configuration statique ──────────────────────────────────────────────────

const TILE_DEFAULT = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

const SUBDOMAINS = Object.freeze(["a", "b", "c"]);

// ─── Géométrie des markers ───────────────────────────────────────────────────

const MARKER_SIZE = 40;

const MARKER_HIT_DIAMETER = 40;
const MARKER_HIT_RADIUS = MARKER_HIT_DIAMETER / 2;

// ─── Dimensions natives de l'asset SVG ───────────────────────────────────────

const ICON_WIDTH = 512;
const ICON_HEIGHT = 512;
const ICON_ANCHOR_Y = ICON_HEIGHT;

// ─── Animation des markers ───────────────────────────────────────────────────
//
// La référence CSS historique était :
//
//   animation: anim-bounce 0.35s ease infinite alternate;
//
// 350 ms correspond donc à UNE demi-oscillation.
// Le cycle complet montée + descente dure 700 ms.

const MARKER_BOUNCE_OFFSET = -16;
const MARKER_BOUNCE_HALF_DURATION = 350;

const MARKER_DROP_DURATION = 1000;

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

// ─── Easings physiques (O(1), zéro allocation) ────────────────────────────────

// Montée (bounce-up) : Ease-Out Quad
// Vitesse max à t=0 (décollage), vitesse nulle à t=1 (apogée)
const easeOutQuad = (t) => t * (2 - t);

// Descente (bounce-down) : Ease-In Quad
// Vitesse nulle à t=0 (apogée), accélération max à t=1 (impact sol)
const easeInQuad = (t) => t * t;

// ─── Easing Drop ─────────────────────────────────────────────────────────────
//
// Reproduction du mouvement :
//
//   0%   : -100%
//   50%  :   0%
//   75%  : -40px
//   100% :   0
//
// L'easing retourne une progression normalisée [0, 1].

const dropEase = (t) => {
	if (t <= 0) {
		return 0;
	}

	if (t >= 1) {
		return 1;
	}

	if (t < 0.5) {
		const p = t / 0.5;

		return p * p * (3 - 2 * p);
	}

	if (t < 0.75) {
		const p = (t - 0.5) / 0.25;
		const eased = p * p * (3 - 2 * p);

		return 1 - 0.12 * eased;
	}

	const p = (t - 0.75) / 0.25;
	const eased = p * p * (3 - 2 * p);

	return 0.88 + 0.12 * eased;
};

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

		if (lng < minX) {
			minX = lng;
		}

		if (lat < minY) {
			minY = lat;
		}

		if (lng > maxX) {
			maxX = lng;
		}

		if (lat > maxY) {
			maxY = lat;
		}
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

	const resolvedServer = parseTileServer(tileServer);

	// ─── État d'interaction ─────────────────────────────────────────────────

	let hoveredIndex = -1;

	/*
	 * Génération logique du hover.
	 *
	 * Elle permet d'invalider toute transition de bounce devenue obsolète
	 * lorsqu'un nouveau changement d'état intervient.
	 */
	let animationGeneration = 0;

	/*
	 * 0 = markers hors champ
	 * 1 = chute initiale
	 * 2 = comportement normal
	 */
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

	// ─── Contrôle du cycle de bounce ─────────────────────────────────────────

	/*
	 * Cette fonction constitue le point central de l'animation.
	 *
	 * Une transition est toujours monotone :
	 *
	 *   0 -> -16
	 *   -16 -> 0
	 *
	 * Le chaînage est effectué uniquement dans onEnd().
	 *
	 * Cela reproduit la sémantique de CSS `alternate` sans demander à
	 * Deck.gl d'interpoler une fonction non monotone.
	 */
	const createVisualLayer = (targetOffset, generation, transition = null) =>
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
				// Pendant la phase initiale, tous les markers sont hors champ.
				if (dropPhase === 0) {
					return [0, dropOffset];
				}

				// Pendant le drop, Deck.gl anime vers la position nominale.
				if (dropPhase === 1) {
					return [0, 0];
				}

				// Comportement nominal.
				if (index === hoveredIndex) {
					return [0, targetOffset];
				}

				return [0, 0];
			},

			transitions: transition
				? {
						getPixelOffset: {
							duration:
								transition === "drop"
									? MARKER_DROP_DURATION
									: MARKER_BOUNCE_HALF_DURATION,

							// Routage explicite de la dynamique selon la phase
							easing:
								transition === "drop"
									? dropEase
									: transition === "bounce-up"
										? easeOutQuad
										: transition === "bounce-down"
											? easeInQuad
											: easeOutQuad, // "leave" (retour doux vers 0)

							onStart: () => {},

							onEnd: () => {
								if (transition === "drop") {
									if (generation !== animationGeneration) return;
									dropPhase = 2;
									return;
								}

								if (generation !== animationGeneration) return;

								if (transition === "leave") return;

								if (transition === "bounce-up") {
									deckgl.setProps({
										layers: [
											tileLayer,
											pickLayer,
											createVisualLayer(0, generation, "bounce-down"),
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
											),
										],
									});
								}
							},

							onInterrupt: () => {},
						},
					}
				: undefined,

			updateTriggers: {
				getPixelOffset: [hoveredIndex, targetOffset, dropPhase],
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

			// Entrée sur le marker
			if (index >= 0) {
				deckgl.setProps({
					layers: [
						tileLayer,
						pickLayer,
						createVisualLayer(MARKER_BOUNCE_OFFSET, generation, "bounce-up"),
					],
				});
				return;
			}

			// Sortie du marker
			deckgl.setProps({
				layers: [
					tileLayer,
					pickLayer,
					createVisualLayer(0, generation, "leave"),
				],
			});
		},
	});

	// ─── Phase 4 : Injection GPU ─────────────────────────────────────────────

	/*
	 * Première couche :
	 *
	 * dropPhase = 0
	 * getPixelOffset() = dropOffset
	 *
	 * Les markers sont donc réellement créés hors champ.
	 */
	const visualLayer = createVisualLayer(0, animationGeneration, null);

	deckgl = new DeckGL({
		container: el,

		initialViewState,

		controller: true,

		getTooltip: ({ object }) => {
			if (!object) {
				return null;
			}

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
	});

	// ─── Déclenchement du drop ───────────────────────────────────────────────

	/*
	 * La couche initiale possède déjà la position hors champ.
	 *
	 * Nous passons ensuite explicitement à dropPhase = 1 et reconstruisons
	 * uniquement la couche visuelle afin que Deck.gl possède bien une valeur
	 * précédente à interpoler.
	 */
	dropPhase = 1;

	deckgl.setProps({
		layers: [
			tileLayer,
			pickLayer,
			createVisualLayer(0, animationGeneration, "drop"),
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

				if (!config) {
					continue;
				}

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

const bootstrap = () => {
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
