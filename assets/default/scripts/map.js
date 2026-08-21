/**
 * @file map2.js
 * @description Rendu cartographique GPU via Deck.gl UMD
 */

// Extraction depuis le namespace global instancié par le script statique UMD
const { DeckGL, TileLayer, IconLayer } = window.deck;

const TILE_DEFAULT = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
const SUBDOMAINS = Object.freeze(["a", "b", "c"]);
// Définition de l'atlas spatial (UV Mapping statique)
const ICON_MAPPING = {
	"marker-orange": {
		x: 0,
		y: 0,
		width: 512,
		height: 512,
		anchorY: 512,
		anchorX: 256,
	},
	"marker-blue": {
		x: 512,
		y: 0,
		width: 512,
		height: 512,
		anchorY: 512,
		anchorX: 256,
	},
};

// ─── Extraction DOM (Single-Pass) ────────────────────────────────────────────

const collectMapConfigs = () => {
	const elements = document.querySelectorAll(".map");
	const configs = new Array(elements.length);
	for (let i = 0; i < elements.length; i++) {
		const el = elements[i];
		configs[i] = {
			el,
			// ... autres propriétés
			placesRaw: el.dataset.places || "[]",
			// Récupération du chemin injecté par Rust (fallback sur la racine par défaut)
			atlasUrl: el.dataset.atlas || "/assets/default/sprites/atlas.svg",
		};
	}
	return configs;
};

// ─── Résolution statique (Zero-Network Latency) ──────────────────────────────

const parseTileServer = (template) => {
	if (!template) return TILE_DEFAULT;

	const cleanTmpl = template.replace(/&amp;/g, "&");

	// Si un sous-domaine est requis, on génère le tableau d'URLs pour le GPU
	if (cleanTmpl.includes("{s}")) {
		const urls = new Array(SUBDOMAINS.length);
		for (let i = 0; i < SUBDOMAINS.length; i++) {
			urls[i] = cleanTmpl.replace("{s}", SUBDOMAINS[i]);
		}
		return urls;
	}

	return cleanTmpl;
};

// ─── Pipeline GPU : Shader de post-traitement des tuiles ─────────────

class ThemedBitmapLayer extends deck.BitmapLayer {
	static layerName = "ThemedBitmapLayer";
	static componentName = "ThemedBitmapLayer";

	getShaders() {
		const shaders = super.getShaders();
		shaders.inject = {
			// Déclaration de la variable (uniform) envoyée par le CPU
			"fs:#decl": "uniform float tileTheme;",

			// Injection de l'algorithme à la fin du pipeline de couleur du fragment
			"fs:DECKGL_FILTER_COLOR": `
        if (tileTheme == 1.0) {
          // Grayscale : Produit scalaire avec les coefficients de luminance standard
          float luma = dot(color.rgb, vec3(0.299, 0.587, 0.114));
          color.rgb = vec3(luma);
        } 
        else if (tileTheme == 2.0) {
          // Dark Mode : grayscale(1) invert(1) brightness(1.1) contrast(0.7)
          float luma = dot(color.rgb, vec3(0.299, 0.587, 0.114));
          float inverted = 1.0 - luma;
          float bright = inverted * 1.1;
          
          // Application mathématique du contraste CSS (basé sur un pivot à 0.5)
          float finalLuma = (bright - 0.5) * 0.7 + 0.5;
          color.rgb = vec3(finalLuma);
        }
		else if (tileTheme == 3.0) {
          // Vintage (Sepia 50%) : Produit scalaire via la matrice W3C standard
          vec3 sepia = vec3(
            dot(color.rgb, vec3(0.393, 0.769, 0.189)),
            dot(color.rgb, vec3(0.349, 0.686, 0.168)),
            dot(color.rgb, vec3(0.272, 0.534, 0.131))
          );
          // mix(x, y, a) = x * (1 - a) + y * a (exécuté en 1 cycle d'horloge matériel)
          color.rgb = mix(color.rgb, sepia, 0.5);
        }
      `,
		};
		return shaders;
	}

	// Interception de l'ordre de dessin pour transférer la prop vers la VRAM
	draw(opts) {
		const { tileTheme = 0.0 } = this.props;
		super.draw(
			Object.assign({}, opts, {
				uniforms: Object.assign({}, opts.uniforms, { tileTheme }),
			}),
		);
	}
}

// ─── Instanciation WebGL (Lazy) ──────────────────────────────────────────────

const initMap = async (config) => {
	const { el, tileServer, minZoom, maxZoom, zoom, attribution, placesRaw } =
		config;

	// Phase 1 : Layout mémoire CPU
	const rawData = JSON.parse(placesRaw);
	const dataLength = rawData.length;
	const gpuData = new Array(dataLength);
	let minX = Infinity,
		minY = Infinity,
		maxX = -Infinity,
		maxY = -Infinity;
	// Détection des thèmes (fallback à 0.0 = couleur d'origine)
	let themeValue = 0.0;
	if (el.classList.contains("map-grayscale")) themeValue = 1.0;
	else if (el.classList.contains("map-dark")) themeValue = 2.0;
	else if (el.classList.contains("map-vintage")) themeValue = 3.0;

	for (let i = 0; i < dataLength; i++) {
		const item = rawData[i];
		const popup = item[0];
		const lat = item[1][0];
		const lng = item[1][1]; // Inversion spatiale AOT : Lat/Lng -> Lng/Lat

		// Ajout de la propriété iconId (pointeur vers l'atlas)
		gpuData[i] = { position: [lng, lat], popup, iconId: "marker-orange" };

		if (lng < minX) minX = lng;
		if (lat < minY) minY = lat;
		if (lng > maxX) maxX = lng;
		if (lat > maxY) maxY = lat;
	}

	// Phase 2 : Calcul de la matrice de vue globale
	// Compensateur de projection : ajuste la valeur Leaflet pour le moteur Deck.gl
	const ZOOM_OFFSET = -1;
	const targetZoom = zoom !== null ? zoom + ZOOM_OFFSET : null;

	// 1. Calcul du clamping de la caméra (application stricte du compensateur)
	const camMinZoom =
		minZoom !== null && minZoom !== undefined ? minZoom + ZOOM_OFFSET : 0;
	const camMaxZoom =
		maxZoom !== null && maxZoom !== undefined ? maxZoom + ZOOM_OFFSET : 20;

	// 2. Injection des limites (minZoom/maxZoom) dans l'état de la caméra
	let initialViewState = {
		longitude: 2.2137,
		latitude: 46.2276,
		zoom: 5,
		minZoom: camMinZoom,
		maxZoom: camMaxZoom,
	};

	if (dataLength > 0) {
		if (minX === maxX && minY === maxY) {
			// 1 point détecté
			initialViewState = {
				longitude: minX,
				latitude: minY,
				zoom: targetZoom || 16,
				minZoom: camMinZoom,
				maxZoom: camMaxZoom,
			};
		} else {
			// N points : Résolution de la matrice de vue via le WebMercatorViewport

			// 1. Lecture des dimensions physiques du conteneur (fallback à 400px si non monté)
			const rect = el.getBoundingClientRect();
			const width = rect.width || 800;
			const height = rect.height || 400;

			// 2. Instanciation éphémère du calculateur de projection
			const viewport = new deck.WebMercatorViewport({ width, height });

			// 3. Calcul mathématique strict (lon, lat, zoom)
			const fitted = viewport.fitBounds(
				[
					[minX, minY],
					[maxX, maxY],
				],
				{ padding: 40 }, // Marge de sécurité de 40px pour l'ombre des icônes
			);

			initialViewState = {
				longitude: fitted.longitude,
				latitude: fitted.latitude,
				// Verrouillage du zoom généré pour ne pas dépasser la résolution des tuiles (camMaxZoom)
				// Note: fitted.zoom est déjà à l'échelle Deck.gl, le ZOOM_OFFSET n'y est pas nécessaire.
				zoom: Math.min(fitted.zoom, camMaxZoom),
				minZoom: camMinZoom,
				maxZoom: camMaxZoom,
			};
		}
	}

	// Phase 3 : UI déportée
	if (attribution) {
		const attrNode = document.createElement("div");
		// Isolation absolue requise car .map est positionné en absolute par la librairie
		attrNode.style.cssText =
			"position: absolute; bottom: 0; right: 0; z-index: 1; background: rgba(255,255,255,0.9); padding: 2px 5px; font-size: 11px; font-family: sans-serif; pointer-events: auto;";
		attrNode.innerHTML = attribution;
		el.appendChild(attrNode);
	}

	const resolvedServer = parseTileServer(tileServer);

	// Phase 4 : Injection GPU
	new DeckGL({
		container: el,
		initialViewState,
		controller: true,
		getTooltip: ({ object }) => {
			if (!object) return null;

			// DeckGL gère le positionnement absolu du nœud DOM projeté depuis les coordonnées de la souris
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
					pointerEvents: "none", // Empêche l'infobulle de bloquer les événements de survol sous-jacents
				},
			};
		},
		layers: [
			new TileLayer({
				id: `raster-tiles-${el.id || Math.random().toString(16).slice(2)}`,
				data: resolvedServer,
				minZoom,
				maxZoom,
				tileSize: 256,
				renderSubLayers: (props) => {
					const { boundingBox } = props.tile;
					// Utilisation du shader personnalisé au lieu du BitmapLayer standard
					return new ThemedBitmapLayer(props, {
						data: null,
						image: props.data,
						bounds: [
							boundingBox[0][0],
							boundingBox[0][1],
							boundingBox[1][0],
							boundingBox[1][1],
						],
						tileTheme: themeValue, // Injection de la variable d'état
					});
				},
			}),
			new IconLayer({
				id: `vector-markers-${el.id || Math.random().toString(16).slice(2)}`,
				data: gpuData,
				pickable: true,
				// Pointeur dynamique géré par le backend
				iconAtlas: config.atlasUrl,
				iconMapping: ICON_MAPPING,
				getIcon: (d) => d.iconId,
				getPosition: (d) => d.position,
				getSize: 40,
			}),
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
				if (!entry.isIntersecting || entry.intersectionRatio < 0.1) continue;

				const el = entry.target;
				const config = pending.get(el);
				if (!config) continue;

				initMap(config);

				pending.delete(el);
				observer.unobserve(el);
			}
		},
		{ threshold: 0.1 },
	); // Seuil abaissé à 0.1 pour anticiper le chargement réseau des tuiles

	for (let i = 0; i < configs.length; i++) {
		observer.observe(configs[i].el);
	}
};

const bootstrap = () => {
	// Attente de l'évaluation complète du script UMD global
	if (typeof window.deck === "undefined") {
		console.warn("Pipeline AOT: deck.gl global namespace is missing.");
		return;
	}
	const configs = collectMapConfigs();
	if (configs.length) observeMaps(configs);
};

// ─── Export ESM / Auto-Boot ──────────────────────────────────────────────────

export { bootstrap, initMap };

// Exécution automatique si le script est inclus directement dans le DOM
if (typeof document !== "undefined") {
	document.readyState === "loading"
		? document.addEventListener("DOMContentLoaded", bootstrap)
		: bootstrap();
}
